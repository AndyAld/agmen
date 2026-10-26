;; agmen's Markdown highlights for the block grammar (tree-sitter-md 0.5.4),
;; which color a spec as the mockup draws it (console.py build_d_spec,
;; frame "Desktop · spec"): a heading line, its marks included, in the
;; accent color; a list marker in the warning color; a spec's "Status:" line
;; in the placeholder color; everything else plain. The names are the ones
;; Gruvbox Material colors that way: `tag` is its accent, `type` its warning
;; and `comment.doc` its placeholder gray, as the XML query uses `type` for
;; attributes. Upstream's query (queries/highlights.scm) is not copied: it
;; names headings `text.title` and markers `punctuation.special`, which the
;; theme leaves uncolored or colors as text.

[
  (atx_heading)
  (setext_heading)
] @tag

[
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
] @type

;; The spec's status line (node 9195): "Status: v3 · drafted by ...".
((paragraph) @comment.doc
  (#match? @comment.doc "^Status: "))

[
  (indented_code_block)
  (fenced_code_block)
] @text.literal

(backslash_escape) @string.escape
