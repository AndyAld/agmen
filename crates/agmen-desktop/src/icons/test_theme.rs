//! The icon theme tests draw with.

use std::path::Path;

use super::FileIcons;
use crate::appearance::IconThemeChoice;

/// A few Material Icon Theme mappings with tiny SVGs
/// (`tests/fixtures/zed-ext`).
pub(crate) fn material_icons() -> FileIcons {
    let ext = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zed-ext");
    let choice = IconThemeChoice::Zed("Material Icon Theme".into());
    let (icons, warning) = FileIcons::new(&choice, &ext);
    assert!(warning.is_none(), "{warning:?}");
    icons
}
