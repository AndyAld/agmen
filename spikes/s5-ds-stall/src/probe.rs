//! One trial: fresh projects, each with watch-like persistent sessions and
//! a hub-like publisher, all started at once. Every message carries the
//! time it was published, so each session's receipt gives one latency.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, bail};
use rumqttc::{
    AsyncClient, ConnectionError, DisconnectProperties, DisconnectReasonCode, Event, EventLoop,
    MqttOptions, Outgoing, Packet, PublishOptions, QoS, SubscribeFilter,
};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::auth::{HUB_USER, Signer, new_id};

/// How long a connect may take, CONNACK and SUBACK together.
const CONNECT_WAIT: Duration = Duration::from_secs(10);
/// Pause between reconnect attempts, and the most attempts in a row.
const RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_FAILURES: u32 = 30;
/// How long closing a client may take.
const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// How often the trial checks whether every message has arrived.
const CHECK_EVERY: Duration = Duration::from_millis(100);
const KEEP_ALIVE_SECS: u16 = 30;
const CLIENT_CAPACITY: usize = 64;

/// The streams a watch session subscribes to that the publisher writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Kind {
    Feed,
    Inbox,
    Ack,
}

impl Kind {
    const ALL: [Self; 3] = [Self::Feed, Self::Inbox, Self::Ack];
}

/// What one trial needs.
#[derive(Debug, Clone)]
pub(crate) struct Trial {
    pub(crate) broker: (String, u16),
    pub(crate) projects: usize,
    pub(crate) sessions: usize,
    pub(crate) rounds: u32,
    pub(crate) interval: Duration,
    pub(crate) deadline: Duration,
    /// Session expiry the watch sessions ask for, in seconds.
    pub(crate) expiry: u32,
    /// The EMQX REST API, to snapshot a session that is missing messages.
    pub(crate) api: Option<Api>,
    /// After the deadline, a project with a session still missing messages
    /// publishes one more round and waits this long for the missing ones,
    /// to tell a message held back until the next write from a lost one.
    pub(crate) nudge: Option<Duration>,
}

/// EMQX's REST API base URL and an API key.
#[derive(Debug, Clone)]
pub(crate) struct Api {
    pub(crate) base: String,
    pub(crate) key: String,
    pub(crate) secret: String,
}

/// One expected message at one session.
#[derive(Debug, Clone)]
pub(crate) struct Delivery {
    pub(crate) client_id: String,
    /// The project's topic prefix.
    pub(crate) project: String,
    pub(crate) kind: Kind,
    pub(crate) round: u32,
    /// `None`: it had not arrived by the deadline.
    pub(crate) latency: Option<Duration>,
}

/// What a trial saw.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub(crate) deliveries: Vec<Delivery>,
    /// Connection errors, each with the client id.
    pub(crate) errors: Vec<String>,
    /// EMQX's view of each session still missing messages at the deadline.
    pub(crate) snapshots: Vec<String>,
}

/// A publish as a session received it.
#[derive(Debug)]
struct Received {
    topic: String,
    payload: Vec<u8>,
    at: Instant,
}

/// What a client's poll task records.
#[derive(Debug, Default)]
struct Log {
    received: Mutex<Vec<Received>>,
    errors: Mutex<Vec<String>>,
}

/// A connected client and its poll task.
#[derive(Debug)]
struct Conn {
    client_id: String,
    client: AsyncClient,
    log: Arc<Log>,
    closing: Arc<AtomicBool>,
    task: JoinHandle<anyhow::Result<()>>,
}

impl Trial {
    /// Runs every project at once and gathers what they saw.
    pub(crate) async fn run(&self, signer: &Arc<Signer>) -> anyhow::Result<Outcome> {
        let mut projects = JoinSet::new();
        for _ in 0..self.projects {
            projects.spawn(run_project(self.clone(), signer.clone()));
        }
        let mut outcome = Outcome::default();
        while let Some(done) = projects.join_next().await {
            let project = done.context("a project task panicked")??;
            outcome.deliveries.extend(project.deliveries);
            outcome.errors.extend(project.errors);
            outcome.snapshots.extend(project.snapshots);
        }
        Ok(outcome)
    }
}

