//! Read and save the person's agent safeguards without replacing other settings.

use std::path::Path;

use agmen_local::safeguards::{Safeguards, TABLE};

use super::{AppearanceError, read_settings_doc, write_settings_text};

/// Loads and validates `[agents]`; a missing file or table uses the defaults.
///
/// # Errors
///
/// Returns an error for unreadable, oversized or invalid settings.
pub(crate) fn load_safeguards(path: &Path) -> Result<Safeguards, AppearanceError> {
    safeguards_from_doc(path, &read_settings_doc(path)?)
}

fn safeguards_from_doc(
    path: &Path,
    doc: &toml_edit::DocumentMut,
) -> Result<Safeguards, AppearanceError> {
    #[derive(serde::Deserialize)]
    struct Settings {
        #[serde(default)]
        agents: Safeguards,
    }

    let parse = |message: String| AppearanceError::Parse {
        path: path.to_owned(),
        message,
    };
    let settings: Settings =
        toml::from_str(&doc.to_string()).map_err(|error| parse(error.to_string()))?;
    settings
        .agents
        .validate()
        .map_err(|error| parse(error.to_string()))?;
    Ok(settings.agents)
}

/// Saves the seven safeguard keys, preserving other settings and comments.
///
/// # Errors
///
/// Leaves the file untouched if its current safeguards or `value` are invalid.
/// Also returns an error if reading or writing the settings fails.
pub(crate) fn save_safeguards(path: &Path, value: &Safeguards) -> Result<(), AppearanceError> {
    let mut doc = read_settings_doc(path)?;
    safeguards_from_doc(path, &doc)?;
    let serialize = |message: String| AppearanceError::Serialize {
        path: path.to_owned(),
        message,
    };
    value
        .validate()
        .map_err(|error| serialize(error.to_string()))?;
    let ours = toml::Value::try_from(value).map_err(|error| serialize(error.to_string()))?;
    let Some(ours) = ours.as_table() else {
        return Err(serialize("safeguards must serialize as a table".into()));
    };
    let table = doc.entry(TABLE).or_insert_with(toml_edit::table);
    let Some(table) = table.as_table_like_mut() else {
        return Err(serialize(format!("[{TABLE}] must be a table")));
    };
    for (key, value) in ours {
        let value = value
            .to_string()
            .parse::<toml_edit::Value>()
            .map_err(|error| serialize(error.to_string()))?;
        match table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
            Some(old) => {
                let decor = old.decor().clone();
                *old = value;
                *old.decor_mut() = decor;
            }
            None => {
                table.insert(key, toml_edit::value(value));
            }
        }
    }
    write_settings_text(path, &doc.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use agmen_local::safeguards::{ChildBudgets, Safeguards};

    use super::*;

    #[test]
    fn save_keeps_other_tables_and_comments() {
        for agents in [
            "[agents] # safeguards\n# depth\nmax_tree_depth = 4 # levels\nfuture_key = 99 # keep me\n",
            "agents = { max_tree_depth = 4, future_key = 99 } # inline\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("settings.toml");
            let unrelated = "# appearance\n[appearance]\nfollow_zed = false # keep\n[desktop]\nstart_at_login = true\n[tui]\nkeys = \"vi\"\n";
            let original = format!("# settings\n{agents}{unrelated}");
            fs::write(&path, &original).unwrap();
            let safeguards = Safeguards {
                limit_tree_depth: false,
                max_tree_depth: 8,
                limit_agents: false,
                max_agents: 32,
                budgets: false,
                child_budgets: ChildBudgets::Separate,
                separate_child_usd: 2.5,
            };
            save_safeguards(&path, &safeguards).unwrap();
            let saved = fs::read_to_string(&path).unwrap();
            assert!(saved.starts_with("# settings\n"));
            assert!(saved.contains(unrelated), "{saved}");
            assert!(saved.contains("future_key = 99"), "{saved}");
            if agents.starts_with('[') {
                for comment in ["# safeguards", "# depth", "# levels", "# keep me"] {
                    assert!(saved.contains(comment), "{saved}");
                }
            } else {
                assert!(saved.contains("# inline"), "{saved}");
            }
            assert_eq!(load_safeguards(&path).unwrap(), safeguards);
            let document: toml::Table = toml::from_str(&saved).unwrap();
            assert_eq!(document["agents"].as_table().unwrap().len(), 8);
        }
    }

    #[test]
    fn save_adds_the_table_to_a_file_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let original = "# mine\n[tui]\nkeys = \"vi\" # keep";
        fs::write(&path, original).unwrap();
        let safeguards = Safeguards {
            budgets: false,
            ..Safeguards::default()
        };
        save_safeguards(&path, &safeguards).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with(original), "{saved}");
        assert_eq!(load_safeguards(&path).unwrap(), safeguards);
    }

    #[test]
    fn invalid_agents_table_is_an_error_and_the_file_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        assert_eq!(load_safeguards(&path).unwrap(), Safeguards::default());
        assert!(!path.exists());
        for original in [
            "agents = 1\n",
            "[agents]\nlimit_agents = \"no\"\n",
            "[agents]\nmax_tree_depth = 0\n",
            "[agents]\nmax_agents = 257\n",
            "[agents]\nchild_budgets = \"other\"\n",
            "[agents]\nseparate_child_usd = 0\n",
            "[agents]\nseparate_child_usd = nan\n",
            "[agents\n",
        ] {
            fs::write(&path, original).unwrap();
            assert!(load_safeguards(&path).is_err(), "{original}");
            assert!(
                save_safeguards(&path, &Safeguards::default()).is_err(),
                "{original}"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
        fs::write(&path, "# keep\n[tui]\nkeys = \"vi\"\n").unwrap();
        let invalid = Safeguards {
            separate_child_usd: 0.0,
            ..Safeguards::default()
        };
        assert!(save_safeguards(&path, &invalid).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# keep\n[tui]\nkeys = \"vi\"\n"
        );
    }
}
