;; agmen (MIT): appended after tree-sitter-rust's stock highlights query, so
;; these patterns win where both match. The mockups draw a capitalized name at
;; the end of a path, as `Tabs` in `use ui::tabs::Tabs;`, in the type color;
;; the stock query calls it a constructor.

((scoped_identifier
  name: (identifier) @type)
 (#match? @type "^[A-Z]"))

;; tree-sitter-rust 0.24.2's all-caps constant pattern ends its regex in a
;; stray quote ("^[A-Z][A-Z\\d_]+$'"), so it never matches. This is the same
;; pattern without the quote, last so it also wins over the path rule above.
((identifier) @constant
 (#match? @constant "^[A-Z][A-Z\\d_]+$"))
