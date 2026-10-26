use super::*;

fn labels(m: &[Match]) -> Vec<usize> {
    m.iter().map(|m| m.index).collect()
}

fn highlighted<'a>(label: &'a str, m: &Match) -> Vec<&'a str> {
    m.ranges.iter().map(|r| &label[r.clone()]).collect()
}

#[test]
fn palette_filters_fuzzy() {
    let entries = [
        "Rescan network cache",
        "Toggle left dock",
        "Spawn worker with codex",
    ];
    let got = filter("swc", &entries);
    assert_eq!(labels(&got), [2, 0]);
    assert_eq!(highlighted(entries[2], &got[0]), ["S", "w", "c"]);
}

#[test]
fn empty_query_returns_all_in_order() {
    let entries = ["b", "a", "c"];
    let got = filter("", &entries);
    assert_eq!(labels(&got), [0, 1, 2]);
    assert!(got.iter().all(|m| m.ranges.is_empty()));
    assert_eq!(labels(&filter("   ", &entries)), [0, 1, 2]);
}

#[test]
fn no_match_returns_empty() {
    assert!(filter("xyz", &["Open settings", "Close tab"]).is_empty());
    assert!(filter("ba", &["ab"]).is_empty());
}

#[test]
fn match_is_case_insensitive() {
    let got = filter("OPEN", &["open settings"]);
    assert_eq!(labels(&got), [0]);
    assert_eq!(highlighted("open settings", &got[0]), ["open"]);
}

#[test]
fn word_start_beats_mid_word() {
    let entries = ["Show output", "Open worker"];
    let got = filter("wo", &entries);
    assert_eq!(labels(&got), [1, 0]);
    assert_eq!(highlighted(entries[1], &got[0]), ["wo"]);
}

#[test]
fn consecutive_run_beats_scattered() {
    let entries = ["Tab order", "Show terminal"];
    let got = filter("ter", &entries);
    assert_eq!(labels(&got), [1, 0]);
    assert_eq!(highlighted(entries[1], &got[0]), ["ter"]);
}

#[test]
fn earlier_match_beats_later() {
    let entries = ["Go to the dock", "Dock left"];
    assert_eq!(labels(&filter("dock", &entries)), [1, 0]);
}

#[test]
fn ties_keep_input_order() {
    let entries = ["Copy path", "Copy name", "Copy link"];
    assert_eq!(labels(&filter("copy", &entries)), [0, 1, 2]);
}

#[test]
fn positions_land_on_char_boundaries_in_non_ascii_label() {
    let label = "Ouvrir le fichier é";
    let got = filter("ofÉ", &[label]);
    assert_eq!(got.len(), 1);
    assert_eq!(highlighted(label, &got[0]), ["O", "f", "é"]);
    assert_eq!(got[0].ranges.last(), Some(&(18..20)));
}

#[test]
fn query_over_cap_does_not_panic() {
    let long = "a".repeat(MAX_QUERY_CHARS * 4);
    let label = "a".repeat(MAX_QUERY_CHARS * 2);
    let got = filter(&long, &[label.as_str()]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].ranges.len(), 1);
    assert_eq!(got[0].ranges[0], 0..MAX_QUERY_CHARS);
}

#[test]
fn results_are_capped() {
    let entries = vec!["entry"; MAX_RESULTS + 10];
    assert_eq!(filter("", &entries).len(), MAX_RESULTS);
    assert_eq!(filter("e", &entries).len(), MAX_RESULTS);
}

#[test]
fn word_start_late_beats_mid_word_early() {
    let entries = ["Undock", "Toggle the left dock"];
    let got = filter("dock", &entries);
    assert_eq!(labels(&got), [1, 0]);
}

#[test]
fn whitespace_inside_query_is_ignored() {
    let entries = ["Spawn worker with codex"];
    assert_eq!(filter("sp w", &entries), filter("spw", &entries));
    assert_eq!(
        highlighted(entries[0], &filter("sp w", &entries)[0]),
        ["Sp", "w"]
    );
}

#[test]
fn camel_case_hump_is_a_word_start() {
    let entries = ["profile", "openFile"];
    let got = filter("of", &entries);
    assert_eq!(labels(&got), [1, 0]);
    assert_eq!(highlighted(entries[1], &got[0]), ["o", "F"]);
}
