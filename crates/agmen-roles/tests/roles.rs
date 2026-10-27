#![cfg(test)]

use agmen_roles::{
    EditScope, Kind, Layers, RoleError, Tier, ToolClass, claude_tools, import_subagent, parse,
    render_prompt,
};
use std::collections::BTreeSet;

fn set<T: Ord>(items: impl IntoIterator<Item = T>) -> BTreeSet<T> {
    items.into_iter().collect()
}
fn layer(xml: &str) -> (tempfile::TempDir, Layers) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("custom.xml"), xml).unwrap();
    let layers = Layers {
        personal: Some(dir.path().to_owned()),
        ..Layers::default()
    };
    (dir, layers)
}
#[test]
fn builtins_parse_and_resolve() {
    let catalog = Layers::default().load().unwrap();
    assert_eq!(catalog.len(), 8);
    let reviewer = catalog.get("reviewer").unwrap();
    assert_eq!(
        reviewer.tools,
        set([ToolClass::Read, ToolClass::Search, ToolClass::Shell])
    );
    assert_eq!(
        reviewer.verbs,
        set([
            "HELLO", "BYE", "FYI", "ASK", "ANSWER", "ACK", "STATUS", "DM", "ALERT", "FOUND",
            "REVIEW"
        ]
        .map(String::from))
    );
    assert!(catalog.get("engineer").unwrap().verbs.contains("LEND"));
    assert_eq!(catalog.get("listener").unwrap().kind, Kind::Listener);
}
#[test]
fn extends_narrows_and_adds_rules() {
    let (_dir, layers) = layer(include_str!("fixtures/narrow-reviewer.xml"));
    let catalog = layers.load().unwrap();
    let role = catalog.get("narrow-reviewer").unwrap();
    assert_eq!(role.tools, set([ToolClass::Read, ToolClass::Search]));
    let prompt = render_prompt(role);
    assert!(
        prompt.find("Read the bodies of earlier reviews").unwrap()
            < prompt.find("Only inspect").unwrap()
    );
}
#[test]
fn ceiling_strips_listener_shell() {
    let (_dir, layers) = layer(include_str!("fixtures/widen-listener.xml"));
    let catalog = layers.load().unwrap();
    let role = catalog.get("listener").unwrap();
    assert_eq!(role.tools, set([ToolClass::Read]));
    assert_eq!(role.stripped, ["tool shell"]);
}
#[test]
fn ceiling_strips_listener_subagents() {
    let (_dir, layers) = layer(
        r#"<role name="listener" extends="base" kind="listener" tier="small"><tools allow="read subagents"/></role>"#,
    );
    let catalog = layers.load().unwrap();
    let role = catalog.get("listener").unwrap();
    assert_eq!(role.tools, set([ToolClass::Read]));
    assert_eq!(role.stripped, ["tool subagents"]);
    assert!(!claude_tools(role).contains(&"Agent"));
}
#[test]
fn ceiling_strips_agent_ruling() {
    let (_dir, layers) = layer(
        r#"<role name="engineer" extends="base" kind="agent" tier="standard"><verbs allow="RULING"/></role>"#,
    );
    let catalog = layers.load().unwrap();
    let role = catalog.get("engineer").unwrap();
    assert!(!role.verbs.contains("RULING"));
    assert_eq!(role.stripped, ["verb RULING"]);
}
#[test]
fn project_overrides_builtin() {
    let dir = tempfile::tempdir().unwrap();
    let roles = dir.path().join(".agmen/roles");
    std::fs::create_dir_all(&roles).unwrap();
    std::fs::write(
        roles.join("reviewer.xml"),
        include_str!("../../../roles/reviewer.xml").replace("tier=\"deep\"", "tier=\"standard\""),
    )
    .unwrap();
    let original = Layers::default().load().unwrap();
    let catalog = Layers {
        project: Some(dir.path().to_owned()),
        ..Layers::default()
    }
    .load()
    .unwrap();
    assert_eq!(catalog.get("reviewer").unwrap().tier, Tier::Standard);
    assert_ne!(
        catalog.get("reviewer").unwrap().sha256,
        original.get("reviewer").unwrap().sha256
    );
}
#[test]
fn unknown_element_is_error() {
    assert!(matches!(
        parse("<role><hooks/></role>"),
        Err(RoleError::Schema(_))
    ));
    assert!(matches!(
        parse(r#"<role name="xx"><rules><hooks/></rules></role>"#),
        Err(RoleError::Schema(_))
    ));
}
#[test]
fn render_prompt_order() {
    let catalog = Layers::default().load().unwrap();
    let prompt = render_prompt(catalog.get("engineer").unwrap());
    assert!(prompt.starts_with("<purpose>"));
    assert!(prompt.find("You are one agent").unwrap() < prompt.find("Take one item").unwrap());
    assert!(prompt.ends_with("</report>"));
    assert!(!prompt.contains("<tools"));
}
#[test]
fn import_subagent_maps_fields() {
    let xml = import_subagent(include_str!("fixtures/subagent.md"), "imported").unwrap();
    assert!(xml.contains("allow=\"read search shell\""));
    assert!(xml.contains("tier=\"deep\""));
    assert!(xml.contains("extends=\"none\""));
    assert!(xml.contains("<purpose>Review the patch &amp; report &lt;findings&gt;."));
    parse(&xml).unwrap();
}
#[test]
fn extends_must_narrow() {
    for body in [
        "<tools allow=\"read edit\"/>",
        "<verbs allow=\"CLAIM\"/>",
        "<edits scope=\"lanes\"/>",
    ] {
        let (_dir, layers) = layer(&format!(
            "<role name=\"custom\" extends=\"reviewer\">{body}</role>"
        ));
        assert!(
            matches!(layers.load(), Err(RoleError::Widening(name)) if name == "custom"),
            "{body}"
        );
    }
    let (_dir, layers) = layer(
        r#"<role name="custom" extends="engineer" kind="agent"><edits scope="paths" globs="**"/></role>"#,
    );
    let catalog = layers.load().unwrap();
    let role = catalog.get("custom").unwrap();
    assert_eq!(role.edits, EditScope::LanesWithin(vec!["**".into()]));
    assert_eq!(
        agmen_proto::EditScope::from(role.edits.clone()),
        agmen_proto::EditScope::LanesWithin {
            globs: vec!["**".into()]
        }
    );
}

#[test]
fn inherited_edit_scopes_keep_lane_and_glob_restrictions() {
    for (ancestor, scope) in [
        ("engineer", EditScope::LanesWithin(vec!["docs/**".into()])),
        ("planner", EditScope::Paths(vec!["docs/**".into()])),
    ] {
        for (body, expected) in [
            ("", Some(scope.clone())),
            (
                r#"<edits scope="paths" globs="docs/**"/>"#,
                Some(scope.clone()),
            ),
            (r#"<edits scope="none"/>"#, Some(EditScope::None)),
            (
                r#"<edits scope="paths" globs="docs/guide/** docs/README.md"/>"#,
                Some(match &scope {
                    EditScope::LanesWithin(_) => EditScope::LanesWithin(vec![
                        "docs/guide/**".into(),
                        "docs/README.md".into(),
                    ]),
                    _ => EditScope::Paths(vec!["docs/guide/**".into(), "docs/README.md".into()]),
                }),
            ),
            (r#"<edits scope="lanes"/>"#, None),
            (r#"<edits scope="paths" globs="**"/>"#, None),
            (r#"<edits scope="paths" globs="docs/** src/**"/>"#, None),
            (r#"<edits scope="paths" globs="docs-other/**"/>"#, None),
        ] {
            let (dir, layers) = layer(&format!(
                r#"<role name="custom" extends="middle">{body}</role>"#
            ));
            std::fs::write(
                dir.path().join("middle.xml"),
                format!(r#"<role name="middle" extends="{ancestor}"><edits scope="paths" globs="docs/**"/></role>"#),
            )
            .unwrap();
            match expected {
                Some(edits) => {
                    let catalog = layers.load().unwrap();
                    let role = catalog.get("custom").unwrap();
                    assert_eq!(role.edits, edits, "{ancestor}: {body}");
                    assert_eq!(role.role.edits, Some(edits), "{ancestor}: {body}");
                }
                None => assert!(
                    matches!(layers.load(), Err(RoleError::Widening(name)) if name == "custom"),
                    "{ancestor}: {body}"
                ),
            }
        }
    }
}
#[test]
fn extends_none_skips_base() {
    let (_dir, layers) = layer(
        r#"<role name="custom" extends="none" kind="agent" tier="standard"><purpose>Alone.</purpose><verbs allow="REVIEW"/></role>"#,
    );
    let catalog = layers.load().unwrap();
    let role = catalog.get("custom").unwrap();
    assert_eq!(role.verbs, set(["REVIEW".into()]));
    assert_eq!(render_prompt(role), "<purpose>Alone.</purpose>");
}
#[test]
fn claude_tools_skips_mcp_classes() {
    let catalog = Layers::default().load().unwrap();
    assert_eq!(
        claude_tools(catalog.get("listener").unwrap()),
        ["Read", "Grep", "Glob"]
    );
}
#[test]
fn cycles_and_excess_depth_are_errors() {
    for parent in ["custom", "other"] {
        let (dir, layers) = layer(&format!(
            "<role name=\"custom\" extends=\"{parent}\" kind=\"agent\" tier=\"small\"/>"
        ));
        std::fs::write(
            dir.path().join("other.xml"),
            "<role name=\"other\" extends=\"custom\"/>",
        )
        .unwrap();
        assert!(matches!(layers.load(), Err(RoleError::TooDeep(name)) if name == "custom"));
    }
    let (dir, layers) = layer("<role name=\"custom\" extends=\"role0\"/>");
    for i in 0..10 {
        std::fs::write(
            dir.path().join(format!("role{i}.xml")),
            format!(
                "<role name=\"role{i}\" extends=\"{}\"/>",
                if i == 9 {
                    "engineer".into()
                } else {
                    format!("role{}", i + 1)
                }
            ),
        )
        .unwrap();
    }
    assert!(matches!(layers.load(), Err(RoleError::TooDeep(name)) if name == "custom"));
}
#[test]
fn schema_settings_and_wire_scope() {
    let role = parse(r#"<role name="custom" version="1" kind="agent" tier="deep"><tools allow="read search" deny="search"/><worktree mode="detached" at="done-commit" remove="after-review"/><edits scope="paths" globs="docs/**"/></role>"#).unwrap();
    assert_eq!(role.version, 1);
    assert_eq!(
        role.worktree.as_ref().unwrap().at.as_deref(),
        Some("done-commit")
    );
    assert_eq!(
        agmen_proto::EditScope::from(role.edits.unwrap()),
        agmen_proto::EditScope::Paths {
            globs: vec!["docs/**".into()]
        }
    );
    for xml in [
        "<role name=\"custom\" version=\"2\"/>",
        "<role name=\"custom\"><tools allow=\"bogus\"/></role>",
    ] {
        assert!(parse(xml).is_err());
    }
    let (_dir, layers) =
        layer(r#"<role name="custom" extends="engineer"><tools deny="shell"/></role>"#);
    assert!(
        !layers
            .load()
            .unwrap()
            .get("custom")
            .unwrap()
            .tools
            .contains(&ToolClass::Shell)
    );
}

#[test]
fn inherited_kind_cannot_change_through_untyped_parent() {
    let (dir, layers) = layer(r#"<role name="custom" extends="middle" kind="listener"/>"#);
    std::fs::write(
        dir.path().join("middle.xml"),
        r#"<role name="middle" extends="reviewer"/>"#,
    )
    .unwrap();
    assert!(matches!(layers.load(), Err(RoleError::Widening(name)) if name == "custom"));
}

#[test]
fn compatibility_honors_denies_and_kind_inheritance() {
    use agmen_roles::compat;
    let mut specs = compat::BUILT_IN
        .iter()
        .map(|(_, xml)| compat::parse(xml).unwrap())
        .collect::<Vec<_>>();
    specs.push(
        compat::parse(r#"<role name="custom" extends="engineer"><tools deny="shell"/></role>"#)
            .unwrap(),
    );
    assert!(
        !compat::resolve("custom", &specs)
            .unwrap()
            .tools
            .iter()
            .any(|t| t == "shell")
    );
    specs.push(compat::parse(r#"<role name="middle" extends="engineer"/>"#).unwrap());
    specs.push(compat::parse(r#"<role name="other" extends="middle" kind="listener"/>"#).unwrap());
    assert!(compat::resolve("other", &specs).is_err());
}

#[test]
fn actual_base_verbs_are_inherited() {
    let (_dir, layers) =
        layer(r#"<role name="base" version="1"><verbs allow="HELLO PROPOSAL"/></role>"#);
    let catalog = layers.load().unwrap();
    assert!(catalog.get("reviewer").unwrap().verbs.contains("PROPOSAL"));
}

#[test]
fn compatibility_applies_kind_ceilings() {
    use agmen_roles::compat;
    let mut specs = compat::BUILT_IN
        .iter()
        .filter(|(name, _)| *name != "listener")
        .map(|(_, xml)| compat::parse(xml).unwrap())
        .collect::<Vec<_>>();
    specs.push(compat::parse(include_str!("fixtures/widen-listener.xml")).unwrap());
    assert_eq!(compat::resolve("listener", &specs).unwrap().tools, ["read"]);
}

#[test]
fn importer_accepts_yaml_forms_and_rejects_unmapped_input() {
    for md in [
        "---\n---\nBody",
        "---\n---",
        "---\r\ntools: [Read, 'Glob', Bash]\r\ndisallowedTools:\r\n  - Bash\r\nmodel: sonnet\r\n---\r\nBody",
    ] {
        let xml = import_subagent(md, "custom").unwrap();
        parse(&xml).unwrap();
    }
    let xml = import_subagent(
        "---\ntools: [Read, Bash]\ndisallowedTools: [Bash]\n---\nBody",
        "custom",
    )
    .unwrap();
    let (_dir, layers) = layer(&xml);
    assert_eq!(
        layers.load().unwrap().get("custom").unwrap().tools,
        set([ToolClass::Read])
    );
    for md in [
        "---\ntools: Unknown\n---\nBody",
        "---\nmodel: unknown\n---\nBody",
        "---\ntools: [Read\n---\nBody",
    ] {
        assert!(import_subagent(md, "custom").is_err());
    }
}

#[test]
fn schema_rejects_namespaces_and_unsafe_paths() {
    for xml in [
        r#"<role xmlns="urn:other" name="custom"/>"#,
        r#"<role name="custom"><edits scope="paths" globs="../outside/**"/></role>"#,
        r#"<role name="custom"><edits scope="paths" globs="/outside/**"/></role>"#,
    ] {
        assert!(parse(xml).is_err(), "{xml}");
    }
}

#[test]
fn personal_precedence_hash_and_duplicates() {
    use sha2::{Digest, Sha256};
    let xml = r#"<role name="custom" extends="reviewer"><purpose>Own.</purpose></role>"#;
    let (dir, layers) = layer(xml);
    let catalog = layers.load().unwrap();
    let mut digest = Sha256::new();
    digest.update(include_str!("../../../roles/base.xml").as_bytes());
    digest.update(include_str!("../../../roles/reviewer.xml").as_bytes());
    digest.update(xml.as_bytes());
    let expected: String = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(catalog.get("custom").unwrap().sha256, expected);
    std::fs::write(dir.path().join("duplicate.xml"), xml).unwrap();
    assert!(layers.load().is_err());
}

#[test]
#[cfg(unix)]
fn layers_skip_unsafe_or_excess_files_and_follow_personal_links() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let xml = r#"<role name="custom" extends="reviewer"/>"#;
    std::fs::write(outside.path().join("custom.xml"), xml).unwrap();
    std::fs::create_dir(root.path().join(".agmen")).unwrap();
    symlink(outside.path(), root.path().join(".agmen/roles")).unwrap();
    let project = Layers {
        project: Some(root.path().to_owned()),
        ..Layers::default()
    };
    assert!(project.load().unwrap().get("custom").is_none());
    let personal = tempfile::tempdir().unwrap();
    symlink(
        outside.path().join("custom.xml"),
        personal.path().join("custom.xml"),
    )
    .unwrap();
    std::fs::write(personal.path().join("oversized.xml"), "x".repeat(65537)).unwrap();
    std::fs::write(personal.path().join("invalid.xml"), [255]).unwrap();
    let layers = Layers {
        personal: Some(personal.path().to_owned()),
        ..Layers::default()
    };
    assert!(layers.load().unwrap().get("custom").is_some());
    let project_root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project_root.path().join(".agmen/roles")).unwrap();
    symlink(
        outside.path().join("custom.xml"),
        project_root.path().join(".agmen/roles/custom.xml"),
    )
    .unwrap();
    let project = Layers {
        project: Some(project_root.path().to_owned()),
        ..Layers::default()
    };
    assert!(project.load().unwrap().get("custom").is_none());
}

#[test]
fn compatibility_preserves_standalone_roles() {
    use agmen_roles::compat;
    let specs = [compat::parse(r#"<role name="custom" kind="agent" tier="small"/>"#).unwrap()];
    let resolved = compat::resolve("custom", &specs).unwrap();
    assert_eq!(resolved.kind, Kind::Agent);
    assert_eq!(resolved.tier, Tier::Small);
    assert!(resolved.verbs.is_empty());
}

#[test]
fn importer_rejects_xml_invalid_or_oversized_output() {
    for md in ["Body\0".to_owned(), "&".repeat(14_000)] {
        assert!(import_subagent(&md, "custom").is_err());
    }
}

#[test]
fn resolved_role_inherits_summary_and_worktree() {
    let (_dir, layers) = layer(include_str!("fixtures/narrow-reviewer.xml"));
    let catalog = layers.load().unwrap();
    let child = catalog.get("narrow-reviewer").unwrap();
    let parent = catalog.get("reviewer").unwrap();
    assert_eq!(child.role.summary, parent.role.summary);
    assert_eq!(child.worktree, parent.worktree);
}

#[test]
fn personal_overrides_project_and_builtin() {
    let root = tempfile::tempdir().unwrap();
    let personal = tempfile::tempdir().unwrap();
    let project_roles = root.path().join(".agmen/roles");
    std::fs::create_dir_all(&project_roles).unwrap();
    let builtin = include_str!("../../../roles/reviewer.xml");
    std::fs::write(
        project_roles.join("reviewer.xml"),
        builtin.replace("tier=\"deep\"", "tier=\"standard\""),
    )
    .unwrap();
    std::fs::write(
        personal.path().join("reviewer.xml"),
        builtin.replace("tier=\"deep\"", "tier=\"small\""),
    )
    .unwrap();
    let layers = Layers {
        project: Some(root.path().to_owned()),
        personal: Some(personal.path().to_owned()),
        ..Layers::default()
    };
    assert_eq!(
        layers.load().unwrap().get("reviewer").unwrap().tier,
        Tier::Small
    );
}

#[test]
fn claude_tools_maps_all_adapter_classes() {
    let (_dir, layers) = layer(
        r#"<role name="custom" extends="none" kind="coordinator" tier="standard"><tools allow="read search edit shell web subagents git-read settings"/></role>"#,
    );
    let catalog = layers.load().unwrap();
    assert_eq!(
        claude_tools(catalog.get("custom").unwrap()),
        [
            "Read",
            "Grep",
            "Glob",
            "Edit",
            "Write",
            "NotebookEdit",
            "Bash",
            "WebFetch",
            "WebSearch",
            "Agent"
        ]
    );
}
