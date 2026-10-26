//! File icons from the operating system, for people without Zed.
//!
//! macOS: Finder's icon through `NSWorkspace iconForFile:`, as TIFF bytes
//! that GPUI decodes. Every call here is a safe fn in objc2-app-kit 0.3.2,
//! so the workspace's `unsafe_code = "forbid"` holds.
// shortcut: Windows (shell icons) and Linux (freedesktop theme) return None until S8 runs there.

use std::path::Path;

/// The OS icon for the file or folder at `path`, as TIFF bytes.
#[cfg(target_os = "macos")]
pub(crate) fn icon_tiff(path: &Path) -> Option<Vec<u8>> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSSize, NSString};

    let path = NSString::from_str(path.to_str()?);
    let image = NSWorkspace::sharedWorkspace().iconForFile(&path);
    // Ask for a small rendition so the TIFF carries 32 px rather than 1024 px.
    image.setSize(NSSize::new(32.0, 32.0));
    image.TIFFRepresentation().map(|data| data.to_vec())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn icon_tiff(_path: &Path) -> Option<Vec<u8>> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn finder_gives_a_tiff_for_a_file_and_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        for path in [file.as_path(), dir.path()] {
            let tiff = icon_tiff(path).unwrap();
            // TIFF starts with II*\0 or MM\0*.
            assert!(
                tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*"),
                "{path:?}"
            );
        }
    }
}
