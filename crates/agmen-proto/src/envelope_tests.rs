use jiff::SignedDuration;
use serde_json::{Value, json};

use super::*;
use crate::verbs::{Payload, Verb};

const NOW: &str = "2026-10-25T16:04:11Z";

fn now() -> Timestamp {
    NOW.parse().unwrap()
}

fn id(n: u8) -> IdentityId {
    format!("ident{n:03}").parse().unwrap()
}

fn msg_id() -> MessageId {
    "01JBX6Q4T7W3C2M8Y9K5N0P1RS".parse().unwrap()
}

fn envelope(payload: Payload) -> ClientEnvelope {
    ClientEnvelope {
        v: 1,
        id: msg_id(),
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: Body::try_from("hello").unwrap(),
        valid_until: None,
        sent_at: now(),
        payload,
    }
}

/// The protocol section 3 example, as a client publishes it.
fn done_json() -> Value {
    json!({
        "v": 1,
        "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS",
        "verb": "DONE",
        "reply_to": null,
        "to": null,
        "mentions": [],
        "items": ["B-412"],
        "lane": "L-UI",
        "body": "Fixed the truncation notice; regression spec added.",
        "data": {
            "item": "B-412",
            "branch": "fix/truncation",
            "commit": "4f2a9c1",
            "base_sha": "9b8e7d6",
            "checks": [{"name": "nextest", "passed": 48, "failed": 0, "baseline": null}],
            "changes": [{"status": "M", "path": "ui/src/notice.ts"}]
        },
        "valid_until": null,
        "sent_at": "2026-10-25T16:04:11.123Z"
    })
}

fn parse(value: &Value) -> Result<ClientEnvelope, ProtoError> {
    ClientEnvelope::from_json(&serde_json::to_vec(value).unwrap())
}

#[test]
fn spec_example_parses_and_round_trips() {
    let env = parse(&done_json()).unwrap();
    assert_eq!(env.payload.verb(), Some(Verb::Done));
    assert!(env.to.is_empty());
    let again = parse(&serde_json::to_value(&env).unwrap()).unwrap();
    assert_eq!(again, env);
    assert_eq!(validate(&env, now()), Ok(()));
}

#[test]
fn to_parses_from_null_absent_or_list() {
    let mut value = done_json();
    value.as_object_mut().unwrap().remove("to");
    assert!(parse(&value).unwrap().to.is_empty());
    value["to"] = json!(["abcdefgh"]);
    assert_eq!(parse(&value).unwrap().to, vec!["abcdefgh".parse().unwrap()]);
}

#[test]
fn unknown_field_rejected() {
    let mut value = done_json();
    value["from"] = json!("abcdefgh");
    let err = parse(&value).unwrap_err();
    assert!(
        matches!(&err, ProtoError::UnknownField(name) if name == "from"),
        "{err}"
    );
    assert_eq!(NackReason::from(&err), NackReason::Schema);
    assert!(serde_json::from_value::<ClientEnvelope>(value).is_err());
}

#[test]
fn oversize_body_maps_to_body_too_large() {
    let mut value = done_json();
    value["body"] = json!("a".repeat(MAX_BODY_BYTES + 1));
    let err = parse(&value).unwrap_err();
    assert_eq!(NackReason::from(&err), NackReason::BodyTooLarge);
}

#[test]
fn oversize_envelope_is_schema() {
    let mut value = done_json();
    value["data"]["branch"] = json!("b".repeat(MAX_ENVELOPE_BYTES));
    let err = parse(&value).unwrap_err();
    assert!(matches!(err, ProtoError::EnvelopeTooLarge { .. }), "{err}");
    assert_eq!(NackReason::from(&err), NackReason::Schema);
}

#[test]
fn stamped_envelope_tolerates_unknown_fields() {
    let mut value = done_json();
    let stamped = json!({
        "stream": "feed",
        "seq": 1842,
        "from": "idn7k2q9",
        "from_handle": "andy/sol",
        "from_kind": "agent",
        "mentioned": [],
        "refs": [{"kind": "item", "start": 0, "end": 5, "target": "B-412"}],
        "hub_at": "2026-10-25T16:04:11.402Z",
        "added_in_a_later_minor_version": true
    });
    for (key, field) in stamped.as_object().unwrap() {
        value[key] = field.clone();
    }
    let env: StampedEnvelope = serde_json::from_value(value).unwrap();
    assert_eq!(env.seq, Seq(1842));
    assert_eq!(env.from_kind, IdentityKind::Agent);
    assert_eq!(env.refs[0].kind, RefKind::Item);
    assert_eq!(env.client.payload.verb(), Some(Verb::Done));
    let round: StampedEnvelope =
        serde_json::from_value(serde_json::to_value(&env).unwrap()).unwrap();
    assert_eq!(round, env);
}

