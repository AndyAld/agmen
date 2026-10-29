use super::*;

const BASE: &str = include_str!("../../../roles/base.xml");
const REVIEWER: &str = include_str!("../../../roles/reviewer.xml");
const ENGINEER: &str = include_str!("../../../roles/engineer.xml");

fn specs(texts: &[&str]) -> Vec<RoleSpec> {
    texts.iter().map(|t| parse(t).unwrap()).collect()
}

#[test]
fn parse_reads_the_settings_of_reviewer_xml() {
    let r = parse(REVIEWER).unwrap();
    assert_eq!(r.name, "reviewer");
    assert_eq!(r.extends.as_deref(), Some("base"));
    assert_eq!(r.kind, Some(RoleKind::Agent));
    assert_eq!(r.tier, Some(Tier::Deep));
    assert_eq!(
        r.tools.as_deref(),
        Some(&["read", "search", "shell"].map(String::from)[..])
    );
    assert_eq!(r.verbs.as_deref(), Some(&["REVIEW".to_owned()][..]));
    assert_eq!(r.edits, Some(Edits::None));
}

#[test]
fn reviewer_gets_base_verbs_plus_review() {
    // Review C164: the panel's verbs come from the files, not a fixture.
    let all = specs(&[BASE, REVIEWER, ENGINEER]);
    let r = resolve("reviewer", &all).unwrap();
    assert_eq!(r.kind, RoleKind::Agent);
    assert_eq!(r.tier, Tier::Deep);
    assert_eq!(r.tools, ["read", "search", "shell"]);
    assert_eq!(r.verbs[0], "REVIEW");
    for base in ["HELLO", "ASK", "ANSWER", "FYI", "DM", "FOUND"] {
        assert!(r.verbs.iter().any(|v| v == base), "{base}");
    }
    assert!(!r.verbs.iter().any(|v| v == "CLAIM"));
    assert_eq!(r.edits, Edits::None);
}

#[test]
fn a_child_inherits_what_it_does_not_declare() {
    let child = r#"<role name="docs-writer" extends="engineer" kind="agent" tier="standard" version="1"><edits scope="paths" globs="docs/** README.md" /></role>"#;
    let all = specs(&[BASE, ENGINEER, child]);
    let r = resolve("docs-writer", &all).unwrap();
    assert_eq!(r.tools, ["read", "search", "edit", "shell"]);
    assert!(r.verbs.iter().any(|v| v == "DONE"));
    assert_eq!(
        r.edits,
        Edits::Paths(vec!["docs/**".into(), "README.md".into()])
    );
    assert_eq!(r.edits.to_string(), "paths docs/** README.md");
}

#[test]
fn bad_role_files_are_errors() {
    assert!(matches!(parse("<role"), Err(RoleError::Xml(_))));
    assert!(matches!(
        parse("<agent name=\"x\"/>"),
        Err(RoleError::NotARole)
    ));
    assert!(matches!(
        parse(r#"<role name="x" kind="robot"/>"#),
        Err(RoleError::Attribute { .. })
    ));
    let all = specs(&[
        BASE,
        r#"<role name="a" extends="b" kind="agent" tier="small"/>"#,
    ]);
    assert!(matches!(resolve("a", &all), Err(RoleError::Missing(name)) if name == "b"));
    let looped = specs(&[
        r#"<role name="a" extends="b" kind="agent" tier="small"/>"#,
        r#"<role name="b" extends="a" kind="agent" tier="small"/>"#,
    ]);
    assert!(matches!(resolve("a", &looped), Err(RoleError::TooDeep(_))));
    assert!(matches!(
        resolve("base", &specs(&[BASE])),
        Err(RoleError::NoKind(_))
    ));
}

#[test]
fn kind_cap_matches_the_hub() {
    use agmen_proto::{IdentityKind, Verb};
    assert!(kind_may_send(RoleKind::Agent, Verb::Claim));
    assert!(!kind_may_send(RoleKind::Agent, Verb::Ruling));
    assert!(kind_may_send(RoleKind::Coordinator, Verb::Assign));
    assert!(kind_may_send(RoleKind::Listener, Verb::Delegated));
    assert!(!kind_may_send(RoleKind::Listener, Verb::Claim));
    // NULL verbs fall back to the kind table, which refuses REVIEW; a role's mint ceiling allows it (D15)
    assert!(!kind_may_send(RoleKind::Agent, Verb::Review));
    assert!(agmen_proto::kind_ceiling(IdentityKind::Agent, Verb::Review));
}

#[test]
fn built_in_names_match_their_files() {
    for (name, text) in BUILT_IN {
        assert_eq!(parse(text).unwrap().name, name);
    }
}
