use serde_json::{Value, json};

use super::*;

fn ts(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn id(text: &str) -> IdentityId {
    text.parse().unwrap()
}

fn item(text: &str) -> ItemKey {
    text.parse().unwrap()
}

fn sha(text: &str) -> GitSha {
    text.parse().unwrap()
}

/// One value per verb, with every optional field set so the round trip covers it.
fn every_payload() -> Vec<Payload> {
    vec![
        Payload::Hello {
            client: "agmen-cli/0.1.0".to_owned(),
            agent: Some("claude-code/2.1.310".to_owned()),
            model: Some("opus".to_owned()),
            looking_at: Some("B-412".to_owned()),
        },
        Payload::Bye {},
        Payload::Fyi {},
        Payload::Ask {
            of: Some(id("abcdefgh")),
        },
        Payload::Answer {},
        Payload::Ack {},
        Payload::Status {
            summary: "halfway".to_owned(),
            items: vec![item("B-1")],
            spend_usd: Some(1.25),
            hours: Some(0.5),
            tokens: Some(Tokens {
                input: 1_000,
                output: 200,
                cache_read: 30_000,
            }),
        },
        Payload::Rollcall {},
        Payload::Claim { item: item("B-1") },
        Payload::Grant {
            item: item("B-1"),
            lease_until: ts("2026-10-25T16:34:11Z"),
        },
        Payload::Deny {
            item: item("B-1"),
            reason: DenyReason::HeldBy,
        },
        Payload::Heartbeat {
            items: vec![item("B-1"), item("B-2")],
        },
        Payload::Release {
            item: item("B-1"),
            reason: Some("lease_expired".to_owned()),
        },
        Payload::Assign {
            item: item("B-1"),
            to: id("abcdefgh"),
        },
        Payload::Delegated {
            worker: id("worker01"),
            agent: "claude-code".to_owned(),
            role: "engineer".parse().unwrap(),
            model: "sonnet".to_owned(),
            tier: "standard".to_owned(),
            budget_usd: 5.0,
            item: Some(item("B-1")),
        },
        Payload::Lend {
            lane: "L-UI".parse().unwrap(),
            paths: vec!["ui/src/**".to_owned()],
            to: id("abcdefgh"),
            until: ts("2026-10-25T18:00:00Z"),
        },
        Payload::Found {
            title: "Notice truncates".to_owned(),
            lane: Some("L-UI".parse().unwrap()),
            severity: Some(Severity::Security),
        },
        Payload::Item { key: item("B-413") },
        Payload::Done {
            item: item("B-412"),
            branch: "fix/truncation".to_owned(),
            commit: sha("4f2a9c1"),
            base_sha: sha("9b8e7d6"),
            checks: vec![Check {
                name: "nextest".to_owned(),
                passed: 48,
                failed: 1,
                baseline: Some(1),
            }],
            changes: vec![Change {
                status: "M".to_owned(),
                path: "ui/src/notice.ts".to_owned(),
            }],
        },
        Payload::Review {
            verdict: Verdict::Changes,
            findings: vec![Finding {
                severity: FindingSeverity::High,
                path: "ui/src/notice.ts".to_owned(),
                line: Some(12),
                what: "off by one".to_owned(),
                trigger: "a 101-character notice".to_owned(),
            }],
            traced: Some("render path".to_owned()),
        },
        Payload::Proposal {
            options: vec!["a".to_owned(), "b".to_owned()],
            deadline: Some(ts("2026-10-26T00:00:00Z")),
        },
        Payload::Plan {
            version: 3,
            path: "docs/plans/x.plan.toml".to_owned(),
            sha: sha("abcdef1"),
        },
        Payload::Decision {
            outcome: Outcome::Go,
            option: Some(1),
            subject: Some("docs/specs/x.md@abcdef1".to_owned()),
        },
        Payload::Ruling {
            subject: "use EMQX".to_owned(),
        },
        Payload::PushRequest {
            remote: "origin".to_owned(),
            git_ref: "refs/heads/fix/truncation".to_owned(),
            sha: sha("4f2a9c1"),
        },
        Payload::PushOk {},
        Payload::PushNo {},
        Payload::Alert {
            need: "budget stop".to_owned(),
            urgency: Urgency::Urgent,
            needs: Needs::Owner,
            actions: vec!["Raise to $10".to_owned()],
        },
        Payload::Dm {},
        Payload::State {
            key: "roster".to_owned(),
            v: 7,
            as_of_seq: Seq(1842),
        },
    ]
}

#[test]
fn payload_round_trip_all_verbs() {
    let payloads = every_payload();
    assert_eq!(payloads.len(), Verb::ALL.len(), "one sample per verb");
    for (payload, verb) in payloads.iter().zip(Verb::ALL) {
        assert_eq!(payload.verb(), verb);
        let value = serde_json::to_value(payload).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 2, "{value}");
        assert_eq!(object["verb"], json!(verb.to_string()));
        assert!(object["data"].is_object(), "{value}");
        let back: Payload = serde_json::from_value(value).unwrap();
        assert_eq!(&back, payload);
    }
}

#[test]
fn verb_names_are_uppercase_wire_names() {
    assert_eq!(Verb::PushRequest.to_string(), "PUSH_REQUEST");
    assert_eq!(Verb::Fyi.to_string(), "FYI");
    assert_eq!(
        serde_json::to_value(Verb::PushOk).unwrap(),
        json!("PUSH_OK")
    );
    assert_eq!(
        serde_json::from_value::<Verb>(json!("ROLLCALL")).unwrap(),
        Verb::Rollcall
    );
}

#[test]
fn optional_data_fields_may_be_absent() {
    let hello: Payload =
        serde_json::from_value(json!({"verb": "HELLO", "data": {"client": "agmen-cli/0.1.0"}}))
            .unwrap();
    assert!(matches!(hello, Payload::Hello { agent: None, .. }));
    let alert: Payload =
        serde_json::from_value(json!({"verb": "ALERT", "data": {"need": "eyes"}})).unwrap();
    assert_eq!(
        alert,
        Payload::Alert {
            need: "eyes".to_owned(),
            urgency: Urgency::Normal,
            needs: Needs::Anyone,
            actions: Vec::new(),
        }
    );
}

#[test]
fn push_request_ref_is_named_ref_on_the_wire() {
    let value: Value = serde_json::to_value(Payload::PushRequest {
        remote: "origin".to_owned(),
        git_ref: "refs/heads/main".to_owned(),
        sha: sha("abcdef1"),
    })
    .unwrap();
    assert_eq!(value["data"]["ref"], json!("refs/heads/main"));
}
