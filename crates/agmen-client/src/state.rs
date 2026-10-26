//! Reads of a project's retained `state/{key}` topics (ruling R60): one-off
//! through a clean `<id>-cli` connection ([`read_state`]), or through a
//! session's own connection (`Session::read_state`, ruling R68). Also
//! handle lookup in the roster. The payloads are bare JSON (ruling R51):
//! `state/head` is `{feed_seq, rules_v, lanes_v, items_v, hub_at}`,
//! `state/roster` an array of `{identity_id, handle, kind, status, agent?,
//! model?, delegated_by?}`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use agmen_proto::{IdentityId, Topics};
use rumqttc_next::{
    AsyncClient, Event, EventLoop, Packet, QoS, SubscribeNoticeError, SubscribeReasonCode,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::error::ClientError;
use crate::identity::IdentityFile;
use crate::link::{CurrentClient, Routes};
use crate::session::{CLOSE_WAIT, CONNECT_WAIT, ClientKind, clean_options};
use crate::token::TokenSource;

/// How long a state read waits for the retained message after the broker
/// granted the subscription. EMQX sends it right after the SUBACK.
const STATE_WAIT: Duration = Duration::from_secs(3);

/// Requests the read's MQTT client queues: a subscribe, an unsubscribe and
/// a disconnect.
const CLIENT_CAPACITY: usize = 4;

/// Reads the project's retained `state/{key}` once, as `identity`: connects
/// as `<id>-cli` with a clean session, subscribes, takes the retained
/// message, unsubscribes and disconnects. Returns the payload as JSON, or
/// `None` when the broker holds no retained message there. `tokens` gives
/// the broker token, as for a session.
///
/// It uses the identity's `-cli` client id, so the broker drops any other
/// connection on that id (a `ClientKind::Cli` session) while it reads.
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use agmen_client::{HubClient, find_identity, read_state, resolve_handle};
///
/// let joined = find_identity(&std::env::current_dir()?)?.ok_or("not joined")?;
/// let file = joined.identity;
/// let hub = HubClient::new(
///     file.hub_url.clone(),
///     file.project.clone(),
///     file.identity_id.clone(),
///     "the identity's hub token",
/// )?;
/// let roster = read_state(&file, &hub, "roster").await?.unwrap_or_default();
/// match resolve_handle(&roster, "adam/coord")? {
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
/// [`ClientError::Timeout`] when the broker does not grant the subscription
/// within 10 s, and [`ClientError`] when the token, the connection or the
/// subscription fails.
pub async fn read_state<T: TokenSource>(
    identity: &IdentityFile,
    tokens: &T,
    key: &str,
) -> Result<Option<Value>, ClientError> {
    check_key(key)?;
    let topic = identity.topics().state(key);
    let token = tokens.token().await?;
    let options = clean_options(
        ClientKind::Cli,
        &identity.broker_url,
        &identity.identity_id,
        token,
    )?;
    let (client, mut eventloop) = AsyncClient::builder(options)
        .capacity(CLIENT_CAPACITY)
        .try_build()
        .map_err(ClientError::MqttClient)?;
    let read = take_retained(&client, &mut eventloop, &topic).await;
    disconnect(&client, &mut eventloop, &topic).await;
    parse_state(key, &read?)
}

/// Refuses a `key` that is not one topic level.
fn check_key(key: &str) -> Result<(), ClientError> {
    if key.is_empty() || key.contains(['/', '+', '#', '\0']) {
        return Err(ClientError::StateKey(key.to_owned()));
    }
    Ok(())
}

/// The retained `state/{key}` `payload` as JSON; `None` when it is empty,
/// which is how the broker hands over a deleted retained message (and how
/// the reads here say that none came).
fn parse_state(key: &str, payload: &[u8]) -> Result<Option<Value>, ClientError> {
    if payload.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|source| ClientError::BadState {
            key: key.to_owned(),
            source,
        })
}

/// The state read a session has in flight, shared with its link task: the
/// link hands the first message on the read's topic to it. A session runs
/// one read at a time.
#[derive(Debug, Clone, Default)]
pub(crate) struct StateSlot(Arc<Mutex<Option<Waiting>>>);

/// A read waiting for the message on `topic`.
#[derive(Debug)]
struct Waiting {
    topic: String,
    tx: oneshot::Sender<Vec<u8>>,
}

impl StateSlot {
    fn lock(&self) -> MutexGuard<'_, Option<Waiting>> {
        // The slot holds no invariant a panicking holder could break.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for the next message on `topic`, replacing any earlier wait.
    fn wait_for(&self, topic: &str) -> oneshot::Receiver<Vec<u8>> {
        let (tx, rx) = oneshot::channel();
        *self.lock() = Some(Waiting {
            topic: topic.to_owned(),
            tx,
        });
        rx
    }

