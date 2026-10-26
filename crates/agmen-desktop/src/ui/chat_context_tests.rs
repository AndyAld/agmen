use std::rc::Rc;

use agmen_proto::{Payload, StampedEnvelope};

use super::*;
use crate::backend::chat_snapshot;

fn ctx() -> Rc<ChatContext> {
    Rc::new(ChatContext::new(Rc::new(chat_snapshot())))
}

fn branch(name: &str, at: Option<&str>) -> RefTarget {
    RefTarget::Branch {
        name: name.into(),
        at: at.map(Into::into),
    }
}

fn file(path: &str, lines: Option<(u32, u32)>) -> RefTarget {
    RefTarget::File {
        path: path.into(),
        lines,
    }
}

fn row<'a>(card: &'a RefCard, label: &str) -> Option<&'a str> {
    card.rows
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, v)| v.as_str())
}

#[test]
fn hover_card_shows_agent_state_and_spend() {
    let card = ctx().card(&RefTarget::Agent("andy/fx1".into()));
    assert_eq!(card.kind, "Agent");
    assert_eq!(card.title, "andy/fx1");
    assert_eq!(row(&card, "Role"), Some("Engineer"));
    assert_eq!(row(&card, "Runs on"), Some("claude"));
    assert_eq!(row(&card, "State"), Some("working"));
    assert_eq!(row(&card, "Owner"), Some("andy (you)"));
    assert_eq!(row(&card, "Spent"), Some("$1.20 of $5.00"));
    assert_eq!(row(&card, "Branch"), Some("fix/c015-main-conflict"));

    let stranger = ctx().card(&RefTarget::Agent("zed/x9".into()));
    assert_eq!(
        row(&stranger, "State"),
        Some("not in this project's roster")
    );
}

#[test]
fn hover_card_shows_branch_worktree_and_last_done() {
    let card = ctx().card(&branch("fix/c015-main-conflict", Some("9e2a71c")));
    assert_eq!(card.kind, "Branch");
    assert_eq!(row(&card, "At"), Some("9e2a71c"));
    assert_eq!(
        row(&card, "Worktree"),
        Some("andy/fx1 · 1 commit, pushed, clean")
    );
    assert_eq!(row(&card, "Base"), Some("origin/main @4d9fdc8"));
    assert_eq!(row(&card, "Last DONE"), Some("andy/fx1 · C-015"));

    let elsewhere = ctx().card(&branch("test/b-415-replay-paging", None));
    assert_eq!(row(&elsewhere, "Worktree"), Some("none on this machine"));
}

#[test]
fn hover_card_shows_item_claim_done_and_review() {
    let card = ctx().card(&RefTarget::Item("C-015".into()));
    assert_eq!(card.kind, "Item");
    assert_eq!(card.title, "C-015");
    assert_eq!(row(&card, "Claimed by"), Some("andy/fx1"));
    assert_eq!(
        row(&card, "Done"),
        Some("andy/fx1 · fix/c015-main-conflict@9e2a71c")
    );
    assert_eq!(row(&card, "Review"), Some("pass · andy/rv2"));

    let unseen = ctx().card(&RefTarget::Item("Z-999".into()));
    assert_eq!(row(&unseen, "Claim"), Some("none in this chat"));
}

#[test]
fn hover_card_for_the_other_kinds() {
    let c = ctx();
    let commit = c.card(&RefTarget::Commit("9e2a71c".into()));
    assert_eq!(
        row(&commit, "In DONE"),
        Some("andy/fx1 · fix/c015-main-conflict")
    );
    let base = c.card(&RefTarget::Commit("4d9fdc8".into()));
    assert_eq!(
        row(&base, "Base of"),
        Some("andy/a1 · feat/c014-release-matrix, andy/fx1 · fix/c015-main-conflict")
    );

    let person = c.card(&RefTarget::Person("andy".into()));
    assert_eq!(person.title, "andy (you)");
    assert_eq!(row(&person, "Agents"), Some("listen, fx1, t7k, rv2, a1"));

    let f = c.card(&file("src/main.rs", Some((2, 8))));
    assert_eq!(f.title, "src/main.rs");
    assert_eq!(row(&f, "Lines"), Some("2 to 8"));
    assert_eq!(
        row(&f, "Changed in"),
        Some("andy/a1 · feat/c014-release-matrix, andy/fx1 · fix/c015-main-conflict")
    );
    let one = c.card(&file("src/main.rs", Some((4, 4))));
    assert_eq!(row(&one, "Lines"), Some("4"));

    let lane = c.card(&RefTarget::Lane("L-CORE".into()));
    assert_eq!(row(&lane, "Edited by"), Some("andy/fx1"));

    let role = c.card(&RefTarget::Role("engineer".into()));
    assert_eq!(row(&role, "Held by"), Some("andy/fx1, andy/a1, sean/a1"));

    let pr = c.card(&RefTarget::Pr(10));
    assert_eq!(pr.title, "#10");
    assert_eq!(row(&pr, "GitHub"), Some("not connected yet"));
}

