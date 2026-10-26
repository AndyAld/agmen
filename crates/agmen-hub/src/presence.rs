//! Presence and the retained roster (protocol section 2).
//!
//! Each identity publishes its retained `presence/{id}` (`online` or
//! `away`), and its last will sets `offline`. The hub reads every
//! project's presence on a plain (not shared) subscription, so each hub
//! instance sees all of it, and the broker hands it the retained
//! messages again after every reconnect. From presence, each identity's
//! latest HELLO and the identities table it builds `state/roster`: every
//! live identity and human member of the project with its status, and
//! publishes it retained, so a client learns who is there from one
//! subscription without waking anyone.
//!
//! [`Hub::on_recorded`] is ingest's hook into this: a HELLO changes the
//! roster, a BYE sets its sender offline, and a ROLLCALL gets the hub's
//! answer (`crate::rollcall`).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use agmen_proto::{IdentityId, IdentityKind, MessageId, ProjectId, Topics, Verb};
use rumqttc_next::{AsyncClient, Publish, PublishOptions};
use serde::de::IntoDeserializer as _;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, mpsc};

use crate::http::{Hub, error_chain};
use crate::ingest::project_exists;
use crate::mqtt::MessagingError;

/// The hub's subscription to every project's presence (ADR-0002).
pub(crate) const PRESENCE_FILTER: &str = "agmen/v1/+/+/presence/+";

/// The `state/` key the roster is published under.
const ROSTER_KEY: &str = "roster";

/// The largest presence payload the hub reads; `{"status":"online"}` is 19
/// bytes, and anything far bigger is not a presence message.
const MAX_PRESENCE_BYTES: usize = 1024;

/// The retained `state/{key}` payload limit (protocol section 8).
const MAX_STATE_BYTES: usize = 4 * 1024;

/// What the hub publishes on `presence/{id}` for a BYE, and what a last
/// will says.
const OFFLINE_PAYLOAD: &[u8] = br#"{"status":"offline"}"#;

/// The most presence messages the presence task applies before it
/// publishes the rosters they changed.
const DRAIN: usize = 256;

/// Whether an identity is there (protocol section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    /// Connected and working.
    Online,
    /// Connected, but its person or agent is idle.
    Away,
    /// Not connected, or never was.
    Offline,
}

/// A `presence/{id}` payload. Fields it does not know are ignored.
#[derive(Debug, Deserialize)]
struct PresenceMessage {
    status: Status,
}

/// One identity in `state/roster`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RosterEntry {
    pub(crate) identity_id: IdentityId,
    pub(crate) handle: String,
    pub(crate) kind: IdentityKind,
    pub(crate) status: Status,
    /// The coding agent its latest HELLO named.
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    /// The model its latest HELLO named.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// The listener that started it. Always `None` in the MVP: nothing
    /// mints delegated workers before the roles-listener plan (rulings
    /// R10, R15), so the identities table has no such column yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    delegated_by: Option<IdentityId>,
}

impl RosterEntry {
    /// An entry with no HELLO details and no `delegated_by`.
    pub(crate) const fn new(
        identity_id: IdentityId,
        handle: String,
        kind: IdentityKind,
        status: Status,
    ) -> Self {
        Self {
            identity_id,
            handle,
            kind,
            status,
            agent: None,
            model: None,
            delegated_by: None,
        }
    }
}

/// Why the roster could not be built.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RosterError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("a stored identity is invalid: {0}")]
    Stored(String),
}

/// What the presence task still has to publish.
#[derive(Debug, Default)]
struct Pending {
    /// Projects whose roster changed.
    rosters: BTreeSet<Topics>,
    /// Identities that sent BYE, whose retained presence the hub sets to
    /// offline.
    offline: BTreeSet<(Topics, IdentityId)>,
}

/// What the hub has read from presence since it last connected.
#[derive(Debug, Default)]
struct View {
    /// The identities that are online or away, by project; anyone absent
    /// is offline. Entries come only from presence messages of this hub's
    /// projects, so it holds at most one per connected identity.
    present: HashMap<ProjectId, HashMap<IdentityId, Status>>,
    /// The projects, this hub's or not, that had a presence message. The
    /// first one of each of this hub's projects publishes its roster even
    /// when it changes no status, since the retained roster may come from
    /// before a reconnect or from another hub process. It holds at most one
    /// entry per project with retained presence on the broker.
    seen: HashSet<Topics>,
}

