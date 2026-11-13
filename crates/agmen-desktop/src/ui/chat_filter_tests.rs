use std::collections::BTreeSet;

use agmen_proto::{IdentityId, IdentityKind, StampedEnvelope};
use serde_json::json;

use super::{ChatFilter, my_agents};
use crate::backend::{AgentRow, AgentStatus, PersonGroup, chat_snapshot};
use crate::ui::chat::visible_rows;

fn message(kind: IdentityKind) -> StampedEnvelope {
    serde_json::from_value(json!({
        "v": 1,
        "id": "01JD0000000000000000000001",
        "body": "test",
        "sent_at": "2026-11-07T22:00:00Z",
        "verb": "FYI",
        "data": {},
        "stream": "feed",
        "seq": 1,
        "from": "seana100",
        "from_handle": "sean/a1",
        "from_kind": kind,
        "hub_at": "2026-11-07T22:00:00Z",
    }))
    .unwrap()
}

#[test]
fn everyone_shows_every_row() {
    assert_eq!(ChatFilter::default(), ChatFilter::Everyone);
    assert_eq!(
        ChatFilter::ALL.map(ChatFilter::label),
        ["My Agents", "Everyone", "Just humans"]
    );
    for kind in [
        IdentityKind::Human,
        IdentityKind::Agent,
        IdentityKind::Coordinator,
        IdentityKind::Listener,
        IdentityKind::Hub,
    ] {
        assert!(ChatFilter::Everyone.shows(&message(kind), &BTreeSet::new()));
    }
}

#[test]
fn just_humans_shows_only_human_senders() {
    let mine = BTreeSet::from(["andyfx10".parse().unwrap()]);
    for kind in [
        IdentityKind::Human,
        IdentityKind::Agent,
        IdentityKind::Coordinator,
        IdentityKind::Listener,
        IdentityKind::Hub,
    ] {
        let mut env = message(kind);
        env.client.to.extend(mine.iter().cloned());
        assert_eq!(
            ChatFilter::JustHumans.shows(&env, &mine),
            kind == IdentityKind::Human,
            "{kind:?}"
        );
    }
}

#[test]
fn my_agents_shows_from_to_and_mentions_of_mine() {
    let id: IdentityId = "andyfx10".parse().unwrap();
    let mine = BTreeSet::from([id.clone()]);
    let other = message(IdentityKind::Agent);
    assert!(!ChatFilter::MyAgents.shows(&other, &mine));
    assert!(!ChatFilter::MyAgents.shows(&other, &BTreeSet::new()));

    let mut from = other.clone();
    from.from = id.clone();
    assert!(ChatFilter::MyAgents.shows(&from, &mine));

    let mut dm = other.clone();
    dm.stream = agmen_proto::Stream::Inbox(id.clone());
    dm.client.to = vec![other.from.clone(), id.clone()];
    assert!(ChatFilter::MyAgents.shows(&dm, &mine));

    let mut mention = other;
    mention.mentioned = vec![mention.from.clone(), id];
    assert!(ChatFilter::MyAgents.shows(&mention, &mine));
    // The hub includes a reply's author in mentioned.
    mention.client.reply_to = Some("01JD0000000000000000000002".parse().unwrap());
    assert!(ChatFilter::MyAgents.shows(&mention, &mine));
}

#[test]
fn my_agents_hides_my_own_posts_that_address_no_agent_of_mine() {
    let mine = BTreeSet::from(["andyfx10".parse().unwrap()]);
    let mut own = message(IdentityKind::Human);
    own.from = "andy0000".parse().unwrap();
    own.from_handle = "andy".into();
    assert!(!ChatFilter::MyAgents.shows(&own, &mine));
    own.client.to.extend(mine.iter().cloned());
    assert!(ChatFilter::MyAgents.shows(&own, &mine));
}

#[test]
fn my_agents_reads_the_you_group_with_listener() {
    let agent = |id: &str, role: &str| AgentRow {
        id: id.parse().unwrap(),
        handle: format!("andy/{role}"),
        name: role.into(),
        role: role.into(),
        harness: "claude".into(),
        status: AgentStatus::Offline,
        depth: 0,
        item: None,
        color: None,
    };
    let mut snapshot = chat_snapshot();
    snapshot.roster = vec![
        PersonGroup {
            id: "sean0000".parse().unwrap(),
            person: "sean".into(),
            you: false,
            agents: vec![agent("seana100", "engineer")],
        },
        PersonGroup {
            id: snapshot.me_id.clone(),
            person: snapshot.me.clone(),
            you: true,
            agents: vec![agent("andyfx10", "engineer"), agent("andylist", "listener")],
        },
    ];
    assert_eq!(
        my_agents(&snapshot),
        BTreeSet::from(["andyfx10".parse().unwrap(), "andylist".parse().unwrap()])
    );
    let limit = agmen_local::safeguards::MAX_MANAGED;
    snapshot.roster[1].agents = (0..=limit)
        .map(|i| agent(&format!("agent{i:08}"), "engineer"))
        .collect();
    assert_eq!(my_agents(&snapshot).len(), limit);
    snapshot.roster.clear();
    assert!(my_agents(&snapshot).is_empty());
}

#[test]
fn filter_and_ledger_combine() {
    let mut value = serde_json::to_value(message(IdentityKind::Agent)).unwrap();
    value["verb"] = json!("HEARTBEAT");
    value["data"] = json!({"items": []});
    let beat: StampedEnvelope = serde_json::from_value(value).unwrap();
    let mine = BTreeSet::from([beat.from.clone()]);
    let messages = [beat];
    assert!(visible_rows(&messages, false, ChatFilter::MyAgents, &mine).is_empty());
    assert_eq!(
        visible_rows(&messages, true, ChatFilter::MyAgents, &mine),
        vec![0]
    );
    assert!(visible_rows(&messages, true, ChatFilter::JustHumans, &mine).is_empty());
    assert!(visible_rows(&messages, true, ChatFilter::MyAgents, &BTreeSet::new()).is_empty());
}
