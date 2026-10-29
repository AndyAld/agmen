use std::path::PathBuf;

use agmen_local::ErrorCode;
use agmen_local::safeguards::{ChildBudgets, Safeguards};

use super::load;

fn file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    (dir, path)
}

#[test]
fn missing_file_or_table_gives_defaults() {
    let (_dir, path) = file();
    assert_eq!(load(None).unwrap(), Safeguards::default());
    assert_eq!(load(Some(&path)).unwrap(), Safeguards::default());
    assert!(!path.exists());
    for text in ["", "[appearance]\ntheme = 'dark'\n", "[agents]\n"] {
        std::fs::write(&path, text).unwrap();
        assert_eq!(load(Some(&path)).unwrap(), Safeguards::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
}

#[test]
fn agents_table_is_read() {
    let (_dir, path) = file();
    std::fs::write(
        &path,
        "[agents]\nlimit_tree_depth = false\nmax_agents = 3\nseparate_child_usd = 2.5\n",
    )
    .unwrap();
    assert_eq!(
        load(Some(&path)).unwrap(),
        Safeguards {
            limit_tree_depth: false,
            max_agents: 3,
            separate_child_usd: 2.5,
            ..Safeguards::default()
        }
    );
    std::fs::write(&path, "[agents]\nlimit_tree_depth = false\nmax_tree_depth = 2\nlimit_agents = false\nmax_agents = 5\nbudgets = false\nchild_budgets = 'separate'\nseparate_child_usd = 1.25\nfuture_setting = true\n").unwrap();
    assert_eq!(
        load(Some(&path)).unwrap(),
        Safeguards {
            limit_tree_depth: false,
            max_tree_depth: 2,
            limit_agents: false,
            max_agents: 5,
            budgets: false,
            child_budgets: ChildBudgets::Separate,
            separate_child_usd: 1.25,
        }
    );
}

#[test]
fn other_tables_are_ignored() {
    let (_dir, path) = file();
    let text = "[appearance]\nbudgets = 'yes'\n[agents]\nmax_agents = 3\n[listener.project1]\nmax_agents = 0\n";
    std::fs::write(&path, text).unwrap();
    assert_eq!(
        load(Some(&path)).unwrap(),
        Safeguards {
            max_agents: 3,
            ..Safeguards::default()
        }
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn invalid_value_is_refused_not_defaulted() {
    let (_dir, path) = file();
    for text in [
        "[agents]\nmax_agents = 0\n",
        "[agents]\nbudgets = 'yes'\n",
        "[broken\n",
        "agents = false\n",
        "[[agents]]\nmax_agents = 3\n",
        "[agents]\nmax_tree_depth = 257\n",
        "[agents]\nseparate_child_usd = nan\n",
        "[agents]\nchild_budgets = 'unknown'\n",
        "[agents]\nlimit_agents = false\nmax_agents = 0\n",
        "[agents]\nbudgets = false\nseparate_child_usd = 0.0\n",
    ] {
        std::fs::write(&path, text).unwrap();
        let error = load(Some(&path)).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest, "{text}");
        assert!(
            error.message.starts_with("settings.toml [agents]: "),
            "{error:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
}

#[test]
fn agents_must_be_a_table() {
    let (_dir, path) = file();
    for text in [
        "agents = []\n",
        "agents = [true, 4, true, 16, true, 'shared', 5.0]\n",
    ] {
        std::fs::write(&path, text).unwrap();
        let error = load(Some(&path)).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest, "{text}");
        assert!(error.message.starts_with("settings.toml [agents]: "));
    }
}

#[test]
fn oversized_file_is_refused() {
    let (_dir, path) = file();
    let limit = 1024 * 1024;
    let text = format!("#{}", "x".repeat(limit - 1));
    std::fs::write(&path, &text).unwrap();
    assert_eq!(load(Some(&path)).unwrap(), Safeguards::default());
    std::fs::write(&path, format!("{text}x")).unwrap();
    let error = load(Some(&path)).unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(error.message.starts_with("settings.toml [agents]: "));
    assert!(error.message.contains("1 MiB"), "{error:?}");
}

#[test]
fn unreadable_file_and_invalid_utf8_are_refused() {
    let (dir, path) = file();
    let error = load(Some(dir.path())).unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(error.message.starts_with("settings.toml [agents]: "));
    std::fs::write(&path, b"# invalid utf8\n\xff").unwrap();
    let error = load(Some(&path)).unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(error.message.starts_with("settings.toml [agents]: "));
}

#[test]
fn config_new_leaves_settings_path_unset() {
    let (dir, path) = file();
    let config = crate::Config::new(
        dir.path().join("agmend.sock"),
        dir.path().join("login.json"),
        agmen_client::TokenStore::Dir(dir.path().join("tokens")),
    );
    assert_eq!(config.settings_file, None);
    assert_eq!(
        load(config.settings_file.as_deref()).unwrap(),
        Safeguards::default()
    );
    assert!(!path.exists());
}
