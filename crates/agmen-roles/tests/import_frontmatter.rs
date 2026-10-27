use agmen_roles::{RoleError, import_subagent, parse};

#[test]
fn quoted_keys_are_rejected_with_the_source_line() {
    for key in [
        "tools",
        "disallowedTools",
        "model",
        "omitClaudeMd",
        "unknown",
    ] {
        for quote in ['\'', '"'] {
            let error = import_subagent(
                &format!("---\n# comment\n{quote}{key}{quote}: Read\n---\nBody"),
                "custom",
            )
            .unwrap_err();
            assert!(matches!(error, RoleError::Schema(_)));
            assert!(
                error
                    .to_string()
                    .contains("subagent YAML line 3: quoted keys are not supported"),
                "{error}"
            );
        }
    }
}

#[test]
fn bom_and_delimiter_trailing_whitespace_preserve_restrictions() {
    for prefix in ["", "\u{feff}"] {
        for opening in ["---", "--- ", "---\t"] {
            for closing in ["---", "--- ", "---\t"] {
                for newline in ["\n", "\r\n"] {
                    let xml = import_subagent(
                        &format!(
                            "{prefix}{opening}{newline}tools: Read{newline}disallowedTools: Bash{newline}{closing}{newline}Body"
                        ),
                        "custom",
                    )
                    .unwrap();
                    assert!(xml.contains("allow=\"read\" deny=\"shell\""), "{xml}");
                    assert!(xml.contains("<purpose>Body</purpose>"), "{xml}");
                }
            }
        }
    }
}

#[test]
fn delimiter_whitespace_at_eof_is_accepted() {
    let xml = import_subagent("\u{feff}--- \ntools: Read\n--- \t", "custom").unwrap();
    assert!(xml.contains("allow=\"read\""), "{xml}");
    assert!(xml.contains("<purpose></purpose>"), "{xml}");
}

#[test]
fn malformed_headers_with_bom_or_delimiter_whitespace_still_fail() {
    for opening in ["\u{feff}---", "--- ", "\u{feff}---\t"] {
        let error = import_subagent(&format!("{opening}\ntools: Unknown\n---\nBody"), "custom")
            .unwrap_err();
        assert!(error.to_string().contains("line 2"), "{error}");
        let error = import_subagent(&format!("{opening}\ntools: Read"), "custom").unwrap_err();
        assert!(error.to_string().contains("line 1"), "{error}");
    }
}

#[test]
fn invalid_tool_and_model_names_in_errors_are_capped_at_64_characters() {
    for character in ['x', '界'] {
        for length in [63, 64, 65, 256] {
            let name = character.to_string().repeat(length);
            let echoed = character.to_string().repeat(length.min(64));
            for header in [
                format!("tools: {name}"),
                format!("disallowedTools: [{name}]"),
                format!("tools:\n  - Read\n  - {name}"),
            ] {
                let error =
                    import_subagent(&format!("---\n{header}\n---\nBody"), "custom").unwrap_err();
                let line = if header.contains('\n') { 4 } else { 2 };
                assert_eq!(
                    error.to_string(),
                    format!(
                        "role schema: subagent YAML line {line}: role schema: cannot map Claude tool {echoed}"
                    )
                );
            }
            let error =
                import_subagent(&format!("---\nmodel: {name}\n---\nBody"), "custom").unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("role schema: subagent YAML line 2: cannot map model {echoed} to a tier")
            );
        }
    }
}

#[test]
fn tool_value_forms_preserve_allow_and_deny() {
    for value in [
        "Read",
        "'Read'",
        "\"Read\"",
        "Read, Glob, Bash",
        "'Read, Glob, Bash'",
        "[Read, 'Glob', \"Bash\"]",
        "[Read, 'Glob', \"Bash\",] # comment",
        "\n  - Read\n  - 'Glob'\n  - \"Bash\"",
        "\n- Read\n- 'Glob'\n- \"Bash\"",
    ] {
        let xml = import_subagent(
            &format!("---\ntools: {value}\ndisallowedTools: {value}\n---\nBody"),
            "custom",
        )
        .unwrap();
        let expected = if value.contains("Glob") {
            "read search shell"
        } else {
            "read"
        };
        assert!(xml.contains(&format!("allow=\"{expected}\"")), "{value}");
        assert!(xml.contains(&format!("deny=\"{expected}\"")), "{value}");
        parse(&xml).unwrap();
    }
}

