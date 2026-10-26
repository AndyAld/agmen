//! The vim mode setting: `vim_mode` in the `[keys]` table of agmen's
//! settings file, which the terminal UI reads too (spec 9.4).
// shortcut: the desktop app only records the choice; vim keys come with the
// terminal UI's keymap, and the desktop reads this setting when they do.

use std::path::Path;

use crate::appearance::{AppearanceError, read_settings_table, write_settings_table};

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
/// and returns the new mode. A missing file or key counts as off.
///
/// # Errors
///
/// When the file cannot be read, parsed or written; a file that does not
/// parse is left alone.
pub(crate) fn toggle_vim_mode(path: &Path) -> Result<VimMode, AppearanceError> {
    let mut table = read_settings_table(path)?.unwrap_or_default();
    let mut keys = match table.remove(TABLE) {
        Some(toml::Value::Table(keys)) => keys,
        _ => toml::Table::new(),
    };
    let on = !matches!(keys.get(KEY), Some(toml::Value::Boolean(true)));
    keys.insert(KEY.to_owned(), toml::Value::Boolean(on));
    table.insert(TABLE.to_owned(), toml::Value::Table(keys));
    write_settings_table(path, &table)?;
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
}
