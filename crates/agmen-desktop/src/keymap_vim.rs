//! The vim mode setting: `vim_mode` in the `[keys]` table of agmen's
//! settings file, which the terminal UI reads too (spec 9.4).
// shortcut: the desktop app only records the choice; vim keys come with the
// terminal UI's keymap, and the desktop reads this setting when they do.

use std::fs;
use std::path::Path;

/// A settings file is small; anything larger is refused.
const MAX_SETTINGS_BYTES: u64 = 4 * 1024 * 1024;
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

/// What can go wrong reading or writing the setting.
#[derive(Debug, thiserror::Error)]
pub(crate) enum VimModeError {
    #[error("cannot read or write {}: {source}", path.display())]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("{} is {len} bytes, too large for a settings file", path.display())]
    TooLarge { path: std::path::PathBuf, len: u64 },
    #[error("cannot parse {}: {message}", path.display())]
    Parse {
        path: std::path::PathBuf,
        message: String,
    },
}

/// Flips vim mode in the settings file at `path`, keeping every other key,
/// and returns the new mode. A missing file or key counts as off.
///
/// # Errors
///
/// When the file cannot be read, parsed or written; a file that does not
/// parse is left alone.
pub(crate) fn toggle_vim_mode(path: &Path) -> Result<VimMode, VimModeError> {
    let io = |source| VimModeError::Io {
        path: path.to_owned(),
        source,
    };
    let mut table = match fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_SETTINGS_BYTES => {
            return Err(VimModeError::TooLarge {
                path: path.to_owned(),
                len: meta.len(),
            });
        }
        Ok(_) => fs::read_to_string(path)
            .map_err(io)?
            .parse::<toml::Table>()
            .map_err(|err| VimModeError::Parse {
                path: path.to_owned(),
                message: err.to_string(),
            })?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(err) => return Err(io(err)),
    };
    let mut keys = match table.remove(TABLE) {
        Some(toml::Value::Table(keys)) => keys,
        _ => toml::Table::new(),
    };
    let on = !matches!(keys.get(KEY), Some(toml::Value::Boolean(true)));
    keys.insert(KEY.to_owned(), toml::Value::Boolean(on));
    table.insert(TABLE.to_owned(), toml::Value::Table(keys));
    let text = toml::to_string_pretty(&table).map_err(|err| VimModeError::Parse {
        path: path.to_owned(),
        message: err.to_string(),
    })?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io)?;
    }
    // Through a temporary file and a rename, so a crash never leaves half a file.
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(io)?;
    fs::rename(&tmp, path).map_err(io)?;
    Ok(if on { VimMode::On } else { VimMode::Off })
}

#[cfg(test)]
mod tests {
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
            Err(VimModeError::Parse { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "not = [toml");
    }
}
