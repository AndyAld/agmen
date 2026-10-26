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
    assert!(has(&found, "role", "tag"), "{found:?}");
    assert!(
        has(&found, "name", "attribute") || has(&found, "name", "property"),
        "{found:?}"
    );
}

#[test]
fn highlight_toml_and_markdown() {
    let toml = names_of("[package]\nname = \"agmen\"\n", Language::Toml);
    assert!(has(&toml, "\"agmen\"", "string"), "{toml:?}");
    let md = names_of("# Title\n\nSome text.\n", Language::Markdown);
    assert!(!md.is_empty(), "markdown gets at least its heading");
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
