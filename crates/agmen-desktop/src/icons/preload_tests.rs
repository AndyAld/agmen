use std::path::{Path, PathBuf};

use gpui::{
    Context, ImgResourceLoader, IntoElement, ParentElement as _, Render, Resource, TestAppContext,
    Window, div, px,
};

use super::*;
use crate::appearance::IconThemeChoice;
use crate::icons::test_theme::material_icons;
use crate::icons::{FileIcon, UiIcon};

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
    let icons = material_icons();
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
    let icons = material_icons();
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

// agmen-c7 fix-hosting-review minor 2: the limit is per call, so a later
// workspace still gets its icons once an earlier one filled the cache.
#[gpui::test]
fn the_limit_counts_each_call_on_its_own(cx: &mut TestAppContext) {
    let icons = material_icons();
    let renderer = cx.update(|cx| cx.svg_renderer());
    icons.preload_up_to([("main.rs", false)], &renderer, 1);
    icons.preload_up_to([("Cargo.lock", false)], &renderer, 1);
    for name in ["main.rs", "Cargo.lock"] {
        let icon = icons.icon(Path::new(name), false, false);
        assert!(matches!(icon, FileIcon::Rendered(_)), "{name}: {icon:?}");
    }
}

#[test]
fn lucide_and_system_choices_preload_nothing() {
    let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(crate::assets::Assets));
    for choice in [IconThemeChoice::Lucide, IconThemeChoice::System] {
        let (icons, _) = FileIcons::new(&choice, Path::new(""));
        icons.preload([("main.rs", false)], &renderer);
        let icon = icons.icon(Path::new("main.rs"), false, false);
        assert!(
            !matches!(icon, FileIcon::Rendered(_)),
            "{choice:?}: {icon:?}"
        );
        if choice == IconThemeChoice::Lucide {
            assert!(matches!(icon, FileIcon::Ui(UiIcon::File)), "{icon:?}");
        }
    }
}
