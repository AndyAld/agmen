//! The link task: owns the broker connection of one session. It polls the
//! event loop, handles each publish as it arrives (`crate::replay`), acks
//! it, reconnects after a failure with a bounded backoff, swaps the broker
//! token before it expires, and falls back to a clean start when the
//! session record is lost.
//!
//! The task never cancels `EventLoop::poll`: a poll dropped halfway can
//! leave rumqttc-next's state machine between steps (one dropped while it
//! sent DISCONNECT left the DISCONNECT unsent and the loop running). So
//! everything happens between polls, and a small timer task wakes the poll
//! when the token is due.
//!
//! Acks happen only here, between polls, for publishes that arrived on the
//! current connection. rumqttc-next acks by packet id, and after a
//! reconnect the same id can name a different publish; an ack held over
//! from the old connection would ack a message nobody handled. rumqttc
//! drops the acks still queued when a connection fails
//! (`EventLoop::clean`), and this task drops the ones it still holds. A
//! dropped ack costs nothing: a resumed session gets the publish again and
//! the cursor drops the duplicate, and a lost session is covered by replay.
//!
//! A token rotation replaces the MQTT client and its event loop (see
//! [`Link::rotate_token`]), so the session's other tasks reach the client
//! through [`CurrentClient`].

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agmen_proto::{IdentityId, Stream, Timestamp, Topics};
use rumqttc_next::{
    AsyncClient, ConnectionError, Event, EventLoop, Outgoing, Packet, Publish, PublishOptions, QoS,
    SubscribeFilter, SubscribeReasonCode,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::error::ClientError;
use crate::event::ClientEvent;
use crate::replay::Streams;
use crate::session::{CLIENT_CAPACITY, CLOSE_WAIT, ClientKind, ONLINE, close_disconnect, emit};
use crate::token::{self, TokenSource};

/// Failed connection attempts in a row before the session gives up.
const MAX_FAILURES: u32 = 60;
/// The first reconnect delay; it doubles up to [`MAX_RETRY_DELAY`].
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
/// How long to wait before asking for a token again after the hub could
/// not be reached.
const TOKEN_RETRY: Duration = Duration::from_secs(30);
/// The longest single wait of the token timer.
const MAX_TIMER: Duration = Duration::from_secs(24 * 3600);

/// What a publish is, by its topic.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    /// An ACK or NACK on `ack/{id}`.
    Reply,
    /// The retained `state/head`.
    Head,
    /// A message on `feed` or `inbox/{id}`, which is this stream.
    Message(Stream),
}

/// The topics a session subscribes to, and what arrives on each.
#[derive(Debug, Clone)]
pub(crate) struct Routes {
    identity: IdentityId,
    ack: String,
    /// `state/head`, `feed` and `inbox/{id}`, for a session that reads them.
    streams: Option<(String, String, String)>,
}

impl Routes {
    pub(crate) fn new(topics: &Topics, identity: &IdentityId, kind: ClientKind) -> Self {
        Self {
            identity: identity.clone(),
            ack: topics.ack(identity),
            streams: kind
                .reads_streams()
                .then(|| (topics.state_head(), topics.feed(), topics.inbox(identity))),
        }
    }

    /// The `ack/{id}` topic.
    pub(crate) fn ack(&self) -> &str {
        &self.ack
    }

    fn filters(&self) -> Vec<SubscribeFilter> {
        let mut filters = vec![self.ack.as_str()];
        if let Some((head, feed, inbox)) = &self.streams {
            filters.extend([head.as_str(), feed.as_str(), inbox.as_str()]);
        }
        filters
            .into_iter()
            .map(|filter| SubscribeFilter::new(filter, QoS::AtLeastOnce))
            .collect()
    }

