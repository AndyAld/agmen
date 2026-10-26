//! The hub's broker connection (ADR-0002): one MQTT 5 client, username
//! `agmen-hub` and client id `agmen-hub-<instance>`, with the hub's own
//! broker token. It reads every project's `up/` topics through a shared
//! subscription and hands each publish to ingest; the sweeper publishes
//! through the same connection.
//!
//! The session is a clean one: while the hub is away nothing queues for
//! it, so a client's publish in that gap gets no ACK and its outbox
//! retries it (protocol 5.1). Each publish from `up/` is acked to the
//! broker only after ingest has answered it, so a hub that dies mid-ingest
//! leaves the publish to another member of the share group.

use std::time::Duration;

use agmen_proto::{Topics, parse_up};
use rumqttc_next::{
    AckMode, AsyncClient, ConnectionError, Event, EventLoop, MqttOptions, Packet, Publish,
    PublishOptions, QoS, SubscribeReasonCode,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::broker_tokens::{HUB_USERNAME, TokenError};
use crate::config::MqttConfig;
use crate::http::Hub;
use crate::ingest::{IngestError, ReplyRoute};

/// How long each of the hub's broker tokens lasts (ADR-0002). EMQX
/// disconnects the hub at expiry and it reconnects with a fresh one.
const HUB_TOKEN_TTL: Duration = Duration::from_secs(3600);
/// MQTT keep-alive, as for clients (ADR-0003).
const KEEP_ALIVE_SECS: u16 = 30;
/// The broker's packet cap (ADR-0002), above the 64 KiB envelope limit.
const MAX_PACKET: u32 = 96 * 1024;
/// How long one step of the first connect (the CONNACK, or the SUBACK) may
/// take.
const CONNECT_WAIT: Duration = Duration::from_secs(10);
/// Failed first connect attempts before the hub gives up: about two
/// minutes with [`retry_delay`].
const FIRST_CONNECT_FAILURES: u32 = 10;
/// Failed connection attempts in a row before the hub gives up and exits.
const MAX_FAILURES: u32 = 60;
/// The first reconnect delay; it doubles up to [`MAX_RETRY_DELAY`].
const FIRST_RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
/// Requests the client queues, and publishes waiting for ingest. EMQX sends
/// at most `max_inflight` (32 in dev) unacked publishes, so the channel to
/// ingest does not fill.
const QUEUE: usize = 64;

/// Why messaging could not start or stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MessagingError {
    /// The hub's broker token could not be signed.
    #[error("minting the hub's broker token failed")]
    Token(#[source] TokenError),
    /// The MQTT client settings are invalid.
    #[error("the MQTT client settings are invalid")]
    Client(#[source] rumqttc_next::ClientBuildError),
    /// The broker refused or dropped the last of the first connect
    /// attempts.
    #[error("connecting to the broker failed")]
    Connect(#[source] ConnectionError),
    /// The broker did not answer a step of the last first connect attempt
    /// in time.
    #[error("the broker did not answer within {CONNECT_WAIT:?}")]
    Timeout,
    /// The broker refused the `up/` subscription.
    #[error("the broker refused the subscription to {filter}: {reason}")]
    Subscribe {
        /// The filter.
        filter: String,
        /// The SUBACK reason.
        reason: String,
    },
    /// The connection failed 60 times in a row.
    #[error("the broker connection failed {attempts} times in a row")]
    GaveUp {
        /// Attempts made.
        attempts: u32,
        /// The last failure.
        #[source]
        source: ConnectionError,
    },
    /// The sweeper failed too many times in a row.
    #[error("the sweeper stopped")]
    Sweeper(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A task ended because another one it depends on ended.
    #[error("the {0} task stopped")]
    Stopped(&'static str),
    /// A task panicked.
    #[error("a messaging task panicked")]
    Panicked(#[source] tokio::task::JoinError),
}

/// The running broker connection, ingest worker and sweeper. Dropping it
/// stops all three.
#[derive(Debug)]
pub struct Messaging {
    tasks: JoinSet<Result<(), MessagingError>>,
}

impl Messaging {
    /// Waits until a task stops, which only happens when it fails, and
    /// returns why. The caller should then exit.
    pub async fn failed(&mut self) -> MessagingError {
        match self.tasks.join_next().await {
            Some(Ok(Err(err))) => err,
            Some(Err(err)) => MessagingError::Panicked(err),
            Some(Ok(Ok(()))) | None => MessagingError::Stopped("messaging"),
        }
    }
}

impl Hub {
    /// Connects to the broker as `agmen-hub-<instance>`, subscribes to
    /// `$share/<group>/agmen/v1/+/+/up/+`, and starts ingest and the
    /// sweeper. It returns once the subscription is granted, so nothing
    /// published to `up/` afterwards is missed. A failed, refused or
    /// unanswered connect step is retried with backoff, about two minutes
    /// in all: in the cluster EMQX reads this hub's own `/v1/jwks`, so after
    /// a cold start it refuses the hub until it has fetched the keys.
    ///
    /// # Errors
    ///
    /// [`MessagingError`] when the token, the client, the connection or the
    /// subscription fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let config = agmen_hub::Config::from_env()?;
    /// let hub = agmen_hub::Hub::connect(&config).await?;
    /// let mut messaging = hub.start_messaging(&config.mqtt).await?;
    /// return Err(messaging.failed().await.into());
    /// # }
    /// ```
    pub async fn start_messaging(&self, config: &MqttConfig) -> Result<Messaging, MessagingError> {
        let token = self
            .tokens()
            .mint_hub(HUB_TOKEN_TTL)
            .map_err(MessagingError::Token)?;
        let mut options = MqttOptions::new(config.client_id(), (config.host.clone(), config.port));
        options
            .set_credentials(HUB_USERNAME, token)
            .set_clean_start(true)
            .set_ack_mode(AckMode::Manual)
            .set_keep_alive(KEEP_ALIVE_SECS)
            .set_max_packet_size(Some(MAX_PACKET));
        let (client, mut eventloop) = AsyncClient::builder(options)
            .capacity(QUEUE)
            .try_build()
            .map_err(MessagingError::Client)?;
        let filter = config.up_filter();
        first_connect(&client, &mut eventloop, &filter, CONNECT_WAIT).await?;
        tracing::info!(client_id = %config.client_id(), %filter, "subscribed to up/");
        let (tx, rx) = mpsc::channel(QUEUE);
        let mut tasks = JoinSet::new();
        tasks.spawn(run_connection(
            eventloop,
            client.clone(),
            self.clone(),
            filter,
            tx,
        ));
        tasks.spawn(run_ingest(self.clone(), client.clone(), rx));
        tasks.spawn(crate::sweeper::run(self.clone(), client));
        Ok(Messaging { tasks })
    }
}

/// Polls until CONNACK, subscribes, and polls until the SUBACK, retrying a
/// failed connection, or a step that takes longer than `wait`, up to
/// [`FIRST_CONNECT_FAILURES`] times.
async fn first_connect(
    client: &AsyncClient,
    eventloop: &mut EventLoop,
    filter: &str,
    wait: Duration,
) -> Result<(), MessagingError> {
    let mut subscribed = false;
    let mut failures = 0_u32;
    loop {
        // A step that times out counts as a failure like a refused connect:
        // a broker that is still starting may accept TCP and answer late.
        // Polling again after the timeout reconnects if the connect itself
        // was cut short, since rumqttc keeps no connection until CONNACK.
        let error = match tokio::time::timeout(wait, eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) if !subscribed => {
                subscribe(client, filter)?;
                subscribed = true;
                continue;
            }
            Ok(Ok(Event::Incoming(Packet::SubAck(ack)))) => {
                return check_suback(filter, &ack.return_codes);
            }
            Ok(Ok(_)) => continue,
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "connecting to the broker failed");
                MessagingError::Connect(err)
            }
            Err(_elapsed) => {
                tracing::warn!(?wait, "the broker did not answer in time");
                MessagingError::Timeout
            }
        };
        failures += 1;
        if failures >= FIRST_CONNECT_FAILURES {
            return Err(error);
        }
        tracing::warn!(failures, "retrying the first broker connect");
        // The session is clean, so a subscription made before the failure
        // is gone.
        subscribed = false;
        tokio::time::sleep(retry_delay(failures)).await;
    }
}

/// Queues the first `up/` subscription. Called from the polling task, so it
/// must not wait on the request queue, which is empty at that point.
fn subscribe(client: &AsyncClient, filter: &str) -> Result<(), MessagingError> {
    client
        .try_subscribe(filter, QoS::AtLeastOnce)
        .map_err(|err| MessagingError::Subscribe {
            filter: filter.to_owned(),
            reason: err.to_string(),
        })
}

fn check_suback(filter: &str, codes: &[SubscribeReasonCode]) -> Result<(), MessagingError> {
    match codes {
        [SubscribeReasonCode::Success(_)] => Ok(()),
        other => Err(MessagingError::Subscribe {
            filter: filter.to_owned(),
            reason: format!("{other:?}"),
        }),
    }
}

/// The delay before reconnect attempt `failures` (1 for the first).
fn retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    FIRST_RETRY_DELAY
        .saturating_mul(1 << doublings)
        .min(MAX_RETRY_DELAY)
}

/// Polls the connection: hands publishes to ingest, resubscribes after
/// each reconnect (the session is clean), and reconnects with a fresh
/// token after a failure, giving up after [`MAX_FAILURES`] in a row.
async fn run_connection(
    mut eventloop: EventLoop,
    client: AsyncClient,
    hub: Hub,
    filter: String,
    tx: mpsc::Sender<Publish>,
) -> Result<(), MessagingError> {
    let mut failures = 0_u32;
    // Dropped with this task, which aborts a resubscribe still waiting.
    let mut resubscribe = JoinSet::new();
    loop {
        while resubscribe.try_join_next().is_some() {}
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                if tx.send(publish).await.is_err() {
                    return Err(MessagingError::Stopped("ingest"));
                }
            }
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                failures = 0;
                tracing::info!("reconnected to the broker");
                // The sweeper may have filled the request queue while the
                // link was down, so wait for room on another task; this one
                // must keep polling to drain the queue.
                let (client, filter) = (client.clone(), filter.clone());
                resubscribe.spawn(async move {
                    if let Err(err) = client.subscribe(filter, QoS::AtLeastOnce).await {
                        tracing::warn!(error = %err, "queueing the resubscribe failed");
                    }
                });
            }
            Ok(Event::Incoming(Packet::SubAck(ack))) => check_suback(&filter, &ack.return_codes)?,
            Ok(_) => {}
            Err(err) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    return Err(MessagingError::GaveUp {
                        attempts: failures,
                        source: err,
                    });
                }
                tracing::warn!(error = %err, failures, "broker connection lost; reconnecting");
                tokio::time::sleep(retry_delay(failures)).await;
                let token = hub
                    .tokens()
                    .mint_hub(HUB_TOKEN_TTL)
                    .map_err(MessagingError::Token)?;
                eventloop.options.set_credentials(HUB_USERNAME, token);
            }
        }
    }
}

