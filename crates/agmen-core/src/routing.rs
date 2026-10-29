//! Mention expansion and message stream routing.

use std::collections::BTreeSet;

use agmen_proto::{
    ClientEnvelope, IdentityId, IdentityKind, Mention, NackReason, Payload, Severity, Stream, Verb,
};

use crate::roster::{Member, Roster, Sender};

/// Whether `kind` may send `verb`, from the "Sent by" column of protocol
/// section 4. Checks that need more than the kind (claim holder, lane
/// owner, the PUSH_REQUEST's owner) belong to the verbs' own handling.
///
/// - "coordinator" there means the `coordinator` kind. A listener with the
///   coordinator role uses its stored permissions for ASSIGN.
/// - This table is the fallback for NULL verbs. REVIEW and PLAN require
///   explicit stored permissions and stay closed in this legacy table.
/// - GRANT, DENY, ITEM and STATE are the hub's own (ruling R34).
#[must_use]
pub const fn may_send(kind: IdentityKind, verb: Verb) -> bool {
    agmen_proto::kind_may_send(kind, verb)
}

/// Whether handling `env` needs the project's members.
#[must_use]
pub fn needs_roster(verb: Verb, env: &ClientEnvelope) -> bool {
    !env.mentions.is_empty()
        || !env.to.is_empty()
        || matches!(
            verb,
            Verb::Fyi
                | Verb::Ask
                | Verb::Answer
                | Verb::Alert
                | Verb::Done
                | Verb::Review
                | Verb::Assign
        )
}

/// What `mentions` expand to (protocol section 3): `all`, `agents` and
/// `humans` expand to the project's live members of those kinds, an
/// identity must be a live member, and the sender is left out.
///
/// # Errors
///
/// `forbidden_mention` for anything but identities from a kind other than
/// human and coordinator, or an identity that is not a live member;
/// `schema` for lane mentions, which remain reserved. Role mentions expand
/// from stored mint roles, including identities currently offline.
pub fn expand_mentions(
    sender: &Sender,
    mentions: &[Mention],
    roster: &Roster,
) -> Result<BTreeSet<IdentityId>, NackReason> {
    let may_group = matches!(sender.kind, IdentityKind::Human | IdentityKind::Coordinator);
    let mut out = BTreeSet::new();
    for mention in mentions {
        let pick: fn(&Member) -> bool = match mention {
            Mention::Identity(id) => {
                if roster.get(id).is_none() {
                    return Err(NackReason::ForbiddenMention);
                }
                out.insert(id.clone());
                continue;
            }
            _ if !may_group => return Err(NackReason::ForbiddenMention),
            Mention::Lane(_) => return Err(NackReason::Schema),
            Mention::Role(role) => {
                out.extend(
                    roster
                        .0
                        .iter()
                        .filter(|member| member.identity_role.as_deref() == Some(role.as_str()))
                        .map(|member| member.id.clone()),
                );
                continue;
            }
            Mention::All => |_| true,
            Mention::Agents => {
                |member| !matches!(member.kind, IdentityKind::Human | IdentityKind::Hub)
            }
            Mention::Humans => |member| member.kind == IdentityKind::Human,
        };
        out.extend(
            roster
                .0
                .iter()
                .filter(|member| pick(member))
                .map(|member| member.id.clone()),
        );
    }
    out.remove(&sender.id);
    Ok(out)
}

/// The `mentioned` a message is stamped with (protocol section 3): its
/// expanded mentions, the author of `reply_to` when a human, and for an
/// ALERT the sender's owning human and the coordinators (protocol 4
/// notifies them, and protocol 3 interrupts only through `mentioned`:
/// ruling R35). Never the sender. Sorted by id.
///
/// # Errors
///
/// Returns the same mention errors as [`expand_mentions`].
pub fn mentioned(
    verb: Verb,
    env: &ClientEnvelope,
    sender: &Sender,
    roster: &Roster,
    parent: Option<&Parent>,
) -> Result<Vec<IdentityId>, NackReason> {
    let mut mentioned = expand_mentions(sender, &env.mentions, roster)?;
    if let Some(parent) = parent
        && (parent.from_kind == IdentityKind::Human || verb == Verb::Review)
    {
        mentioned.insert(parent.from.clone());
    }
    if verb == Verb::Alert {
        mentioned.extend(sender.owner.iter().cloned());
        mentioned.extend(roster.coordinators().cloned());
    }
    mentioned.remove(&sender.id);
    Ok(mentioned.into_iter().collect())
}

