//! A session: one MQTT 5 connection to the broker as one of an identity's
//! three client ids (ADR-0002, ruling R4), the outbox that publishes
//! through it, and the stream of [`ClientEvent`]s it reports.
//!
//! Four tasks run per session. The link task (`crate::link`) owns the
//! broker connection: it handles and acks each publish between polls,
//! reconnects, and swaps the broker token before it expires; a timer task
//! wakes it when the token is due. The replayer (`crate::replay`) fills
//! gaps in the feed and inbox from the hub over HTTP. The sender task here
//! publishes what the outbox holds, again and again with backoff, until the
//! hub answers.

use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use agmen_proto::{ClientEnvelope, IdentityId, MAX_ENVELOPE_BYTES, Stream, Timestamp, Topics};
use rumqttc_next::{
    AckMode, AsyncClient, DisconnectProperties, DisconnectReasonCode, LastWill, MqttOptions,
    PublishOptions, QoS, Transport,
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use url::Url;

use crate::cursor::Cursors;
use crate::error::ClientError;
use crate::event::ClientEvent;
use crate::hub::HubClient;
use crate::link::{CurrentClient, Link, LinkParts, Routes, token_timer};
use crate::mqtt_store::FileSessionStore;
use crate::outbox::Outbox;
use crate::replay;
use crate::state::{StateReads, StateSlot};
use crate::token::{self, TokenSource};

/// The identity's SQLite file in its state folder.
pub const STATE_DB: &str = "agmen.db";

/// MQTT keep-alive (ADR-0003): EMQX drops a silent client after 1.5 times
/// this.
const KEEP_ALIVE_SECS: u16 = 30;
/// The broker's packet cap (ADR-0002), above the 64 KiB envelope limit.
const MAX_PACKET: u32 = 96 * 1024;
/// Requests the MQTT client queues (ADR-0003).
pub(crate) const CLIENT_CAPACITY: usize = 64;
/// How long a disconnected persistent session lasts at the broker (24 h).
const DEFAULT_SESSION_EXPIRY: Duration = Duration::from_secs(86_400);
/// How long the first connect may take, CONNACK and SUBACK together.
pub(crate) const CONNECT_WAIT: Duration = Duration::from_secs(10);
/// How long [`Session::close`] waits for the broker to take the DISCONNECT.
pub(crate) const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// Events waiting for the application. When it stops reading them the
/// session waits, which in turn holds back the broker.
const EVENT_QUEUE: usize = 256;
/// The longest the sender sleeps without looking at the outbox, so a
/// message another process enqueued is sent even when nothing wakes this
/// one.
const SENDER_IDLE: Duration = Duration::from_secs(5);
/// The retained `presence/{id}` a session publishes when it connects
/// (protocol section 2).
pub(crate) const ONLINE: &[u8] = br#"{"status":"online"}"#;
/// Its last will, retained on the same topic.
const OFFLINE: &[u8] = br#"{"status":"offline"}"#;

/// Which of its three client ids a session connects as (ADR-0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ClientKind {
    /// `<id>-mcp`, the MCP server: a persistent session that publishes
    /// and gets delivery replies.
    Mcp,
    /// `<id>-watch`, `agmen watch`: a persistent session that also delivers
    /// the feed and the identity's inbox in order, with its delivery
    /// cursors.
    Watch,
    /// `<id>-cli`, a one-shot command (ruling R4): a clean session that
    /// publishes and gets delivery replies, and leaves nothing at the
    /// broker.
    Cli,
}

impl ClientKind {
    /// The client id suffix.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Watch => "watch",
            Self::Cli => "cli",
        }
    }

    /// Whether the broker keeps the session while the client is away.
    const fn persistent(self) -> bool {
        matches!(self, Self::Mcp | Self::Watch)
    }

    /// Whether the session delivers feed and inbox messages.
    pub(crate) const fn reads_streams(self) -> bool {
        matches!(self, Self::Watch)
    }

    /// Whether the session publishes the identity's presence (protocol
    /// section 2): a retained online on each connect and a retained
    /// offline last will. Only `Watch` does (ruling R65): an identity has
    /// one `presence/{id}`, so if its `Mcp` session also set the will, an
    /// MCP server that stopped or restarted would show the identity offline
    /// while its watch still ran. A one-shot `Cli` command never changes
    /// the roster.
    pub(crate) const fn publishes_presence(self) -> bool {
        matches!(self, Self::Watch)
    }
}

