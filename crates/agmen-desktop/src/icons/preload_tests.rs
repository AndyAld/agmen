use std::path::{Path, PathBuf};

use gpui::{
    Context, ImgResourceLoader, IntoElement, ParentElement as _, Render, Resource, TestAppContext,
    Window, div, px,
};

use super::*;
use crate::appearance::IconThemeChoice;
use crate::backend::chat_snapshot;
use crate::icons::{FileIcon, UiIcon};

/// The test icon theme: a few Material mappings with tiny SVGs.
fn icons() -> FileIcons {
    let ext = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zed-ext");
    let choice = IconThemeChoice::Zed("Material Icon Theme".into());
    let (icons, warning) = FileIcons::new(&choice, &ext);
    assert!(warning.is_none(), "{warning:?}");
    icons
}

fn svg_of(icon: &FileIcon) -> Option<PathBuf> {
    match icon {
        FileIcon::Svg(path) => Some(path.clone()),
        _ => None,
    }
}

/// Draws one file icon the way the trees and tabs do.
struct OneIcon(FileIcon);

impl Render for OneIcon {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child(self.0.render(gpui::black(), px(16.)))
    }
}

/// Whether drawing `icon` once leaves gpui loading `svg` in the background,
/// so the first frame draws an empty slot.
fn first_frame_waits_for(icon: FileIcon, svg: &Path, cx: &mut TestAppContext) -> bool {
    let (_, cx) = cx.add_window_view(|_, _| OneIcon(icon));
    cx.update(|window, cx| drop(window.draw(cx)));
    cx.update(|_, cx| cx.has_asset::<ImgResourceLoader>(&Resource::Path(svg.into())))
}

#[gpui::test]
fn preloaded_theme_icons_draw_on_the_first_frame(cx: &mut TestAppContext) {
    let icons = icons();
    let path = Path::new("main.rs");
    let before = icons.icon(path, false, false);
    let svg = svg_of(&before).unwrap();
    assert!(first_frame_waits_for(before, &svg, cx));

    let renderer = cx.update(|cx| cx.svg_renderer());
    icons.preload([("main.rs", false), ("src", true)], &renderer);
    let after = icons.icon(path, false, false);
    assert!(matches!(after, FileIcon::Rendered(_)), "{after:?}");
    for open in [false, true] {
        let folder = icons.icon(Path::new("src"), true, open);
        assert!(matches!(folder, FileIcon::Rendered(_)), "{folder:?}");
    }
    let mut fresh = TestAppContext::single();
    assert!(!first_frame_waits_for(after, &svg, &mut fresh));
}

#[gpui::test]
fn preload_stops_at_its_limit(cx: &mut TestAppContext) {
    let icons = icons();
    let renderer = cx.update(|cx| cx.svg_renderer());
    icons.preload_up_to([("main.rs", false), ("Cargo.lock", false)], &renderer, 1);
    let rendered = ["main.rs", "Cargo.lock"]
        .iter()
        .filter(|name| {
            matches!(
                icons.icon(Path::new(name), false, false),
                FileIcon::Rendered(_)
            )
        })
        .count();
    assert_eq!(rendered, 1);
}

#[test]
fn lucide_and_system_choices_preload_nothing() {
    let (icons, _) = FileIcons::new(&IconThemeChoice::Lucide, Path::new(""));
    let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(crate::assets::Assets));
    icons.preload([("main.rs", false)], &renderer);
    assert!(matches!(
        icons.icon(Path::new("main.rs"), false, false),
        FileIcon::Ui(UiIcon::File)
    ));
}

#[test]
fn snapshot_entries_cover_trees_tabs_and_the_preview() {
    let entries = snapshot_entries(&chat_snapshot());
    for (name, is_dir) in [
        // The Project tree's files and folders, and the Git panel's changes.
        ("main.rs", false),
        ("tabs.rs", false),
        ("Cargo.lock", false),
        ("src", true),
        ("ui", true),
        ("tests", true),
        // Role file and settings file tabs, and the Roles tab.
        ("reviewer.xml", false),
        ("settings.toml", false),
        // Settings' icon preview.
        ("release.yml", false),
        // The theme's defaults, such as the tree's root folder.
        ("", true),
        ("", false),
    ] {
        assert!(
            entries.iter().any(|(n, d)| n == name && *d == is_dir),
            "{name} {is_dir}"
        );
    }
    let mut unique = entries.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), entries.len(), "each name once");
}
