//! Presence statuses and the roster built from them.
//!
//! [`Statuses`] is who is online or away. Anyone absent is offline. The
//! roster joins that map with the store's live identities, human members
//! and each identity's latest HELLO.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use agmen_proto::{IdentityId, IdentityKind, MAX_HELLO_FIELD_BYTES, ProjectId, Topics};
use serde::de::IntoDeserializer as _;
use serde::{Deserialize, Serialize};

use crate::{Core, Reads, RosterRow, Store, StoreError};

/// Whether an identity is there (protocol section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Connected and working.
    Online,
    /// Connected, but its person or agent is idle.
    Away,
    /// Not connected, or never was.
    Offline,
}

/// One identity in the roster (`GET /v1/projects/{p}/state?key=roster`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RosterEntry {
    pub(crate) identity_id: IdentityId,
    pub(crate) handle: String,
    pub(crate) kind: IdentityKind,
    /// Connected, idle, or absent. Absent identities are not stored in [`Statuses`].
    pub status: Status,
    /// The role supplied at mint, never taken from a HELLO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) role: Option<String>,
    /// The coding agent its latest HELLO named, unless mint stored one.
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    /// The model its latest HELLO named.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// The listener that started it, supplied at mint.
    #[serde(skip_serializing_if = "Option::is_none")]
    delegated_by: Option<IdentityId>,
}

impl RosterEntry {
    /// An entry with no HELLO details and no `delegated_by`.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub const fn new(
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
            role: None,
            agent: None,
            model: None,
            delegated_by: None,
        }
    }

    /// An entry with the mint row and the latest HELLO filled in.
    #[expect(
        clippy::too_many_arguments,
        reason = "one argument per roster column, so the private fields stay private"
    )]
    pub(crate) fn complete(
        identity_id: IdentityId,
        handle: String,
        kind: IdentityKind,
        status: Status,
        role: Option<String>,
        agent: Option<String>,
        model: Option<String>,
        delegated_by: Option<IdentityId>,
    ) -> Self {
        Self {
            identity_id,
            handle,
            kind,
            status,
            role,
            agent,
            model,
            delegated_by,
        }
    }
}

/// Who is online or away, by project.
///
/// Entries come from presence messages the broker admitted, one per
/// connected identity. Offline identities are absent, not stored.
#[derive(Debug, Default)]
pub struct Statuses {
    view: Mutex<View>,
}

#[derive(Debug, Default)]
struct View {
    present: HashMap<ProjectId, HashMap<IdentityId, Status>>,
}

impl Statuses {
    /// `id`'s status in `project`. Absent identities are [`Status::Offline`].
    #[must_use]
    pub fn status(&self, project: &ProjectId, id: &IdentityId) -> Status {
        self.view
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .present
            .get(project)
            .and_then(|ids| ids.get(id))
            .copied()
            .unwrap_or(Status::Offline)
    }

