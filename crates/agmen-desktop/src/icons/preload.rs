//! Draws the file icons a window opens on in its first frame.
//!
//! `img()` loads a theme SVG on the background executor and repaints the
//! view when it arrives. macOS stops a window's display link while AppKit
//! reports the window occluded (covered, on another Space, or behind the
//! lock screen), so a window that opens there keeps its first frames and
//! icons that load afterwards never draw: `--screen` shots taken that way
//! had empty icon slots in the trees and tabs. Rendering a window's icons
//! before its first frame (`ui/icon_preload.rs` names them) puts them in
//! that frame; any other icon still loads in the background.

use gpui::SvgRenderer;

use super::FileIcons;

/// The most theme SVGs rendered ahead: about 0.4 ms each in a release
/// build and 10 ms in a debug build (Material icons, 2026-10-26).
const MAX_PRELOADED: usize = 128;

impl FileIcons {
    /// Renders now the theme SVG of each entry, a file or folder name and
    /// whether it is a folder (folders both open and closed), so views draw
    /// them without waiting. Does nothing without a Zed icon theme.
    pub(crate) fn preload<'a>(
        &self,
        entries: impl IntoIterator<Item = (&'a str, bool)>,
        renderer: &SvgRenderer,
    ) {
        self.preload_up_to(entries, renderer, MAX_PRELOADED);
    }

    pub(super) fn preload_up_to<'a>(
        &self,
        entries: impl IntoIterator<Item = (&'a str, bool)>,
        renderer: &SvgRenderer,
        limit: usize,
    ) {
        let Some(theme) = &self.theme else {
            return;
        };
        let mut rendered = self.rendered.borrow_mut();
        // The limit bounds this call's work, not the cache, so a later
        // workspace still gets its icons.
        let mut added = 0;
        for (name, is_dir) in entries {
            let svgs = if is_dir {
                [theme.folder(name, false), theme.folder(name, true)]
            } else {
                [theme.file(name), None]
            };
            for svg in svgs.into_iter().flatten() {
                if added >= limit {
                    return;
                }
                if rendered.contains_key(&svg) {
                    continue;
                }
                // An SVG that cannot be read or drawn here fails the same
                // way in `img()`, which draws nothing; leave it to that path.
                let image = std::fs::read(&svg)
                    .ok()
                    .and_then(|bytes| renderer.render_single_frame(&bytes, 1.0).ok());
                if let Some(image) = image {
                    rendered.insert(svg, image);
                    added += 1;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "preload_tests.rs"]
mod tests;