    fn route(&self, topic: &str) -> Option<Route> {
        if topic == self.ack {
            return Some(Route::Reply);
        }
        let (head, feed, inbox) = self.streams.as_ref()?;
        if topic == head {
            Some(Route::Head)
        } else if topic == feed {
            Some(Route::Message(Stream::Feed))
        } else if topic == inbox {
            Some(Route::Message(Stream::Inbox(self.identity.clone())))
        } else {
            None
        }
    }
}

/// The session's current MQTT client, for the tasks other than the link.
/// A token rotation replaces it, so they ask for it each time they use it.
#[derive(Debug, Clone)]
pub(crate) struct CurrentClient(watch::Receiver<AsyncClient>);

impl CurrentClient {
    /// A handle to read `client`, and the sender the link replaces it on.
    pub(crate) fn new(client: AsyncClient) -> (watch::Sender<AsyncClient>, Self) {
        let (tx, rx) = watch::channel(client);
        (tx, Self(rx))
    }

    /// The client in use now.
    pub(crate) fn current(&self) -> AsyncClient {
        self.0.borrow().clone()
    }
}

/// What the link task needs; [`Link::new`] adds its own state.
pub(crate) struct LinkParts<T> {
    pub(crate) eventloop: EventLoop,
    /// The client of `eventloop`; [`CurrentClient`] reads it.
    pub(crate) client: watch::Sender<AsyncClient>,
    pub(crate) tokens: T,
    pub(crate) identity: IdentityId,
    pub(crate) persistent: bool,
    pub(crate) routes: Routes,
    pub(crate) streams: Streams,
    pub(crate) events: mpsc::Sender<ClientEvent>,
    pub(crate) connected: watch::Sender<bool>,
    /// When to swap the current broker token; the token timer watches it.
    pub(crate) refresh_at: watch::Sender<Timestamp>,
    /// Told the outcome of the first connect, then dropped.
    pub(crate) ready: Option<oneshot::Sender<Result<(), ClientError>>>,
    /// Set by `Session::close` before it queues DISCONNECT.
    pub(crate) closing: Arc<AtomicBool>,
    /// The identity's `presence/{id}`, for a session that publishes
    /// presence.
    pub(crate) presence: Option<String>,
}

/// The link task.
pub(crate) struct Link<T> {
    p: LinkParts<T>,
    /// Which connection is current, and whether it is up.
    gate: AckGate,
    /// Whether the application was last told the link is up.
    announced: bool,
    /// Failed attempts in a row.
    failures: u32,
    /// Handled publishes whose acks did not fit the client's queue yet,
    /// each with the connection it arrived on.
    unacked: VecDeque<(Publish, u64)>,
    /// Subscribe requests waiting for room in the client's queue.
    requests: JoinSet<()>,
}

/// Whether the link keeps running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

impl<T: TokenSource> Link<T> {
    pub(crate) fn new(parts: LinkParts<T>) -> Self {
        Self {
            p: parts,
            gate: AckGate::default(),
            announced: false,
            failures: 0,
            unacked: VecDeque::new(),
            requests: JoinSet::new(),
        }
    }

    /// Runs until the session disconnects (`Ok`) or the link fails for good.
    pub(crate) async fn run(mut self) -> Result<(), ClientError> {
        loop {
            while self.requests.try_join_next().is_some() {}
            self.flush_acks();
            let due = Timestamp::now() >= *self.p.refresh_at.borrow();
            if due && self.gate.is_up() && !self.closing() {
                self.rotate_token().await?;
            }
            let polled = self.p.eventloop.poll().await;
            if self.on_poll(polled).await? == Flow::Stop {
                return Ok(());
            }
        }
    }

    fn closing(&self) -> bool {
        self.p.closing.load(Ordering::SeqCst)
    }

