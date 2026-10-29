use agmen_roles::{
    AgentSpec, AgentTemplate, MAX_OPTIONS, MAX_REFERENCES, MAX_TEMPLATE_BYTES, Named, PackRef,
    Reference, RoleError, Template, TemplateError, TemplateOption, parse_template,
};

#[test]
fn spec_agent_example_parses() {
    let template = parse_template(include_str!("fixtures/templates/rust-reviewer.agent.xml"))
        .expect("spec example");
    let Template::Agent(named) = &template else {
        panic!("expected an agent template");
    };
    let pack = named.template.skills[0]
        .pack
        .as_ref()
        .expect("first skill pack");
    assert_eq!(pack.to_string(), "github:groupdrive/agmen-rust@1a2b3c4");
    assert!(named.template.skills[1].pack.is_none());
    assert_eq!(template.name(), "rust-reviewer");
    assert_eq!(
        template,
        Template::Agent(Named {
            name: "rust-reviewer".into(),
            saved_by: None,
            template: AgentTemplate {
                description: Some("Reviews Rust DONEs with the house rules.".into()),
                agent: AgentSpec {
                    kind: Some("claude".into()),
                    model: Some("claude-sonnet-5-5".into()),
                    effort: Some("high".into()),
                },
                role: Some("reviewer".into()),
                skills: vec![
                    Reference {
                        name: "rust-review".into(),
                        pack: Some(PackRef {
                            source: "github:groupdrive/agmen-rust".into(),
                            sha: "1a2b3c4".into(),
                        }),
                    },
                    Reference {
                        name: "cc-skill-coding-standards".into(),
                        pack: None,
                    },
                ],
                mcp: vec![Reference {
                    name: "docs".into(),
                    pack: Some(PackRef {
                        source: "github:groupdrive/agmen-rust".into(),
                        sha: "1a2b3c4".into(),
                    }),
                }],
                budget_usd: Some("5.00".into()),
                prompt: Some("Review the DONE you are given.".into()),
                options: vec![TemplateOption {
                    name: "context-window".into(),
                    value: "1m".into(),
                }],
            },
        })
    );
}

