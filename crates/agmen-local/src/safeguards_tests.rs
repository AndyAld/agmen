use super::*;

#[test]
fn defaults_match_the_rulings() {
    assert_eq!(TABLE, "agents");
    assert_eq!(MAX_MANAGED, 256);
    assert_eq!(MAX_LIMIT, 256);
    let defaults = Safeguards::default();
    assert_eq!(
        defaults,
        Safeguards {
            limit_tree_depth: true,
            max_tree_depth: 4,
            limit_agents: true,
            max_agents: 16,
            budgets: true,
            child_budgets: ChildBudgets::Shared,
            separate_child_usd: 5.0,
        }
    );
    assert!(defaults.validate().is_ok());
}

#[test]
fn limits_are_bounded() {
    for value in [0, 257, u32::MAX] {
        for key in ["max_tree_depth", "max_agents"] {
            let mut settings = Safeguards::default();
            match key {
                "max_tree_depth" => settings.max_tree_depth = value,
                _ => settings.max_agents = value,
            }
            let error = settings.validate().unwrap_err();
            assert!(matches!(
                error,
                SafeguardsError::OutOfRange { key: bad_key, value: bad_value }
                    if bad_key == key && bad_value == value
            ));
            assert_eq!(
                error.to_string(),
                format!("{key} must be 1 to 256, not {value}")
            );
        }
    }
    for value in [1, 256] {
        let settings = Safeguards {
            max_tree_depth: value,
            max_agents: value,
            ..Safeguards::default()
        };
        assert!(settings.validate().is_ok());
    }
    for value in [
        0.0,
        -1.0,
        0.009,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        1000.01,
    ] {
        let settings = Safeguards {
            separate_child_usd: value,
            ..Safeguards::default()
        };
        let error = settings.validate().unwrap_err();
        assert!(matches!(error, SafeguardsError::SeparateChildUsd));
        assert_eq!(error.to_string(), "separate_child_usd must be 0.01 to 1000");
    }
    for value in [0.01, 1000.0] {
        let settings = Safeguards {
            separate_child_usd: value,
            ..Safeguards::default()
        };
        assert!(settings.validate().is_ok());
    }
}

#[test]
fn child_budgets_spell_lowercase() {
    for (mode, text) in [
        (ChildBudgets::Shared, "\"shared\""),
        (ChildBudgets::Separate, "\"separate\""),
    ] {
        assert_eq!(serde_json::to_string(&mode).unwrap(), text);
        assert_eq!(serde_json::from_str::<ChildBudgets>(text).unwrap(), mode);
    }
    assert!(serde_json::from_str::<ChildBudgets>("\"Shared\"").is_err());
}

#[test]
fn warnings_end_with_the_registry_line() {
    let depth = "Agents can launch agents without limit. A looping agent can start processes until the machine or your API rate limit runs out.";
    let agents = "Agents can launch any number of agents across all trees. A looping agent can start processes until the machine or your API rate limit runs out.";
    let registry = "agmend still stops at 256 agents.";
    assert!(DEPTH_OFF_WARNING.starts_with(depth));
    assert!(DEPTH_OFF_WARNING.ends_with(registry));
    assert!(AGENTS_OFF_WARNING.ends_with(registry));
    assert_eq!(DEPTH_OFF_WARNING, format!("{depth} {registry}"));
    assert_eq!(AGENTS_OFF_WARNING, format!("{agents} {registry}"));
    assert_eq!(
        BUDGETS_OFF_WARNING,
        "No spending caps. Agents spend until you stop them."
    );
}

#[test]
fn omitted_fields_default_and_unknown_fields_are_ignored() {
    let settings: Safeguards =
        serde_json::from_str(r#"{"limit_agents":false,"future_setting":3}"#).unwrap();
    assert_eq!(
        settings,
        Safeguards {
            limit_agents: false,
            ..Safeguards::default()
        }
    );
    assert_eq!(
        serde_json::from_str::<Safeguards>("{}").unwrap(),
        Safeguards::default()
    );
    assert_eq!(
        serde_json::from_value::<Safeguards>(serde_json::to_value(settings).unwrap()).unwrap(),
        settings
    );
}