/// Where the broker is: `mqtt://host:port` (plain TCP, as in dev),
/// `mqtts://host:port` (MQTT over TLS) or `wss://host[:port]/path` (MQTT
/// over WebSocket over TLS, as behind HAProxy in the cluster). TLS is
/// checked against the system's root certificates. In JSON, such as the
/// identity file's `broker_url`, it is that URL as a string.
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use agmen_client::Broker;
/// let broker: Broker = "mqtt://localhost:1883".parse()?;
/// assert_eq!(broker.to_string(), "mqtt://localhost:1883");
/// assert!("mqtt://localhost".parse::<Broker>().is_err(), "the port is required");
/// assert!("https://localhost:1883".parse::<Broker>().is_err());
/// # Ok(()) }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct Broker(Url);

impl FromStr for Broker {
    type Err = ClientError;

    fn from_str(s: &str) -> Result<Self, ClientError> {
        let bad = || ClientError::BrokerAddress(s.to_owned());
        let url = Url::parse(s).map_err(|_| bad())?;
        let host = url.host_str().is_some_and(|host| !host.is_empty());
        let ok = match url.scheme() {
            "mqtt" | "mqtts" => host && url.port().is_some(),
            "wss" => host,
            _ => false,
        };
        if ok { Ok(Self(url)) } else { Err(bad()) }
    }
}

impl TryFrom<String> for Broker {
    type Error = ClientError;

    fn try_from(s: String) -> Result<Self, ClientError> {
        s.parse()
    }
}

impl Serialize for Broker {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl fmt::Display for Broker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = self.0.as_str();
        // Url adds a `/` path to mqtt://host:port; print it as given.
        f.write_str(url.strip_suffix('/').unwrap_or(url))
    }
}

impl Broker {
    /// MQTT options for `client_id` at this broker.
    pub(crate) fn options(&self, client_id: &str) -> Result<MqttOptions, ClientError> {
        match (self.0.scheme(), self.0.host_str(), self.0.port()) {
            ("mqtt", Some(host), Some(port)) => Ok(MqttOptions::new(client_id, (host, port))),
            ("mqtts", Some(host), Some(port)) => {
                #[cfg(not(test))]
                let tls = Transport::try_tls_with_default_config();
                // Unit tests trust only their CA and never read the user's keychain.
                #[cfg(test)]
                let tls = Ok::<_, rumqttc_next::TlsError>(tests::mqtt_tls());
                let tls =
                    tls.map_err(|err| ClientError::BrokerAddress(format!("{}: {err}", self.0)))?;
                let mut options = MqttOptions::new(client_id, (host, port));
                options.set_transport(tls);
                Ok(options)
            }
            ("wss", Some(_), _) => {
                MqttOptions::try_websocket_with_default_tls(client_id, self.0.as_str())
                    .map_err(|err| ClientError::BrokerAddress(format!("{}: {err}", self.0)))
            }
            _ => Err(ClientError::BrokerAddress(self.0.to_string())),
        }
    }
}

/// What a session needs: which client id, which project, the hub, the
/// broker, the identity's state folder, and where broker tokens come from.
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use agmen_client::{ClientKind, HubClient, Session, SessionConfig};
/// use agmen_proto::Topics;
///
/// let topics = Topics::new("tenant01".parse()?, "project1".parse()?);
/// let hub = HubClient::new(
///     "https://api.agmen.example".parse()?,
///     topics.project().clone(),
///     "idn7k2q9".parse()?,
///     "the identity's hub token",
/// )?;
/// let config = SessionConfig::new(
///     ClientKind::Watch,
///     topics,
///     hub,
///     "mqtt://localhost:1883".parse()?,
///     "/home/andy/.local/state/agmen/idn7k2q9",
/// );
/// let (session, mut events) = Session::connect(config).await?;
/// while let Some(event) = events.recv().await {
///     println!("{}", serde_json::to_string(&event)?);
/// }
/// session.close().await?;
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
pub struct SessionConfig<T = HubClient> {
    kind: ClientKind,
    topics: Topics,
    hub: HubClient,
    broker: Broker,
    state_dir: PathBuf,
    tokens: T,
    session_expiry: Duration,
}