/// One project: connect its watch sessions, publish `rounds` rounds to its
/// feed and to each session's inbox and ack, wait for them, close.
async fn run_project(trial: Trial, signer: Arc<Signer>) -> anyhow::Result<Outcome> {
    let prefix = format!("agmen/v1/{}/{}", new_id(), new_id());
    let mut connecting = JoinSet::new();
    for _ in 0..trial.sessions {
        let id = new_id();
        let token = signer.identity(&prefix, &id)?;
        let filters = [
            "feed".to_owned(),
            format!("inbox/{id}"),
            format!("ack/{id}"),
            "state/head".to_owned(),
        ]
        .map(|topic| format!("{prefix}/{topic}"));
        let mut options = options(&trial, &format!("{id}-watch"), &id, token);
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(trial.expiry));
        connecting.spawn(connect(options, filters.to_vec()));
    }
    let mut watches = Vec::with_capacity(trial.sessions);
    let mut outcome = Outcome::default();
    while let Some(done) = connecting.join_next().await {
        match done.context("a connect task panicked")? {
            Ok(conn) => watches.push(conn),
            Err(err) => outcome.errors.push(format!("{err:#}")),
        }
    }
    let hub_id = format!("agmen-hub-{}", new_id());
    let hub = connect(
        options(&trial, &hub_id, HUB_USER, signer.hub()?),
        Vec::new(),
    )
    .await?;
    let start = Instant::now();
    let mut sent = Vec::new();
    for round in 0..trial.rounds {
        let at = start.elapsed();
        publish_round(&hub, &prefix, &watches, round, at).await?;
        sent.push(at);
        tokio::time::sleep(trial.interval).await;
    }
    let expected = trial.rounds as usize * Kind::ALL.len();
    let until = start + trial.deadline;
    while Instant::now() < until && watches.iter().any(|w| received(w) < expected) {
        tokio::time::sleep(CHECK_EVERY).await;
    }
    // The sessions that miss messages at the deadline, with what they miss.
    let mut short = Vec::new();
    for watch in &watches {
        let deliveries = deliveries(watch, &prefix, &sent, start);
        let missing = missing(&deliveries);
        if !missing.is_empty() {
            let seen = match &trial.api {
                Some(api) => snapshot(api, &watch.client_id).await,
                None => watch.client_id.clone(),
            };
            outcome.snapshots.push(format!(
                "missing at deadline: {seen} missing {} (project {prefix}, publisher {hub_id})",
                missing.join(",")
            ));
            short.push((watch, missing));
        }
        outcome.deliveries.extend(deliveries);
    }
    if let Some(wait) = trial.nudge
        && !short.is_empty()
    {
        publish_round(&hub, &prefix, &watches, trial.rounds, start.elapsed()).await?;
        let until = Instant::now() + wait;
        let complete = |watch: &Conn| missing(&deliveries(watch, &prefix, &sent, start)).is_empty();
        while Instant::now() < until && short.iter().any(|(watch, _)| !complete(watch)) {
            tokio::time::sleep(CHECK_EVERY).await;
        }
        for (watch, before) in short {
            let after = missing(&deliveries(watch, &prefix, &sent, start));
            let released: Vec<&str> = before
                .iter()
                .filter(|label| !after.contains(label))
                .map(String::as_str)
                .collect();
            outcome.snapshots.push(format!(
                "after a nudge: {} got {} of the {} missing [{}], still missing [{}]",
                watch.client_id,
                released.len(),
                before.len(),
                released.join(","),
                after.join(","),
            ));
        }
    }
    for conn in watches.into_iter().chain([hub]) {
        let errors = std::mem::take(&mut *lock(&conn.log.errors));
        outcome.errors.extend(
            errors
                .into_iter()
                .map(|e| format!("{}: {e}", conn.client_id)),
        );
        close(conn).await;
    }
    Ok(outcome)
}

/// Publishes round `round` (sent `at` after the start) to the project's
/// feed and to each watch's inbox and ack.
async fn publish_round(
    hub: &Conn,
    prefix: &str,
    watches: &[Conn],
    round: u32,
    at: Duration,
) -> anyhow::Result<()> {
    let payload = format!("{round}:{}", at.as_micros()).into_bytes();
    let mut topics = vec![format!("{prefix}/feed")];
    for watch in watches {
        let id = watch.client_id.trim_end_matches("-watch");
        topics.push(format!("{prefix}/inbox/{id}"));
        topics.push(format!("{prefix}/ack/{id}"));
    }
    for topic in topics {
        hub.client
            .publish(topic, payload.clone(), PublishOptions::at_least_once())
            .await
            .with_context(|| format!("queueing round {round}"))?;
    }
    Ok(())
}

