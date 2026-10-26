//! One-off reads of a project's retained `state/{key}` topics (ruling
//! R60), and handle lookup in the roster. The payloads are bare JSON (ruling
//! R51): `state/head` is `{feed_seq, rules_v, lanes_v, items_v, hub_at}`,
//! `state/roster` an array of `{identity_id, handle, kind, status, agent?,
//! model?, delegated_by?}`.

use std::time::Duration;

use agmen_proto::IdentityId;
use rumqttc_next::{AsyncClient, Event, EventLoop, Packet, QoS, SubscribeReasonCode};
use serde::Deserialize;
use serde_json::Value;
use tokio::time::Instant;

use crate::error::ClientError;
use crate::identity::IdentityFile;
use crate::session::{CLOSE_WAIT, CONNECT_WAIT, ClientKind, clean_options};
use crate::token::TokenSource;

/// How long [`read_state`] waits for the retained message after the broker
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
    if key.is_empty() || key.contains(['/', '+', '#', '\0']) {
        return Err(ClientError::StateKey(key.to_owned()));
    }
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
    let payload = read?;
    if payload.is_empty() {
        // An empty retained message is a deleted one.
        return Ok(None);
    }
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|source| ClientError::BadState {
            key: key.to_owned(),
            source,
        })
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