impl SessionConfig<HubClient> {
    /// A session of `kind` in `topics`' project, as the identity `hub` acts
    /// for, keeping its SQLite file and MQTT session records in `state_dir`
    /// (one folder per identity). Broker tokens come from the hub.
    #[must_use]
    pub fn new(
        kind: ClientKind,
        topics: Topics,
        hub: HubClient,
        broker: Broker,
        state_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            kind,
            topics,
            tokens: hub.clone(),
            hub,
            broker,
            state_dir: state_dir.into(),
            session_expiry: DEFAULT_SESSION_EXPIRY,
        }
    }
}

impl<T> SessionConfig<T> {
    /// Gets broker tokens from `tokens` instead of the hub.
    #[must_use]
    pub fn with_token_source<U: TokenSource>(self, tokens: U) -> SessionConfig<U> {
        SessionConfig {
            kind: self.kind,
            topics: self.topics,
            hub: self.hub,
            broker: self.broker,
            state_dir: self.state_dir,
            tokens,
            session_expiry: self.session_expiry,
        }
    }

    /// How long the broker keeps a persistent session while the client is
    /// away; 24 hours unless set (ADR-0003). Whole seconds count. A
    /// `ClientKind::Cli` session never persists.
    #[must_use]
    pub const fn with_session_expiry(mut self, expiry: Duration) -> Self {
        self.session_expiry = expiry;
        self
    }
}

/// A connected session. Dropping it stops its tasks without a DISCONNECT;
/// [`Session::close`] disconnects cleanly.
#[derive(Debug)]
pub struct Session {
    client: CurrentClient,
    outbox: Outbox,
    cursors: Cursors,
    wake: Arc<Notify>,
    /// Tells the link task that DISCONNECT is coming.
    closing: Arc<AtomicBool>,
    /// Whether the session publishes presence, so its DISCONNECT asks the
    /// broker to send the last will.
    presence: bool,
    state_reads: StateReads,
    tasks: Task<Result<(), ClientError>>,
}

/// A spawned task that is stopped when this is dropped.
#[derive(Debug)]
struct Task<T>(JoinHandle<T>);

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Waits until the link, the replayer or the sender stops, then stops the
/// rest, so a session never runs on with a part missing (a replayer gone
/// would leave the link holding back messages for it forever). Its result
/// is the first task's: `Ok` after a DISCONNECT, else the error that
/// stopped the session.
async fn supervise(
    mut link: Task<Result<(), ClientError>>,
    mut replayer: Task<Result<(), ClientError>>,
    mut sender: Task<Result<(), ClientError>>,
    _timer: Task<()>,
) -> Result<(), ClientError> {
    // The link first: when it stops, the others stop because it did.
    let first = tokio::select! {
        biased;
        ended = &mut link.0 => ended,
        ended = &mut replayer.0 => ended,
        ended = &mut sender.0 => ended,
    };
    match first {
        Ok(result) => result,
        Err(err) if err.is_panic() => Err(ClientError::Panicked(err)),
        Err(_) => Err(ClientError::Closed),
    }
}

/// The events of a session, in the order they happened. It ends when the
/// session stops; [`Session::close`] then says why.
///
/// Read it, or drop it: while it is full the session waits.
#[derive(Debug)]
pub struct Events {
    rx: mpsc::Receiver<ClientEvent>,
}

impl Events {
    /// The next event, or `None` once the session has stopped.
    pub async fn recv(&mut self) -> Option<ClientEvent> {
        self.rx.recv().await
    }
}

impl futures_core::Stream for Events {
    type Item = ClientEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<ClientEvent>> {
        self.rx.poll_recv(cx)
    }
}

