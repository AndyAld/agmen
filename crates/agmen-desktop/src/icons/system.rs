//! File icons from the operating system, for people without Zed.
//!
//! macOS: Finder's icon for a file type through `NSWorkspace
//! iconForContentType:`, cut down to one 64 px representation and returned as
//! TIFF bytes, which GPUI decodes. Looking up by type, not by path, gives the
//! right icon for files that are not on this disk (fixtures, other people's
//! worktrees). Every call here is a safe fn in objc2-app-kit 0.3.2, so the
//! workspace's `unsafe_code = "forbid"` holds.
// shortcut: Windows (shell icons) and Linux (freedesktop theme) return None until S8 runs there.

/// What to look up an OS icon for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum IconKind {
    Folder,
    /// A file with this extension, lowercase, without the dot.
    Extension(String),
    /// A file with no extension.
    Plain,
}

/// The pixel width of the one representation kept.
const PIXELS: isize = 64;

/// The OS icon for `kind` as TIFF bytes: one 64 px bitmap, a few tens of KB.
/// Slow (tens of ms): call it off the UI thread.
#[cfg(target_os = "macos")]
pub(crate) fn icon_tiff(kind: &IconKind) -> Option<Vec<u8>> {
    use objc2::AnyThread as _;
    use objc2_app_kit::{NSImage, NSWorkspace};
    use objc2_foundation::{NSSize, NSString};
    use objc2_uniform_type_identifiers::UTType;

    let ut = match kind {
        IconKind::Folder => UTType::typeWithIdentifier(&NSString::from_str("public.folder")),
        IconKind::Extension(ext) => UTType::typeWithFilenameExtension(&NSString::from_str(ext)),
        IconKind::Plain => None,
    }
    .or_else(|| UTType::typeWithIdentifier(&NSString::from_str("public.data")))?;
    let full = NSWorkspace::sharedWorkspace().iconForContentType(&ut);
    // The icon holds up to 32 representations (16 to 1024 px); keep the
    // smallest one at least 64 px wide, or the largest there is.
    let reps = full.representations();
    let rep = reps
        .iter()
        .filter(|r| r.pixelsWide() >= PIXELS)
        .min_by_key(|r| r.pixelsWide())
        .or_else(|| reps.iter().max_by_key(|r| r.pixelsWide()))?;
    let small = NSImage::initWithSize(NSImage::alloc(), NSSize::new(32.0, 32.0));
    small.addRepresentation(&rep);
    small.TIFFRepresentation().map(|data| data.to_vec())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn icon_tiff(_kind: &IconKind) -> Option<Vec<u8>> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn finder_icons_differ_by_type_and_stay_small() {
        let rust = icon_tiff(&IconKind::Extension("rs".into())).unwrap();
        let folder = icon_tiff(&IconKind::Folder).unwrap();
        let plain = icon_tiff(&IconKind::Plain).unwrap();
        for tiff in [&rust, &folder, &plain] {
            assert!(tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*"));
            assert!(tiff.len() < 128 * 1024, "{} bytes", tiff.len());
        }
        assert_ne!(rust, folder);
        assert_ne!(folder, plain);
    }
}
