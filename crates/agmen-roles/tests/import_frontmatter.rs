use agmen_roles::{RoleError, import_subagent, parse};

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
