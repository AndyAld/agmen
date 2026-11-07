use super::*;

#[tokio::test]
async fn walk_reads_at_most_32_parents() {
    let reads = std::sync::atomic::AtomicUsize::new(0);
    let error = walk(1000, 9999, |pid| {
        reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::future::ready(Ok(pid - 1))
    })
    .await
    .unwrap_err();
    assert_eq!(error.cause, Cause::DepthLimit);
    assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 32);
}

#[tokio::test]
async fn walk_stops_at_init_or_daemon_and_rejects_cycles() {
    for stop in [1, 42] {
        assert_eq!(
            walk(100, 42, |_| std::future::ready(Ok(stop)))
                .await
                .unwrap(),
            vec![100, stop]
        );
    }
    let error = walk(100, 42, |_| std::future::ready(Ok(100)))
        .await
        .unwrap_err();
    assert_eq!(error.cause, Cause::InvalidAncestry);
}

#[test]
fn containment_matches_process_ids_and_process_groups() {
    let pid = i32::try_from(std::process::id()).unwrap();
    assert!(contains_pid(&BTreeSet::from([pid]), pid).unwrap());
    let pgid = rustix::process::getpgrp().as_raw_nonzero().get();
    assert!(contains_pid(&BTreeSet::from([pgid]), pid).unwrap());
}

#[tokio::test]
async fn a_person_outside_every_worker_walks_to_init_without_a_refusal() {
    // The runner above this test is no descendant of it, so the walk climbs through
    // processes agmend does not own, as it does for the person's shell, up to pid 1.
    let parent = i32::try_from(std::os::unix::process::parent_id()).unwrap();
    let guard = Guard::default();
    assert!(
        !guard
            .contains_peer(Ok(parent), &BTreeSet::from([i32::MAX]))
            .await
    );
    assert_eq!(*guard.failure.lock().unwrap(), None);
}

#[tokio::test]
async fn a_peer_is_the_owner_only_when_known_unmanaged_and_outside_every_agent() {
    let guard = Guard::default();
    let me = i32::try_from(std::process::id()).unwrap();
    let outside = BTreeSet::from([i32::MAX]);
    let unknown = || Err(std::io::Error::other("no peer"));
    assert!(
        Peer::new(Ok(me), outside.clone(), false)
            .is_owner(&guard)
            .await
    );
    // In an agent's group when it connected, whatever the walk finds.
    assert!(
        !Peer::new(Ok(me), outside.clone(), true)
            .is_owner(&guard)
            .await
    );
    let own_group = BTreeSet::from([rustix::process::getpgrp().as_raw_nonzero().get()]);
    assert!(!Peer::new(Ok(me), own_group, false).is_owner(&guard).await);
    // No pid is refused, walking or not.
    assert!(!Peer::new(unknown(), outside, false).is_owner(&guard).await);
    assert!(
        !Peer::new(unknown(), BTreeSet::new(), false)
            .is_owner(&guard)
            .await
    );
}

#[test]
fn linux_stat_uses_the_parent_after_the_final_closing_parenthesis() {
    assert_eq!(stat_parent("42 (a (process) name)) S 21 42 0").unwrap(), 21);
    for invalid in ["", "42 (name)", "42 (name) S nope", "42 (name) S -1"] {
        assert!(stat_parent(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn macos_snapshot_requires_complete_unique_pid_parent_rows() {
    let parsed = parse_snapshot(b" 1 0\n 20 1\n 30 20\n").unwrap();
    assert_eq!(parsed.get(&30), Some(&20));
    for invalid in [
        b"".as_slice(),
        b"1",
        b"1 x",
        b"1 0 extra",
        b"1 0\n1 0",
        b"-1 0",
        b"1 -1",
        b"\xff",
    ] {
        assert!(parse_snapshot(invalid).is_err(), "{invalid:?}");
    }
}

#[tokio::test]
async fn empty_groups_skip_even_an_unreadable_walk() {
    let guard = Guard::default();
    assert!(
        !guard
            .check(&BTreeSet::new(), async {
                panic!("walk was polled without running agents")
            })
            .await
    );
}

#[tokio::test]
async fn ancestry_timeout_refuses_the_peer() {
    let guard = Guard::default();
    let start = std::time::Instant::now();
    assert!(
        guard
            .check(&BTreeSet::from([i32::MAX]), std::future::pending())
            .await
    );
    assert!(start.elapsed() >= ANCESTRY_TIMEOUT);
    assert_eq!(*guard.failure.lock().unwrap(), Some(Cause::TimedOut));
}

#[tokio::test]
async fn successful_lookup_resets_the_failure_cause() {
    let guard = Guard::default();
    let groups = BTreeSet::from([i32::MAX]);
    assert!(
        guard
            .check(&groups, async {
                Err(Error::new(Cause::InvalidSnapshot, "unreadable snapshot"))
            })
            .await
    );
    assert_eq!(*guard.failure.lock().unwrap(), Some(Cause::InvalidSnapshot));
    assert!(!guard.check(&groups, async { Ok(false) }).await);
    assert_eq!(*guard.failure.lock().unwrap(), None);
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "explicit 50-connection process snapshot measurement"]
async fn macos_walk_cost_for_50_connections() {
    let mut worker = tokio::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut person = tokio::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let groups = BTreeSet::from([i32::try_from(worker.id().unwrap()).unwrap()]);
    let pid = i32::try_from(person.id().unwrap()).unwrap();
    let guard = Guard::default();
    let mut elapsed = Vec::with_capacity(50);
    for _ in 0..50 {
        let started = std::time::Instant::now();
        assert!(!std::hint::black_box(
            guard
                .contains_peer(Ok(std::hint::black_box(pid)), &groups)
                .await
        ));
        assert_eq!(
            *guard.failure.lock().unwrap(),
            None,
            "measurement must complete its walk"
        );
        elapsed.push(started.elapsed());
    }
    person.kill().await.unwrap();
    worker.kill().await.unwrap();
    elapsed.sort_unstable();
    println!(
        "macOS ancestry: 50 successful walks, median {:?}, worst {:?}, best {:?}",
        (elapsed[24] + elapsed[25]) / 2,
        elapsed[49],
        elapsed[0]
    );
}
