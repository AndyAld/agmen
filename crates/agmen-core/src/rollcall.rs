//! The hub's answer to ROLLCALL (protocol section 4): a STATUS in the feed,
//! in reply to the ROLLCALL, listing who is online or away from the same
//! presence the roster shows. No agent is woken to answer.
//!
//! `data.summary` is one line, `"<n> online: <handle>, <handle> (away)"`,
//! or `"Nobody is online."`; the body lists the same identities with their
//! kinds and ids. Clients that need the list as data read
//! `GET /projects/{p}/state?key=roster`.

use agmen_proto::{Body, IdentityId, IdentityKind, MessageId, Payload, ProtoError, Stream, Topics};

use crate::presence::{RosterEntry, Status};
use crate::{Core, PostError, RosterError, Store};

/// The most identities one answer lists; past it the summary ends with
/// "and N more", which keeps the body far below its 32 KiB limit.
const MAX_LISTED: usize = 100;

/// Why [`Core::answer_rollcall`] could not answer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RollcallError {
    /// Building the roster failed. The source is the [`RosterError`].
    #[error("building the roster failed")]
    Roster(#[from] RosterError),
    /// The answer's body is invalid.
    #[error("the answer's body is invalid")]
    Body(#[from] ProtoError),
    /// Posting the answer failed.
    #[error("posting the answer failed")]
    Post(#[from] PostError),
}

impl<S: Store> Core<S> {
    /// Posts the STATUS that answers ROLLCALL `rollcall` from `asker`.
    ///
    /// It mentions the asker when a human, as any reply to a human's message
    /// does (protocol section 3). The list names at most 100 identities.
    ///
    /// # Errors
    ///
    /// Returns [`RollcallError`] when the roster, the body or the post fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example<S: agmen_core::Store>(
    /// #     core: &agmen_core::Core<S>,
    /// #     rollcall: &agmen_proto::MessageId,
    /// #     asker: &agmen_proto::IdentityId,
    /// # ) -> Result<(), Box<dyn std::error::Error>> {
    /// let topics = agmen_proto::Topics::new("tenant01".parse()?, "project1".parse()?);
    /// core.answer_rollcall(&topics, rollcall, asker, agmen_proto::IdentityKind::Agent).await?;
    /// # Ok(()) }
    /// ```
    pub async fn answer_rollcall(
        &self,
        topics: &Topics,
        rollcall: &MessageId,
        asker: &IdentityId,
        asker_kind: IdentityKind,
    ) -> Result<(), RollcallError> {
        let roster = self.roster(topics).await?;
        let here: Vec<&RosterEntry> = roster
            .iter()
            .filter(|entry| entry.status != Status::Offline)
            .collect();
        let (summary, body) = describe(&here);
        self.post_as_hub(
            topics,
            crate::HubPost {
                payload: Payload::Status {
                    summary,
                    items: Vec::new(),
                    spend_usd: None,
                    hours: None,
                    tokens: None,
                },
                body: Body::try_from(body)?,
                reply_to: Some(rollcall.clone()),
                items: Vec::new(),
                lane: None,
                mentioned: if asker_kind == IdentityKind::Human {
                    vec![asker.clone()]
                } else {
                    Vec::new()
                },
                streams: vec![Stream::Feed],
            },
        )
        .await?;
        Ok(())
    }
}

/// The summary line and the body for the identities in `here`, in roster
/// (handle) order.
fn describe(here: &[&RosterEntry]) -> (String, String) {
    if here.is_empty() {
        let none = "Nobody is online.".to_owned();
        return (none.clone(), none);
    }
    let listed = &here[..here.len().min(MAX_LISTED)];
    let away = |entry: &RosterEntry| entry.status == Status::Away;
    let mut names: Vec<String> = listed
        .iter()
        .map(|entry| {
            if away(entry) {
                format!("{} (away)", entry.handle)
            } else {
                entry.handle.clone()
            }
        })
        .collect();
    let mut lines: Vec<String> = listed
        .iter()
        .map(|entry| {
            let status = if away(entry) { ", away" } else { "" };
            format!(
                "- {} ({}, `{}`{status})",
                entry.handle, entry.kind, entry.identity_id
            )
        })
        .collect();
    let more = here.len() - listed.len();
    if more > 0 {
        names.push(format!("and {more} more"));
        lines.push(format!("- and {more} more"));
    }
    let summary = format!("{} online: {}", here.len(), names.join(", "));
    let body = format!("Online now:\n{}\n", lines.join("\n"));
    (summary, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, handle: &str, status: Status) -> RosterEntry {
        RosterEntry::new(
            id.parse().unwrap(),
            handle.to_owned(),
            IdentityKind::Agent,
            status,
        )
    }

    #[test]
    fn describes_who_is_here() {
        let sol = entry("solsolsol", "andy/sol", Status::Online);
        let dee = entry("deedeedee", "andy/dee", Status::Away);
        let (summary, body) = describe(&[&dee, &sol]);
        assert_eq!(summary, "2 online: andy/dee (away), andy/sol");
        assert_eq!(
            body,
            "Online now:\n- andy/dee (agent, `deedeedee`, away)\n- andy/sol (agent, `solsolsol`)\n"
        );
        assert_eq!(describe(&[]).0, "Nobody is online.");
    }

    #[test]
    fn caps_the_list() {
        let many: Vec<RosterEntry> = (0..MAX_LISTED + 3)
            .map(|i| {
                entry(
                    &format!("agent{i:04}"),
                    &format!("andy/a{i:04}"),
                    Status::Online,
                )
            })
            .collect();
        let refs: Vec<&RosterEntry> = many.iter().collect();
        let (summary, body) = describe(&refs);
        assert!(summary.starts_with("103 online: andy/a0000, "), "{summary}");
        assert!(summary.ends_with("andy/a0099, and 3 more"), "{summary}");
        assert!(body.ends_with("- and 3 more\n"));
        assert!(Body::try_from(body).is_ok());
    }
}
