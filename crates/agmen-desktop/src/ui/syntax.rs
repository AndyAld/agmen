//! Syntax highlighting with tree-sitter for the file types agmen shows:
//! Rust, TOML (and Cargo.lock), XML (role files) and Markdown. Highlights
//! carry Zed's syntax names, which `Theme.syntax` colors.
// shortcut: Markdown uses the block grammar only (headings, lists, code fences); inline emphasis and links come with the inline grammar's injection when a screen needs them.

use std::cell::RefCell;
use std::ops::Range;
use std::path::Path;

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Files larger than this are shown plain: highlighting runs on the UI
/// thread and must stay quick (about 14 ms per 100 KB in release, ADR-0012).
// shortcut: UI-thread highlighting caps files at 128 KiB; highlight on the
// background executor when larger files need colors.
const MAX_HIGHLIGHT_BYTES: usize = 128 * 1024;

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

/// agmen's XML highlights, which color tags and attributes as the mockups
/// draw role files (see the file's header).
const XML_HIGHLIGHTS: &str = include_str!("queries/xml.scm");

/// agmen's Markdown highlights, which color headings, list markers and a
/// spec's status line as the mockup draws a spec (see the file's header).
const MARKDOWN_HIGHLIGHTS: &str = include_str!("queries/markdown.scm");

/// agmen's Rust patterns, appended to the stock query (see the file's header).
const RUST_AGMEN: &str = include_str!("queries/rust-agmen.scm");

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
    /// The name Zed's status bar gives the language.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Toml => "TOML",
            Self::Xml => "XML",
            Self::Markdown => "Markdown",
            Self::Plain => "Plain Text",
        }
    }

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
        let rust_query;
        let (language, name, query): (tree_sitter::Language, &str, &str) = match self {
            Self::Rust => {
                rust_query = format!("{}\n{RUST_AGMEN}", tree_sitter_rust::HIGHLIGHTS_QUERY);
                (
                    tree_sitter_rust::LANGUAGE.into(),
                    "rust",
                    rust_query.as_str(),
                )
            }
            Self::Toml => (
                tree_sitter_toml_ng::LANGUAGE.into(),
                "toml",
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            ),
            Self::Xml => (tree_sitter_xml::LANGUAGE_XML.into(), "xml", XML_HIGHLIGHTS),
            Self::Markdown => (
                tree_sitter_md::LANGUAGE.into(),
                "markdown",
                MARKDOWN_HIGHLIGHTS,
            ),
            Self::Plain => return None,
        };
        // A query that does not compile leaves the file plain. Configurations
        // are cached per language, so this logs once per language.
        let mut config = match HighlightConfiguration::new(language, name, query, "", "") {
            Ok(config) => config,
            Err(err) => {
                eprintln!("agmen-desktop: the {name} highlight query does not build: {err}");
                return None;
            }
        };
        config.configure(NAMES);
        Some(config)
    }
}

/// agmen's own highlight name for text in the line number color, which no
/// Zed syntax name gives: a Markdown table's "|" (console.py `build_d_spec`
/// draws them in the gutter's color, nodes 9228 to 9232).
pub(crate) const LINE_NUMBER: &str = "line_number";

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
    /// Languages whose highlight error was already logged.
    static LOGGED: RefCell<Vec<Language>> = const { RefCell::new(Vec::new()) };
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
        let highlights = HIGHLIGHTER.with_borrow_mut(|highlighter| {
            run(highlighter, config, text).unwrap_or_else(|err| {
                // Once per language: a grammar that fails tends to fail again.
                LOGGED.with_borrow_mut(|logged| {
                    if !logged.contains(&language) {
                        logged.push(language);
                        eprintln!("agmen-desktop: highlighting {language:?} stopped: {err}");
                    }
                });
                Vec::new()
            })
        });
        if language == Language::Markdown {
            with_table_pipes(text, highlights)
        } else {
            highlights
        }
    })
}

/// Adds a [`LINE_NUMBER`] highlight for each "|" on a line that starts with
/// one, where no other highlight (a code block, an escape) covers it. The
/// block grammar parses a table only when it has a delimiter row, and a
/// spec's table may have none (console.py's `MD` does not), so the pipes
/// are found by line, as console.py finds them.
fn with_table_pipes(text: &str, mut highlights: Vec<Highlight>) -> Vec<Highlight> {
    let covered = |at: usize, highlights: &[Highlight]| {
        let ix = highlights.partition_point(|h| h.range.end <= at);
        highlights.get(ix).is_some_and(|h| h.range.start <= at)
    };
    let mut pipes = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        if line.trim_start_matches(' ').starts_with('|') {
            pipes.extend(
                line.match_indices('|')
                    .map(|(at, _)| start + at)
                    .filter(|at| !covered(*at, &highlights))
                    .map(|at| Highlight {
                        range: at..at + 1,
                        name: LINE_NUMBER,
                    }),
            );
        }
        start += line.len();
    }
    if !pipes.is_empty() {
        highlights.extend(pipes);
        highlights.sort_by_key(|h| h.range.start);
    }
    highlights
}

/// Highlights `text`, or the error that stopped it before any event.
fn run(
    highlighter: &mut Highlighter,
    config: &HighlightConfiguration,
    text: &str,
) -> Result<Vec<Highlight>, tree_sitter_highlight::Error> {
    let events = highlighter.highlight(config, text.as_bytes(), None, None, |_| None)?;
    let mut out: Vec<Highlight> = Vec::new();
    // One entry per open capture, None for a name agmen did not ask for, so
    // every HighlightEnd pops its own HighlightStart (C154).
    let mut stack: Vec<Option<&'static str>> = Vec::new();
    for event in events {
        match event {
            Ok(HighlightEvent::HighlightStart(h)) => {
                stack.push(NAMES.get(h.0).copied());
            }
            Ok(HighlightEvent::HighlightEnd) => {
                stack.pop();
            }
            Ok(HighlightEvent::Source { start, end }) => {
                let (Some(Some(name)), true) = (
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
    Ok(out)
}

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod tests;