/// Labels such as `Feed[4]` for the messages in `deliveries` that have not
/// arrived.
fn missing(deliveries: &[Delivery]) -> Vec<String> {
    deliveries
        .iter()
        .filter(|d| d.latency.is_none())
        .map(|d| format!("{:?}[{}]", d.kind, d.round))
        .collect()
}

/// The settings agmen's clients use (ADR-0003), with automatic acks.
fn options(trial: &Trial, client_id: &str, username: &str, token: String) -> MqttOptions {
    let mut options = MqttOptions::new(client_id, trial.broker.clone());
    options
        .set_credentials(username, token)
        .set_keep_alive(KEEP_ALIVE_SECS)
        .set_max_packet_size(Some(96 * 1024));
    options
}

/// Connects, subscribes to `filters` (if any) and returns once the broker
/// granted them, or once CONNACK came for a client without filters.
async fn connect(options: MqttOptions, filters: Vec<String>) -> anyhow::Result<Conn> {
    let client_id = options.client_id();
    let (client, eventloop) = AsyncClient::builder(options)
        .capacity(CLIENT_CAPACITY)
        .try_build()
        .context("building an MQTT client")?;
    let log = Arc::new(Log::default());
    let closing = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = oneshot::channel();
    let task = tokio::spawn(poll(
        eventloop,
        client.clone(),
        filters,
        ready_tx,
        log.clone(),
        closing.clone(),
    ));
    let conn = Conn {
        client_id,
        client,
        log,
        closing,
        task,
    };
    match tokio::time::timeout(CONNECT_WAIT, ready_rx).await {
        Ok(Ok(())) => Ok(conn),
        Ok(Err(_)) => {
            let errors = lock(&conn.log.errors).join("; ");
            close(conn).await;
            bail!("connect failed: {errors}")
        }
        Err(_) => {
            let id = conn.client_id.clone();
            let errors = lock(&conn.log.errors).join("; ");
            close(conn).await;
            bail!("{id}: no CONNACK and SUBACK within {CONNECT_WAIT:?} ({errors})")
        }
    }
}

/// Polls the event loop, subscribing after every CONNACK (as agmen-client
/// does) and recording each publish with the time it arrived.
async fn poll(
    mut eventloop: EventLoop,
    client: AsyncClient,
    filters: Vec<String>,
    ready: oneshot::Sender<()>,
    log: Arc<Log>,
    closing: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut ready = Some(ready);
    let mut failures = 0_u32;
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                failures = 0;
                if filters.is_empty() {
                    signal(&mut ready);
                } else {
                    let subscribe = filters
                        .iter()
                        .map(|f| SubscribeFilter::new(f.as_str(), QoS::AtLeastOnce));
                    client
                        .try_subscribe_many(subscribe)
                        .context("queueing the subscribe")?;
                }
            }
            Ok(Event::Incoming(Packet::SubAck(_))) => signal(&mut ready),
            Ok(Event::Incoming(Packet::Publish(publish))) => lock(&log.received).push(Received {
                topic: String::from_utf8_lossy(&publish.topic).into_owned(),
                payload: publish.payload.to_vec(),
                at: Instant::now(),
            }),
            Ok(Event::Outgoing(Outgoing::Disconnect)) => return Ok(()),
            Ok(_) => {}
            Err(ConnectionError::RequestsDone) => return Ok(()),
            Err(_) if closing.load(Ordering::SeqCst) => return Ok(()),
            Err(err) => {
                lock(&log.errors).push(err.to_string());
                failures += 1;
                if failures >= MAX_FAILURES {
                    bail!("gave up after {failures} failed connects: {err}");
                }
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
}

fn signal(ready: &mut Option<oneshot::Sender<()>>) {
    if let Some(ready) = ready.take() {
        // The connect may have timed out already; nobody waits then.
        if ready.send(()).is_err() {}
    }
}

