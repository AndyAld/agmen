;; agmen (MIT): appended after tree-sitter-rust's stock highlights query, so
;; these patterns win where both match. The mockups draw a capitalized name at
;; the end of a path, as `Tabs` in `use ui::tabs::Tabs;`, in the type color;
;; the stock query calls it a constructor.

((scoped_identifier
  name: (identifier) @type)
 (#match? @type "^[A-Z]"))