    /// Sets `id`'s status. Returns whether it changed.
    ///
    /// Offline removes the identity. The lock is not held across an await.
    pub fn set(&self, project: &ProjectId, id: &IdentityId, status: Status) -> bool {
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

    /// Forgets every status. A reconnect does this, then reads retained
    /// presence again, so an identity that left while this process was away
    /// does not stay online.
    pub fn clear(&self) {
        self.view
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .present
            .clear();
    }
}

/// Why [`Core::roster`] could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RosterError {
    /// The store failed. The source is the backend error.
    #[error("database error")]
    Database(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A stored identity id, kind or delegator could not be parsed.
    #[error("a stored identity is invalid: {0}")]
    Stored(String),
}

impl<S: Store> Core<S> {
    /// `topics`' roster: every live identity and every human member, by
    /// handle, with its status and its latest HELLO's agent and model.
    ///
    /// A minted agent name wins over the HELLO's agent. The model always
    /// comes from the latest HELLO. Both HELLO fields are cut to
    /// [`MAX_HELLO_FIELD_BYTES`]. An identity with no presence row is offline.
    ///
    /// # Errors
    ///
    /// Returns [`RosterError::Database`] when a store read fails and
    /// [`RosterError::Stored`] when a stored id or kind cannot be parsed.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example<S: agmen_core::Store>(core: &agmen_core::Core<S>) -> Result<(), Box<dyn std::error::Error>> {
    /// let topics = agmen_proto::Topics::new("tenant01".parse()?, "project1".parse()?);
    /// let roster = core.roster(&topics).await?;
    /// let _ = roster.len();
    /// # Ok(()) }
    /// ```
    pub async fn roster(&self, topics: &Topics) -> Result<Vec<RosterEntry>, RosterError> {
        let project = topics.project();
        let mut reader = self.store().reader();
        let members = reader.roster_rows(project).await.map_err(roster_store)?;
        let hellos = trimmed_hellos(reader.latest_hellos(project).await.map_err(roster_store)?);
        members
            .into_iter()
            .map(|row| assemble(self, project, row, &hellos))
            .collect()
    }
}

fn trimmed_hellos(hellos: crate::LatestHellos) -> crate::LatestHellos {
    hellos
        .into_iter()
        .map(|(id, (agent, model))| (id, (agent.map(hello_field), model.map(hello_field))))
        .collect()
}

fn assemble<S: Store>(
    core: &Core<S>,
    project: &ProjectId,
    row: RosterRow,
    hellos: &crate::LatestHellos,
) -> Result<RosterEntry, RosterError> {
    let identity_id = row
        .identity_id
        .parse()
        .map_err(|err| RosterError::Stored(format!("{err}")))?;
    let kind = IdentityKind::deserialize(row.kind.as_str().into_deserializer())
        .map_err(|err: serde::de::value::Error| RosterError::Stored(err.to_string()))?;
    let (agent, model) = hellos.get(&identity_id).cloned().unwrap_or_default();
    let delegated_by = row
        .delegated_by
        .map(|id| id.parse())
        .transpose()
        .map_err(|err| RosterError::Stored(format!("{err}")))?;
    Ok(RosterEntry::complete(
        identity_id.clone(),
        row.handle,
        kind,
        core.statuses().status(project, &identity_id),
        row.role,
        row.agent.or(agent),
        model,
        delegated_by,
    ))
}

fn roster_store(err: StoreError) -> RosterError {
    match err {
        StoreError::Backend(source) => RosterError::Database(source),
        StoreError::Stored(text) => RosterError::Stored(text),
        other => RosterError::Database(Box::new(other)),
    }
}

/// `field` cut to [`MAX_HELLO_FIELD_BYTES`] at a character boundary. Ingest
/// refuses a longer HELLO field, so this only trims one recorded before that
/// limit, which would otherwise inflate the roster array.
fn hello_field(mut field: String) -> String {
    field.truncate(field.floor_char_boundary(MAX_HELLO_FIELD_BYTES));
    field
}

#[cfg(test)]
mod tests {
    use super::*;

    use agmen_proto::MAX_HELLO_FIELD_BYTES;

    const OFFLINE_PAYLOAD: &[u8] = br#"{"status":"offline"}"#;

    #[derive(Debug, serde::Deserialize)]
    struct PresenceMessage {
        status: Status,
    }

    #[test]
    fn statuses_change_and_clear() {
        let state = Statuses::default();
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
        state.clear();
        assert_eq!(state.status(&project, &id), Status::Offline);
    }