#[test]
fn version_must_be_one() {
    let mut env = envelope(Payload::Fyi {});
    env.v = 2;
    assert_eq!(validate(&env, now()), Err(NackReason::Schema));
}

#[test]
fn valid_until_skew() {
    let mut env = envelope(Payload::Fyi {});
    env.valid_until = Some(now() - SignedDuration::from_secs(119));
    assert_eq!(validate(&env, now()), Ok(()));
    env.valid_until = Some(now() - SignedDuration::from_secs(120));
    assert_eq!(validate(&env, now()), Ok(()));
    env.valid_until = Some(now() - SignedDuration::from_secs(121));
    assert_eq!(validate(&env, now()), Err(NackReason::Expired));
    env.valid_until = Some(Timestamp::MAX);
    assert_eq!(validate(&env, now()), Ok(()));
}

#[test]
fn dm_requires_recipients() {
    let mut dm = envelope(Payload::Dm {});
    assert_eq!(validate(&dm, now()), Err(NackReason::Schema));
    dm.to = (0..9).map(id).collect();
    assert_eq!(validate(&dm, now()), Err(NackReason::Schema));
    dm.to = (0..8).map(id).collect();
    assert_eq!(validate(&dm, now()), Ok(()));
    let mut fyi = envelope(Payload::Fyi {});
    fyi.to = vec![id(1)];
    assert_eq!(validate(&fyi, now()), Err(NackReason::Schema));
}

fn delegated() -> Payload {
    Payload::Delegated {
        worker: id(1),
        agent: "claude-code".to_owned(),
        role: "engineer".parse().unwrap(),
        model: "sonnet".to_owned(),
        tier: "standard".to_owned(),
        budget_usd: 5.0,
        item: None,
    }
}

#[test]
fn delegated_requires_reply_to() {
    let mut env = envelope(delegated());
    assert_eq!(validate(&env, now()), Err(NackReason::Schema));
    env.reply_to = Some(msg_id());
    assert_eq!(validate(&env, now()), Ok(()));
}

#[test]
fn replies_require_reply_to() {
    for payload in [
        Payload::Answer {},
        Payload::Ack {},
        Payload::PushOk {},
        Payload::PushNo {},
    ] {
        let mut env = envelope(payload);
        assert_eq!(validate(&env, now()), Err(NackReason::Schema), "{env:?}");
        env.reply_to = Some(msg_id());
        assert_eq!(validate(&env, now()), Ok(()), "{env:?}");
    }
}

#[test]
fn claim_requires_valid_until() {
    let mut claim = envelope(Payload::Claim {
        item: "B-1".parse().unwrap(),
    });
    assert_eq!(validate(&claim, now()), Err(NackReason::Schema));
    claim.valid_until = Some(now() + SignedDuration::from_secs(60));
    assert_eq!(validate(&claim, now()), Ok(()));
    claim.valid_until = Some(now() - SignedDuration::from_secs(600));
    assert_eq!(validate(&claim, now()), Err(NackReason::Expired));
}

#[test]
fn done_requires_item_and_commit() {
    for (field, replacement) in [
        ("item", None),
        ("commit", None),
        ("item", Some(json!(""))),
        ("commit", Some(json!(""))),
        ("commit", Some(json!("not a sha"))),
    ] {
        let mut value = done_json();
        let data = value["data"].as_object_mut().unwrap();
        match replacement {
            None => {
                data.remove(field);
            }
            Some(bad) => {
                data.insert(field.to_owned(), bad);
            }
        }
        let err = parse(&value).unwrap_err();
        assert_eq!(NackReason::from(&err), NackReason::Schema, "{field}: {err}");
    }
}

