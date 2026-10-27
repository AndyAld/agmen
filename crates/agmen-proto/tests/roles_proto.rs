//! Role and listener protocol contracts from Task 2.

use agmen_proto::{
    ClientEnvelope, IdentityKind, Mention, NackReason, Needs, Payload, Urgency, validate,
};
use serde_json::json;

#[test]
fn review_requires_reply_to() {
    let mut env: ClientEnvelope = serde_json::from_value(json!({
        "v": 1,
        "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS",
        "body": "Reviewed the DONE.",
        "sent_at": "2026-10-25T16:04:11Z",
        "verb": "REVIEW",
        "data": {"verdict": "pass", "findings": []}
    }))
    .unwrap();
    assert_eq!(validate(&env, env.sent_at), Err(NackReason::Schema));
    env.reply_to = Some("01JBX6Q4T7W3C2M8Y9K5N0P1RT".parse().unwrap());
    assert_eq!(validate(&env, env.sent_at), Ok(()));
}

#[test]
fn role_mention_round_trip() {
    let mention: Mention = "role:reviewer".parse().unwrap();
    assert_eq!(mention, Mention::Role("reviewer".parse().unwrap()));
    assert_eq!(mention.to_string(), "role:reviewer");
    let wire = json!("role:reviewer");
    assert_eq!(serde_json::to_value(&mention).unwrap(), wire);
    assert_eq!(serde_json::from_value::<Mention>(wire).unwrap(), mention);
    for invalid in ["role:Reviewer", "role:"] {
        assert!(invalid.parse::<Mention>().is_err());
        assert!(serde_json::from_value::<Mention>(json!(invalid)).is_err());
    }
}

#[test]
fn alert_defaults() {
    let payload: Payload = serde_json::from_value(json!({
        "verb": "ALERT", "data": {"need": "Review the plan"}
    }))
    .unwrap();
    assert_eq!(
        payload,
        Payload::Alert {
            need: "Review the plan".into(),
            urgency: Urgency::Normal,
            needs: Needs::Anyone,
            actions: Vec::new(),
        }
    );
}

#[test]
fn delegated_requires_agent_and_role() {
    let wire = json!({
        "verb": "DELEGATED",
        "data": {
            "worker": "idn7k2q9", "agent": "codex", "role": "engineer",
            "model": "model", "tier": "standard", "budget_usd": 5.0
        }
    });
    assert!(serde_json::from_value::<Payload>(wire.clone()).is_ok());
    for required in ["agent", "role"] {
        let mut missing = wire.clone();
        missing["data"].as_object_mut().unwrap().remove(required);
        assert!(serde_json::from_value::<Payload>(missing).is_err());
    }
}

#[test]
fn listener_kind_serializes() {
    assert_eq!(
        serde_json::to_value(IdentityKind::Listener).unwrap(),
        json!("listener")
    );
    assert_eq!(
        serde_json::from_value::<IdentityKind>(json!("listener")).unwrap(),
        IdentityKind::Listener
    );
    assert!(serde_json::from_value::<IdentityKind>(json!("dispatcher")).is_err());
}

#[test]
fn role_spec_round_trip() {
    use std::collections::BTreeSet;

    use agmen_proto::{EditScope, RoleSpec};

    for (edits, wire_edits) in [
        (EditScope::None, json!({"scope": "none"})),
        (EditScope::Lanes, json!({"scope": "lanes"})),
        (
            EditScope::Paths {
                globs: vec!["docs/**".into(), ".agmen/**".into()],
            },
            json!({"scope": "paths", "globs": ["docs/**", ".agmen/**"]}),
        ),
    ] {
        let spec = RoleSpec {
            role: "planner".parse().unwrap(),
            verbs: BTreeSet::from(["PROPOSAL".into(), "PLAN".into()]),
            edits,
            role_sha256: "a".repeat(64),
        };
        let wire = json!({
            "role": "planner", "verbs": ["PLAN", "PROPOSAL"],
            "edits": wire_edits, "role_sha256": "a".repeat(64)
        });
        assert_eq!(serde_json::to_value(&spec).unwrap(), wire);
        assert_eq!(serde_json::from_value::<RoleSpec>(wire).unwrap(), spec);
    }
    for edits in [json!({"scope": "unknown"}), json!({"scope": "paths"})] {
        assert!(serde_json::from_value::<EditScope>(edits).is_err());
    }
}

#[test]
fn plan_requires_positive_version() {
    for version in [0, 1, u32::MAX] {
        let env: ClientEnvelope = serde_json::from_value(json!({
            "v": 1,
            "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS",
            "body": "Plan revision.",
            "sent_at": "2026-10-25T16:04:11Z",
            "verb": "PLAN",
            "data": {"version": version, "path": "docs/plan.md", "sha": "4f2a9c1"}
        }))
        .unwrap();
        let expected = if version == 0 {
            Err(NackReason::Schema)
        } else {
            Ok(())
        };
        assert_eq!(validate(&env, env.sent_at), expected, "version {version}");
    }
}
