//! Reads the person's agent safeguard settings.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use agmen_local::ApiError;
use agmen_local::safeguards::{Safeguards, TABLE};

const MAX_FILE_BYTES: u64 = 1024 * 1024;

fn bad(reason: impl std::fmt::Display) -> ApiError {
    crate::console::bad_request(format!("settings.toml [agents]: {reason}"))
}

pub(crate) fn load(path: Option<&Path>) -> Result<Safeguards, ApiError> {
    let Some(path) = path else {
        return Ok(Safeguards::default());
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Safeguards::default()),
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
        return Ok(Safeguards::default());
    };
    if !table.is_table() {
        return Err(bad("expected a table"));
    }
    let settings: Safeguards = table.try_into().map_err(bad)?;
    settings.validate().map_err(bad)?;
    Ok(settings)
}

#[cfg(test)]
#[path = "safeguards_tests.rs"]
mod tests;