    #[test]
    fn hello_fields_are_cut_at_a_character_boundary() {
        assert_eq!(hello_field("opus".to_owned()), "opus");
        let long = "é".repeat(MAX_HELLO_FIELD_BYTES);
        let cut = hello_field(long);
        assert_eq!(cut.len(), MAX_HELLO_FIELD_BYTES);
        assert_eq!(cut, "é".repeat(MAX_HELLO_FIELD_BYTES / 2));
        let odd = format!("a{}", "é".repeat(MAX_HELLO_FIELD_BYTES));
        assert_eq!(hello_field(odd).len(), MAX_HELLO_FIELD_BYTES - 1);
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

    #[tokio::test]
    async fn roster_lists_identities_and_humans_by_handle() {
        use std::time::Duration;

        use agmen_proto::Seq;

        use crate::{Core, MemStore, Member, MemberRole};

        let store = MemStore::default();
        let topics = Topics::new("tenant01".parse().unwrap(), "project1".parse().unwrap());
        let project = topics.project().clone();
        // The ids sort the other way round from the handles, so a store that
        // lists rows by id instead of by handle fails the order check below.
        let amy: IdentityId = "zzzzzzzz".parse().unwrap();
        let sam: IdentityId = "mmmmmmmm".parse().unwrap();
        let zed: IdentityId = "aaaaaaaa".parse().unwrap();
        store
            .seed_live_identity(&project, &amy, "amy", IdentityKind::Agent)
            .await
            .unwrap();
        store
            .seed_live_identity(&project, &zed, "zed", IdentityKind::Agent)
            .await
            .unwrap();
        store
            .set_minted_agent(&project, &zed, "minted-agent")
            .await
            .unwrap();
        store
            .seed_member(
                &project,
                Member {
                    id: sam.clone(),
                    handle: "sam".to_owned(),
                    kind: IdentityKind::Human,
                    owner: None,
                    role: Some(MemberRole::Member),
                    identity_role: None,
                },
            )
            .await
            .unwrap();
        insert_hello(
            &store,
            &project,
            &amy,
            Seq(1),
            Some("old-agent"),
            Some("old"),
        )
        .await;
        insert_hello(
            &store,
            &project,
            &amy,
            Seq(2),
            Some("hello-agent"),
            Some("opus"),
        )
        .await;
        insert_hello(
            &store,
            &project,
            &zed,
            Seq(1),
            Some("hello-zed"),
            Some("sonnet"),
        )
        .await;

        let core = Core::new(store, Duration::from_secs(60));
        assert!(core.statuses().set(&project, &amy, Status::Online));
        assert!(core.statuses().set(&project, &zed, Status::Away));
        let roster = core.roster(&topics).await.unwrap();

        assert_eq!(roster.len(), 3);
        assert_eq!(roster[0].handle, "amy");
        assert_eq!(roster[0].kind, IdentityKind::Agent);
        assert_eq!(roster[0].status, Status::Online);
        assert_eq!(roster[0].agent.as_deref(), Some("hello-agent"));
        assert_eq!(roster[0].model.as_deref(), Some("opus"));
        assert_eq!(roster[0].role, None);
        assert_eq!(roster[1].handle, "sam");
        assert_eq!(roster[1].kind, IdentityKind::Human);
        assert_eq!(roster[1].status, Status::Offline);
        assert_eq!(
            roster[1].role, None,
            "a human membership role is not the mint role"
        );
        assert_eq!(roster[1].agent, None);
        assert_eq!(roster[1].model, None);
        assert_eq!(roster[2].handle, "zed");
        assert_eq!(roster[2].status, Status::Away);
        assert_eq!(
            roster[2].agent.as_deref(),
            Some("minted-agent"),
            "the minted agent wins over the latest HELLO"
        );
        assert_eq!(roster[2].model.as_deref(), Some("sonnet"));
    }

    async fn insert_hello(
        store: &crate::MemStore,
        project: &ProjectId,
        from: &IdentityId,
        seq: agmen_proto::Seq,
        agent: Option<&str>,
        model: Option<&str>,
    ) {
        use agmen_proto::{Stream, Timestamp};

        use crate::{MessageRow, Store, StoreTx};

        let envelope = serde_json::json!({
            "verb": "HELLO",
            "data": { "agent": agent, "model": model },
        });
        let id = crate::new_message_id(Timestamp::now()).unwrap();
        let mut tx = store.begin().await.unwrap();
        tx.insert_message(&MessageRow {
            project,
            stream: &Stream::Feed,
            seq,
            id: &id,
            from,
            payload_hash: b"hash",
            envelope_json: &envelope.to_string(),
            hub_at: Timestamp::now(),
            ack_row: true,
        })
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
}
