//! The vim mode setting: `vim_mode` in the `[keys]` table of agmen's
//! settings file, which the terminal UI reads too (spec 9.4).
// shortcut: the desktop app only records the choice; vim keys come with the
// terminal UI's keymap, and the desktop reads this setting when they do.

use std::path::Path;

use crate::appearance::{AppearanceError, read_settings_doc, write_settings_text};

/// The table this setting lives in.
const TABLE: &str = "keys";
/// The setting's key in [`TABLE`].
const KEY: &str = "vim_mode";

/// Whether keys work like vim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum VimMode {
    #[default]
    Off,
    On,
}

/// Flips vim mode in the settings file at `path`, keeping every other key,
/// table, comment and blank line, and returns the new mode. A missing file or
/// key counts as off.
///
/// # Errors
///
/// When the file cannot be read, parsed or written, or its `keys` is not a
/// table; a file that does not parse is left alone.
pub(crate) fn toggle_vim_mode(path: &Path) -> Result<VimMode, AppearanceError> {
    // shortcut: a write from agmend that lands between reading settings.toml and renaming the
    // temp file over it is lost; revisit with a lock file both writers take.
    let mut doc = read_settings_doc(path)?;
    match doc.get(TABLE) {
        Some(item) if item.is_table_like() => {}
        None => {
            doc.insert(TABLE, toml_edit::table());
        }
        // Replacing it would lose the person's value without a word.
        Some(other) => {
            return Err(AppearanceError::Parse {
                path: path.to_owned(),
                message: format!("`{TABLE}` is a {}, not a table", other.type_name()),
            });
        }
    }
    if let Some(table) = doc.get_mut(TABLE).and_then(toml_edit::Item::as_table_mut)
        && !table.is_dotted()
    {
        table.set_implicit(false);
    }
    let Some(keys) = doc
        .get_mut(TABLE)
        .and_then(toml_edit::Item::as_table_like_mut)
    else {
        return Err(AppearanceError::Serialize {
            path: path.to_owned(),
            message: format!("the [{TABLE}] table could not be made"),
        });
    };
    let on = keys.get(KEY).and_then(toml_edit::Item::as_bool) != Some(true);
    match keys.get_mut(KEY).and_then(toml_edit::Item::as_value_mut) {
        // Keeps the comments around the old value.
        Some(old) => {
            let decor = old.decor().clone();
            *old = toml_edit::Value::from(on);
            *old.decor_mut() = decor;
        }
        None => {
            keys.insert(KEY, toml_edit::value(on));
        }
    }
    write_settings_text(path, &doc.to_string())?;
    Ok(if on { VimMode::On } else { VimMode::Off })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn vim_mode_toggle_records_the_choice_and_keeps_other_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agmen/settings.toml");
        assert_eq!(toggle_vim_mode(&path).unwrap(), VimMode::On);
        fs::write(
            &path,
            "[appearance]\ntheme = \"One Dark\"\n\n[keys]\nvim_mode = true\nleader = \"space\"\n",
        )
        .unwrap();
        assert_eq!(toggle_vim_mode(&path).unwrap(), VimMode::Off);
        let saved: toml::Table = fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(saved["appearance"]["theme"].as_str(), Some("One Dark"));
        assert_eq!(saved["keys"]["leader"].as_str(), Some("space"));
        assert_eq!(saved["keys"]["vim_mode"].as_bool(), Some(false));
    }

    #[test]
    fn vim_mode_leaves_a_broken_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        fs::write(&path, "not = [toml").unwrap();
        assert!(matches!(
            toggle_vim_mode(&path),
            Err(AppearanceError::Parse { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "not = [toml");
    }

    #[test]
    fn vim_mode_refuses_a_keys_value_that_is_not_a_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        fs::write(&path, "keys = \"x\"\n").unwrap();
        assert!(matches!(
            toggle_vim_mode(&path),
            Err(AppearanceError::Parse { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "keys = \"x\"\n");
    }

    #[test]
    fn vim_toggle_keeps_comments_and_other_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let initial = "# hand-edited agmen settings\n\
[desktop]\n\
# last active checkout\n\
last_project = \"p1\"\n\
\n\
[keys]\n\
vim_mode = false # modal editing\n\
leader = \"space\"\n\
\n\
[listener.p1]\n\
# triage model and daily cap\n\
model = \"claude-haiku-5-5\"\n\
\n\
daily_usd = \"25.00\"\n\
reviews = \"on\"\n\
\n\
[appearance]\n\
follow_zed = false\n\
theme = \"One Dark\"\n";
        fs::write(&path, initial).unwrap();

        assert_eq!(toggle_vim_mode(&path).unwrap(), VimMode::On);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            initial.replace("vim_mode = false", "vim_mode = true")
        );
    }
}
