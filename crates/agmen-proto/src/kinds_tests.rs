use super::*;
use crate::IdentityKind::{Agent, Coordinator, Hub, Human, Listener};
use crate::Verb::{
    Ack, Alert, Answer, Ask, Assign, Bye, Claim, Decision, Delegated, Deny, Dm, Done, Found, Fyi,
    Grant, Heartbeat, Hello, Item, Lend, Plan, Proposal, PushNo, PushOk, PushRequest, Release,
    Review, Rollcall, Ruling, State, Status,
};

const KINDS: [IdentityKind; 5] = [Human, Agent, Coordinator, Listener, Hub];

// Each row records (kind, verb, NULL-verb fallback, mint ceiling).
const KIND_TABLE: [(IdentityKind, Verb, bool, bool); KINDS.len() * Verb::ALL.len()] = [
    (Human, Hello, true, true),
    (Human, Bye, true, true),
    (Human, Fyi, true, true),
    (Human, Ask, true, true),
    (Human, Answer, true, true),
    (Human, Ack, true, true),
    (Human, Status, true, true),
    (Human, Rollcall, true, true),
    (Human, Claim, false, false),
    (Human, Grant, false, false),
    (Human, Deny, false, false),
    (Human, Heartbeat, false, false),
    (Human, Release, false, false),
    (Human, Assign, false, false),
    (Human, Delegated, false, false),
    (Human, Lend, true, true),
    (Human, Found, true, true),
    (Human, Item, false, false),
    (Human, Done, false, false),
    (Human, Review, false, false),
    (Human, Proposal, true, true),
    (Human, Plan, false, false),
    (Human, Decision, true, true),
    (Human, Ruling, true, true),
    (Human, PushRequest, false, false),
    (Human, PushOk, true, true),
    (Human, PushNo, true, true),
    (Human, Alert, true, true),
    (Human, Dm, true, true),
    (Human, State, false, false),
    (Agent, Hello, true, true),
    (Agent, Bye, true, true),
    (Agent, Fyi, true, true),
    (Agent, Ask, true, true),
    (Agent, Answer, true, true),
    (Agent, Ack, true, true),
    (Agent, Status, true, true),
    (Agent, Rollcall, false, false),
    (Agent, Claim, true, true),
    (Agent, Grant, false, false),
    (Agent, Deny, false, false),
    (Agent, Heartbeat, true, true),
    (Agent, Release, true, true),
    (Agent, Assign, false, false),
    (Agent, Delegated, false, false),
    (Agent, Lend, true, true),
    (Agent, Found, true, true),
    (Agent, Item, false, false),
    (Agent, Done, true, true),
    (Agent, Review, false, true),
    (Agent, Proposal, true, true),
    (Agent, Plan, false, true),
    (Agent, Decision, false, false),
    (Agent, Ruling, false, false),
    (Agent, PushRequest, true, true),
    (Agent, PushOk, false, false),
    (Agent, PushNo, false, false),
    (Agent, Alert, true, true),
    (Agent, Dm, true, true),
    (Agent, State, false, false),
    (Coordinator, Hello, true, true),
    (Coordinator, Bye, true, true),
    (Coordinator, Fyi, true, true),
    (Coordinator, Ask, true, true),
    (Coordinator, Answer, true, true),
    (Coordinator, Ack, true, true),
    (Coordinator, Status, true, true),
    (Coordinator, Rollcall, true, true),
    (Coordinator, Claim, true, true),
    (Coordinator, Grant, false, false),
    (Coordinator, Deny, false, false),
    (Coordinator, Heartbeat, true, true),
    (Coordinator, Release, true, true),
    (Coordinator, Assign, true, true),
    (Coordinator, Delegated, false, false),
    (Coordinator, Lend, true, true),
    (Coordinator, Found, true, true),
    (Coordinator, Item, false, false),
    (Coordinator, Done, true, true),
    (Coordinator, Review, false, true),
    (Coordinator, Proposal, true, true),
    (Coordinator, Plan, false, true),
    (Coordinator, Decision, true, true),
    (Coordinator, Ruling, true, true),
    (Coordinator, PushRequest, false, false),
    (Coordinator, PushOk, false, false),
    (Coordinator, PushNo, false, false),
    (Coordinator, Alert, true, true),
    (Coordinator, Dm, true, true),
    (Coordinator, State, false, false),
    (Listener, Hello, true, true),
    (Listener, Bye, true, true),
    (Listener, Fyi, true, true),
    (Listener, Ask, true, true),
    (Listener, Answer, true, true),
    (Listener, Ack, true, true),
    (Listener, Status, true, true),
    (Listener, Rollcall, false, false),
    (Listener, Claim, false, false),
    (Listener, Grant, false, false),
    (Listener, Deny, false, false),
    (Listener, Heartbeat, false, false),
    (Listener, Release, false, false),
    (Listener, Assign, false, true),
    (Listener, Delegated, true, true),
    (Listener, Lend, false, false),
    (Listener, Found, true, true),
    (Listener, Item, false, false),
    (Listener, Done, false, false),
    (Listener, Review, false, false),
    (Listener, Proposal, true, true),
    (Listener, Plan, false, false),
    (Listener, Decision, false, false),
    (Listener, Ruling, false, false),
    (Listener, PushRequest, false, false),
    (Listener, PushOk, false, false),
    (Listener, PushNo, false, false),
    (Listener, Alert, true, true),
    (Listener, Dm, true, true),
    (Listener, State, false, false),
    (Hub, Hello, false, false),
    (Hub, Bye, false, false),
    (Hub, Fyi, false, false),
    (Hub, Ask, false, false),
    (Hub, Answer, false, false),
    (Hub, Ack, false, false),
    (Hub, Status, false, false),
    (Hub, Rollcall, false, false),
    (Hub, Claim, false, false),
    (Hub, Grant, false, false),
    (Hub, Deny, false, false),
    (Hub, Heartbeat, false, false),
    (Hub, Release, false, false),
    (Hub, Assign, false, false),
    (Hub, Delegated, false, false),
    (Hub, Lend, false, false),
    (Hub, Found, false, false),
    (Hub, Item, false, false),
    (Hub, Done, false, false),
    (Hub, Review, false, false),
    (Hub, Proposal, false, false),
    (Hub, Plan, false, false),
    (Hub, Decision, false, false),
    (Hub, Ruling, false, false),
    (Hub, PushRequest, false, false),
    (Hub, PushOk, false, false),
    (Hub, PushNo, false, false),
    (Hub, Alert, false, false),
    (Hub, Dm, false, false),
    (Hub, State, false, false),
];

#[test]
fn kind_tables_match_the_spec() {
    for kind in KINDS {
        for verb in Verb::ALL {
            let rows: Vec<_> = KIND_TABLE
                .iter()
                .filter(|row| row.0 == kind && row.1 == verb)
                .collect();
            assert_eq!(rows.len(), 1, "{kind} {verb}: exactly one literal row");
            let (_, _, may_send, ceiling) = *rows[0];
            assert_eq!(
                kind_may_send(kind, verb),
                may_send,
                "{kind} {verb}: fallback"
            );
            assert_eq!(kind_ceiling(kind, verb), ceiling, "{kind} {verb}: ceiling");
        }
    }
}

#[test]
fn kind_ceiling_extends_kind_may_send() {
    for kind in KINDS {
        for verb in Verb::ALL {
            assert!(
                !kind_may_send(kind, verb) || kind_ceiling(kind, verb),
                "{kind} {verb}"
            );
        }
    }
}