/// The message a `reply_to` names, as the sender can see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parent {
    /// The author of the parent message.
    pub from: IdentityId,
    /// The author's identity kind.
    pub from_kind: IdentityKind,
    /// The stored verb name.
    pub verb: String,
    /// Every stream the parent was stored in.
    pub streams: BTreeSet<Stream>,
}

/// The streams a message goes to (protocol section 4):
///
/// - DM, and ASK with `to` (ruling R28): each recipient's inbox, the
///   sender's inbox and each owning human's inbox; never the feed.
/// - ANSWER, DELEGATED and REVIEW: the streams of the message they reply
///   to, so a reply in a private thread stays private; the feed when that
///   message was in the feed or is unknown.
/// - ASSIGN: the feed and the assignee's inbox.
/// - HEARTBEAT: the sender's own inbox, so it is not in the feed (protocol
///   4) and the ACK still names a copy.
/// - FOUND with severity `security`: the inboxes of the sender and its
///   owning human; the feed gets only the hub's ITEM.
/// - Everything else: the feed.
///
/// # Errors
///
/// `schema` when a recipient or assignee is not a live member.
pub fn route(
    verb: Verb,
    env: &ClientEnvelope,
    sender: &Sender,
    roster: &Roster,
    parent: Option<&Parent>,
) -> Result<BTreeSet<Stream>, NackReason> {
    let feed = || BTreeSet::from([Stream::Feed]);
    match verb {
        Verb::Dm => fan_out(&env.to, sender, roster),
        Verb::Ask if !env.to.is_empty() => fan_out(&env.to, sender, roster),
        Verb::Answer | Verb::Delegated | Verb::Review => Ok(match parent {
            Some(parent)
                if !parent.streams.is_empty() && !parent.streams.contains(&Stream::Feed) =>
            {
                let mut streams = parent.streams.clone();
                streams.insert(Stream::Inbox(sender.id.clone()));
                streams
            }
            _ => feed(),
        }),
        Verb::Assign => {
            let Payload::Assign { to, .. } = &env.payload else {
                return Err(NackReason::Schema);
            };
            if roster.get(to).is_none() {
                return Err(NackReason::Schema);
            }
            Ok(BTreeSet::from([Stream::Feed, Stream::Inbox(to.clone())]))
        }
        Verb::Heartbeat => Ok(BTreeSet::from([Stream::Inbox(sender.id.clone())])),
        Verb::Found
            if matches!(
                env.payload,
                Payload::Found {
                    severity: Some(Severity::Security),
                    ..
                }
            ) =>
        {
            fan_out(&[], sender, roster)
        }
        _ => Ok(feed()),
    }
}

/// The inboxes of `to`, of the sender, and of every owning human of them.
///
/// # Errors
///
/// Returns `schema` when a recipient is not a live member.
pub fn fan_out(
    to: &[IdentityId],
    sender: &Sender,
    roster: &Roster,
) -> Result<BTreeSet<Stream>, NackReason> {
    let mut streams = BTreeSet::from([Stream::Inbox(sender.id.clone())]);
    streams.extend(sender.owner.iter().cloned().map(Stream::Inbox));
    for recipient in to {
        let member = roster.get(recipient).ok_or(NackReason::Schema)?;
        streams.insert(Stream::Inbox(recipient.clone()));
        streams.extend(member.owner.iter().cloned().map(Stream::Inbox));
    }
    Ok(streams)
}

/// The copy an ACK reports (ruling R33a): the feed copy when there is one,
/// else the sender's own inbox copy, else the first.
#[must_use]
pub fn ack_stream(streams: &BTreeSet<Stream>, from: &IdentityId) -> Option<Stream> {
    let own = Stream::Inbox(from.clone());
    if streams.contains(&Stream::Feed) {
        Some(Stream::Feed)
    } else if streams.contains(&own) {
        Some(own)
    } else {
        streams.first().cloned()
    }
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod tests;
