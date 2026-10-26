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

#[test]
fn forward_over_limit_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let Instance::Primary(listener) = acquire(dir.path(), &[]).unwrap() else {
        panic!("primary");
    };
    let mut raw = UnixStream::connect(dir.path().join(SOCKET)).unwrap();
    raw.write_all(&vec![b'a'; MAX_FORWARD_BYTES + 1024])
        .unwrap();
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