#[test]
fn empty_template_is_valid() {
    assert_eq!(
        parse_template(r#"<agent-template name="plain"/>"#).expect("empty template"),
        Template::Agent(Named {
            name: "plain".into(),
            saved_by: None,
            template: AgentTemplate::default(),
        })
    );
}

#[test]
fn nameless_root_is_rejected() {
    let error = parse_template("<agent-template/>").expect_err("missing name");
    assert!(matches!(error, TemplateError::NoName), "{error}");
}

#[test]
fn rejects_bad_values() {
    let cases = [
        (
            r#"<agent-template name="plain"><agent model="-rf"/></agent-template>"#,
            "model",
        ),
        (
            r#"<agent-template name="plain"><agent effort="HIGH"/></agent-template>"#,
            "effort",
        ),
        (
            r#"<agent-template name="plain"><skills><skill name="s" pack="github:x@XYZ"/></skills></agent-template>"#,
            "pack",
        ),
        (
            r#"<agent-template name="plain"><budget usd="0"/></agent-template>"#,
            "usd",
        ),
        (
            r#"<agent-template name="plain"><budget usd="1000.01"/></agent-template>"#,
            "usd",
        ),
        (
            r#"<agent-template name="plain"><budget usd="1e3"/></agent-template>"#,
            "usd",
        ),
        (
            r#"<agent-template name="plain"><options><option name="a" value="1"/><option name="a" value="2"/></options></agent-template>"#,
            "option",
        ),
        (
            r#"<agent-template name="plain"><role name="Bad"/></agent-template>"#,
            "role",
        ),
        (r#"<agent-template name="plain" version="2"/>"#, "version"),
        (
            r#"<agent-template name="plain"><hooks/></agent-template>"#,
            "hooks",
        ),
        (
            r#"<agent-template name="plain"><description><b>x</b></description></agent-template>"#,
            "description",
        ),
    ];
    for (xml, field) in cases {
        let error = parse_template(xml).expect_err(field);
        assert!(matches!(error, TemplateError::Schema(_)), "{xml}: {error}");
        assert!(error.to_string().contains(field), "{xml}: {error}");
    }
}

#[test]
fn oversized_input_is_rejected() {
    let description = "x".repeat(33 * 1024);
    let xml = format!(
        "<agent-template name=\"plain\"><description>{description}</description></agent-template>"
    );
    let error = parse_template(&xml).expect_err("too large");
    assert!(
        matches!(error, TemplateError::TooLarge { limit: 32768 }),
        "{error}"
    );
}

#[test]
fn option_helpers_cover_every_option() {
    let mut template = parse_template(include_str!("fixtures/templates/rust-reviewer.agent.xml"))
        .expect("spec example");
    assert_eq!(template.option_names(), ["context-window"]);
    assert_eq!(template.strip_options(), 1);
    assert!(template.option_names().is_empty());
}

fn agent_xml(body: &str) -> String {
    format!(r#"<agent-template name="plain">{body}</agent-template>"#)
}

fn expect_schema(xml: &str, needle: &str) {
    let result = parse_template(xml);
    assert!(
        result.as_ref().is_err_and(|error| {
            matches!(error, TemplateError::Schema(_)) && error.to_string().contains(needle)
        }),
        "{xml}: expected a schema error containing {needle:?}, got {result:?}"
    );
}

fn padded_to(total: usize) -> String {
    let (head, tail) = (
        r#"<agent-template name="plain"><!--"#,
        "--></agent-template>",
    );
    format!(
        "{head}{}{tail}",
        "x".repeat(total - head.len() - tail.len())
    )
}

#[test]
fn rejects_bad_root_values() {
    let long_saved_by = format!(
        r#"<agent-template name="plain" saved-by="{}"/>"#,
        "x".repeat(65)
    );
    let root_cases = [
        (r#"<agent-template name="plain" saved-by=""/>"#, "saved-by"),
        (long_saved_by.as_str(), "saved-by"),
        (
            r#"<agent-template name="plain" saved-by="a&#10;b"/>"#,
            "saved-by",
        ),
        (r#"<agent-template name="Plain"/>"#, "name"),
        (r#"<agent-template name="p"/>"#, "name"),
        (
            r#"<agent-template name="plain" foo="x"/>"#,
            "unknown attribute foo",
        ),
        (
            r#"<agent-template name="plain" xmlns="urn:x"/>"#,
            "namespaced",
        ),
        (
            r#"<agent-template name="plain" xmlns:a="urn:x"><a:agent/></agent-template>"#,
            "namespaced",
        ),
        (
            r#"<agent-template name="plain" xmlns:a="urn:x"><agent a:kind="x"/></agent-template>"#,
            "unknown attribute kind",
        ),
    ];
    for (xml, needle) in root_cases {
        expect_schema(xml, needle);
    }
}

#[test]
fn rejects_bad_agent_body_values() {
    let body_cases = [
        (r#"<agent kind="Claude"/>"#, "kind"),
        (r#"<agent kind="1claude"/>"#, "kind"),
        (r#"<agent model="a b"/>"#, "model"),
        (r#"<agent effort=""/>"#, "effort"),
        (r#"<agent foo="x"/>"#, "unknown attribute foo"),
        (r#"<agent><b/></agent>"#, "unknown element b"),
        (r#"<role/>"#, "role has no name"),
        (r#"<role name="x"/>"#, "role"),
        (r#"<budget/>"#, "budget has no usd"),
        (r#"<budget usd="5"><b/></budget>"#, "unknown element b"),
        (r#"<skills><skill/></skills>"#, "skill has no name"),
        (r#"<skills><skill name=".hidden"/></skills>"#, "skill"),
        (r#"<skills><skill name="-x"/></skills>"#, "skill"),
        (r#"<skills><skill name="a b"/></skills>"#, "skill"),
        (
            r#"<skills><skill name="s"/><skill name="s"/></skills>"#,
            "duplicate skill",
        ),
        (
            r#"<mcp><server name="s"/><server name="s"/></mcp>"#,
            "duplicate server",
        ),
        (
            r#"<skills><tool name="x"/></skills>"#,
            "unknown element tool in skills",
        ),
        (
            r#"<skills><skill name="s"><b/></skill></skills>"#,
            "unknown element b in skill",
        ),
        (r#"<skills foo="x"/>"#, "unknown attribute foo"),
        (
            r#"<skills><skill name="s" pack="no-at-sign"/></skills>"#,
            "pack",
        ),
        (
            r#"<skills><skill name="s" pack="x@abcdef"/></skills>"#,
            "pack",
        ),
        (
            r#"<skills><skill name="s" pack="a b@abcdef1"/></skills>"#,
            "pack",
        ),
        (
            r#"<skills><skill name="s" pack="@abcdef1"/></skills>"#,
            "pack",
        ),
        (
            r#"<skills><skill name="s" pack="x@ABCDEF1"/></skills>"#,
            "pack",
        ),
        (
            r#"<options><option value="a"/></options>"#,
            "option has no name",
        ),
        (
            r#"<options><option name="a"/></options>"#,
            "option has no value",
        ),
        (
            r#"<options><option name="Bad" value="a"/></options>"#,
            "option",
        ),
        (
            r#"<options><flag/></options>"#,
            "unknown element flag in options",
        ),
        (r#"<prompt>a<b/></prompt>"#, "nested markup in prompt"),
        (r#"<prompt foo="x">a</prompt>"#, "unknown attribute foo"),
        (
            r#"<description>a<b/></description>"#,
            "nested markup in description",
        ),
    ];
    for (body, needle) in body_cases {
        expect_schema(&agent_xml(body), needle);
    }
}

#[test]
fn duplicate_singletons_are_rejected() {
    for (tag, element) in [
        ("description", "<description>a</description>"),
        ("agent", "<agent/>"),
        ("role", r#"<role name="reviewer"/>"#),
        ("skills", "<skills/>"),
        ("mcp", "<mcp/>"),
        ("budget", r#"<budget usd="1"/>"#),
        ("prompt", "<prompt>a</prompt>"),
        ("options", "<options/>"),
    ] {
        expect_schema(
            &agent_xml(&format!("{element}{element}")),
            &format!("duplicate {tag}"),
        );
    }
}

#[test]
fn text_limits_are_in_bytes_after_trimming() {
    let description = |text: &str| agent_xml(&format!("<description>{text}</description>"));
    let prompt = |text: &str| agent_xml(&format!("<prompt>{text}</prompt>"));
    parse_template(&description(&"x".repeat(1024))).expect("1024 bytes");
    parse_template(&description(&format!("  {}\n  ", "x".repeat(1024)))).expect("trimmed first");
    expect_schema(&description(&"x".repeat(1025)), "description");
    // 341 euro signs are 1023 bytes and 342 are 1026, so the limit counts bytes.
    parse_template(&description(&"\u{20ac}".repeat(341))).expect("1023 bytes");
    expect_schema(&description(&"\u{20ac}".repeat(342)), "description");
    parse_template(&prompt(&"x".repeat(16 * 1024))).expect("16 KiB");
    expect_schema(&prompt(&"x".repeat(16 * 1024 + 1)), "prompt");
}

#[test]
fn list_limits_are_enforced() {
    let skills = |n: usize| {
        let items: String = (0..n).map(|i| format!(r#"<skill name="s{i}"/>"#)).collect();
        agent_xml(&format!("<skills>{items}</skills>"))
    };
    let servers = |n: usize| {
        let items: String = (0..n)
            .map(|i| format!(r#"<server name="s{i}"/>"#))
            .collect();
        agent_xml(&format!("<mcp>{items}</mcp>"))
    };
    let options = |n: usize| {
        let items: String = (0..n)
            .map(|i| format!(r#"<option name="o{i}" value="v"/>"#))
            .collect();
        agent_xml(&format!("<options>{items}</options>"))
    };
    parse_template(&skills(MAX_REFERENCES)).expect("64 skills");
    expect_schema(&skills(MAX_REFERENCES + 1), "skill list exceeds 64");
    parse_template(&servers(MAX_REFERENCES)).expect("64 servers");
    expect_schema(&servers(MAX_REFERENCES + 1), "server list exceeds 64");
    parse_template(&options(MAX_OPTIONS)).expect("32 options");
    expect_schema(&options(MAX_OPTIONS + 1), "option list exceeds 32");
}

#[test]
fn size_limit_is_exact() {
    parse_template(&padded_to(MAX_TEMPLATE_BYTES)).expect("exactly the limit");
    let error = parse_template(&padded_to(MAX_TEMPLATE_BYTES + 1)).expect_err("one byte over");
    assert!(
        matches!(error, TemplateError::TooLarge { limit: 32768 }),
        "{error}"
    );
}

#[test]
fn doctype_and_entities_are_refused_like_roles() {
    let internal = r#"<!DOCTYPE a [<!ENTITY x "y">]>"#;
    let external = r#"<!DOCTYPE a [<!ENTITY x SYSTEM "file:///etc/passwd">]>"#;
    let laughs = r#"<!DOCTYPE l [<!ENTITY b "lol"><!ENTITY x "&b;&b;&b;&b;&b;&b;&b;&b;">]>"#;
    for doctype in [internal, external, laughs, "<!DOCTYPE agent-template>"] {
        let template = format!(
            r#"{doctype}<agent-template name="plain"><description>&x;</description></agent-template>"#
        );
        let role = format!(r#"{doctype}<role name="plain"><summary>&x;</summary></role>"#);
        let error = parse_template(&template).expect_err("a DOCTYPE");
        assert!(matches!(error, TemplateError::Xml(_)), "{error}");
        let role_error = agmen_roles::parse(&role).expect_err("roles refuse it too");
        assert!(matches!(role_error, RoleError::Xml(_)), "{role_error}");
        assert_eq!(error.to_string(), role_error.to_string());
    }
    let named = agent_xml("<description>&nbsp;</description>");
    let error = parse_template(&named).expect_err("an undeclared entity");
    assert!(matches!(error, TemplateError::Xml(_)), "{error}");
}

const TEXT_XML: &str = r#"<agent-template name="plain"><description>&lt;b&gt;x&lt;/b&gt; &amp; &quot;q&quot;</description><prompt><![CDATA[</prompt><options><option name="a" value="b"/></options>]]></prompt></agent-template>"#;

#[test]
fn entities_and_cdata_in_text_stay_text() {
    let template = parse_template(TEXT_XML).expect("text");
    let Template::Agent(named) = &template else {
        panic!("expected an agent template");
    };
    assert_eq!(
        named.template.description.as_deref(),
        Some(r#"<b>x</b> & "q""#)
    );
    assert_eq!(
        named.template.prompt.as_deref(),
        Some(r#"</prompt><options><option name="a" value="b"/></options>"#)
    );
    assert!(named.template.options.is_empty());
}

#[test]
fn hostile_markup_nesting_is_rejected() {
    // Nested markup that is not a <node> stops at its first level.
    let xml = agent_xml(&format!(
        "<description>{}{}</description>",
        "<b>".repeat(4000),
        "</b>".repeat(4000)
    ));
    assert!(xml.len() <= MAX_TEMPLATE_BYTES);
    expect_schema(&xml, "nested markup in description");
    // Opened and never closed is a well-formedness error, not a hang.
    let unclosed = format!(r#"<team-template name="one">{}"#, "<node>".repeat(4000));
    let error = parse_template(&unclosed).expect_err("unclosed");
    assert!(matches!(error, TemplateError::Xml(_)), "{error}");
}
