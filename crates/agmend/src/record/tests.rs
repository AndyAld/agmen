use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::task::{Context, Waker};
use std::time::Duration;

use agmen_core::{Store, StoreError, StoreTx};
use agmen_proto::{Seq, Timestamp};
use rusqlite::Connection;
use tempfile::TempDir;

use super::schema::{RECORD_DB, RecordError, open};
use super::{SqliteStore, micros, test_clock};

const TEST_WAIT: Duration = Duration::from_secs(2);
const HOLD_FOR: Duration = Duration::from_millis(200);
const TICK_EVERY: Duration = Duration::from_millis(10);
const MAX_TICKS: usize = 100;

async fn store() -> (TempDir, SqliteStore) {
    tokio::task::spawn_blocking(|| {
        let dir = tempfile::tempdir().unwrap();
        let store = open(
            &dir.path().join(RECORD_DB),
            std::sync::Arc::new(Timestamp::now),
        )
        .unwrap();
        (dir, store)
    })
    .await
    .unwrap()
}

#[test]
fn record_file_is_private() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("state");
    let path = folder.join(RECORD_DB);
    let store = open(&path, std::sync::Arc::new(Timestamp::now)).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let conn = store.conn.try_lock().unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(
        conn.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let topics = super::test_rows::project(&conn);
    let owner = super::test_rows::human(&conn, &topics, "admin");
    super::test_rows::identity(&conn, &topics, "agent", &owner);
}

#[test]
fn newer_record_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(RECORD_DB);
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    assert!(matches!(
        open(&path, std::sync::Arc::new(Timestamp::now)),
        Err(RecordError::Version(2))
    ));
    let conn = Connection::open(&path).unwrap();
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
}