#[test]
fn mentions_only_on_chat_verbs() {
    let mut fyi = envelope(Payload::Fyi {});
    fyi.mentions = vec![Mention::All];
    assert_eq!(validate(&fyi, now()), Ok(()));
    let mut claim = envelope(Payload::Claim {
        item: "B-1".parse().unwrap(),
    });
    claim.valid_until = Some(now());
    claim.mentions = vec![Mention::All];
    assert_eq!(validate(&claim, now()), Err(NackReason::ForbiddenMention));
    fyi.mentions = (0..9).map(|n| Mention::Identity(id(n))).collect();
    assert_eq!(validate(&fyi, now()), Err(NackReason::ForbiddenMention));
    fyi.mentions.truncate(8);
    fyi.mentions.push(Mention::Humans);
    assert_eq!(validate(&fyi, now()), Ok(()));
}

#[test]
fn ask_takes_zero_to_eight_recipients() {
    let mut ask = envelope(Payload::Ask { of: None });
    assert_eq!(validate(&ask, now()), Ok(()));
    ask.to = vec![id(1)];
    assert_eq!(validate(&ask, now()), Ok(()));
    ask.to = (0..8).map(id).collect();
    assert_eq!(validate(&ask, now()), Ok(()));
    ask.to = (0..9).map(id).collect();
    assert_eq!(validate(&ask, now()), Err(NackReason::Schema));
}

/// A stamped FYI with its verb and data replaced by ones this version lacks.
fn stamped_with_verb(verb: &str, data: Value) -> Value {
    json!({
        "v": 1,
        "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS",
        "verb": verb,
        "data": data,
        "body": "hi",
        "sent_at": NOW,
        "stream": "feed",
        "seq": 1843,
        "from": "idn7k2q9",
        "from_handle": "andy/sol",
        "from_kind": "agent",
        "hub_at": NOW
    })
}

#[test]
fn stamped_envelope_keeps_unknown_verb() {
    let data = json!({"pull": 7, "nested": {"ok": true}});
    let value = stamped_with_verb("WAVE", data.clone());
    let env: StampedEnvelope = serde_json::from_value(value).unwrap();
    assert_eq!(env.seq, Seq(1843));
    assert_eq!(env.stream, Stream::Feed);
    assert_eq!(
        env.client.payload,
        Payload::Unknown {
            verb: "WAVE".to_owned(),
            data: data.clone()
        }
    );
    assert_eq!(env.client.payload.verb(), None);
    let out = serde_json::to_value(&env).unwrap();
    assert_eq!(out["verb"], json!("WAVE"));
    assert_eq!(out["data"], data);
    let round: StampedEnvelope = serde_json::from_value(out).unwrap();
    assert_eq!(round, env);
}

#[test]
fn stamped_envelope_still_checks_known_verb_data() {
    let value = stamped_with_verb("CLAIM", json!({"item": "not a key"}));
    assert!(serde_json::from_value::<StampedEnvelope>(value).is_err());
}

#[test]
fn client_envelope_rejects_unknown_verb() {
    let mut value = done_json();
    value["verb"] = json!("WAVE");
    let err = parse(&value).unwrap_err();
    assert!(
        matches!(&err, ProtoError::UnknownVerb(name) if name == "WAVE"),
        "{err}"
    );
    assert_eq!(NackReason::from(&err), NackReason::Schema);
    assert!(serde_json::from_value::<ClientEnvelope>(value).is_err());
}

#[test]
fn validate_bounds_hello_fields() {
    let hello = |client: &str, agent: Option<&str>, model: Option<&str>, looking: Option<&str>| {
        envelope(Payload::Hello {
            client: client.to_owned(),
            agent: agent.map(str::to_owned),
            model: model.map(str::to_owned),
            looking_at: looking.map(str::to_owned),
        })
    };
    let at_limit = "m".repeat(MAX_HELLO_FIELD_BYTES);
    let over = "m".repeat(MAX_HELLO_FIELD_BYTES + 1);
    let fine = hello(&at_limit, Some(&at_limit), Some(&at_limit), Some(&at_limit));
    assert_eq!(validate(&fine, now()), Ok(()));
    for env in [
        hello(&over, None, None, None),
        hello("x", Some(&over), None, None),
        hello("x", None, Some(&"m".repeat(50_000)), None),
        hello("x", None, None, Some(&over)),
    ] {
        assert_eq!(validate(&env, now()), Err(NackReason::Schema));
    }
}

#[test]
fn validate_rejects_unknown_payload() {
    let env = envelope(Payload::Unknown {
        verb: "WAVE".to_owned(),
        data: json!({}),
    });
    assert_eq!(validate(&env, now()), Err(NackReason::Schema));
}