    fn clear(&self) {
        self.lock().take();
    }

    /// Hands `payload` to the read waiting on `topic`. Returns whether one
    /// was.
    pub(crate) fn offer(&self, topic: &str, payload: &[u8]) -> bool {
        let mut slot = self.lock();
        if slot.as_ref().is_none_or(|waiting| waiting.topic != topic) {
            return false;
        }
        if let Some(waiting) = slot.take()
            && waiting.tx.send(payload.to_vec()).is_err()
        {
            tracing::debug!(%topic, "the state read stopped waiting");
        }
        true
    }
}

/// What a session's state reads need (`Session::read_state`).
#[derive(Debug)]
pub(crate) struct StateReads {
    topics: Topics,
    /// What the session subscribes to on every connect.
    routes: Routes,
    /// The read in flight, which the link task feeds.
    slot: StateSlot,
    /// Held for the whole of a read: one at a time.
    turn: tokio::sync::Mutex<()>,
}

impl StateReads {
    pub(crate) fn new(topics: Topics, routes: Routes, slot: StateSlot) -> Self {
        Self {
            topics,
            routes,
            slot,
            turn: tokio::sync::Mutex::new(()),
        }
    }

    /// Reads the retained `state/{key}` through the session's connection
    /// `client`.
    pub(crate) async fn read(
        &self,
        client: &CurrentClient,
        key: &str,
    ) -> Result<Option<Value>, ClientError> {
        check_key(key)?;
        let topic = self.topics.state(key);
        let keep = self.routes.subscribes(&topic);
        let _turn = self.turn.lock().await;
        read_through(client, &self.slot, key, topic, keep).await
    }
}

/// Reads the retained `state/{key}` at `topic` through a session's
/// connection: subscribes, takes the first message on the topic (the
/// retained one, sent right after the SUBACK), and unsubscribes unless
/// `keep` says the session subscribes to the topic anyway. The caller runs
/// one read at a time per session.
async fn read_through(
    client: &CurrentClient,
    slot: &StateSlot,
    key: &str,
    topic: String,
    keep: bool,
) -> Result<Option<Value>, ClientError> {
    let payload = slot.wait_for(&topic);
    let subscribed = Subscribed {
        client,
        slot,
        topic,
        keep,
    };
    // QoS 1 as the session's own subscriptions, so subscribing again to
    // one it keeps (a watch's state/head) does not lower its QoS.
    let notice = client
        .current()
        .subscribe_tracked(subscribed.topic.clone(), QoS::AtLeastOnce)
        .await
        .map_err(|_| ClientError::Closed)?;
    match tokio::time::timeout(CONNECT_WAIT, notice.wait_completion_async()).await {
        Ok(Ok(())) => {}
        Ok(Err(SubscribeNoticeError::V5SubAckFailure(codes))) => {
            return Err(ClientError::Subscribe(format!("{codes:?}")));
        }
        Ok(Err(err)) => return Err(ClientError::StateRead(err)),
        Err(_) => return Err(ClientError::Timeout(CONNECT_WAIT)),
    }
    let got = match tokio::time::timeout(STATE_WAIT, payload).await {
        Ok(Ok(payload)) => payload,
        // Nothing retained there: the broker sends nothing after the
        // SUBACK. The sender is gone only if another read replaced this
        // one's wait, which the caller rules out; read it as nothing too.
        Ok(Err(_)) | Err(_) => Vec::new(),
    };
    drop(subscribed);
    parse_state(key, &got)
}

/// A state read's subscription. Dropping it, also when the read is
/// cancelled, stops the wait and queues the UNSUBSCRIBE, so a persistent
/// session does not keep the topic.
struct Subscribed<'a> {
    client: &'a CurrentClient,
    slot: &'a StateSlot,
    topic: String,
    keep: bool,
}

impl Drop for Subscribed<'_> {
    fn drop(&mut self) {
        self.slot.clear();
        if !self.keep
            && let Err(err) = self.client.current().try_unsubscribe(self.topic.clone())
        {
            // The session stopped, or its queue is full; a later read of
            // the same key unsubscribes again.
            tracing::debug!(error = %err, topic = %self.topic, "queueing the UNSUBSCRIBE failed");
        }
    }
}