#[test]
fn scalar_models_booleans_and_body_are_preserved() {
    for (model, tier) in [
        ("haiku", "small"),
        ("'opus'", "deep"),
        ("\"sonnet\"", "standard"),
    ] {
        for (flag, parent) in [("true", "none"), ("false", "base")] {
            let xml = import_subagent(
                &format!("---\nmodel: {model}\nomitClaudeMd: {flag}\n---\nBody\n---\nmodel: bogus"),
                "custom",
            )
            .unwrap();
            assert!(xml.contains(&format!("tier=\"{tier}\"")));
            assert!(xml.contains(&format!("extends=\"{parent}\"")));
            assert!(xml.contains("Body\n---\nmodel: bogus"));
        }
    }
}

#[test]
fn quoted_escapes_are_decoded_before_model_mapping() {
    for (model, expected) in [
        (r#""claude-\"opus\"""#, "deep"),
        (r#""claude-\\haiku""#, "small"),
        ("'claude-it''s-sonnet'", "standard"),
        (r#""\u006f\x70\U00000075s""#, "deep"),
    ] {
        let xml = import_subagent(&format!("---\nmodel: {model}\n---\nBody"), "custom").unwrap();
        assert!(xml.contains(&format!("tier=\"{expected}\"")), "{model}");
    }
}

#[test]
fn empty_lists_and_ignored_keys_are_accepted() {
    let xml = import_subagent(
        "---\nname: different\ndescription: |\n  A description\nunknown:\n  nested: ignored\nunknownList:\n- ignored\ntools: []\ndisallowedTools: []\n---\nBody",
        "custom",
    )
    .unwrap();
    assert!(xml.contains("name=\"custom\""));
    assert!(xml.contains("allow=\"\" deny=\"\""));
}

#[test]
fn malformed_frontmatter_reports_the_source_line() {
    for (header, line) in [
        ("model opus", 2),
        ("model: opus\nbroken", 3),
        ("tools: [Read", 2),
        ("tools: [Read,, Bash]", 2),
        ("tools: [Read # comment, Bash]", 2),
        ("tools: [Read # comment]", 2),
        ("tools: [Read, [Bash]]", 2),
        ("tools:\n  - Read\n  - [Bash]", 4),
        ("tools: 'Read", 2),
        ("tools: \"Read\\q\"", 2),
        (r#"tools: "\uD800""#, 2),
        (r#"tools: "\x0z""#, 2),
        (r#"tools: "\U00110000""#, 2),
        ("model: 'opus' garbage", 2),
        ("model: unknown", 2),
        ("model: {tier: opus}", 2),
        ("model: |\n  opus", 2),
        ("omitClaudeMd: yes", 2),
        ("omitClaudeMd: \"true\"", 2),
        ("tools: Read\ntools: Bash", 3),
        ("  model: opus", 2),
        ("tools:\n  - Read\n    - Bash", 4),
    ] {
        let error = import_subagent(&format!("---\n{header}\n---\nBody"), "custom").unwrap_err();
        assert!(matches!(error, RoleError::Schema(_)), "{header}: {error}");
        assert!(
            error.to_string().contains(&format!("line {line}")),
            "{header}: {error}"
        );
    }
}

#[test]
fn comments_do_not_change_values() {
    let xml = import_subagent(
        "---\n# comment\ntools: # list\n  - 'Read' # item\nmodel: \"opus\" # model\nomitClaudeMd: true # boolean\n---\nBody",
        "custom",
    )
    .unwrap();
    assert!(xml.contains("allow=\"read\""));
    assert!(xml.contains("tier=\"deep\""));
    assert!(xml.contains("extends=\"none\""));
}

#[test]
fn missing_delimiter_reports_a_line_and_no_header_keeps_body() {
    let error = import_subagent("---\nmodel: opus", "custom").unwrap_err();
    assert!(error.to_string().contains("line 1"), "{error}");
    let xml = import_subagent("model: opus\n---\nBody", "custom").unwrap();
    assert!(xml.contains("<purpose>model: opus\n---\nBody</purpose>"));
    assert!(xml.contains("tier=\"standard\""));
}

#[test]
fn frontmatter_line_count_is_bounded() {
    let accepted = format!("---\n{}---\nBody", "# comment\n".repeat(1024));
    import_subagent(&accepted, "custom").unwrap();
    let rejected = format!("---\n{}---\nBody", "# comment\n".repeat(1025));
    let error = import_subagent(&rejected, "custom").unwrap_err();
    assert!(error.to_string().contains("line 1026"), "{error}");
}

#[test]
fn import_size_is_bounded() {
    let accepted = format!("---\n#{}\n---\nBody", "x".repeat(65_536 - 14));
    import_subagent(&accepted, "custom").unwrap();
    let error = import_subagent(
        &format!("---\n#{}\n---\nBody", "x".repeat(65_536)),
        "custom",
    )
    .unwrap_err();
    assert!(error.to_string().contains("64 KiB"), "{error}");
}

#[test]
fn indirect_key_syntax_is_refused_with_the_source_line() {
    for line in [
        "{tools: Read}: x",
        "{tools: Read}",
        "[tools]: x",
        "? tools",
        "?: x",
        "&a tools: Read",
        "&a: x",
        "*a: x",
        "!!str tools: Read",
        "!tag: x",
        "<<: {tools: Read}",
        "<<: *base",
        "x<<y: z",
        "|tools: Read",
        ">tools: Read",
        "%tools: Read",
        "@tools: Read",
        "`tools`: Read",
        "tools\"x\": Read",
        "tools[0]: Read",
        "to&ols: Read",
    ] {
        let error =
            import_subagent(&format!("---\n# comment\n{line}\n---\nBody"), "custom").unwrap_err();
        assert!(matches!(error, RoleError::Schema(_)), "{line}: {error}");
        assert!(
            error.to_string().contains("subagent YAML line 3"),
            "{line}: {error}"
        );
    }
}

#[test]
fn alias_and_tag_values_for_known_keys_are_refused() {
    for header in [
        "x: &a Read\ntools: *a",
        "tools: !!str Read",
        "model: *a",
        "model: &a opus",
        "omitClaudeMd: *a",
    ] {
        let error = import_subagent(&format!("---\n{header}\n---\nBody"), "custom").unwrap_err();
        assert!(
            error.to_string().contains("subagent YAML line"),
            "{header}: {error}"
        );
    }
}

#[test]
fn unknown_key_bodies_cannot_hide_or_inject_known_keys() {
    let xml = import_subagent(
        "---\ndescription: |\n  Helper.\n  tools: Bash\n  model: haiku\n  disallowedTools: Read\n  omitClaudeMd: true\ntools: Read\n---\nBody",
        "custom",
    )
    .unwrap();
    assert!(xml.contains("allow=\"read\""), "{xml}");
    assert!(!xml.contains("deny="), "{xml}");
    assert!(xml.contains("tier=\"standard\""), "{xml}");
    assert!(xml.contains("extends=\"base\""), "{xml}");

    let xml = import_subagent(
        "---\ndescription: >-\n  folded\n\n  tools: Bash\n# tools: Shell\nunknown:\n  - tools: Bash\n  nested:\n    tools: Bash\nmodel: opus\ntools:\n  - Read\n---\nBody",
        "custom",
    )
    .unwrap();
    assert!(xml.contains("allow=\"read\""), "{xml}");
    assert!(xml.contains("tier=\"deep\""), "{xml}");

    let xml = import_subagent(
        "---\ndescription: \"multi\n  tools: Bash\n  line\"\ntools: Read\n---\nBody",
        "custom",
    )
    .unwrap();
    assert!(xml.contains("allow=\"read\""), "{xml}");
}