/// The hub's view of who is there, shared by ingest and the presence task.
#[derive(Debug, Default)]
pub(crate) struct PresenceState {
    view: Mutex<View>,
    pending: Mutex<Pending>,
    /// Wakes the presence task when `pending` gained something.
    ready: Notify,
}

impl PresenceState {
    /// `id`'s status in `project`.
    pub(crate) fn status(&self, project: &ProjectId, id: &IdentityId) -> Status {
        self.view
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .present
            .get(project)
            .and_then(|ids| ids.get(id))
            .copied()
            .unwrap_or(Status::Offline)
    }

    /// Whether `topics`' project had a presence message since the hub
    /// connected.
    fn seen(&self, topics: &Topics) -> bool {
        self.view
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .seen
            .contains(topics)
    }

    /// Notes that `topics`' project had a presence message; returns whether
    /// it is the first since the hub connected.
    fn mark_seen(&self, topics: &Topics) -> bool {
        self.view
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .seen
            .insert(topics.clone())
    }

    /// Sets `id`'s status; returns whether it changed.
    fn set(&self, project: &ProjectId, id: &IdentityId, status: Status) -> bool {
        let present = &mut self
            .view
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .present;
        if status == Status::Offline {
            let Some(ids) = present.get_mut(project) else {
                return false;
            };
            let removed = ids.remove(id).is_some();
            if ids.is_empty() {
                present.remove(project);
            }
            removed
        } else {
            present
                .entry(project.clone())
                .or_default()
                .insert(id.clone(), status)
                != Some(status)
        }
    }

    /// Forgets every status and every project seen. Called on each
    /// connect: the broker then sends every retained presence again, and
    /// one cleared while the hub was away (or lost with a broker restart)
    /// must not stay online.
    pub(crate) fn clear(&self) {
        *self.view.lock().unwrap_or_else(PoisonError::into_inner) = View::default();
    }

    /// Queues a roster publish for `topics`' project: its presence changed,
    /// or an identity was minted or revoked in it.
    pub(crate) fn roster_changed(&self, topics: &Topics) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rosters
            .insert(topics.clone());
        self.ready.notify_one();
    }

    fn take(&self) -> Pending {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Hub {
    /// What happens after ingest recorded message `id` from `from` in
    /// `topics`' project: a HELLO changes the roster's agent and model, a
    /// BYE sets the sender offline (protocol section 4), and a ROLLCALL
    /// gets the hub's STATUS. Failures are logged: the message itself is
    /// recorded either way.
    pub(crate) async fn on_recorded(
        &self,
        topics: &Topics,
        verb: Verb,
        from: &IdentityId,
        from_kind: IdentityKind,
        id: &MessageId,
    ) {
        let presence = self.presence();
        match verb {
            Verb::Hello => presence.roster_changed(topics),
            Verb::Bye => {
                presence.set(topics.project(), from, Status::Offline);
                presence
                    .pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .offline
                    .insert((topics.clone(), from.clone()));
                presence.roster_changed(topics);
            }
            Verb::Rollcall => {
                if let Err(err) = crate::rollcall::answer(self, topics, id, from, from_kind).await {
                    tracing::warn!(%from, %id, error = %error_chain(&err), "answering a ROLLCALL failed");
                }
            }
            _ => {}
        }
    }

    /// `topics`' roster: every live identity and every human member, by
    /// handle, with its status and its latest HELLO's agent and model.
    pub(crate) async fn roster(&self, topics: &Topics) -> Result<Vec<RosterEntry>, RosterError> {
        let project = topics.project();
        let members: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id, kind, handle FROM identities
             WHERE project_id = $1 AND revoked_at IS NULL
             UNION ALL
             SELECT h.id, 'human', h.handle FROM memberships m
             JOIN humans h ON h.id = m.human_id
             WHERE m.project_id = $1
             ORDER BY 3",
        )
        .bind(project.as_str())
        .fetch_all(self.pool())
        .await?;
        // Uses the messages_latest_hello index (0005_presence).
        let hellos: HashMap<String, (Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT DISTINCT ON (from_id)
                    from_id, envelope -> 'data' ->> 'agent', envelope -> 'data' ->> 'model'
             FROM messages
             WHERE project_id = $1 AND stream = 'feed' AND envelope ->> 'verb' = 'HELLO'
             ORDER BY from_id, seq DESC",
        )
        .bind(project.as_str())
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|(from, agent, model)| (from, (agent, model)))
        .collect();
        members
            .into_iter()
            .map(|(id, kind, handle)| {
                let (agent, model) = hellos.get(&id).cloned().unwrap_or_default();
                let identity_id: IdentityId = id
                    .parse()
                    .map_err(|err| RosterError::Stored(format!("{err}")))?;
                let kind = IdentityKind::deserialize(kind.as_str().into_deserializer())
                    .map_err(|err: serde::de::value::Error| RosterError::Stored(err.to_string()))?;
                let status = self.presence().status(project, &identity_id);
                Ok(RosterEntry {
                    agent,
                    model,
                    ..RosterEntry::new(identity_id, handle, kind, status)
                })
            })
            .collect()
    }
}

