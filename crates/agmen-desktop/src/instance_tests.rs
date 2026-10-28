use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::*;

const WAIT: Duration = Duration::from_secs(5);

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn first_acquire_is_primary() {
    let dir = tempfile::tempdir().unwrap();
    let Instance::Primary(_listener) = acquire(dir.path(), &[]).unwrap() else {
        panic!("the first instance should be primary");
    };
    let mode = std::fs::metadata(dir.path().join(SOCKET))
        .unwrap()
        .permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o600
    );
}

#[test]
fn second_instance_forwards_and_exits() {
    let dir = tempfile::tempdir().unwrap();
    let Instance::Primary(listener) = acquire(dir.path(), &[]).unwrap() else {
        panic!("primary");
    };
    let second = acquire(dir.path(), &args(&["--screen", "chat"])).unwrap();
    assert!(matches!(second, Instance::Secondary));
    let got = listener.recv_timeout(WAIT).unwrap();
    assert_eq!(got.args, args(&["--screen", "chat"]));
    assert!(got.focus);
}

#[test]
fn stale_socket_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    // A crash leaves the socket file behind with nobody listening or locking.
    std::os::unix::net::UnixListener::bind(dir.path().join(SOCKET)).unwrap();
    let Instance::Primary(listener) = acquire(dir.path(), &[]).unwrap() else {
        panic!("a stale socket must not block the next launch");
    };
    assert!(matches!(
        acquire(dir.path(), &args(&["x"])).unwrap(),
        Instance::Secondary
    ));
    assert_eq!(listener.recv_timeout(WAIT).unwrap().args, args(&["x"]));
}

fn json_line(args: Vec<String>) -> Vec<u8> {
    let mut line = serde_json::to_vec(&Forwarded { focus: true, args }).unwrap();
    line.push(b'\n');
    line
}

/// A valid forward line exactly `len` bytes long, newline included.
fn line_of_len(len: usize) -> Vec<u8> {
    let overhead = json_line(vec![String::new()]).len();
    let line = json_line(vec!["a".repeat(len - overhead)]);
    assert_eq!(line.len(), len);
    line
}

#[test]
fn valid_line_over_limit_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let Instance::Primary(listener) = acquire(dir.path(), &[]).unwrap() else {
        panic!("primary");
    };
    let mut raw = UnixStream::connect(dir.path().join(SOCKET)).unwrap();
    // The primary hangs up once the cap is reached, so this write may fail
    // with a broken pipe; that is the behavior under test.
    drop(raw.write_all(&line_of_len(MAX_FORWARD_BYTES + 1)));
    drop(raw);
    assert!(matches!(
        acquire(dir.path(), &args(&["ok"])).unwrap(),
        Instance::Secondary
    ));
    assert_eq!(listener.recv_timeout(WAIT).unwrap().args, args(&["ok"]));
}

#[test]
fn line_at_limit_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let Instance::Primary(listener) = acquire(dir.path(), &[]).unwrap() else {
        panic!("primary");
    };
    let mut raw = UnixStream::connect(dir.path().join(SOCKET)).unwrap();
    raw.write_all(&line_of_len(MAX_FORWARD_BYTES)).unwrap();
    drop(raw);
    let got = listener.recv_timeout(WAIT).unwrap();
    assert_eq!(got.args.len(), 1);
}

#[test]
fn oversized_args_still_focus_the_primary() {
    let dir = tempfile::tempdir().unwrap();
    let Instance::Primary(listener) = acquire(dir.path(), &[]).unwrap() else {
        panic!("primary");
    };
    let huge = vec!["x".repeat(MAX_FORWARD_BYTES)];
    assert!(matches!(
        acquire(dir.path(), &huge).unwrap(),
        Instance::Secondary
    ));
    let got = listener.recv_timeout(WAIT).unwrap();
    assert!(got.focus);
    assert!(
        got.args.is_empty(),
        "too-long args are dropped, focus still goes through"
    );
}

#[test]
fn stale_lock_and_socket_are_replaced() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(LOCK), b"").unwrap();
    std::os::unix::net::UnixListener::bind(dir.path().join(SOCKET)).unwrap();
    assert!(matches!(
        acquire(dir.path(), &[]).unwrap(),
        Instance::Primary(_)
    ));
}

#[test]
fn folder_is_private() {
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("agmen");
    let _primary = acquire(&inner, &[]).unwrap();
    let mode = std::fs::metadata(&inner).unwrap().permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o700
    );
}

#[test]
fn forward_over_limit_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let Instance::Primary(listener) = acquire(dir.path(), &[]).unwrap() else {
        panic!("primary");
    };
    let mut raw = UnixStream::connect(dir.path().join(SOCKET)).unwrap();
    // The primary may hang up first; see valid_line_over_limit_is_dropped.
    drop(raw.write_all(&vec![b'a'; MAX_FORWARD_BYTES + 1024]));
    drop(raw);
    let mut bad = UnixStream::connect(dir.path().join(SOCKET)).unwrap();
    bad.write_all(b"not json\n").unwrap();
    drop(bad);
    assert!(matches!(
        acquire(dir.path(), &args(&["ok"])).unwrap(),
        Instance::Secondary
    ));
    assert_eq!(listener.recv_timeout(WAIT).unwrap().args, args(&["ok"]));
}
