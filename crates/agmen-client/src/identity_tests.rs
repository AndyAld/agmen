//! Each test makes a git repository in a temporary folder.

use std::process::Command;

use tempfile::TempDir;

use super::*;

const JSON: &str = r#"{"tenant":"tenant01","project":"project1","identity_id":"idn7k2q9",
  "handle":"andy/sol","owner_id":"andyandy","hub_url":"https://hub.example.com",
  "broker_url":"wss://agmen.example.com/mqtt","role":"engineer"}"#;

#[test]
fn hub_url_policy_identity_file() {
    let (_tmp, root) = repo();
    let path = join(&root, &root);
    for hub in [
        "http://localhost",
        "http://127.0.0.1",
        "http://127.2.3.4",
        "http://[::1]",
        "https://hub.example",
    ] {
        fs::write(&path, JSON.replace("https://hub.example.com", hub)).unwrap();
        assert_eq!(
            find_identity(&root).unwrap().unwrap().identity.hub_url,
            hub.parse::<Url>().unwrap()
        );
    }
    for hub in ["http://hub.example", "ftp://localhost"] {
        fs::write(&path, JSON.replace("https://hub.example.com", hub)).unwrap();
        let error = find_identity(&root).expect_err("unsafe identity hub");
        assert!(format!("{error:?}").contains("use https"), "{error:?}");
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A new repository whose real path is the returned root.
fn repo() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap().join("work");
    fs::create_dir(&root).unwrap();
    run_git(&root, &["init", "-q"]);
    (dir, root)
}

/// Writes `.agmen/identity.json` under `dir` and, as `agmen join` does,
/// excludes `.agmen/` in `root`'s repository.
fn join(root: &Path, dir: &Path) -> PathBuf {
    fs::create_dir_all(dir.join(IDENTITY_DIR)).unwrap();
    let path = dir.join(IDENTITY_DIR).join(IDENTITY_FILE);
    fs::write(&path, JSON).unwrap();
    fs::write(root.join(".git/info/exclude"), ".agmen/\n").unwrap();
    path
}

fn why(result: Result<Option<Joined>, IdentityError>) -> String {
    match result {
        Err(IdentityError::Untrusted { why, .. }) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn trusts_an_excluded_file_at_the_root() {
    let (_tmp, root) = repo();
    join(&root, &root);
    let deep = root.join("src/deep");
    fs::create_dir_all(&deep).unwrap();
    for cwd in [&root, &deep] {
        let joined = find_identity(cwd).unwrap().unwrap();
        assert_eq!(joined.root, root);
        assert_eq!(joined.identity.identity_id.as_str(), "idn7k2q9");
        assert_eq!(joined.identity.hub_url.as_str(), "https://hub.example.com/");
        assert_eq!(
            joined.identity.broker_url.to_string(),
            "wss://agmen.example.com/mqtt"
        );
        assert_eq!(joined.identity.me().owner_id.as_str(), "andyandy");
    }
}

#[test]
fn not_joined_without_a_file_or_outside_git() {
    let (tmp, root) = repo();
    assert!(find_identity(&root).unwrap().is_none());
    // The temporary folder above the repository is not a worktree.
    assert!(find_identity(tmp.path()).unwrap().is_none());
}

#[test]
fn refuses_a_file_git_does_not_ignore() {
    let (_tmp, root) = repo();
    join(&root, &root);
    fs::write(root.join(".git/info/exclude"), "").unwrap();
    assert!(why(find_identity(&root)).contains("git does not ignore it"));
}

#[test]
fn refuses_a_file_below_the_root() {
    let (_tmp, root) = repo();
    let sub = root.join("sub");
    join(&root, &sub);
    assert!(why(find_identity(&sub)).contains("not at the worktree root"));
}

#[cfg(unix)]
#[test]
fn refuses_links() {
    use std::os::unix::fs::symlink;

    let (tmp, root) = repo();
    // The folder is a link to a real folder elsewhere.
    let elsewhere = tmp.path().join("elsewhere");
    join(&root, &elsewhere);
    symlink(elsewhere.join(IDENTITY_DIR), root.join(IDENTITY_DIR)).unwrap();
    assert!(why(find_identity(&root)).contains(".agmen is a symbolic link"));

    // The file is a link.
    fs::remove_file(root.join(IDENTITY_DIR)).unwrap();
    fs::create_dir(root.join(IDENTITY_DIR)).unwrap();
    symlink(
        elsewhere.join(IDENTITY_DIR).join(IDENTITY_FILE),
        root.join(IDENTITY_DIR).join(IDENTITY_FILE),
    )
    .unwrap();
    assert!(why(find_identity(&root)).contains("identity.json is a symbolic link"));
}

/// On a case-insensitive file system (APFS by default), a committed
/// `.AGMEN/identity.json` opens as `.agmen/identity.json`, and a committed
/// `.gitignore` naming `.agmen/` makes check-ignore pass. The stored-name
/// check refuses it.
#[test]
fn refuses_a_case_alias() {
    let (_tmp, root) = repo();
    fs::create_dir(root.join(".AGMEN")).unwrap();
    fs::write(root.join(".AGMEN").join(IDENTITY_FILE), JSON).unwrap();
    fs::write(root.join(".gitignore"), ".agmen/\n").unwrap();
    if fs::symlink_metadata(root.join(IDENTITY_DIR).join(IDENTITY_FILE)).is_err() {
        // A case-sensitive file system: nothing is found, which is safe.
        assert!(find_identity(&root).unwrap().is_none());
        return;
    }
    assert!(why(find_identity(&root)).contains("no entry named exactly .agmen"));
}

#[test]
fn reports_a_malformed_file() {
    let (_tmp, root) = repo();
    let path = join(&root, &root);
    fs::write(&path, r#"{"tenant":"tenant01"}"#).unwrap();
    assert!(matches!(
        find_identity(&root),
        Err(IdentityError::Parse { .. })
    ));
}

/// `JSON` with `broker_url` set to `broker`, or removed for `None`.
fn with_broker(broker: Option<&str>) -> serde_json::Result<IdentityFile> {
    let mut file: serde_json::Value = serde_json::from_str(JSON).unwrap();
    match broker {
        Some(broker) => file["broker_url"] = broker.into(),
        None => {
            file.as_object_mut().unwrap().remove("broker_url");
        }
    }
    serde_json::from_value(file)
}

#[test]
fn broker_url_takes_mqtt_mqtts_and_wss() {
    for good in [
        "mqtt://localhost:1883",
        "mqtts://agmen.example.com:8883",
        "wss://agmen.example.com/mqtt",
        "wss://agmen.example.com:8443/mqtt",
    ] {
        let identity = with_broker(Some(good)).unwrap();
        assert_eq!(identity.broker_url.to_string(), good);
        // agmen join writes the file with the same value.
        assert_eq!(serde_json::to_value(&identity).unwrap()["broker_url"], good);
    }
}

#[test]
fn broker_url_is_required_and_refused_at_parse_time() {
    assert!(with_broker(None).is_err(), "the field is required");
    for bad in [
        "",
        "https://agmen.example.com",
        "ws://agmen.example.com/mqtt",
        "tcp://localhost:1883",
        "mqtt://localhost",
        "mqtts://agmen.example.com",
        "localhost:1883",
    ] {
        let err = with_broker(Some(bad)).unwrap_err();
        assert!(err.to_string().contains("broker address"), "{bad:?}: {err}");
    }
}