/// Whether `topic` is a `presence/{id}` topic.
pub(crate) fn is_presence_topic(topic: &str) -> bool {
    parse_presence(topic).is_some()
}

/// The project topics and identity of `agmen/v1/{t}/{p}/presence/{id}`.
fn parse_presence(topic: &str) -> Option<(Topics, IdentityId)> {
    let mut levels = topic.split('/');
    let (Some("agmen"), Some("v1"), Some(tenant), Some(project), Some("presence"), Some(id), None) = (
        levels.next(),
        levels.next(),
        levels.next(),
        levels.next(),
        levels.next(),
        levels.next(),
        levels.next(),
    ) else {
        return None;
    };
    Some((
        Topics::new(tenant.parse().ok()?, project.parse().ok()?),
        id.parse().ok()?,
    ))
}

/// The presence task: applies each presence message, then publishes the
/// retained presence BYEs asked for and the rosters that changed.
pub(crate) async fn run(
    hub: Hub,
    client: AsyncClient,
    mut rx: mpsc::Receiver<Publish>,
) -> Result<(), MessagingError> {
    loop {
        tokio::select! {
            publish = rx.recv() => {
                let Some(publish) = publish else {
                    return Err(MessagingError::Stopped("connection"));
                };
                handle(&hub, &client, &publish).await?;
                for _ in 1..DRAIN {
                    let Ok(publish) = rx.try_recv() else { break };
                    handle(&hub, &client, &publish).await?;
                }
            }
            () = hub.presence().ready.notified() => {}
        }
        flush(&hub, &client).await;
    }
}

/// Acks `publish` to the broker, then applies it. Acking first is safe:
/// presence is retained, so a hub that dies before applying it reads it
/// again after it reconnects, and the ack never waits on the database.
async fn handle(hub: &Hub, client: &AsyncClient, publish: &Publish) -> Result<(), MessagingError> {
    client
        .ack(publish)
        .await
        .map_err(|_| MessagingError::Stopped("connection"))?;
    let topic = String::from_utf8_lossy(&publish.topic);
    let Some((topics, id)) = parse_presence(&topic) else {
        tracing::debug!(%topic, "dropped a publish that is not presence");
        return Ok(());
    };
    let status = if publish.payload.is_empty() {
        // A cleared retained presence.
        Status::Offline
    } else if publish.payload.len() > MAX_PRESENCE_BYTES {
        tracing::debug!(%topic, "dropped an oversized presence message");
        return Ok(());
    } else {
        match serde_json::from_slice::<PresenceMessage>(&publish.payload) {
            Ok(message) => message.status,
            Err(err) => {
                tracing::debug!(%topic, error = %err, "dropped a malformed presence message");
                return Ok(());
            }
        }
    };
    let presence = hub.presence();
    // Skip what changes nothing before the lookup (hubs that share a broker
    // see each other's presence), except a project's first message since
    // the hub connected, which republishes its roster.
    if presence.seen(&topics) && presence.status(topics.project(), &id) == status {
        return Ok(());
    }
    let ours = match project_exists(hub.pool(), &topics).await {
        Ok(ours) => ours,
        Err(err) => {
            tracing::warn!(%topic, error = %err, "looking up a presence message's project failed");
            return Ok(());
        }
    };
    let first = presence.mark_seen(&topics);
    if !ours {
        return Ok(());
    }
    if presence.set(topics.project(), &id, status) || first {
        presence.roster_changed(&topics);
    }
    Ok(())
}