impl Session {
    /// Opens the identity's SQLite file, gets a broker token, connects as
    /// `<id>-<kind>` and subscribes. It returns once the broker has granted
    /// the subscriptions, so nothing published to this identity afterwards
    /// is missed.
    ///
    /// A persistent session whose MQTT session record is lost or corrupt
    /// while the broker still holds the session connects once with a clean
    /// start instead; what the broker had queued is then replayed from the
    /// hub.
    ///
    /// # Errors
    ///
    /// [`ClientError`] when the state folder, the token, the connection or
    /// a subscription fails, or the broker does not answer within 10 s.
    pub async fn connect<T: TokenSource>(
        config: SessionConfig<T>,
    ) -> Result<(Self, Events), ClientError> {
        let SessionConfig {
            kind,
            topics,
            hub,
            broker,
            state_dir,
            tokens,
            session_expiry,
        } = config;
        let identity = hub.identity().clone();
        let (outbox, cursors) = open_state(state_dir.clone()).await?;
        let token = tokens.token().await?;
        let refresh_at = token::refresh_at(Timestamp::now(), token::expiry(&token)?);
        let presence = kind
            .publishes_presence()
            .then(|| topics.presence(&identity));
        let options = mqtt_options(
            kind,
            &broker,
            &identity,
            token,
            &state_dir,
            session_expiry,
            presence.as_deref(),
        )?;
        let (client, eventloop) = AsyncClient::builder(options)
            .capacity(CLIENT_CAPACITY)
            .try_build()
            .map_err(ClientError::MqttClient)?;
        let (client_tx, client) = CurrentClient::new(client);

        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
        let (connected_tx, connected_rx) = watch::channel(false);
        let (refresh_tx, refresh_rx) = watch::channel(refresh_at);
        let (ready_tx, ready_rx) = oneshot::channel();
        let closing = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let watched = if kind.reads_streams() {
            vec![Stream::Feed, Stream::Inbox(identity.clone())]
        } else {
            Vec::new()
        };
        let (streams, replayer) = replay::delivery(
            watched,
            cursors.clone(),
            outbox.clone(),
            hub,
            events_tx.clone(),
        );
        let routes = Routes::new(&topics, &identity, kind);
        let state_slot = StateSlot::default();
        let timer = token_timer(client.clone(), routes.ack().to_owned(), refresh_rx);
        let link = Link::new(LinkParts {
            eventloop,
            client: client_tx,
            tokens,
            identity: identity.clone(),
            persistent: kind.persistent(),
            routes: routes.clone(),
            streams,
            events: events_tx.clone(),
            connected: connected_tx,
            refresh_at: refresh_tx,
            ready: Some(ready_tx),
            closing: closing.clone(),
            presence: presence.clone(),
            state_reads: state_slot.clone(),
        });
        let state_reads = StateReads::new(topics.clone(), routes, state_slot, connected_rx.clone());
        let sender = Sender {
            client: client.clone(),
            outbox: outbox.clone(),
            up: topics.up(&identity),
            events: events_tx,
            connected: connected_rx,
            wake: wake.clone(),
        };
        let tasks = Task(tokio::spawn(supervise(
            Task(tokio::spawn(link.run())),
            Task(tokio::spawn(replayer.run())),
            Task(tokio::spawn(sender.run())),
            Task(tokio::spawn(timer)),
        )));
        let session = Self {
            client,
            outbox,
            cursors,
            wake,
            closing,
            presence: presence.is_some(),
            state_reads,
            tasks,
        };
        match tokio::time::timeout(CONNECT_WAIT, ready_rx).await {
            Ok(Ok(Ok(()))) => Ok((session, Events { rx: events_rx })),
            Ok(Ok(Err(err))) => Err(err),
            // The link task ended without saying why: it stopped or panicked.
            Ok(Err(_)) => Err(session.close().await.err().unwrap_or(ClientError::Closed)),
            Err(_) => Err(ClientError::Timeout(CONNECT_WAIT)),
        }
    }