/// Sends DISCONNECT with a session expiry of 0, so the broker drops even a
/// persistent session, and waits for the poll task to end.
async fn close(conn: Conn) {
    conn.closing.store(true, Ordering::SeqCst);
    let properties = DisconnectProperties {
        session_expiry_interval: Some(0),
        reason_string: None,
        user_properties: Vec::new(),
        server_reference: None,
    };
    let queued = conn
        .client
        .try_disconnect_with_properties(DisconnectReasonCode::NormalDisconnection, properties);
    if let Err(err) = queued {
        eprintln!("{}: queueing DISCONNECT failed: {err}", conn.client_id);
    }
    let mut task = conn.task;
    match tokio::time::timeout(CLOSE_WAIT, &mut task).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(err))) => eprintln!("{}: {err:#}", conn.client_id),
        Ok(Err(err)) => eprintln!("{}: poll task failed: {err}", conn.client_id),
        Err(_) => {
            eprintln!(
                "{}: DISCONNECT not sent within {CLOSE_WAIT:?}",
                conn.client_id
            );
            task.abort();
        }
    }
}

fn received(conn: &Conn) -> usize {
    lock(&conn.log.received).len()
}

/// Each message `conn` should have got, with its latency if it came.
fn deliveries(conn: &Conn, prefix: &str, sent: &[Duration], start: Instant) -> Vec<Delivery> {
    let received = lock(&conn.log.received);
    let mut out = Vec::with_capacity(sent.len() * Kind::ALL.len());
    for (round, at) in (0_u32..).zip(sent) {
        for kind in Kind::ALL {
            let latency = received
                .iter()
                .filter(|r| kind_of(prefix, &r.topic) == Some(kind))
                .find(|r| round_of(&r.payload) == Some(round))
                .map(|r| r.at.duration_since(start).saturating_sub(*at));
            out.push(Delivery {
                client_id: conn.client_id.clone(),
                project: prefix.to_owned(),
                kind,
                round,
                latency,
            });
        }
    }
    out
}

fn kind_of(prefix: &str, topic: &str) -> Option<Kind> {
    let rest = topic.strip_prefix(prefix)?.strip_prefix('/')?;
    if rest == "feed" {
        Some(Kind::Feed)
    } else if rest.starts_with("inbox/") {
        Some(Kind::Inbox)
    } else if rest.starts_with("ack/") {
        Some(Kind::Ack)
    } else {
        None
    }
}

/// The round number at the start of a `<round>:<micros>` payload.
fn round_of(payload: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(payload).ok()?;
    text.split_once(':')?.0.parse().ok()
}

/// EMQX's view of `client_id`: whether it is connected and durable, its
/// subscriptions, streams and send counters.
async fn snapshot(api: &Api, client_id: &str) -> String {
    let url = format!("{}/clients/{client_id}", api.base);
    let response = reqwest::Client::new()
        .get(&url)
        .basic_auth(&api.key, Some(&api.secret))
        .send()
        .await;
    let body = match response {
        Ok(response) => response.text().await,
        Err(err) => return format!("{client_id}: GET failed: {err}"),
    };
    let fields = body
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let Some(fields) = fields else {
        return format!("{client_id}: unreadable answer");
    };
    let picked: Vec<String> = [
        "connected",
        "durable",
        "subscriptions_cnt",
        "n_streams",
        "send_msg",
        "inflight_cnt",
        "mqueue_len",
    ]
    .iter()
    .map(|field| format!("{field}={}", fields.get(field).unwrap_or(&Value::Null)))
    .collect();
    format!("{client_id}: {}", picked.join(" "))
}

/// Locks `mutex`, taking the data even if a holder panicked: the logs are
/// plain lists that a panic cannot leave half written.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_kind_and_round() {
        let prefix = "agmen/v1/t/p";
        assert_eq!(kind_of(prefix, "agmen/v1/t/p/feed"), Some(Kind::Feed));
        assert_eq!(kind_of(prefix, "agmen/v1/t/p/inbox/x"), Some(Kind::Inbox));
        assert_eq!(kind_of(prefix, "agmen/v1/t/p/ack/x"), Some(Kind::Ack));
        assert_eq!(kind_of(prefix, "agmen/v1/t/p/state/head"), None);
        assert_eq!(kind_of(prefix, "agmen/v1/t/q/feed"), None);
        assert_eq!(round_of(b"7:123"), Some(7));
        assert_eq!(round_of(b"x"), None);
    }
}
