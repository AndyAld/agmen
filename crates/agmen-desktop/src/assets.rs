//! Files built into the binary: Lucide's UI icons and their license.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

macro_rules! lucide {
    ($($name:literal),* $(,)?) => {
        &[$((
            concat!("icons/lucide/", $name, ".svg"),
            include_bytes!(concat!("../assets/icons/lucide/", $name, ".svg")) as &[u8],
        )),*]
    };
}

/// Every built-in asset by its path under `assets/`.
static FILES: &[(&str, &[u8])] = lucide![
    "bell",
    "check",
    "chevron-down",
    "chevron-left",
    "chevron-right",
    "circle-alert",
    "corner-down-left",
    "diff",
    "file",
    "folder",
    "folder-open",
    "folder-tree",
    "git-branch",
    "messages-square",
    "minus",
    "square",
    "copy",
    "panel-right",
    "pencil",
    "plus",
    "search",
    "settings",
    "sparkle",
    "square-terminal",
    "user",
    "users",
    "x",
];

macro_rules! fonts {
    ($($path:literal),* $(,)?) => {
        &[$(include_bytes!(concat!("../assets/fonts/", $path)) as &[u8]),*]
    };
}

/// Zed's default fonts, which Zed itself bundles: IBM Plex Sans (`.ZedSans`)
/// and Lilex (`.ZedMono`), both under the SIL Open Font License 1.1, whose
/// text ships next to them. GPUI maps the dotted names to these families.
static FONTS: &[&[u8]] = fonts![
    "ibm-plex-sans/IBMPlexSans-Regular.ttf",
    "ibm-plex-sans/IBMPlexSans-Italic.ttf",
    "ibm-plex-sans/IBMPlexSans-SemiBold.ttf",
    "ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf",
    "lilex/Lilex-Regular.ttf",
    "lilex/Lilex-Italic.ttf",
    "lilex/Lilex-Bold.ttf",
    "lilex/Lilex-BoldItalic.ttf",
];

/// Registers the bundled fonts with GPUI's text system.
///
/// # Errors
///
/// When the text system rejects a font file.
pub(crate) fn load_fonts(cx: &gpui::App) -> gpui::Result<()> {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect())
}

/// Lucide's ISC license, which must ship with the icons (shown in About).
#[expect(
    dead_code,
    reason = "Settings About shows it in Task 17 of the desktop plan"
)]
pub(crate) const LUCIDE_LICENSE: &str = include_str!("../assets/icons/LICENSE-lucide.txt");

/// The built-in assets, for GPUI's `svg()` paths.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Assets;

impl Assets {
    pub(crate) fn get(path: &str) -> Option<&'static [u8]> {
        FILES
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| *bytes)
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(Self::get(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(FILES
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_fonts_are_truetype() {
        assert_eq!(FONTS.len(), 8);
        for font in FONTS {
            assert!(font.starts_with(&[0, 1, 0, 0]), "not a TrueType file");
        }
    }
}