    /// Writes `env` to the outbox and has it published. The hub's answer
    /// arrives as a [`ClientEvent::Delivery`]; until then the outbox sends
    /// it again with backoff, across restarts.
    ///
    /// # Errors
    ///
    /// [`ClientError::EnvelopeTooLarge`] for an envelope over 64 KiB, which
    /// the broker would refuse, and [`ClientError`] when the outbox cannot
    /// store it.
    pub async fn publish(&self, env: ClientEnvelope) -> Result<(), ClientError> {
        let len = serde_json::to_vec(&env).map_err(ClientError::Encode)?.len();
        if len > MAX_ENVELOPE_BYTES {
            return Err(ClientError::EnvelopeTooLarge {
                len,
                limit: MAX_ENVELOPE_BYTES,
            });
        }
        let outbox = self.outbox.clone();
        blocking(move || outbox.enqueue(&env)).await?;
        self.wake.notify_one();
        Ok(())
    }

    /// Reads the project's retained `state/{key}` through this session's
    /// own connection (ruling R68): subscribes, takes the retained message,
    /// and unsubscribes again. Returns the payload as JSON, or `None` when
    /// the broker holds no retained message there (it waits 3 s after the
    /// SUBACK for one). Unlike [`crate::read_state`] it opens no `<id>-cli`
    /// connection, so it never makes the broker drop a one-shot command of
    /// the same identity. Reads on one session run one at a time.
    ///
    /// ```no_run
    /// # async fn example(session: agmen_client::Session) -> Result<(), Box<dyn std::error::Error>> {
    /// let roster = session.read_state("roster").await?.unwrap_or_default();
    /// match agmen_client::resolve_handle(&roster, "adam/coord")? {
    ///     Some(id) => println!("adam/coord is {id}"),
    ///     None => println!("no adam/coord in this project"),
    /// }
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// [`ClientError::StateKey`] when `key` is not one topic level,
    /// [`ClientError::BadState`] when the payload is not JSON,
    /// [`ClientError::Subscribe`] when the broker refuses the subscription,
    /// [`ClientError::Timeout`] carrying 10 s when the broker does not
    /// answer the subscription within that time, or carrying 3 s (the wait
    /// for the retained message) when the connection is down or drops
    /// before the retained message came while the session reconnects,
    /// [`ClientError::StateRead`] when the connection went before the
    /// broker answered, and [`ClientError::Closed`] when the session has
    /// stopped.
    pub async fn read_state(&self, key: &str) -> Result<Option<Value>, ClientError> {
        self.state_reads.read(&self.client, key).await
    }

    /// The identity's outbox.
    #[must_use]
    pub const fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    /// The identity's cursors.
    #[must_use]
    pub const fn cursors(&self) -> &Cursors {
        &self.cursors
    }

    /// Sends DISCONNECT, so the broker keeps a persistent session for its
    /// expiry and forgets a `Cli` one, then stops the session's tasks. A
    /// session that publishes presence disconnects with reason 0x04, so the
    /// broker sends its last will and the roster shows it offline.
    ///
    /// # Errors
    ///
    /// The error that stopped the session earlier, if one did.
    pub async fn close(mut self) -> Result<(), ClientError> {
        // The link stops acking first: rumqttc would send an ack queued
        // after DISCONNECT, and EMQX aborts the connection for it.
        self.closing.store(true, Ordering::SeqCst);
        let closed = tokio::time::timeout(CLOSE_WAIT, async {
            // Fails only when the session has already stopped; its result
            // says why.
            let (reason, properties) = close_disconnect(self.presence);
            let sent = self
                .client
                .current()
                .disconnect_with_properties(reason, properties)
                .await;
            if sent.is_err() {
                tracing::debug!("the session had already stopped");
            }
            (&mut self.tasks.0).await
        })
        .await;
        match closed {
            Ok(Ok(result)) => result,
            Ok(Err(err)) if err.is_panic() => Err(ClientError::Panicked(err)),
            Ok(Err(_)) => Ok(()),
            Err(_) => {
                tracing::warn!("the broker did not take the DISCONNECT within {CLOSE_WAIT:?}");
                Ok(())
            }
        }
    }
}

