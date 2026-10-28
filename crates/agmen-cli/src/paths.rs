//! Where agmen keeps its files on this machine: agmen-client's paths
//! (`config_dir`, `state_dir`, `login_file`), as anyhow results.
//!
//! Tokens are not here: they go through agmen-client's token store.

use std::path::PathBuf;

use anyhow::Result;

/// The state folder that holds one folder per identity.
pub(crate) fn state_base() -> Result<PathBuf> {
    Ok(agmen_client::state_dir()?)
}

/// The login file.
pub(crate) fn login_file() -> Result<PathBuf> {
    Ok(agmen_client::login_file()?)
}
