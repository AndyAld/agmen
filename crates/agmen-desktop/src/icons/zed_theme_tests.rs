use std::path::{Path, PathBuf};

use super::*;

fn ext_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zed-ext")
}

fn theme() -> IconTheme {
    IconTheme::load(&ext_dir(), "Material Icon Theme").unwrap()
}

fn name(path: Option<PathBuf>) -> String {
    path.unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn material_theme_found_by_name() {
    assert!(IconTheme::load(&ext_dir(), "Material Icon Theme").is_ok());
    assert!(IconTheme::load(&ext_dir(), "No Such Icons").is_err());
}

#[test]
fn icon_theme_stem_beats_suffix() {
    let t = theme();
    assert_eq!(name(t.file("Cargo.lock")), "lock.svg");
    assert_eq!(name(t.file("Cargo.toml")), "toml.svg");
    assert_eq!(name(t.file("main.rs")), "rust.svg");
}

#[test]
fn icon_theme_longest_suffix_wins() {
    // "tar.gz" beats "gz"; and gz.svg does not exist, so plain .gz falls to the default.
    assert_eq!(name(theme().file("logs.tar.gz")), "tgz.svg");
    assert_eq!(name(theme().file("x.gz")), "file.svg");
}

#[test]
fn icon_theme_named_folder_open_and_closed() {
    let t = theme();
    assert_eq!(name(t.folder("src", false)), "folder-src.svg");
    assert_eq!(name(t.folder("src", true)), "folder-src-open.svg");
    assert_eq!(name(t.folder("docs", true)), "folder-open.svg");
}

#[test]
fn icon_theme_missing_file_falls_back() {
    // An unknown type gets the theme's default icon.
    assert_eq!(name(theme().file("README.weird")), "file.svg");
}

#[test]
fn icon_paths_stay_inside_the_theme() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("evil/icon_themes");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("t.json"),
        r#"{"themes":[{"name":"Evil","appearance":"dark",
            "file_icons":{"default":{"path":"../../../../etc/hosts"}}}]}"#,
    )
    .unwrap();
    let t = IconTheme::load(dir.path(), "Evil").unwrap();
    assert_eq!(t.file("x.txt"), None);
}

#[test]
fn every_ui_icon_asset_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    for icon in UiIcon::ALL {
        assert!(root.join(icon.path()).is_file(), "{}", icon.path());
        assert!(
            crate::assets::Assets::get(icon.path()).is_some(),
            "{}",
            icon.path()
        );
    }
}