/// The DISCONNECT that ends a session: reason 0x04 (Disconnect with Will
/// Message) for a session that publishes presence, so the broker sends
/// its last will and the roster shows it offline, else a normal one.
pub(crate) fn close_disconnect(presence: bool) -> (DisconnectReasonCode, DisconnectProperties) {
    let reason = if presence {
        DisconnectReasonCode::DisconnectWithWillMessage
    } else {
        DisconnectReasonCode::NormalDisconnection
    };
    let properties = DisconnectProperties {
        session_expiry_interval: None,
        reason_string: None,
        user_properties: Vec::new(),
        server_reference: None,
    };
    (reason, properties)
}

/// The connection settings of ADR-0003 for `identity`'s `kind` client id,
/// with `token` as the password, and no session kept at the broker.
pub(crate) fn clean_options(
    kind: ClientKind,
    broker: &Broker,
    identity: &IdentityId,
    token: String,
) -> Result<MqttOptions, ClientError> {
    let mut options = broker.options(&format!("{identity}-{}", kind.suffix()))?;
    options
        .set_credentials(identity.as_str(), token)
        .set_ack_mode(AckMode::Manual)
        .set_keep_alive(KEEP_ALIVE_SECS)
        .set_max_packet_size(Some(MAX_PACKET))
        .set_clean_start(true);
    Ok(options)
}

/// [`clean_options`], except that a persistent session keeps its MQTT
/// session record in `state_dir` and lasts `session_expiry` at the broker,
/// and that a session with a `presence` topic leaves a retained offline
/// there as its last will.
fn mqtt_options(
    kind: ClientKind,
    broker: &Broker,
    identity: &IdentityId,
    token: String,
    state_dir: &Path,
    session_expiry: Duration,
    presence: Option<&str>,
) -> Result<MqttOptions, ClientError> {
    let mut options = clean_options(kind, broker, identity, token)?;
    if let Some(topic) = presence {
        options.set_last_will(LastWill::new(
            topic,
            OFFLINE.to_vec(),
            QoS::AtLeastOnce,
            true,
            None,
        ));
    }
    if kind.persistent() {
        let client_id = options.client_id();
        let store = FileSessionStore::new(state_dir.join(format!("{client_id}.mqtt")));
        let expiry = u32::try_from(session_expiry.as_secs()).unwrap_or(u32::MAX);
        options
            .set_session_store_arc(Arc::new(store))
            .set_session_store_scope(broker.to_string())
            .set_clean_start(false)
            .set_session_expiry_interval(Some(expiry));
    }
    Ok(options)
}

/// Creates `dir` and opens the outbox and the cursors in its SQLite file.
async fn open_state(dir: PathBuf) -> Result<(Outbox, Cursors), ClientError> {
    blocking(move || {
        std::fs::create_dir_all(&dir).map_err(|source| ClientError::StateDir {
            path: dir.clone(),
            source,
        })?;
        let db = dir.join(STATE_DB);
        Ok((Outbox::open(&db)?, Cursors::open(&db)?))
    })
    .await
}

/// Runs `work` (SQLite or file I/O) on a blocking thread.
pub(crate) async fn blocking<R: Send + 'static>(
    work: impl FnOnce() -> Result<R, ClientError> + Send + 'static,
) -> Result<R, ClientError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(ClientError::Panicked)?
}

/// Sends `event` to the application. An application that dropped its
/// [`Events`] does not want them, so a closed queue is not an error.
pub(crate) async fn emit(events: &mpsc::Sender<ClientEvent>, event: ClientEvent) {
    if events.send(event).await.is_err() {
        tracing::trace!("the application dropped its events");
    }
}

/// Publishes what the outbox holds while the session is connected.
struct Sender {
    client: CurrentClient,
    outbox: Outbox,
    up: String,
    events: mpsc::Sender<ClientEvent>,
    connected: watch::Receiver<bool>,
    wake: Arc<Notify>,
}