/// Connects, subscribes to `topic` and returns the payload of the first
/// message on it: empty when none comes within [`STATE_WAIT`] of the
/// SUBACK.
async fn take_retained(
    client: &AsyncClient,
    eventloop: &mut EventLoop,
    topic: &str,
) -> Result<Vec<u8>, ClientError> {
    let mut deadline = Instant::now() + CONNECT_WAIT;
    let mut subscribed = false;
    loop {
        let Ok(polled) = tokio::time::timeout_at(deadline, eventloop.poll()).await else {
            return if subscribed {
                Ok(Vec::new())
            } else {
                Err(ClientError::Timeout(CONNECT_WAIT))
            };
        };
        match polled.map_err(ClientError::Connect)? {
            Event::Incoming(Packet::ConnAck(_)) => client
                .try_subscribe(topic, QoS::AtMostOnce)
                .map_err(|_| ClientError::Closed)?,
            Event::Incoming(Packet::SubAck(ack)) => {
                if let Some(refused) = ack
                    .return_codes
                    .iter()
                    .find(|code| !matches!(code, SubscribeReasonCode::Success(_)))
                {
                    return Err(ClientError::Subscribe(format!("{refused:?}")));
                }
                subscribed = true;
                deadline = Instant::now() + STATE_WAIT;
            }
            Event::Incoming(Packet::Publish(publish)) if publish.topic == topic.as_bytes() => {
                return Ok(publish.payload.to_vec());
            }
            _ => {}
        }
    }
}

/// Unsubscribes from `topic` and disconnects, waiting at most
/// [`CLOSE_WAIT`]. A failure here does not matter: the session is clean, so
/// the broker keeps nothing of it either way.
async fn disconnect(client: &AsyncClient, eventloop: &mut EventLoop, topic: &str) {
    if client.try_unsubscribe(topic).is_err() || client.try_disconnect().is_err() {
        tracing::debug!("the state read's connection had already stopped");
        return;
    }
    let closed = tokio::time::timeout(CLOSE_WAIT, async {
        while eventloop.poll().await.is_ok() {}
    })
    .await;
    if closed.is_err() {
        tracing::debug!("the broker did not take the state read's DISCONNECT in time");
    }
}

/// The part of a `state/roster` entry [`resolve_handle`] reads.
#[derive(Debug, Deserialize)]
struct RosterEntry {
    identity_id: IdentityId,
    handle: String,
}

/// The id of the roster entry whose handle is exactly `handle` (`adam/coord`
/// for an identity, `adam` for a person), or `None` when no entry has it.
/// `roster` is the retained `state/roster` as [`read_state`] returns it.
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let roster = serde_json::json!([
///     { "identity_id": "idn7k2q9", "handle": "andy/sol", "kind": "agent", "status": "online" },
/// ]);
/// let id = agmen_client::resolve_handle(&roster, "andy/sol")?;
/// assert_eq!(id.map(|id| id.to_string()).as_deref(), Some("idn7k2q9"));
/// assert_eq!(agmen_client::resolve_handle(&roster, "andy/luna")?, None);
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// [`ClientError::BadState`] when `roster` is not an array of entries with
/// an `identity_id` and a `handle`.
pub fn resolve_handle(roster: &Value, handle: &str) -> Result<Option<IdentityId>, ClientError> {
    let entries =
        Vec::<RosterEntry>::deserialize(roster).map_err(|source| ClientError::BadState {
            key: "roster".to_owned(),
            source,
        })?;
    Ok(entries
        .into_iter()
        .find(|entry| entry.handle == handle)
        .map(|entry| entry.identity_id))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn resolve_handle_finds_exact_handles() {
        let roster = json!([
            { "identity_id": "humanandy", "handle": "andy", "kind": "human", "status": "offline" },
            { "identity_id": "idn7k2q9", "handle": "andy/sol", "kind": "agent", "status": "online",
              "agent": "claude-code/2.1.310" },
        ]);
        let found = |handle| {
            resolve_handle(&roster, handle)
                .unwrap()
                .map(|id| id.to_string())
        };
        assert_eq!(found("andy/sol").as_deref(), Some("idn7k2q9"));
        assert_eq!(found("andy").as_deref(), Some("humanandy"));
        assert_eq!(found("Andy/Sol"), None, "handles match exactly");
        assert_eq!(found("andy/luna"), None);
        assert_eq!(resolve_handle(&json!([]), "andy").unwrap(), None);
    }

    #[test]
    fn resolve_handle_refuses_what_is_not_a_roster() {
        for bad in [
            json!(null),
            json!({ "identity_id": "idn7k2q9", "handle": "andy/sol" }),
            json!([{ "handle": "andy/sol" }]),
            json!([{ "identity_id": "NOT AN ID", "handle": "andy/sol" }]),
        ] {
            assert!(
                matches!(
                    resolve_handle(&bad, "andy/sol"),
                    Err(ClientError::BadState { .. })
                ),
                "{bad}"
            );
        }
    }
}