#[tokio::test]
async fn next_seq_starts_at_one_per_key() {
    let (_dir, store) = store().await;
    let mut tx = store.begin().await.unwrap();
    tx.lock_head("a").await.unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(1));
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(2));
    assert_eq!(tx.next_seq("b").await.unwrap(), Seq(1));
    tx.commit().await.unwrap();
    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(3));
    assert_eq!(tx.next_seq("b").await.unwrap(), Seq(2));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn dropped_tx_records_nothing() {
    let (_dir, store) = store().await;
    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(1));
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(2));
    drop(tx);
    let heads = store
        .read(|conn, _| {
            conn.query_row("SELECT count(*) FROM stream_heads", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(super::backend)
        })
        .await
        .unwrap();
    assert_eq!(heads, 0);
    let error = store
        .write(|conn, now| {
            super::heads::next_seq(conn, now, "a".into())?;
            Err::<(), _>(StoreError::Stored("abort this write".into()))
        })
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::Stored(_)));
    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(1));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn cancelled_call_leaves_no_partial_transaction() {
    let (_dir, store) = store().await;
    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(1));
    {
        let call = tx.next_seq("a");
        tokio::pin!(call);
        // One poll submits the statement; cancellation may race its completion.
        let _polled = call.as_mut().poll(&mut Context::from_waker(Waker::noop()));
    }
    drop(tx);
    let mut tx = tokio::time::timeout(TEST_WAIT, store.begin())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(1));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn reopen_after_uncommitted_tx_keeps_numbering_gapless() {
    let (dir, store) = store().await;
    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(1));
    tx.commit().await.unwrap();
    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(2));
    drop(tx);
    let path: PathBuf = dir.path().join(RECORD_DB);
    let reopened = tokio::task::spawn_blocking(move || {
        drop(store);
        open(&path, std::sync::Arc::new(Timestamp::now)).unwrap()
    })
    .await
    .unwrap();
    let mut tx = reopened.begin().await.unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(2));
    tx.commit().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn waiting_caller_does_not_block_the_runtime() {
    let (_dir, store) = store().await;
    let tx = store.begin().await.unwrap();
    let holder = tokio::spawn(async move {
        tokio::time::sleep(HOLD_FOR).await;
        tx.commit().await.unwrap();
    });
    let waiting_store = store.clone();
    let waiter = tokio::spawn(async move {
        waiting_store.begin().await.unwrap().commit().await.unwrap();
    });
    let mut interval = tokio::time::interval(TICK_EVERY);
    let mut ticks = 0;
    for _ in 0..MAX_TICKS {
        if holder.is_finished() {
            break;
        }
        tokio::time::timeout(TEST_WAIT, interval.tick())
            .await
            .unwrap();
        ticks += 1;
    }
    assert!(
        ticks >= 10,
        "only {ticks} timer ticks while SQLite was held"
    );
    tokio::time::timeout(TEST_WAIT, holder)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(TEST_WAIT, waiter)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn acquire_wait_is_bounded() {
    let (_dir, store) = store().await;
    let store = store.with_acquire_wait(Duration::from_millis(50));
    let tx = store.begin().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), store.begin())
        .await
        .unwrap();
    assert!(matches!(result, Err(StoreError::Backend(_))));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn tx_clock_is_fixed_at_begin() {
    let (dir, initial) = store().await;
    let at: Timestamp = "2026-11-02T12:00:00.123456Z".parse().unwrap();
    let (clock, time) = test_clock(at);
    let path = dir.path().join(RECORD_DB);
    let store = tokio::task::spawn_blocking(move || {
        drop(initial);
        open(&path, clock).unwrap()
    })
    .await
    .unwrap();
    let mut tx = store.begin().await.unwrap();
    *time.lock().unwrap() = at + Duration::from_secs(5);
    assert_eq!(tx.now_micros(), micros(at));
    let seen = tx.call(|_, now| Ok(now)).await;
    assert_eq!(seen.unwrap(), micros(at));
    assert_eq!(super::from_micros(tx.now_micros()).unwrap(), at);
    assert!(super::from_micros(i64::MAX).is_err());
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn wal_sidecars_are_private() {
    let (dir, store) = store().await;
    let mut tx = store.begin().await.unwrap();
    tx.next_seq("a").await.unwrap();
    tx.commit().await.unwrap();
    let mut seen = 0;
    for suffix in ["-wal", "-shm"] {
        let mut name = dir.path().join(RECORD_DB).into_os_string();
        name.push(suffix);
        let mode = std::fs::metadata(&name).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{suffix} sidecar mode");
        seen += 1;
    }
    assert_eq!(seen, 2);
}

#[tokio::test]
async fn a_panicking_blocking_call_neither_wedges_nor_leaks_writes() {
    let (_dir, store) = store().await;
    let store = store.with_acquire_wait(TEST_WAIT);
    let read = store.read(|_, _| -> Result<(), StoreError> { panic!("reader panics") });
    assert!(matches!(read.await, Err(StoreError::Backend(_))));
    let write = store.write(|conn, now| -> Result<(), StoreError> {
        super::heads::next_seq(conn, now, "a".into())?;
        panic!("writer panics after a statement");
    });
    assert!(matches!(write.await, Err(StoreError::Backend(_))));
    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(1));
    let call = tx.call(|conn, now| -> Result<(), StoreError> {
        super::heads::next_seq(conn, now, "a".into())?;
        panic!("tx call panics after a statement");
    });
    assert!(matches!(call.await, Err(StoreError::Backend(_))));
    // The transaction lost its connection, so it can neither commit nor continue.
    assert!(tx.next_seq("a").await.is_err());
    assert!(tx.commit().await.is_err());
    let mut tx = tokio::time::timeout(TEST_WAIT, store.begin())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tx.next_seq("a").await.unwrap(), Seq(1));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn write_clock_is_taken_once_the_connection_is_held() {
    let (dir, initial) = store().await;
    let at: Timestamp = "2026-11-02T12:00:00Z".parse().unwrap();
    let (clock, time) = test_clock(at);
    let path = dir.path().join(RECORD_DB);
    let store = tokio::task::spawn_blocking(move || {
        drop(initial);
        open(&path, clock).unwrap()
    })
    .await
    .unwrap();
    let holder = store.begin().await.unwrap();
    let waiting = store.clone();
    let waiter = tokio::spawn(async move { waiting.write(|_, now| Ok(now)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let later = at + Duration::from_secs(5);
    *time.lock().unwrap() = later;
    holder.commit().await.unwrap();
    let now = tokio::time::timeout(TEST_WAIT, waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(now, micros(later));
}

#[tokio::test]
async fn tx_clock_starts_once_the_lock_is_held() {
    let (dir, initial) = store().await;
    let at: Timestamp = "2026-11-02T12:00:00Z".parse().unwrap();
    let (clock, time) = test_clock(at);
    let path = dir.path().join(RECORD_DB);
    let store = tokio::task::spawn_blocking(move || {
        drop(initial);
        open(&path, clock).unwrap()
    })
    .await
    .unwrap();
    let holder = store.begin().await.unwrap();
    let waiting = store.clone();
    let waiter = tokio::spawn(async move { waiting.begin().await.map(|tx| tx.now_micros()) });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let later = at + Duration::from_secs(5);
    *time.lock().unwrap() = later;
    holder.commit().await.unwrap();
    let now = tokio::time::timeout(TEST_WAIT, waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(now, micros(later));
}
