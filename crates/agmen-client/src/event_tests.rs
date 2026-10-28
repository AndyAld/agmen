use agmen_proto::{NackReason, Seq};
use serde_json::{Value, json};

use super::*;

const ME: &str = "idmememe";
const OWNER: &str = "idowner1";
const OTHER: &str = "idother1";
const MY_MESSAGE: &str = "01JBX6Q4T7W3C2M8Y9K5N0P1AA";

fn me() -> Me {
    Me {
        identity_id: ME.parse().unwrap(),
        handle: "owner/sol".to_owned(),
        owner_id: OWNER.parse().unwrap(),
    }
}

/// A stamped message from `from` on `stream` with `verb` and `data`, plus
/// any `extra` top-level fields.
fn stamped(from: &str, stream: &str, verb: &str, data: Value, extra: Value) -> StampedEnvelope {
    let mut value = json!({
        "v": 1, "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS", "verb": verb, "data": data,
        "body": "hi", "sent_at": "2026-10-25T16:04:11Z", "stream": stream, "seq": 3,
        "from": from, "from_handle": "x/y", "from_kind": "agent",
        "hub_at": "2026-10-25T16:04:11Z",
    });
    for (key, field) in extra.as_object().unwrap() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).unwrap()
}

fn is_mine(id: &MessageId) -> bool {
    id.as_str() == MY_MESSAGE
}

#[test]
fn events_serialize_as_r31_says() {
    let line = |event: &ClientEvent| serde_json::to_value(event).unwrap();
    assert_eq!(
        line(&ClientEvent::Me(me())),
        json!({"kind": "self", "identity_id": ME, "handle": "owner/sol", "owner_id": OWNER})
    );
    let message = stamped(OTHER, "feed", "FYI", json!({}), json!({}));
    let value = line(&ClientEvent::message(message.clone()));
    assert_eq!(value["kind"], "message");
    assert_eq!(value["message"], serde_json::to_value(&message).unwrap());
    assert_eq!(value.as_object().unwrap().len(), 2);
    let id: MessageId = MY_MESSAGE.parse().unwrap();
    assert_eq!(
        line(&ClientEvent::Delivery {
            id: id.clone(),
            state: OutboxState::Pending { attempts: 2 }
        }),
        json!({"kind": "delivery", "id": MY_MESSAGE, "state": {"state": "pending", "attempts": 2}})
    );
    assert_eq!(
        line(&ClientEvent::Delivery {
            id: id.clone(),
            state: OutboxState::Recorded {
                stream: Stream::Feed,
                seq: Seq(9)
            }
        }),
        json!({"kind": "delivery", "id": MY_MESSAGE,
               "state": {"state": "recorded", "stream": "feed", "seq": 9}})
    );
    assert_eq!(
        line(&ClientEvent::Delivery {
            id,
            state: OutboxState::Rejected {
                reason: NackReason::ForbiddenVerb
            }
        }),
        json!({"kind": "delivery", "id": MY_MESSAGE,
               "state": {"state": "rejected", "reason": "forbidden_verb"}})
    );
    assert_eq!(
        line(&ClientEvent::Connection { connected: false }),
        json!({"kind": "connection", "connected": false})
    );
    // Lines parse back.
    let parsed: ClientEvent =
        serde_json::from_value(json!({"kind": "connection", "connected": true})).unwrap();
    assert_eq!(parsed, ClientEvent::Connection { connected: true });
}

#[test]
fn messages_for_me() {
    let me = me();
    let inbox = format!("inbox:{ME}");
    let cases = [
        (
            "mentioned",
            stamped(OTHER, "feed", "FYI", json!({}), json!({"mentioned": [ME]})),
        ),
        (
            "a DM to me",
            stamped(OTHER, &inbox, "DM", json!({}), json!({"to": [ME]})),
        ),
        (
            "an ASK to me",
            stamped(OTHER, &inbox, "ASK", json!({}), json!({"to": [OTHER, ME]})),
        ),
        (
            "an ASSIGN to me",
            stamped(
                OTHER,
                "feed",
                "ASSIGN",
                json!({"item": "B-1", "to": ME}),
                json!({}),
            ),
        ),
        (
            "a GRANT in my inbox",
            stamped(
                "hubhubhub",
                &inbox,
                "GRANT",
                json!({"item": "B-1", "lease_until": "2026-10-25T16:34:11Z"}),
                json!({}),
            ),
        ),
        (
            "a DENY in my inbox",
            stamped(
                "hubhubhub",
                &inbox,
                "DENY",
                json!({"item": "B-1", "reason": "held_by"}),
                json!({}),
            ),
        ),
        (
            "an ANSWER to my message",
            stamped(
                OTHER,
                "feed",
                "ANSWER",
                json!({}),
                json!({"reply_to": MY_MESSAGE}),
            ),
        ),
        (
            "an ALERT from my owner",
            stamped(OWNER, "feed", "ALERT", json!({"need": "look"}), json!({})),
        ),
    ];
    for (what, message) in &cases {
        assert!(me.is_for_me(message, is_mine), "{what}");
    }
}

#[test]
fn messages_not_for_me() {
    let me = me();
    let inbox = format!("inbox:{ME}");
    let cases = [
        (
            "my own message that mentions me",
            stamped(ME, "feed", "FYI", json!({}), json!({"mentioned": [ME]})),
        ),
        (
            "my own DM, copied to my inbox",
            stamped(ME, &inbox, "DM", json!({}), json!({"to": [OTHER]})),
        ),
        (
            "a plain FYI",
            stamped(OTHER, "feed", "FYI", json!({}), json!({})),
        ),
        (
            "an ASK to someone else",
            stamped(OTHER, "feed", "ASK", json!({}), json!({"to": [OTHER]})),
        ),
        (
            "an ASSIGN to someone else",
            stamped(
                OTHER,
                "feed",
                "ASSIGN",
                json!({"item": "B-1", "to": OTHER}),
                json!({}),
            ),
        ),
        (
            "a GRANT on the feed",
            stamped(
                "hubhubhub",
                "feed",
                "GRANT",
                json!({"item": "B-1", "lease_until": "2026-10-25T16:34:11Z"}),
                json!({}),
            ),
        ),
        (
            "an ANSWER to someone else's message",
            stamped(
                OTHER,
                "feed",
                "ANSWER",
                json!({}),
                json!({"reply_to": "01JBX6Q4T7W3C2M8Y9K5N0P1BB"}),
            ),
        ),
        (
            "an ALERT from someone else",
            stamped(OTHER, "feed", "ALERT", json!({"need": "look"}), json!({})),
        ),
        (
            "an unknown verb",
            stamped(OTHER, &inbox, "WAVE", json!({}), json!({"to": [ME]})),
        ),
    ];
    for (what, message) in &cases {
        assert!(!me.is_for_me(message, is_mine), "{what}");
    }
}