/// Ingests each publish in arrival order, sends any reply ingest did not
/// queue, then acks the publish to the broker.
///
/// shortcut: one worker for every sender, about 200 publishes per second;
/// shard by sender (keeping each sender's order) if throughput matters.
async fn run_ingest(
    hub: Hub,
    client: AsyncClient,
    mut rx: mpsc::Receiver<Publish>,
) -> Result<(), MessagingError> {
    while let Some(publish) = rx.recv().await {
        handle(&hub, &client, &publish).await;
        if client.ack(&publish).await.is_err() {
            return Err(MessagingError::Stopped("connection"));
        }
    }
    Err(MessagingError::Stopped("connection"))
}

async fn handle(hub: &Hub, client: &AsyncClient, publish: &Publish) {
    let topic = String::from_utf8_lossy(&publish.topic);
    let Ok((tenant, project, from)) = parse_up(&topic) else {
        tracing::warn!(%topic, "dropped a publish on a topic that is not up/");
        return;
    };
    let topics = Topics::new(tenant, project);
    let handled = match hub.ingest_publish(&topics, &from, &publish.payload).await {
        Ok(handled) => handled,
        Err(err @ (IngestError::NoMessageId | IngestError::UnknownProject)) => {
            tracing::debug!(%from, %topic, reason = %err, "dropped a publish");
            return;
        }
        Err(err) => {
            // The broker gets its ack anyway: the client resends after its
            // timeout, and this hub may be able to answer by then.
            tracing::warn!(%from, error = %err, "could not ingest a publish");
            return;
        }
    };
    if handled.route == ReplyRoute::Outbox {
        return;
    }
    let payload = match serde_json::to_vec(&handled.reply) {
        Ok(payload) => payload,
        Err(err) => {
            tracing::error!(error = %err, "encoding a reply failed");
            return;
        }
    };
    // A reply lost here is not retried: the client resends after its
    // timeout and gets the same answer.
    if let Err(err) = client
        .publish(topics.ack(&from), payload, PublishOptions::at_least_once())
        .await
    {
        tracing::warn!(%from, error = %err, "queueing a reply failed");
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    /// MQTT 5 CONNACK: session not present, success, no properties.
    const CONNACK: [u8; 5] = [0x20, 0x03, 0x00, 0x00, 0x00];

    /// Reads one MQTT packet and returns its first byte and its body.
    async fn read_packet(conn: &mut TcpStream) -> (u8, Vec<u8>) {
        let header = conn.read_u8().await.unwrap();
        let mut len = 0_usize;
        for shift in [0, 7, 14, 21] {
            let byte = conn.read_u8().await.unwrap();
            len |= usize::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
        }
        let mut body = vec![0; len];
        conn.read_exact(&mut body).await.unwrap();
        (header, body)
    }

    /// A broker that accepts the first connection and never answers it, as
    /// EMQX may while it starts, then answers the second one properly.
    async fn slow_then_ready_broker(listener: TcpListener) -> [TcpStream; 2] {
        let (silent, _) = listener.accept().await.unwrap();
        let (mut conn, _) = listener.accept().await.unwrap();
        let (connect, _) = read_packet(&mut conn).await;
        assert_eq!(connect, 0x10, "expected CONNECT");
        conn.write_all(&CONNACK).await.unwrap();
        let (subscribe, body) = read_packet(&mut conn).await;
        assert_eq!(subscribe, 0x82, "expected SUBSCRIBE");
        // SUBACK for the same packet id: no properties, granted QoS 1.
        conn.write_all(&[0x90, 0x04, body[0], body[1], 0x00, 0x01])
            .await
            .unwrap();
        [silent, conn]
    }

    #[tokio::test]
    async fn first_connect_retries_after_a_step_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = tokio::spawn(slow_then_ready_broker(listener));
        let options = MqttOptions::new("agmen-hub-test", ("127.0.0.1".to_owned(), port));
        let (client, mut eventloop) = AsyncClient::builder(options)
            .capacity(QUEUE)
            .try_build()
            .unwrap();

        let connected = tokio::time::timeout(
            Duration::from_secs(10),
            first_connect(
                &client,
                &mut eventloop,
                "agmen/v1/+/+/up/+",
                Duration::from_millis(300),
            ),
        )
        .await
        .expect("first_connect did not finish within 10 s");

        assert!(connected.is_ok(), "{connected:?}");
        broker.await.unwrap();
    }

    #[test]
    fn retry_delays_double_to_the_cap() {
        assert_eq!(retry_delay(1), Duration::from_millis(500));
        assert_eq!(retry_delay(2), Duration::from_secs(1));
        assert_eq!(retry_delay(4), Duration::from_secs(4));
        assert_eq!(retry_delay(7), MAX_RETRY_DELAY);
        assert_eq!(retry_delay(u32::MAX), MAX_RETRY_DELAY);
    }
}