impl Sender {
    async fn run(mut self) -> Result<(), ClientError> {
        loop {
            // Wait for a connection; the link task's end closes the watch.
            if self.connected.wait_for(|up| *up).await.is_err() {
                return Ok(());
            }
            // What a lost connection may have dropped goes out at once.
            let outbox = self.outbox.clone();
            blocking(move || outbox.retry_now(Timestamp::now())).await?;
            while *self.connected.borrow_and_update() {
                self.send_due().await?;
                let outbox = self.outbox.clone();
                let next = blocking(move || outbox.next_due()).await?;
                let wait = next
                    .and_then(|at| Duration::try_from(at.duration_since(Timestamp::now())).ok())
                    .map_or(SENDER_IDLE, |wait| wait.min(SENDER_IDLE));
                tokio::select! {
                    () = self.wake.notified() => {}
                    () = tokio::time::sleep(wait) => {}
                    changed = self.connected.changed() => {
                        if changed.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    /// Publishes every due envelope once and records the attempt.
    async fn send_due(&self) -> Result<(), ClientError> {
        let outbox = self.outbox.clone();
        let due = blocking(move || outbox.due(Timestamp::now())).await?;
        for env in due {
            if !*self.connected.borrow() {
                break;
            }
            let payload = serde_json::to_vec(&env).map_err(ClientError::Encode)?;
            let published = self
                .client
                .current()
                .publish(self.up.clone(), payload, PublishOptions::at_least_once())
                .await;
            if let Err(err) = published {
                // A token rotation replaced the client while this waited
                // for room; the outbox sends it again on the next connect.
                // When the link has stopped, the wait for a connection
                // ends the sender.
                tracing::debug!(error = %err, "the client refused a publish");
                break;
            }
            let (outbox, id) = (self.outbox.clone(), env.id.clone());
            let state = blocking(move || outbox.sent(&id, Timestamp::now())).await?;
            emit(&self.events, ClientEvent::Delivery { id: env.id, state }).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_sessions_leave_an_offline_will() {
        assert!(ClientKind::Watch.publishes_presence());
        assert!(!ClientKind::Mcp.publishes_presence());
        assert!(!ClientKind::Cli.publishes_presence());
        let broker: Broker = "mqtt://localhost:1883".parse().unwrap();
        let id: IdentityId = "idn7k2q9".parse().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let topic = "agmen/v1/tenant01/project1/presence/idn7k2q9";
        let options = |kind, presence| {
            mqtt_options(
                kind,
                &broker,
                &id,
                "token".to_owned(),
                dir.path(),
                DEFAULT_SESSION_EXPIRY,
                presence,
            )
            .unwrap()
        };
        let will = options(ClientKind::Watch, Some(topic)).last_will().unwrap();
        assert_eq!(&will.topic[..], topic.as_bytes());
        assert_eq!(&will.message[..], OFFLINE);
        assert!(will.retain);
        assert_eq!(will.qos, QoS::AtLeastOnce);
        assert!(options(ClientKind::Mcp, None).last_will().is_none());
        assert!(options(ClientKind::Cli, None).last_will().is_none());
    }

    #[test]
    fn mqtts_connects_over_tls() {
        let broker: Broker = "mqtts://agmen.example.com:8883".parse().unwrap();
        assert_eq!(broker.to_string(), "mqtts://agmen.example.com:8883");
        let options = broker.options("idn7k2q9-cli").unwrap();
        assert!(matches!(options.transport(), Transport::Tls(_)));
        let plain: Broker = "mqtt://localhost:1883".parse().unwrap();
        assert!(matches!(
            plain.options("idn7k2q9-cli").unwrap().transport(),
            Transport::Tcp
        ));
    }

    pub(super) fn mqtt_tls() -> Transport {
        use rumqttc_next::tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto};

        // Only the public CA certificate is kept; its private key was discarded.
        let mut roots = RootCertStore::empty();
        roots
            .add(include_bytes!("test_data/mqtt-ca.der").to_vec().into())
            .unwrap();
        assert_eq!(roots.len(), 1);
        let config =
            ClientConfig::builder_with_provider(Arc::new(crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
        Transport::tls_with_config(config.into())
    }
}
