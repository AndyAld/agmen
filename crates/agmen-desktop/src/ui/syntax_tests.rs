use std::path::Path;

use super::*;

fn names_of(text: &str, language: Language) -> Vec<(&str, &'static str)> {
    highlight(text, language)
        .into_iter()
        .map(|h| (&text[h.range], h.name))
        .collect()
}

fn has(found: &[(&str, &'static str)], text: &str, name_prefix: &str) -> bool {
    found
        .iter()
        .any(|(t, n)| *t == text && n.starts_with(name_prefix))
}

#[test]
fn highlight_rust_keywords() {
    let found = names_of(
        "fn main() {\n    let tabs = Tabs::default();\n}\n",
        Language::Rust,
    );
    assert!(has(&found, "fn", "keyword"), "{found:?}");
    assert!(has(&found, "let", "keyword"), "{found:?}");
    assert!(has(&found, "Tabs", "type"), "{found:?}");
    assert!(has(&found, "main", "function"), "{found:?}");
}

#[test]
fn highlight_xml_tags() {
    let found = names_of(
        "<role name=\"reviewer\" kind=\"agent\">\n  <rule>Scope</rule>\n</role>\n",
        Language::Xml,
    );
    // V82: the tag name and its "<" are one run.
    assert!(has(&found, "<role", "tag"), "{found:?}");
    // V82: an attribute's name and `=` are one warning colored run.
    assert!(has(&found, "name=", "type"), "{found:?}");
}

#[test]
fn highlight_toml_and_markdown() {
    let toml = names_of("[package]\nname = \"agmen\"\n", Language::Toml);
    assert!(has(&toml, "\"agmen\"", "string"), "{toml:?}");
    let md = names_of("# Title\n\nSome text.\n", Language::Markdown);
    // Review C157: the heading's text is a title and its marker punctuation.
    assert!(has(&md, "Title", "text.title"), "{md:?}");
    assert!(has(&md, "#", "punctuation"), "{md:?}");
}

#[test]
fn highlights_are_sorted_and_disjoint() {
    let text = "fn a() { let b = \"c\"; } // d\n";
    let found = highlight(text, Language::Rust);
    for pair in found.windows(2) {
        assert!(pair[0].range.end <= pair[1].range.start, "{found:?}");
    }
    for h in &found {
        assert!(text.is_char_boundary(h.range.start) && text.is_char_boundary(h.range.end));
    }
}

#[test]
fn plain_and_huge_text_get_no_highlights() {
    assert!(highlight("fn main() {}", Language::Plain).is_empty());
    let huge = "fn a() {}\n".repeat(MAX_HIGHLIGHT_BYTES / 10 + 1);
    assert!(highlight(&huge, Language::Rust).is_empty());
}

#[test]
fn language_from_path() {
    assert_eq!(Language::for_path(Path::new("src/main.rs")), Language::Rust);
    assert_eq!(Language::for_path(Path::new("Cargo.toml")), Language::Toml);
    assert_eq!(Language::for_path(Path::new("Cargo.lock")), Language::Toml);
    assert_eq!(
        Language::for_path(Path::new("roles/reviewer.xml")),
        Language::Xml
    );
    assert_eq!(
        Language::for_path(Path::new("README.md")),
        Language::Markdown
    );
    assert_eq!(Language::for_path(Path::new("notes.txt")), Language::Plain);
}

#[test]
#[ignore = "a timing for ADR-0012; run with cargo test --release -- --ignored --nocapture"]
fn time_the_highlight_cap() {
    // Review C160: real Rust (this crate's text input), repeated to the cap.
    let source = include_str!("text_input.rs");
    let mut text = String::new();
    while text.len() + source.len() <= MAX_HIGHLIGHT_BYTES {
        text.push_str(source);
    }
    let mut runs = Vec::new();
    for _ in 0..5 {
        let start = std::time::Instant::now();
        let found = highlight(&text, Language::Rust);
        runs.push(start.elapsed());
        assert!(!found.is_empty());
    }
    runs.sort();
    println!(
        "{} bytes: min {:?}, median {:?}, max {:?}",
        text.len(),
        runs[0],
        runs[2],
        runs[4]
    );
}

/// The syntax name over byte `at` of `text`, if any.
fn name_at(text: &str, language: Language, at: usize) -> Option<&'static str> {
    highlight(text, language)
        .into_iter()
        .find(|h| h.range.contains(&at))
        .map(|h| h.name)
}

#[test]
fn xml_highlights_follow_the_mockup() {
    // V82: console.py's TOKEN draws `<tag` and `>` in accent, ` attr=` in
    // warning and the whole quoted value, quotes too, in success.
    let text = "<!-- c -->\n<role name=\"reviewer\">\n  <tools allow=\"Read\" />\n  <rule>x</rule>\n</role>\n";
    let at = |needle: &str| text.find(needle).unwrap();
    let name = |offset: usize| name_at(text, Language::Xml, offset);
    assert_eq!(name(at("<!--")), Some("comment"));
    let open = at("<role");
    assert_eq!(name(open), Some("tag"), "<");
    assert_eq!(name(open + 1), Some("tag"), "role");
    assert_eq!(name(at("name=")), Some("type"), "attribute name");
    assert_eq!(name(at("=\"reviewer")), Some("type"), "=");
    let value = at("\"reviewer\"");
    assert_eq!(name(value), Some("string"), "opening quote");
    assert_eq!(name(value + 1), Some("string"));
    assert_eq!(
        name(value + "\"reviewer".len()),
        Some("string"),
        "closing quote"
    );
    assert_eq!(name(value + "\"reviewer\"".len()), Some("tag"), ">");
    assert_eq!(name(at("/>")), Some("tag"), "/>");
    assert_eq!(name(at("</rule")), Some("tag"), "</");
    assert_eq!(name(at("x</rule")), None, "character data stays plain");
}

#[test]
fn rust_function_names_are_functions_not_types() {
    // V103: a definition, a path call and a plain call all get `function`,
    // which Andy's theme colors green, while `Tabs` stays a type.
    let text = "fn main() { Tabs::default(); app::run(tabs) }\n";
    let at = |needle: &str| text.find(needle).unwrap();
    let name = |offset: usize| name_at(text, Language::Rust, offset);
    assert_eq!(
        name(at("main")),
        Some("function"),
        "{:?}",
        names_of(text, Language::Rust)
    );
    assert_eq!(name(at("default")), Some("function"));
    assert_eq!(name(at("run")), Some("function"));
    assert_eq!(name(at("Tabs")), Some("type"));
}
