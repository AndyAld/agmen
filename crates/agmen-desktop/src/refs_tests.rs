use agmen_proto::{Ref, RefKind};

use super::*;
use crate::backend::chat_snapshot;

fn spans_of(body: &str, hub: &[Ref], known: &Known) -> Vec<(String, RefTarget)> {
    find_refs(body, hub, known)
        .into_iter()
        .map(|s| (body[s.range.clone()].to_owned(), s.target))
        .collect()
}

fn chat_spans() -> Vec<Vec<(String, RefTarget)>> {
    let snapshot = chat_snapshot();
    let known = Known::from_snapshot(&snapshot);
    snapshot
        .messages
        .iter()
        .map(|m| spans_of(m.client.body.as_str(), &m.refs, &known))
        .collect()
}

fn text(spans: &[(String, RefTarget)]) -> Vec<&str> {
    spans.iter().map(|(t, _)| t.as_str()).collect()
}

#[test]
fn refs_find_every_kind_in_chat_fixture() {
    let all = chat_spans();
    // The chips the mockup draws (agmen-c7's Task 9 sheet, section 4).
    assert_eq!(
        text(&all[0]),
        [
            "C014",
            "feat/c014-release-matrix@7c8835e",
            "#9",
            "release.yml"
        ]
    );
    assert_eq!(text(&all[1]), ["4d9fdc8", "src/main.rs:2-8", "@sean/a1"]);
    assert!(all[2].is_empty());
    assert_eq!(
        text(&all[3]),
        ["016f235", "4d9fdc8", "4d9fdc8", "src/main.rs:2-8"]
    );
    assert_eq!(text(&all[4]), ["andy/fx1", "fix/c015-main-conflict"]);
    assert_eq!(text(&all[6]), ["C015", "fix/c015-main-conflict@9e2a71c"]);
    assert!(all[7].is_empty());
    assert_eq!(text(&all[8]), ["@andy", "#10"]);

    assert_eq!(all[0][0].1, RefTarget::Item("C-014".into()));
    assert_eq!(
        all[0][1].1,
        RefTarget::Branch {
            name: "feat/c014-release-matrix".into(),
            at: Some("7c8835e".into())
        }
    );
    assert_eq!(all[0][2].1, RefTarget::Pr(9));
    assert_eq!(
        all[0][3].1,
        RefTarget::File {
            path: ".github/workflows/release.yml".into(),
            lines: None
        }
    );
    assert_eq!(all[1][0].1, RefTarget::Commit("4d9fdc8".into()));
    assert_eq!(
        all[1][1].1,
        RefTarget::File {
            path: "src/main.rs".into(),
            lines: Some((2, 8))
        }
    );
    assert_eq!(all[1][2].1, RefTarget::Agent("sean/a1".into()));
    assert_eq!(all[8][0].1, RefTarget::Person("andy".into()));
}

#[test]
fn refs_ignore_non_commit_hex() {
    let known = Known::from_snapshot(&chat_snapshot());
    assert!(spans_of("see abc1234 and deadbee", &[], &known).is_empty());
}

#[test]
fn refs_ignore_version_numbers() {
    let known = Known::from_snapshot(&chat_snapshot());
    assert!(spans_of("bump to v1.2.3 or 1.2.3, see 2026-10-25", &[], &known).is_empty());
}

#[test]
fn text_that_only_looks_like_a_ref_stays_plain() {
    // No hub ref: identities, items and PRs are the hub's to stamp (spec 9.3).
    let known = Known::from_snapshot(&chat_snapshot());
    assert!(spans_of("andy/fx1 did C015 in #9", &[], &known).is_empty());
    // A branch shape git does not know stays plain too.
    assert!(spans_of("try fix/not-a-branch", &[], &known).is_empty());
}

fn hub(kind: RefKind, start: u32, end: u32, target: &str) -> Ref {
    Ref {
        kind,
        start,
        end,
        target: target.into(),
    }
}

#[test]
fn refs_reject_offsets_off_char_boundary() {
    let known = Known::from_snapshot(&chat_snapshot());
    let body = "café C015";
    // "é" is 2 bytes at 3..5; a ref ending at 4 splits it.
    let bad = [
        hub(RefKind::Item, 0, 4, "C-015"),
        hub(RefKind::Item, 6, 99, "C-015"),
    ];
    assert!(spans_of(body, &bad, &known).is_empty());
    let good = [hub(RefKind::Item, 6, 10, "C-015")];
    assert_eq!(text(&spans_of(body, &good, &known)), ["C015"]);
}

#[test]
fn refs_prefer_hub_ref_over_regex_overlap() {
    let known = Known::from_snapshot(&chat_snapshot());
    // The hub stamps the whole name; the regex would match the commit inside.
    let body = "lane L-CORE 4d9fdc8";
    let refs = [hub(RefKind::Lane, 5, 19, "L-CORE")];
    let spans = spans_of(body, &refs, &known);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].1, RefTarget::Lane("L-CORE".into()));
}

#[test]
fn refs_pr_and_item_and_lane() {
    let known = Known::from_snapshot(&chat_snapshot());
    let body = "R-1 in L-CORE, PR #12 for @reviewers";
    let refs = [
        hub(RefKind::Item, 0, 3, "R-1"),
        hub(RefKind::Lane, 7, 13, "L-CORE"),
        hub(RefKind::Pr, 18, 21, "12"),
        hub(RefKind::Role, 26, 36, "reviewer"),
    ];
    let spans = spans_of(body, &refs, &known);
    assert_eq!(
        spans.into_iter().map(|(_, t)| t).collect::<Vec<_>>(),
        [
            RefTarget::Item("R-1".into()),
            RefTarget::Lane("L-CORE".into()),
            RefTarget::Pr(12),
            RefTarget::Role("reviewer".into()),
        ]
    );
}

#[test]
fn spans_are_sorted_and_disjoint() {
    let snapshot = chat_snapshot();
    let known = Known::from_snapshot(&snapshot);
    for m in &snapshot.messages {
        let spans = find_refs(m.client.body.as_str(), &m.refs, &known);
        for pair in spans.windows(2) {
            assert!(
                pair[0].range.end <= pair[1].range.start,
                "{:?}",
                m.client.body
            );
        }
    }
}
