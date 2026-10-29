use agmen_roles::{
    AgentSpec, AgentTemplate, MAX_NODES, MAX_OPTIONS, MAX_REFERENCES, MAX_TEMPLATE_BYTES, Named,
    NodeAgent, PackRef, Reference, RoleError, Template, TemplateError, TemplateOption,
    agents_started, parse_template, to_xml,
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

#[test]
fn spec_team_example_parses() {
    let template =
        parse_template(include_str!("fixtures/templates/eng-team.team.xml")).expect("eng-team");
    let Template::Team(named) = template else {
        panic!("expected a team template");
    };
    assert_eq!(named.name, "eng-team");
    assert_eq!(
        named.template.description.as_deref(),
        Some("A coordinator with two engineers; each engineer can start reviewers.")
    );
    let top = &named.template.nodes[0];
    assert_eq!(top.agent, NodeAgent::Named("coordinator-opus".into()));
    assert_eq!(top.count, 1);
    let engineer = &top.children[0];
    assert_eq!(engineer.agent, NodeAgent::Named("rust-engineer".into()));
    assert_eq!(engineer.count, 2);
    let reviewer = &engineer.children[0];
    assert_eq!(reviewer.agent, NodeAgent::Named("rust-reviewer".into()));
    assert_eq!(reviewer.count, 0);
    assert!(reviewer.children.is_empty());
    assert_eq!(agents_started(&named.template.nodes), 3);
}

#[test]
fn inline_node_parses() {
    let template = parse_template(
        r#"<team-template name="one"><node><agent-template><agent kind="codex"/></agent-template></node></team-template>"#,
    )
    .expect("inline node");
    let Template::Team(named) = template else {
        panic!("expected a team template");
    };
    let NodeAgent::Inline(agent) = &named.template.nodes[0].agent else {
        panic!("expected an inline agent");
    };
    assert_eq!(agent.agent.kind.as_deref(), Some("codex"));
}

#[test]
fn node_needs_exactly_one_agent() {
    let cases = [
        r#"<team-template name="one"><node template="a1"><agent-template><agent kind="codex"/></agent-template></node></team-template>"#,
        r#"<team-template name="one"><node></node></team-template>"#,
        r#"<team-template name="one"><node><agent-template name="x"><agent kind="codex"/></agent-template></node></team-template>"#,
    ];
    for xml in cases {
        let error = parse_template(xml).expect_err("node");
        assert!(matches!(error, TemplateError::Schema(_)), "{xml}: {error}");
        assert!(error.to_string().contains("node"), "{xml}: {error}");
    }
}

#[test]
fn count_product_is_capped() {
    for counts in [vec!["64", "64", "64"], vec!["4", "4", "4"]] {
        let error = parse_template(&nested_counts(&counts)).expect_err("too many agents");
        assert!(matches!(error, TemplateError::Schema(_)), "{error}");
        assert!(error.to_string().contains("64 agents"), "{error}");
    }
    let template = parse_template(&nested_counts(&["4", "3", "3"])).expect("52 agents");
    let Template::Team(named) = template else {
        panic!("expected a team template");
    };
    assert_eq!(agents_started(&named.template.nodes), 52);
}

#[test]
fn depth_33_is_rejected() {
    let error = parse_template(&nested_nodes(33)).expect_err("33 levels");
    assert!(matches!(error, TemplateError::Schema(_)), "{error}");
    assert!(error.to_string().contains("32 levels"), "{error}");
    parse_template(&nested_nodes(32)).expect("32 levels");
}

#[test]
fn count_out_of_range_is_rejected() {
    for count in ["65", "-1"] {
        let xml = format!(
            "<team-template name=\"one\"><node template=\"a1\" count=\"{count}\"/></team-template>"
        );
        let error = parse_template(&xml).expect_err(count);
        assert!(
            matches!(error, TemplateError::Schema(_)),
            "{count}: {error}"
        );
    }
}

fn nested_counts(counts: &[&str]) -> String {
    let mut xml = String::from(r#"<team-template name="big">"#);
    for count in counts {
        xml.push_str(&format!(r#"<node template="a1" count="{count}">"#));
    }
    for _ in counts {
        xml.push_str("</node>");
    }
    xml.push_str("</team-template>");
    xml
}

fn nested_nodes(depth: usize) -> String {
    let mut xml = String::from(r#"<team-template name="deep">"#);
    for _ in 0..depth {
        xml.push_str(r#"<node template="a1">"#);
    }
    for _ in 0..depth {
        xml.push_str("</node>");
    }
    xml.push_str("</team-template>");
    xml
}

fn team_xml(body: &str) -> String {
    format!(r#"<team-template name="one">{body}</team-template>"#)
}

const TEAM_WITH_OPTIONS: &str = r#"<team-template name="opts">
  <node>
    <agent-template><options><option name="a" value="1"/><option name="b" value="2"/></options></agent-template>
    <node><agent-template><options><option name="c" value="3"/></options></agent-template></node>
  </node>
  <node template="named-one"/>
  <node><agent-template><options><option name="d" value="4"/></options></agent-template></node>
</team-template>"#;

#[test]
fn option_helpers_reach_inline_team_nodes() {
    let mut template = parse_template(TEAM_WITH_OPTIONS).expect("team with options");
    assert_eq!(template.option_names(), ["a", "b", "c", "d"]);
    assert_eq!(template.strip_options(), 4);
    assert!(template.option_names().is_empty());
}

#[test]
fn rejects_bad_team_shapes() {
    let cases = [
        (r#"<team-template name="one"/>"#.to_owned(), "no node"),
        (team_xml("<description>a</description>"), "no node"),
        (
            team_xml(
                r#"<description>a</description><description>b</description><node template="a1"/>"#,
            ),
            "duplicate description",
        ),
        (
            team_xml(r#"<agent/><node template="a1"/>"#),
            "unknown element agent",
        ),
        (team_xml(r#"<node template="A1"/>"#), "template"),
        (
            team_xml(r#"<node template="a1" foo="x"/>"#),
            "unknown attribute foo",
        ),
        (
            team_xml(r#"<node template="a1"><agent/></node>"#),
            "unknown element agent in node",
        ),
        (
            team_xml(r#"<node><agent-template/><agent-template/></node>"#),
            "more than one agent-template",
        ),
        (
            team_xml(r#"<node><agent-template version="2"/></node>"#),
            "version",
        ),
        (
            team_xml(r#"<node><agent-template saved-by="x"/></node>"#),
            "node",
        ),
        (
            r#"<team-template name="one" saved-by=""><node template="a1"/></team-template>"#
                .to_owned(),
            "saved-by",
        ),
    ];
    for (xml, needle) in cases {
        expect_schema(&xml, needle);
    }
}

#[test]
fn node_limit_is_enforced() {
    let nodes = |n: usize| team_xml(&r#"<node template="a1" count="0"/>"#.repeat(n));
    parse_template(&nodes(MAX_NODES)).expect("64 nodes");
    expect_schema(&nodes(MAX_NODES + 1), "more than 64 node elements");
}

#[test]
fn hostile_node_nesting_is_rejected() {
    // 1200 nested <node> elements fit in the size limit and stop at level 33.
    let deep = nested_nodes(1200);
    assert!(deep.len() <= MAX_TEMPLATE_BYTES);
    expect_schema(&deep, "32 levels");
}

#[test]
fn count_product_does_not_overflow() {
    // 64 to the 32nd power does not fit in a u32. The product saturates and the
    // cap still reports too many agents.
    expect_schema(&nested_counts(&["64"; 32]), "64 agents");
}

#[test]
fn round_trips() {
    let inline = r#"<team-template name="mix" saved-by="ada"><description>A mix.</description><node count="2"><agent-template><agent kind="codex" model="gpt-5.4" effort="low"/><role name="engineer"/><prompt>Go.</prompt></agent-template><node template="rust-reviewer" count="0"/></node></team-template>"#;
    for xml in [
        include_str!("fixtures/templates/rust-reviewer.agent.xml"),
        include_str!("fixtures/templates/eng-team.team.xml"),
        inline,
        r#"<agent-template name="plain"/>"#,
    ] {
        let template = parse_template(xml).expect("parse");
        assert_eq!(
            parse_template(&to_xml(&template)).expect("round trip"),
            template
        );
    }
}

#[test]
fn markup_in_text_round_trips() {
    let template = Template::Agent(Named {
        name: "plain".into(),
        saved_by: None,
        template: AgentTemplate {
            description: Some(r#"a < b & "c" ]]> 'd'"#.into()),
            prompt: Some("</prompt><options>".into()),
            ..AgentTemplate::default()
        },
    });
    let xml = to_xml(&template);
    assert_eq!(parse_template(&xml).expect("round trip"), template);
    let doc = roxmltree::Document::parse(&xml).expect("xml");
    assert!(
        doc.descendants().all(|node| !node.has_tag_name("options")),
        "{xml}"
    );
}

#[test]
fn writes_the_spec_shape() {
    let template = parse_template(r#"<agent-template name="plain"/>"#).expect("empty");
    assert_eq!(
        to_xml(&template),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<agent-template name=\"plain\" version=\"1\" />\n"
    );
}

#[test]
fn carriage_return_in_text_round_trips() {
    // A parser turns a raw carriage return into a newline, so only a character
    // reference keeps it.
    let template = parse_template(&agent_xml(
        "<description>a&#13;b</description><prompt>c&#13;&#10;d</prompt>",
    ))
    .expect("character references");
    let Template::Agent(named) = &template else {
        panic!("expected an agent template");
    };
    assert_eq!(named.template.description.as_deref(), Some("a\rb"));
    assert_eq!(named.template.prompt.as_deref(), Some("c\r\nd"));
    assert_eq!(
        parse_template(&to_xml(&template)).expect("round trip"),
        template
    );
}

#[test]
fn quote_heavy_text_round_trips_within_the_limit() {
    // 10 KiB of quotes and apostrophes is a valid prompt. Written back as
    // &quot; and &apos; it would be 60 KB and the parser would refuse it.
    let prompt = "'\"".repeat(5 * 1024);
    let template =
        parse_template(&agent_xml(&format!("<prompt>{prompt}</prompt>"))).expect("valid prompt");
    let xml = to_xml(&template);
    assert!(xml.len() <= MAX_TEMPLATE_BYTES, "{} bytes", xml.len());
    assert_eq!(parse_template(&xml).expect("round trip"), template);
}

#[test]
fn attribute_whitespace_is_written_as_references() {
    // A parser turns a raw tab, newline or carriage return in an attribute into
    // a space. Written as references they reach the parser, which refuses
    // control characters in saved-by, so nothing changes silently.
    let template = Template::Agent(Named {
        name: "plain".into(),
        saved_by: Some("a\nb\tc\rd".into()),
        template: AgentTemplate::default(),
    });
    let xml = to_xml(&template);
    assert!(xml.contains(r#"saved-by="a&#10;b&#9;c&#13;d""#), "{xml}");
    expect_schema(&xml, "saved-by");
}

#[test]
fn greater_than_is_escaped_only_where_xml_needs_it() {
    let template = parse_template(&agent_xml(
        "<prompt>a &gt; b ]&gt; c ]]&gt; d ]]]&gt;</prompt>",
    ))
    .expect("text");
    let xml = to_xml(&template);
    assert!(
        xml.contains("<prompt>a > b ]> c ]]&gt; d ]]]&gt;</prompt>"),
        "{xml}"
    );
    assert_eq!(parse_template(&xml).expect("round trip"), template);
}

#[test]
fn markup_in_attribute_values_round_trips() {
    let template = parse_template(
        r#"<agent-template name="plain"><skills><skill name="s" pack="git@host:a&amp;b/&quot;x&quot;&lt;y&gt;&apos;z@abcdef1"/></skills><options><option name="a" value="&lt;&amp;&quot;&apos;&gt;"/></options></agent-template>"#,
    )
    .expect("markup in attributes");
    let Template::Agent(named) = &template else {
        panic!("expected an agent template");
    };
    assert_eq!(named.template.options[0].value, r#"<&"'>"#);
    assert_eq!(
        parse_template(&to_xml(&template)).expect("round trip"),
        template
    );
}

#[test]
fn entities_and_cdata_in_text_round_trip() {
    let template = parse_template(TEXT_XML).expect("text");
    assert_eq!(
        parse_template(&to_xml(&template)).expect("round trip"),
        template
    );
}

#[test]
fn stripped_options_are_not_written() {
    let mut template = parse_template(TEAM_WITH_OPTIONS).expect("team with options");
    assert!(to_xml(&template).contains("<option"));
    template.strip_options();
    let xml = to_xml(&template);
    assert!(!xml.contains("<option"), "{xml}");
}
