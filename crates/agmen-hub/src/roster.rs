//! The roster and the small head published on retained `state/roster`.
//!
//! [`Hub::roster`] is every live identity and human member. The retained
//! topic carries only [`RosterHead`], so it stays under the 4 KiB state
//! limit at any roster size. The full array is
//! `GET /v1/projects/{p}/state?key=roster`.

use std::collections::HashMap;

use agmen_proto::{IdentityId, IdentityKind, MAX_HELLO_FIELD_BYTES, Topics};
use serde::de::IntoDeserializer as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::Hub;
use crate::presence::{RosterEntry, Status};

/// The retained `state/roster` payload: how many entries, how many are
/// online, and a short hash of the full array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RosterHead {
    pub(crate) count: usize,
    /// Entries whose status is `online`. `away` is not counted.
    pub(crate) online: usize,
    /// First 16 lowercase hex characters of the SHA-256 of the array's JSON.
    pub(crate) etag: String,
}

/// Why the roster could not be built.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RosterError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("a stored identity is invalid: {0}")]
    Stored(String),
}

/// `count`, `online` and the etag of `roster`.
///
/// `etag` is the first 16 lowercase hex characters of the SHA-256 of
/// `serde_json::to_vec(roster)`.
#[must_use]
pub(crate) fn roster_head(roster: &[RosterEntry]) -> RosterHead {
    let online = roster
        .iter()
        .filter(|entry| entry.status == Status::Online)
        .count();
    // RosterEntry is strings, enums and options of those, so this does not
    // fail. An empty stand-in keeps a head publishable if it ever does.
    let encoded = serde_json::to_vec(roster).unwrap_or_else(|err| {
        tracing::error!(error = %err, "encoding the roster for its etag failed");
        Vec::new()
    });
    RosterHead {
        count: roster.len(),
        online,
        etag: etag_of(&encoded),
    }
}

/// The first 16 lowercase hex characters of the SHA-256 of `bytes`.
fn etag_of(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = crate::db::hex(digest.as_slice());
    debug_assert!(
        hex.len() >= 16,
        "SHA-256 hex is 64 characters, got {}",
        hex.len()
    );
    hex.truncate(16);
    hex
}

impl Hub {
    /// `topics`' roster: every live identity and every human member, by
    /// handle, with its status and its latest HELLO's agent and model.
    pub(crate) async fn roster(&self, topics: &Topics) -> Result<Vec<RosterEntry>, RosterError> {
        let project = topics.project();
        type RosterRow = (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let members: Vec<RosterRow> = sqlx::query_as(
            "SELECT id, kind, handle, role, agent, delegated_by FROM identities
             WHERE project_id = $1 AND revoked_at IS NULL
             UNION ALL
             SELECT h.id, 'human', h.handle, NULL::text, NULL::text, NULL::text FROM memberships m
             JOIN humans h ON h.id = m.human_id
             WHERE m.project_id = $1
             ORDER BY 3",
        )
        .bind(project.as_str())
        .fetch_all(self.pool())
        .await?;
        // Uses the messages_latest_hello index (0005_presence). One row per
        // identity that has posted a HELLO, so at most one per live identity.
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
        .map(
            |(from, agent, model): (String, Option<String>, Option<String>)| {
                (from, (agent.map(hello_field), model.map(hello_field)))
            },
        )
        .collect();
        members
            .into_iter()
            .map(|(id, kind, handle, role, minted_agent, delegated_by)| {
                let (agent, model) = hellos.get(&id).cloned().unwrap_or_default();
                let identity_id: IdentityId = id
                    .parse()
                    .map_err(|err| RosterError::Stored(format!("{err}")))?;
                let kind = IdentityKind::deserialize(kind.as_str().into_deserializer())
                    .map_err(|err: serde::de::value::Error| RosterError::Stored(err.to_string()))?;
                let status = self.presence().status(project, &identity_id);
                let delegated_by = delegated_by
                    .map(|id| id.parse())
                    .transpose()
                    .map_err(|err| RosterError::Stored(format!("{err}")))?;
                Ok(RosterEntry::complete(
                    identity_id,
                    handle,
                    kind,
                    status,
                    role,
                    minted_agent.or(agent),
                    model,
                    delegated_by,
                ))
            })
            .collect()
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

    fn entry(n: usize, handle: &str, status: Status) -> RosterEntry {
        let id = format!("id{n:06}");
        RosterEntry::new(
            id.parse().unwrap(),
            handle.to_owned(),
            IdentityKind::Agent,
            status,
        )
    }

    #[test]
    fn head_counts_and_hashes() {
        let roster = vec![
            entry(1, "andy/sol", Status::Online),
            entry(2, "andy/dee", Status::Away),
            entry(3, "andy", Status::Offline),
        ];
        let head = roster_head(&roster);
        assert_eq!(head.count, 3);
        assert_eq!(head.online, 1, "away is not online");
        assert_eq!(head.etag.len(), 16);
        assert!(
            head.etag
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{}",
            head.etag
        );
        let encoded = serde_json::to_vec(&roster).unwrap();
        let digest = Sha256::digest(&encoded);
        assert_eq!(head.etag, &crate::db::hex(digest.as_slice())[..16]);
        assert_eq!(
            roster_head(&roster).etag,
            head.etag,
            "the same array hashes the same"
        );

        let mut changed = roster.clone();
        changed[0] = entry(1, "andy/sol", Status::Away);
        assert_ne!(
            roster_head(&changed).etag,
            head.etag,
            "a status change changes the etag"
        );
    }

    #[test]
    fn head_stays_small() {
        let handle = "h".repeat(39);
        // 500 is the bound: a head of any roster this size stays under 4 KiB.
        let roster: Vec<_> = (0..500)
            .map(|n| entry(n, &handle, Status::Offline))
            .collect();
        let bytes = serde_json::to_vec(&roster_head(&roster)).unwrap();
        assert!(bytes.len() < 4_096, "{} bytes", bytes.len());
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
}
