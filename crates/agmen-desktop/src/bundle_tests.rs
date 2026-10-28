use crate::Launch;
use crate::bundle::*;
use std::path::{Path, PathBuf};

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let macos = dir.path().join("agmen.app/Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    for name in ["agmen-desktop", "agmend", "agmen"] {
        std::fs::write(macos.join(name), b"binary").unwrap();
    }
    (dir, macos.join("agmen-desktop"))
}

#[test]
fn locate_finds_helpers_beside_the_exe() {
    let (dir, exe) = fixture();
    let bundle = locate(&exe).unwrap();
    assert_eq!(bundle.app, dir.path().join("agmen.app"));
    assert_eq!(bundle.agmend, exe.with_file_name("agmend"));
    assert_eq!(bundle.agmen, exe.with_file_name("agmen"));
}

#[test]
fn locate_refuses_a_bare_binary() {
    assert!(locate(Path::new("/usr/local/bin/agmen-desktop")).is_none());
}

#[test]
fn locate_refuses_symlinked_helpers() {
    for name in ["agmend", "agmen"] {
        let (_dir, exe) = fixture();
        let helper = exe.with_file_name(name);
        std::fs::remove_file(&helper).unwrap();
        std::os::unix::fs::symlink(&exe, &helper).unwrap();
        assert!(locate(&exe).is_none());
    }
}

#[test]
fn locate_refuses_missing_helpers_directories_and_wrong_exe() {
    let (_dir, exe) = fixture();
    assert!(locate(&exe.with_file_name("agmend")).is_none());
    let helper = exe.with_file_name("agmen");
    std::fs::remove_file(&helper).unwrap();
    assert!(locate(&exe).is_none());
    std::fs::create_dir(&helper).unwrap();
    assert!(locate(&exe).is_none());
}

#[test]
fn version_flag_parses() {
    let launch = Launch::parse(&["--version".into()]).unwrap();
    assert_eq!(launch, Launch::Version);
    assert!(!launch.takes_instance_lock());
    assert_eq!(launch.fixture_screen(), None);
    assert!(Launch::parse(&["--version".into(), "extra".into()]).is_err());
}

#[test]
fn bundle_detection() {
    assert!(in_app_bundle(Path::new(
        "/Applications/agmen.app/Contents/MacOS/agmen-desktop"
    )));
    assert!(!in_app_bundle(Path::new(
        "/Users/a/agmen/target/debug/agmen-desktop"
    )));
    assert!(!in_app_bundle(Path::new(
        "/x/agmen.app/Contents/Resources/agmen-desktop"
    )));
    assert!(!in_app_bundle(Path::new("agmen-desktop")));
}

#[test]
fn bundle_scripts_build_sign_and_check_the_host_hub() {
    let build = include_str!("../../../scripts/macos-app.sh");
    let inspect = include_str!("../../../scripts/test-macos-app.sh");
    assert!(build.contains("-p agmen-cli -p agmen-hub"));
    assert!(build.contains("for package in agmend agmen-cli agmen-hub; do"));
    assert!(build.contains("for binary in agmen-desktop agmend agmen agmen-hub; do"));
    assert!(build.contains("for binary in agmend agmen agmen-hub; do"));
    assert!(inspect.contains("for binary in agmen-desktop agmend agmen agmen-hub; do"));
    assert!(inspect.contains("for binary in agmend agmen agmen-hub; do"));
    assert!(inspect.contains("\"$app/Contents/MacOS/agmen-hub\" --help"));
}
