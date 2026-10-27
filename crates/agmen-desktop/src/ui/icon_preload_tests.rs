use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use gpui::{ImgResourceLoader, Resource, TestAppContext};

use super::*;
use crate::backend::{Backend as _, FixtureBackend, Screen, chat_snapshot};
use crate::icons::FileIcon;
use crate::icons::test_theme::material_icons;
use crate::ui::Workspace;
use crate::ui::test_support::init;

/// The theme SVGs `snapshot`'s names map to, folders open and closed.
fn theme_svgs(snapshot: &Snapshot) -> BTreeSet<PathBuf> {
    let icons = material_icons();
    let mut svgs = BTreeSet::new();
    for (name, is_dir) in snapshot_entries(snapshot) {
        for open in [false, true] {
            if let FileIcon::Svg(svg) = icons.icon(Path::new(&name), is_dir, open) {
                svgs.insert(svg);
            }
        }
    }
    svgs
}

// agmen-c7 fix-hosting-review: `Workspace::new` renders the icons ahead.
#[gpui::test]
fn a_workspace_draws_its_theme_icons_on_the_first_frame(cx: &mut TestAppContext) {
    init(cx);
    cx.update(|cx| cx.set_global(material_icons()));
    let screen = Screen::Project;
    let snapshot = FixtureBackend::for_screen(screen).unwrap().snapshot();
    let svgs = theme_svgs(&snapshot);
    let rust = material_icons().icon(Path::new("main.rs"), false, false);
    assert!(matches!(&rust, FileIcon::Svg(svg) if svgs.contains(svg)));

    let (_, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(snapshot, window, cx);
        workspace.show_screen(screen, cx);
        workspace
    });
    cx.update(|window, cx| drop(window.draw(cx)));
    let waiting: Vec<&PathBuf> = svgs
        .iter()
        .filter(|svg| {
            cx.update(|_, cx| {
                cx.has_asset::<ImgResourceLoader>(&Resource::Path(svg.as_path().into()))
            })
        })
        .collect();
    assert!(
        waiting.is_empty(),
        "loading after the first frame: {waiting:?}"
    );
}

// t19: a spec tab (ridgeline-design.md on the plan screens, with no
// worktree) drew an empty icon slot, because no preload named the plan's
// spec files. Every theme SVG any screen's first frame asks for is ready
// by then, so none is still loading after it.
#[gpui::test]
fn every_screen_draws_its_theme_icons_on_the_first_frame(cx: &mut TestAppContext) {
    init(cx);
    cx.update(|cx| cx.set_global(material_icons()));
    let icons_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/zed-ext/material-icon-theme/icons");
    let theme_svgs: Vec<PathBuf> = std::fs::read_dir(&icons_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let mut waiting = Vec::new();
    for screen in Screen::ALL {
        // Each screen starts with nothing rendered or loading, so one
        // screen's icons cannot cover another's.
        cx.update(|cx| {
            cx.set_global(material_icons());
            for svg in &theme_svgs {
                cx.remove_asset::<ImgResourceLoader>(&Resource::Path(svg.as_path().into()));
            }
        });
        let snapshot = FixtureBackend::for_screen(screen).unwrap().snapshot();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let mut workspace = Workspace::new(snapshot, window, cx);
            workspace.show_screen(screen, cx);
            workspace
        });
        cx.update(|window, cx| drop(window.draw(cx)));
        for svg in &theme_svgs {
            let loading = cx.update(|_, cx| {
                cx.has_asset::<ImgResourceLoader>(&Resource::Path(svg.as_path().into()))
            });
            if loading {
                waiting.push(format!("{screen:?}: {}", svg.display()));
            }
        }
    }
    assert!(
        waiting.is_empty(),
        "loading after the first frame: {waiting:?}"
    );
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

// agmen-c7 fix-hosting-review minor 2: past the preload limit, the names
// left out are the deepest, not the last in alphabetical order.
#[test]
fn shallow_names_come_before_deeper_ones() {
    let entries = snapshot_entries(&chat_snapshot());
    let at = |name: &str, is_dir: bool| {
        entries
            .iter()
            .position(|(n, d)| n == name && *d == is_dir)
            .unwrap_or_else(|| panic!("{name} {is_dir}"))
    };
    assert!(at("settings.toml", false) < at("src", true));
    assert!(at("src", true) < at("main.rs", false));
    assert!(at("tests", true) < at("tabs.rs", false));
}
