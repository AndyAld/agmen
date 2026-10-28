# 0012: Syntax highlighting in the desktop app

Status: accepted
Date: 2026-10-26

## Context

The desktop app shows files read-only: project files with change bars (Task 11), role files (Task 10), and diffs (Task 12). The mockups color them like Zed does, from the theme's syntax colors. The plan allowed either tree-sitter or a small keyword lexer, depending on what `cargo deny` accepts.

## Decision

Use tree-sitter through `tree-sitter-highlight` 0.27.1, with the grammars `tree-sitter-rust` 0.24.2, `tree-sitter-toml-ng` 0.7.0 (TOML and Cargo.lock), `tree-sitter-xml` 0.7.0 (role files) and `tree-sitter-md` 0.5.4 (block grammar). All are MIT, and `cargo deny check` passes with them. Each grammar ships its own `highlights.scm`; agmen asks for Zed's highlight names (`keyword`, `string`, `type`, `tag`, ...), so a capture such as `keyword.function` lands on the nearest name, and `Theme.syntax` (Task 2b) colors it, falling back to the parent name (`keyword`).

`ui::syntax::highlight(text, language)` returns sorted, non-overlapping ranges with their names; `ui::code_view::CodeView` splits them per line once, when the view is made, and draws lines with `uniform_list`, so long files scroll without re-highlighting. Files over 128 KiB are shown plain. Highlighting runs on the UI thread when the view is made. Measured in release on this Mac (`time_the_highlight_cap`, five runs of real Rust): 510,810 bytes took 64 to 83 ms, median 66 ms, about four frames at 60 Hz, so the first cap of 512 KiB was lowered; 102,162 bytes took 13 to 35 ms, median 14 ms. Moving highlighting to the background executor would let larger files keep their colors.

## Consequences

- The grammars are C code compiled by `cc` at build time; the build needs a C compiler (Xcode's, already required for gpui).
- Markdown inline emphasis and links need the inline grammar as an injection; not done yet (a `shortcut:` in `syntax.rs`).
- Other languages are a grammar crate and one match arm each.