/// `count` more DONEs like the fixture's andy/fx1 DONE, each from its own
/// branch, all touching `src/main.rs`.
fn many_dones(count: usize) -> Vec<StampedEnvelope> {
    let snapshot = chat_snapshot();
    let done = snapshot.messages[6].clone();
    (0..count)
        .map(|i| {
            let mut env = done.clone();
            env.client.id = format!("01JD{i:022}").parse().unwrap();
            if let Payload::Done { branch, .. } = &mut env.client.payload {
                *branch = format!("fix/many-{i}");
            }
            env
        })
        .collect()
}

#[test]
fn hover_card_rows_list_a_few_and_count_the_rest() {
    let mut snapshot = chat_snapshot();
    snapshot.messages.extend(many_dones(2_000));
    let c = ChatContext::new(Rc::new(snapshot));
    let card = c.card(&file("src/main.rs", None));
    let changed = row(&card, "Changed in").unwrap();
    // The fixture's two DONEs plus 2,000: the last four show.
    assert!(changed.ends_with(" and 1998 more"), "{changed}");
    assert_eq!(changed.matches(", ").count(), MAX_CARD_ENTRIES - 1);
    assert!(changed.starts_with("andy/fx1 · fix/many-1996"), "{changed}");
}

#[test]
fn repeated_entries_show_once() {
    let mut snapshot = chat_snapshot();
    let again = snapshot.messages[6].clone();
    snapshot.messages.push(again);
    let c = ChatContext::new(Rc::new(snapshot));
    let card = c.card(&RefTarget::Commit("9e2a71c".into()));
    assert_eq!(
        row(&card, "In DONE"),
        Some("andy/fx1 · fix/c015-main-conflict")
    );
}

#[test]
fn bodies_are_scanned_once_per_snapshot() {
    let snapshot = chat_snapshot();
    let m = snapshot.messages[1].clone();
    let c = ChatContext::new(Rc::new(snapshot));
    let first = c.body_of(&m);
    assert!(Rc::ptr_eq(&first, &c.body_of(&m)));
    assert_eq!(first.spans.len(), 3);
    assert_eq!(first.body.as_ref(), m.client.body.as_str());

    // A message that is not in the snapshot is not kept.
    let mut stranger = m;
    stranger.client.id = "01JD0000000000000000000001".parse().unwrap();
    assert!(!Rc::ptr_eq(&c.body_of(&stranger), &c.body_of(&stranger)));
}

#[test]
fn body_cache_is_keyed_by_hub_position_not_client_id() {
    // A second sender reusing the first message's client id must not be shown
    // the first message's cached body.
    let mut snapshot = chat_snapshot();
    let first = snapshot.messages[0].clone();
    let mut spoof = serde_json::to_value(&first).unwrap();
    spoof["seq"] = serde_json::json!(first.seq.0 + 1000);
    spoof["from_handle"] = serde_json::json!("zed/x9");
    spoof["body"] = serde_json::json!("a different body");
    let spoof: StampedEnvelope = serde_json::from_value(spoof).unwrap();
    assert_eq!(spoof.client.id, first.client.id);
    snapshot.messages.push(spoof.clone());
    let ctx = ChatContext::new(Rc::new(snapshot));
    assert_eq!(
        ctx.body_of(&first).body.as_ref(),
        first.client.body.as_str()
    );
    assert_eq!(ctx.body_of(&spoof).body.as_ref(), "a different body");
}