    async fn on_poll(
        &mut self,
        polled: Result<Event, ConnectionError>,
    ) -> Result<Flow, ClientError> {
        match polled {
            Ok(Event::Incoming(Packet::ConnAck(_))) => self.on_connected().await,
            Ok(Event::Incoming(Packet::SubAck(ack))) => self.on_suback(&ack.return_codes),
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                self.on_publish(publish).await?;
                Ok(Flow::Continue)
            }
            Ok(_) => Ok(Flow::Continue),
            Err(ConnectionError::RequestsDone) => Ok(Flow::Stop),
            // After DISCONNECT the broker may close first; either way the
            // session is over, so do not reconnect.
            Err(err) if self.closing() => {
                tracing::debug!(error = %err, "the connection ended while closing");
                Ok(Flow::Stop)
            }
            Err(err) => self.on_error(err).await,
        }
    }

    async fn on_connected(&mut self) -> Result<Flow, ClientError> {
        self.gate.connected();
        self.failures = 0;
        if self.p.persistent && self.p.eventloop.options.clean_start() {
            // A clean start replaced a session whose record was lost; keep
            // the new one from now on.
            self.p.eventloop.options.set_clean_start(false);
        }
        self.p.connected.send_replace(true);
        if !self.announced {
            self.announced = true;
            emit(&self.p.events, ClientEvent::Connection { connected: true }).await;
        }
        // Subscribing again on a resumed session changes nothing but makes
        // the broker send the retained state/head again. The request waits
        // on another task: this one must keep polling to drain the queue.
        let (client, filters) = (self.client(), self.p.routes.filters());
        self.requests.spawn(async move {
            if let Err(err) = client.subscribe_many(filters).await {
                tracing::warn!(error = %err, "queueing the subscribe failed");
            }
        });
        // Online again on every connect: the last will of a lost connection
        // set the retained presence offline.
        if let Some(topic) = self.p.presence.clone() {
            let client = self.client();
            self.requests.spawn(async move {
                let retained = PublishOptions::at_least_once().retained();
                if let Err(err) = client.publish(topic, ONLINE.to_vec(), retained).await {
                    tracing::warn!(error = %err, "queueing the online presence failed");
                }
            });
        }
        self.p.streams.on_connected();
        Ok(Flow::Continue)
    }

    fn on_suback(&mut self, codes: &[SubscribeReasonCode]) -> Result<Flow, ClientError> {
        if let Some(refused) = codes
            .iter()
            .find(|code| !matches!(code, SubscribeReasonCode::Success(_)))
        {
            let err = ClientError::Subscribe(format!("{refused:?}"));
            return match self.p.ready.take() {
                Some(ready) => {
                    report(ready, Err(err));
                    Ok(Flow::Stop)
                }
                None => Err(err),
            };
        }
        if let Some(ready) = self.p.ready.take() {
            report(ready, Ok(()));
        }
        Ok(Flow::Continue)
    }

    /// Handles `publish`, then acks it.
    async fn on_publish(&mut self, publish: Publish) -> Result<(), ClientError> {
        let generation = self.gate.generation();
        let topic = String::from_utf8_lossy(&publish.topic).into_owned();
        let streams = &self.p.streams;
        match self.p.routes.route(&topic) {
            Some(Route::Reply) => streams.on_reply(&publish.payload).await?,
            Some(Route::Head) => streams.on_head(&publish.payload).await?,
            Some(Route::Message(stream)) => streams.on_message(stream, &publish.payload).await?,
            None => {
                tracing::warn!(%topic, "dropped a publish on a topic this session does not read")
            }
        }
        self.unacked.push_back((publish, generation));
        self.flush_acks();
        Ok(())
    }

    async fn on_error(&mut self, err: ConnectionError) -> Result<Flow, ClientError> {
        let lost_record = self.p.persistent
            && matches!(
                err,
                ConnectionError::SessionStateMismatch {
                    session_present: true,
                    ..
                }
            );
        if let Some(ready) = self.p.ready.take() {
            // The first connect: fall back once, else fail fast so the
            // caller hears why.
            if lost_record && !self.p.eventloop.options.clean_start() {
                self.fall_back();
                self.p.ready = Some(ready);
                return Ok(Flow::Continue);
            }
            report(ready, Err(ClientError::Connect(err)));
            return Ok(Flow::Stop);
        }
        self.went_down().await;
        self.failures += 1;
        if self.failures >= MAX_FAILURES {
            return Err(ClientError::GaveUp {
                attempts: self.failures,
                source: err,
            });
        }
        if lost_record {
            self.fall_back();
            return Ok(Flow::Continue);
        }
        tracing::warn!(error = %err, failures = self.failures, "broker connection lost; reconnecting");
        tokio::time::sleep(retry_delay(self.failures)).await;
        if Timestamp::now() >= *self.p.refresh_at.borrow() {
            self.refresh_token().await?;
        }
        Ok(Flow::Continue)
    }

    /// The broker holds a session this client has no record of (the
    /// record file was lost or corrupt). Connect once with a clean start,
    /// which drops the broker's session and its queue; replay delivers what
    /// was queued, from the cursors.
    fn fall_back(&mut self) {
        tracing::warn!(
            "the MQTT session record is missing; starting a new session and replaying from the hub"
        );
        self.p.eventloop.options.set_clean_start(true);
    }

    async fn went_down(&mut self) {
        self.gate.lost();
        self.p.connected.send_replace(false);
        if self.announced {
            self.announced = false;
            emit(&self.p.events, ClientEvent::Connection { connected: false }).await;
        }
    }

    /// The current client.
    fn client(&self) -> AsyncClient {
        self.p.client.borrow().clone()
    }

    /// The token is due for renewal while connected. MQTT carries the
    /// password only in CONNECT, so reconnect with the fresh token now,
    /// rather than be dropped by the broker at `exp`.
    ///
    /// The old connection ends with a normal DISCONNECT (0x00), which makes
    /// the broker discard the last will, so the roster never shows the
    /// identity offline for a rotation. rumqttc-next ends an event loop for
    /// good once it has sent DISCONNECT, so the session goes on with a new
    /// client and event loop on the same options; a persistent one resumes
    /// its broker session from the session store, as after a restart.
    /// Publishes queued on the old client are dropped: the outbox sends
    /// them again on the next connect, and a dropped ack costs nothing (see
    /// the module docs).
    async fn rotate_token(&mut self) -> Result<(), ClientError> {
        if !self.refresh_token().await? {
            return Ok(());
        }
        tracing::info!("reconnecting with a fresh broker token");
        self.gate.lost();
        self.p.connected.send_replace(false);
        let queued = self.p.client.borrow().try_disconnect();
        match queued {
            Ok(()) => match tokio::time::timeout(CLOSE_WAIT, self.finish_disconnect()).await {
                Ok(finished) => finished?,
                // The timeout may cut a poll short (see the module docs),
                // which is harmless here: the event loop is replaced below.
                // Dropping it closes the socket without DISCONNECT, so the
                // broker sends the last will then.
                Err(_) => tracing::warn!(
                    "the broker did not take the DISCONNECT within {CLOSE_WAIT:?}; the last will may fire"
                ),
            },
            Err(err) => {
                tracing::warn!(error = %err, "queueing the DISCONNECT failed; the last will may fire");
            }
        }
        let options = self.p.eventloop.options.clone();
        let (client, eventloop) = AsyncClient::builder(options)
            .capacity(CLIENT_CAPACITY)
            .try_build()
            .map_err(ClientError::MqttClient)?;
        // Dropping the old event loop closes its socket.
        self.p.eventloop = eventloop;
        self.p.client.send_replace(client);
        if self.closing() {
            // Session::close may have queued its DISCONNECT on the old
            // client. Queue it on the new one, which connects only to send
            // it, so a presence session still leaves its will.
            let (reason, properties) = close_disconnect(self.p.presence.is_some());
            let queued = self
                .p
                .client
                .borrow()
                .try_disconnect_with_properties(reason, properties);
            if let Err(err) = queued {
                tracing::warn!(error = %err, "queueing the closing DISCONNECT failed");
            }
        }
        Ok(())
    }

    /// Polls the old event loop until it has sent DISCONNECT, or until its
    /// connection ends. Publishes that arrive meanwhile are handled but not
    /// acked (the gate is down), so a resumed session gets them again and
    /// the cursor drops the duplicate. The caller bounds the wait.
    async fn finish_disconnect(&mut self) -> Result<(), ClientError> {
        loop {
            match self.p.eventloop.poll().await {
                Ok(Event::Outgoing(Outgoing::Disconnect)) => return Ok(()),
                Ok(Event::Incoming(Packet::Publish(publish))) => self.on_publish(publish).await?,
                Ok(_) => {}
                Err(err) => {
                    tracing::debug!(error = %err, "the connection ended while disconnecting");
                    return Ok(());
                }
            }
        }
    }

    /// Fetches a fresh token for the next connect. `Ok(false)` when the hub
    /// could not be reached; it is asked again later.
    ///
    /// # Errors
    ///
    /// The hub's refusal (revoked identity), which is final.
    async fn refresh_token(&mut self) -> Result<bool, ClientError> {
        let fetched = match self.p.tokens.token().await {
            Ok(token) => token::expiry(&token).map(|exp| (token, exp)),
            Err(err) => Err(err),
        };
        let now = Timestamp::now();
        match fetched {
            Ok((token, exp)) => {
                self.p.refresh_at.send_replace(token::refresh_at(now, exp));
                self.p
                    .eventloop
                    .options
                    .set_credentials(self.p.identity.as_str(), token);
                Ok(true)
            }
            Err(err) if err.is_refusal() => Err(err),
            Err(err) => {
                tracing::warn!(error = %err, "fetching a broker token failed; trying again later");
                let retry = jiff::SignedDuration::try_from(TOKEN_RETRY).unwrap_or_default();
                self.p
                    .refresh_at
                    .send_replace(now.checked_add(retry).unwrap_or(now));
                Ok(false)
            }
        }
    }

    /// Acks the handled publishes, dropping those from an earlier
    /// connection (see the module docs) and all of them once the session is
    /// closing: rumqttc sends an ack queued after DISCONNECT anyway, and EMQX
    /// aborts the connection for it.
    fn flush_acks(&mut self) {
        while let Some((publish, generation)) = self.unacked.pop_front() {
            if self.closing() || !self.gate.admits(generation) {
                tracing::debug!(pkid = publish.pkid, "not acking a publish");
                continue;
            }
            let acked = self.p.client.borrow().try_ack(&publish);
            match acked {
                Ok(()) => {}
                Err(rumqttc_next::ClientError::RequestChannelFull(_)) => {
                    // The next poll drains the queue; try again then.
                    self.unacked.push_front((publish, generation));
                    break;
                }
                Err(err) => tracing::warn!(error = %err, "queueing an ack failed"),
            }
        }
    }
}

