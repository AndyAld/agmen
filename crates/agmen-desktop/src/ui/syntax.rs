//! Syntax highlighting with tree-sitter for the file types agmen shows:
//! Rust, TOML (and Cargo.lock), XML (role files) and Markdown. Highlights
//! carry Zed's syntax names, which `Theme.syntax` colors.
// shortcut: Markdown uses the block grammar only (headings, lists, code fences); inline emphasis and links come with the inline grammar's injection when a screen needs them.

use std::cell::RefCell;
use std::ops::Range;
use std::path::Path;

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Files larger than this are shown plain: highlighting runs on the UI
/// thread and must stay quick.
pub(crate) const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;

/// The highlight names asked of every grammar, Zed's names. A capture maps
/// to the longest of these that prefixes it (`keyword.function` to `keyword`).
const NAMES: &[&str] = &[
    "attribute",
    "boolean",
    "comment",
    "comment.doc",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "emphasis",
    "emphasis.strong",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "keyword",
    "label",
    "link_text",
    "link_uri",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.list_marker",
    "punctuation.special",
    "string",
    "string.escape",
    "string.special",
    "tag",
    "text.literal",
    "text.title",
    "title",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// A file type with a grammar, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Language {
    Rust,
    Toml,
    Xml,
    Markdown,
    Plain,
}

impl Language {
    pub(crate) fn for_path(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => Self::Rust,
            Some("toml" | "lock") => Self::Toml,
            Some("xml") => Self::Xml,
            Some("md" | "markdown") => Self::Markdown,
            _ => Self::Plain,
        }
    }

    fn configuration(self) -> Option<HighlightConfiguration> {
        let (language, name, query): (tree_sitter::Language, &str, &str) = match self {
            Self::Rust => (
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY,
            ),
            Self::Toml => (
                tree_sitter_toml_ng::LANGUAGE.into(),
                "toml",
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            ),
            Self::Xml => (
                tree_sitter_xml::LANGUAGE_XML.into(),
                "xml",
                tree_sitter_xml::XML_HIGHLIGHT_QUERY,
            ),
            Self::Markdown => (
                tree_sitter_md::LANGUAGE.into(),
                "markdown",
                tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            ),
            Self::Plain => return None,
        };
        // A query that does not compile leaves the file plain.
        let mut config = HighlightConfiguration::new(language, name, query, "", "").ok()?;
        config.configure(NAMES);
        Some(config)
    }
}

/// One highlighted byte range and its Zed syntax name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Highlight {
    pub(crate) range: Range<usize>,
    pub(crate) name: &'static str,
}

thread_local! {
    /// Grammars are built once per thread (the UI thread), on first use.
    static CONFIGS: RefCell<Vec<(Language, Option<HighlightConfiguration>)>> =
        const { RefCell::new(Vec::new()) };
    static HIGHLIGHTER: RefCell<Highlighter> = RefCell::new(Highlighter::new());
}

/// The highlights in `text`, sorted and never overlapping (the innermost
/// capture wins). Plain for unknown languages and files over
/// [`MAX_HIGHLIGHT_BYTES`].
pub(crate) fn highlight(text: &str, language: Language) -> Vec<Highlight> {
    if language == Language::Plain || text.len() > MAX_HIGHLIGHT_BYTES {
        return Vec::new();
    }
    CONFIGS.with_borrow_mut(|configs| {
        if !configs.iter().any(|(l, _)| *l == language) {
            configs.push((language, language.configuration()));
        }
        let Some(config) = configs
            .iter()
            .find(|(l, _)| *l == language)
            .and_then(|(_, c)| c.as_ref())
        else {
            return Vec::new();
        };
        HIGHLIGHTER.with_borrow_mut(|highlighter| run(highlighter, config, text))
    })
}

fn run(
    highlighter: &mut Highlighter,
    config: &HighlightConfiguration,
    text: &str,
) -> Vec<Highlight> {
    let Ok(events) = highlighter.highlight(config, text.as_bytes(), None, None, |_| None) else {
        return Vec::new();
    };
    let mut out: Vec<Highlight> = Vec::new();
    let mut stack: Vec<&'static str> = Vec::new();
    for event in events {
        match event {
            Ok(HighlightEvent::HighlightStart(h)) => {
                if let Some(name) = NAMES.get(h.0) {
                    stack.push(name);
                }
            }
            Ok(HighlightEvent::HighlightEnd) => {
                stack.pop();
            }
            Ok(HighlightEvent::Source { start, end }) => {
                let (Some(name), true) = (
                    stack.last(),
                    text.is_char_boundary(start) && text.is_char_boundary(end),
                ) else {
                    continue;
                };
                // Merge with the previous run when it continues it.
                match out.last_mut() {
                    Some(last) if last.range.end == start && last.name == *name => {
                        last.range.end = end
                    }
                    _ => out.push(Highlight {
                        range: start..end,
                        name,
                    }),
                }
            }
            // A parse that fails part way keeps what it highlighted so far.
            Err(_) => break,
        }
    }
    out
}

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod tests;