/// Publishes what [`PresenceState::take`] returns. A publish that cannot be
/// queued is logged: the connection task reports a broken connection, and
/// the next change publishes the roster again.
async fn flush(hub: &Hub, client: &AsyncClient) {
    let Pending { rosters, offline } = hub.presence().take();
    let retained = PublishOptions::at_least_once().retained();
    for (topics, id) in offline {
        if let Err(err) = client
            .publish(
                topics.presence(&id),
                OFFLINE_PAYLOAD.to_vec(),
                retained.clone(),
            )
            .await
        {
            tracing::warn!(%id, error = %err, "queueing an offline presence failed");
        }
    }
    for topics in rosters {
        let roster = match hub.roster(&topics).await {
            Ok(roster) => roster,
            Err(err) => {
                tracing::warn!(project = %topics.project(), error = %error_chain(&err), "building the roster failed");
                continue;
            }
        };
        let payload = match serde_json::to_vec(&roster) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::error!(error = %err, "encoding the roster failed");
                continue;
            }
        };
        // shortcut: the roster goes out whole even past the 4 KiB state
        // limit (about 25 identities); serve the full roster over
        // `GET /projects/{p}/state?key=roster` and keep a small head here
        // once projects grow past it.
        if payload.len() > MAX_STATE_BYTES {
            tracing::warn!(project = %topics.project(), bytes = payload.len(), "the roster is over the 4 KiB state limit");
        }
        if let Err(err) = client
            .publish(topics.state(ROSTER_KEY), payload, retained.clone())
            .await
        {
            tracing::warn!(project = %topics.project(), error = %err, "queueing the roster failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_topics_parse() {
        let (topics, id) = parse_presence("agmen/v1/tenant01/project1/presence/abcdefgh").unwrap();
        assert_eq!(
            topics,
            Topics::new("tenant01".parse().unwrap(), "project1".parse().unwrap())
        );
        assert_eq!(id.as_str(), "abcdefgh");
        for bad in [
            "agmen/v1/tenant01/project1/up/abcdefgh",
            "agmen/v1/tenant01/project1/presence/abcdefgh/x",
            "agmen/v1/tenant01/project1/presence",
            "agmen/v2/tenant01/project1/presence/abcdefgh",
            "agmen/v1/tenant01/project1/presence/ABC",
            "agmen/v1/t/project1/presence/abcdefgh",
        ] {
            assert!(!is_presence_topic(bad), "{bad}");
        }
    }

    #[test]
    fn statuses_change_and_clear() {
        let state = PresenceState::default();
        let project: ProjectId = "project1".parse().unwrap();
        let id: IdentityId = "abcdefgh".parse().unwrap();
        assert_eq!(state.status(&project, &id), Status::Offline);
        assert!(
            !state.set(&project, &id, Status::Offline),
            "already offline"
        );
        assert!(state.set(&project, &id, Status::Online));
        assert!(!state.set(&project, &id, Status::Online), "no change");
        assert!(state.set(&project, &id, Status::Away));
        assert_eq!(state.status(&project, &id), Status::Away);
        assert!(state.set(&project, &id, Status::Offline));
        assert!(
            state.view.lock().unwrap().present.is_empty(),
            "offline is absent"
        );
        state.set(&project, &id, Status::Online);
        let topics = Topics::new("tenant01".parse().unwrap(), project.clone());
        assert!(!state.seen(&topics));
        assert!(state.mark_seen(&topics), "the first since connect");
        assert!(!state.mark_seen(&topics));
        state.clear();
        assert_eq!(state.status(&project, &id), Status::Offline);
        assert!(!state.seen(&topics), "a connect starts over");
    }

    #[test]
    fn roster_entries_omit_what_is_unset() {
        let entry = RosterEntry {
            model: Some("opus".to_owned()),
            ..RosterEntry::new(
                "abcdefgh".parse().unwrap(),
                "andy/sol".to_owned(),
                IdentityKind::Agent,
                Status::Away,
            )
        };
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            serde_json::json!({
                "identity_id": "abcdefgh", "handle": "andy/sol", "kind": "agent",
                "status": "away", "model": "opus",
            })
        );
        assert_eq!(
            serde_json::from_slice::<PresenceMessage>(OFFLINE_PAYLOAD)
                .unwrap()
                .status,
            Status::Offline
        );
    }
}