/// Which connection is current. A publish may be acked only while the
/// connection it arrived on is up (see the module docs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AckGate {
    /// Counts CONNACKs.
    generation: u64,
    up: bool,
}

impl AckGate {
    /// A CONNACK: a new connection is current.
    const fn connected(&mut self) {
        self.generation += 1;
        self.up = true;
    }

    /// The current connection is gone.
    const fn lost(&mut self) {
        self.up = false;
    }

    const fn is_up(self) -> bool {
        self.up
    }

    /// The connection a publish arriving now arrives on.
    const fn generation(self) -> u64 {
        self.generation
    }

    /// Whether a publish that arrived on connection `generation` may be
    /// acked now.
    const fn admits(self, generation: u64) -> bool {
        self.up && generation == self.generation
    }
}

/// Wakes the link's poll when the broker token is due, by subscribing to
/// `ack/{id}` again: a request makes the poll return, the link then sees
/// the token is due between polls, and subscribing again to a topic the
/// session holds changes nothing. Ends when the link task does.
pub(crate) async fn token_timer(
    client: CurrentClient,
    ack: String,
    mut refresh_at: watch::Receiver<Timestamp>,
) {
    loop {
        let at = *refresh_at.borrow_and_update();
        tokio::select! {
            () = tokio::time::sleep_until(instant_at(at)) => {
                // A client replaced meanwhile refuses the request; the link
                // has rotated then, and moves the time on below.
                if let Err(err) = client.current().subscribe(ack.clone(), QoS::AtLeastOnce).await {
                    tracing::debug!(error = %err, "waking the link failed");
                }
                // Wait for the link to move the time on before waking it
                // again.
                if refresh_at.changed().await.is_err() {
                    return;
                }
            }
            changed = refresh_at.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// Tells `Session::connect` how the first connect went. It may have given
/// up waiting already, in which case nobody needs the answer.
fn report(ready: oneshot::Sender<Result<(), ClientError>>, outcome: Result<(), ClientError>) {
    if ready.send(outcome).is_err() {
        tracing::debug!("the first connect finished after the caller stopped waiting");
    }
}

/// The reconnect delay after `failures` failed attempts in a row.
fn retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    FIRST_RETRY_DELAY
        .saturating_mul(1 << doublings)
        .min(MAX_RETRY_DELAY)
}

/// The tokio instant of wall-clock time `at`; now for a time past.
fn instant_at(at: Timestamp) -> Instant {
    let wait = Duration::try_from(at.duration_since(Timestamp::now())).unwrap_or(Duration::ZERO);
    Instant::now() + wait.min(MAX_TIMER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delays_double_to_the_cap() {
        assert_eq!(retry_delay(1), Duration::from_secs(1));
        assert_eq!(retry_delay(3), Duration::from_secs(4));
        assert_eq!(retry_delay(6), MAX_RETRY_DELAY);
        assert_eq!(retry_delay(u32::MAX), MAX_RETRY_DELAY);
    }

    #[test]
    fn acks_only_on_the_connection_a_publish_came_in_on() {
        let mut gate = AckGate::default();
        gate.connected();
        let before = gate.generation();
        assert!(gate.admits(before));
        // The link drops; while it is down nothing is acked.
        gate.lost();
        assert!(!gate.admits(before));
        // After the reconnect the old packet id may name another publish.
        gate.connected();
        assert!(!gate.admits(before), "a stale ack is refused");
        assert!(gate.admits(gate.generation()));
        // A token swap drops the link the same way.
        let current = gate.generation();
        gate.lost();
        gate.connected();
        assert!(!gate.admits(current));
    }

    #[test]
    fn routes_by_topic() {
        let topics = Topics::new("tenant01".parse().unwrap(), "project1".parse().unwrap());
        let me: IdentityId = "idn7k2q9".parse().unwrap();
        let other: IdentityId = "idother1".parse().unwrap();
        let watch = Routes::new(&topics, &me, ClientKind::Watch);
        assert_eq!(watch.route(&topics.ack(&me)), Some(Route::Reply));
        assert_eq!(watch.route(&topics.state_head()), Some(Route::Head));
        assert_eq!(
            watch.route(&topics.feed()),
            Some(Route::Message(Stream::Feed))
        );
        assert_eq!(
            watch.route(&topics.inbox(&me)),
            Some(Route::Message(Stream::Inbox(me.clone())))
        );
        assert_eq!(watch.route(&topics.inbox(&other)), None);
        assert_eq!(watch.filters().len(), 4);
        let cli = Routes::new(&topics, &me, ClientKind::Cli);
        assert_eq!(cli.route(&topics.ack(&me)), Some(Route::Reply));
        assert_eq!(cli.route(&topics.feed()), None);
        assert_eq!(cli.filters().len(), 1);
    }
}
