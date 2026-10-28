//! Paths within the installed macOS app bundle.

use std::path::{Path, PathBuf};

/// Whether `exe` sits inside a macOS app bundle (`X.app/Contents/MacOS/x`).
pub(crate) fn in_app_bundle(exe: &Path) -> bool {
    let mut parts = exe.components().rev().skip(1);
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(macos), Some(contents), Some(app))
            if macos.as_os_str() == "MacOS"
                && contents.as_os_str() == "Contents"
                && Path::new(app.as_os_str()).extension().is_some_and(|e| e == "app")
    )
}

/// The bundle and its two helpers; service setup consumes these in Task 6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bundle {
    pub app: PathBuf,
    pub agmend: PathBuf,
    pub agmen: PathBuf,
}

/// Locates only the desktop executable's bundle with regular helper files.
pub(crate) fn locate(exe: &Path) -> Option<Bundle> {
    if !in_app_bundle(exe) || exe.file_name()? != "agmen-desktop" {
        return None;
    }
    let macos = exe.parent()?;
    let agmend = macos.join("agmend");
    let agmen = macos.join("agmen");
    for helper in [&agmend, &agmen] {
        if !std::fs::symlink_metadata(helper)
            .ok()?
            .file_type()
            .is_file()
        {
            return None;
        }
    }
    Some(Bundle {
        app: macos.parent()?.parent()?.to_path_buf(),
        agmend,
        agmen,
    })
}

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;
