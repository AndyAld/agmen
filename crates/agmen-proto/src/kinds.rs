//! Identity kind permissions for NULL-verb fallback and role minting.

use crate::{IdentityKind, Verb};

/// Whether an identity with NULL stored verbs may send a protocol verb.
///
/// This fallback checks only the identity kind. REVIEW and PLAN require
/// explicit stored permissions. GRANT, DENY, ITEM and STATE are emitted
/// by the hub itself and are never accepted through ingest.
///
/// ```
/// use agmen_proto::{IdentityKind, Verb, kind_may_send};
///
/// assert!(kind_may_send(IdentityKind::Agent, Verb::Claim));
/// assert!(!kind_may_send(IdentityKind::Agent, Verb::Review));
/// ```
#[must_use]
pub const fn kind_may_send(kind: IdentityKind, verb: Verb) -> bool {
    use IdentityKind::{Agent, Coordinator, Human, Listener};
    match verb {
        Verb::Hello
        | Verb::Bye
        | Verb::Fyi
        | Verb::Ask
        | Verb::Answer
        | Verb::Ack
        | Verb::Status
        | Verb::Alert
        | Verb::Dm
        | Verb::Found
        | Verb::Proposal => matches!(kind, Human | Agent | Coordinator | Listener),
        Verb::Rollcall | Verb::Decision | Verb::Ruling => matches!(kind, Human | Coordinator),
        Verb::Claim | Verb::Heartbeat | Verb::Release | Verb::Done => {
            matches!(kind, Agent | Coordinator)
        }
        Verb::Lend => matches!(kind, Human | Agent | Coordinator),
        Verb::Assign => matches!(kind, Coordinator),
        Verb::Delegated => matches!(kind, Listener),
        Verb::PushRequest => matches!(kind, Agent),
        Verb::PushOk | Verb::PushNo => matches!(kind, Human),
        Verb::Review | Verb::Plan | Verb::Grant | Verb::Deny | Verb::Item | Verb::State => false,
    }
}

/// Whether an identity kind may retain a requested verb when minted with a role.
///
/// The mint ceiling extends [`kind_may_send`] with REVIEW and PLAN for
/// agents and coordinators, and ASSIGN for listeners. The hub additionally
/// restricts listener ASSIGN to the coordinator role. Checks that depend
/// on ownership or project membership remain with the hub.
///
/// ```
/// use agmen_proto::{IdentityKind, Verb, kind_ceiling, kind_may_send};
///
/// assert!(!kind_may_send(IdentityKind::Agent, Verb::Review));
/// assert!(kind_ceiling(IdentityKind::Agent, Verb::Review));
/// assert!(kind_ceiling(IdentityKind::Listener, Verb::Assign));
/// ```
#[must_use]
pub const fn kind_ceiling(kind: IdentityKind, verb: Verb) -> bool {
    kind_may_send(kind, verb)
        || (matches!(kind, IdentityKind::Agent | IdentityKind::Coordinator)
            && matches!(verb, Verb::Review | Verb::Plan))
        || (matches!(kind, IdentityKind::Listener) && matches!(verb, Verb::Assign))
}

#[cfg(test)]
#[path = "kinds_tests.rs"]
mod tests;
