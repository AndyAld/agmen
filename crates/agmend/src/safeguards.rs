//! Reads the person's agent safeguard settings.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use agmen_local::ApiError;
use agmen_local::safeguards::{Safeguards, TABLE};

const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// How many unknown `[agents]` keys one read names.
const MAX_UNKNOWN_REPORTED: usize = 16;

/// The `[agents]` keys `Safeguards` reads; any other key is reported and
/// ignored. A test keeps this in step with the type.
const KNOWN_KEYS: [&str; 7] = [
    "limit_tree_depth",
    "max_tree_depth",
    "limit_agents",
    "max_agents",
    "budgets",
    "child_budgets",
    "separate_child_usd",
];

fn bad(reason: impl std::fmt::Display) -> ApiError {
    crate::console::bad_request(format!("settings.toml [agents]: {reason}"))
}

#[cfg(test)]
pub(crate) fn load(path: Option<&Path>) -> Result<Safeguards, ApiError> {
    load_report(path).map(|(settings, _)| settings)
}

pub(crate) fn load_report(path: Option<&Path>) -> Result<(Safeguards, Option<String>), ApiError> {
    let Some(path) = path else {
        return Ok((Safeguards::default(), None));
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok((Safeguards::default(), None));
        }
        Err(error) => return Err(bad(error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(bad)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(bad("settings file exceeds 1 MiB"));
    }
    let text = String::from_utf8(bytes).map_err(bad)?;
    let mut document: toml::Table = toml::from_str(&text).map_err(bad)?;
    let Some(table) = document.remove(TABLE) else {
        return Ok((Safeguards::default(), None));
    };
    if !table.is_table() {
        return Err(bad("expected a table"));
    }
    let warnings: Vec<_> = table
        .as_table()
        .into_iter()
        .flat_map(|table| table.keys())
        .filter(|key| !KNOWN_KEYS.contains(&key.as_str()))
        .take(MAX_UNKNOWN_REPORTED)
        .map(|key| format!("unknown setting {key} in [agents], ignored"))
        .collect();
    let settings: Safeguards = table.try_into().map_err(bad)?;
    settings.validate().map_err(bad)?;
    Ok((
        settings,
        (!warnings.is_empty()).then(|| warnings.join("; ")),
    ))
}

#[cfg(test)]
#[path = "safeguards_tests.rs"]
mod tests;
