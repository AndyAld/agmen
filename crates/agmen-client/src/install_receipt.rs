//! The install receipt shared by channel ownership checks.

use std::io::Read as _;
use std::path::{Path, PathBuf};

/// An installer's version and the absolute paths it owns. Contains no credentials.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallReceipt {
    /// The installation channel, for example `shell`.
    pub channel: String,
    /// Installed release version.
    pub version: String,
    /// Absolute paths owned by that channel.
    pub owned_paths: Vec<PathBuf>,
}

/// A receipt could not be safely read. Callers must block ownership changes.
#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    /// The state directory could not be located.
    #[error(transparent)]
    Path(#[from] crate::PathError),
    /// Reading failed.
    #[error("reading install receipt failed: {0}")]
    Io(#[from] std::io::Error),
    /// The receipt is malformed.
    #[error("invalid install receipt: {0}")]
    Json(#[from] serde_json::Error),
    /// Its size, fields or paths are invalid.
    #[error("invalid install receipt: {0}")]
    Invalid(&'static str),
}

/// The single receipt path, under the shared state directory.
///
/// # Errors
/// Returns the state-directory lookup error.
pub fn install_receipt_path() -> Result<PathBuf, crate::PathError> {
    Ok(crate::state_dir()?.join("install-receipt.json"))
}

/// Reads the fixed receipt. Absence is distinct from unreadable or invalid data.
///
/// # Errors
/// Returns [`ReceiptError`] when ownership cannot be established safely.
pub fn read_install_receipt() -> Result<Option<InstallReceipt>, ReceiptError> {
    read_install_receipt_at(&install_receipt_path()?)
}

/// Reads at most 64 KiB from an explicit receipt path, for isolated homes.
///
/// # Errors
/// Returns [`ReceiptError`] for unreadable files or invalid receipt data.
pub fn read_install_receipt_at(path: &Path) -> Result<Option<InstallReceipt>, ReceiptError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Err(ReceiptError::Invalid("expected a regular file"));
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(ReceiptError::Invalid("expected a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(ReceiptError::Invalid("exceeds 64 KiB"));
    }
    let receipt: InstallReceipt = serde_json::from_slice(&bytes)?;
    if receipt.channel.is_empty()
        || receipt.version.is_empty()
        || receipt.channel.chars().any(char::is_control)
        || receipt.version.chars().any(char::is_control)
        || receipt
            .owned_paths
            .iter()
            .any(|path| !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0))
    {
        return Err(ReceiptError::Invalid(
            "invalid channel, version or owned path",
        ));
    }
    Ok(Some(receipt))
}
