//! Spike S5: per-stream numbering stays gapless and in commit order when many
//! transactions number the same streams at once.
//!
//! Needs the dev stack's Postgres in `AGMEN_IT_PG`. The concurrency test writes
//! to its own scratch table and uses stream names unique to the run, then
//! deletes the rows it wrote, so reruns start clean and other tests' rows stay
//! untouched. The migration test works in a scratch schema that it drops.

// clippy.toml's allow-*-in-tests covers only #[test] functions, not the
// helpers below them; a failed step in a helper should fail the test.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers fail the test on error"
)]

use std::collections::BTreeMap;
use std::hash::{BuildHasher, RandomState};
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, PgPool};
use tokio::task::JoinSet;

const TASKS: u32 = 50;
const TXS_PER_TASK: u32 = 40;
const STREAMS: u64 = 3;
const MAX_SLEEP_MS: u64 = 5;

/// A random number in `0..=max`. `RandomState::new` draws fresh keys on every
/// call, which is random enough for test jitter without a `rand` dependency.
fn random_up_to(max: u64) -> u64 {
    RandomState::new().hash_one(0_u8) % (max + 1)
}

fn pg_url() -> String {
    std::env::var("AGMEN_IT_PG")
        .expect("AGMEN_IT_PG is unset: source deploy/dev/.secrets/dev.env and start the dev stack")
}

/// A name unique to this process and moment, for streams and schemas.
fn run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}_{nanos}", std::process::id())
}

async fn pool() -> PgPool {
    // One connection per task, so all 50 transactions can be open at once.
    PgPoolOptions::new()
        .max_connections(TASKS)
        .connect(&pg_url())
        .await
        .expect("connect to AGMEN_IT_PG")
}

async fn create_scratch_table(pool: &PgPool) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS it_numbering_rows (
            run text NOT NULL,
            stream text NOT NULL,
            seq bigint NOT NULL,
            task integer NOT NULL,
            n integer NOT NULL,
            committed_at timestamptz,
            PRIMARY KEY (run, stream, seq)
        )",
    )
    .execute(pool)
    .await
    .expect("create it_numbering_rows");
}

/// Runs one task's transactions: number a random stream, record the row,
/// hold the transaction open for a random 0 to 5 ms, stamp the time, commit.
async fn run_task(pool: PgPool, run: String, streams: Vec<String>, task: u32) {
    for n in 0..TXS_PER_TASK {
        let stream = &streams[usize::try_from(random_up_to(STREAMS - 1)).unwrap()];
        let mut tx = pool.begin().await.expect("begin");
        let seq = agmen_hub::next_seq(&mut tx, stream)
            .await
            .expect("next_seq");
        sqlx::query(
            "INSERT INTO it_numbering_rows (run, stream, seq, task, n) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&run)
        .bind(stream)
        .bind(seq)
        .bind(i32::try_from(task).unwrap())
        .bind(i32::try_from(n).unwrap())
        .execute(&mut *tx)
        .await
        .expect("insert scratch row");
        tokio::time::sleep(Duration::from_millis(random_up_to(MAX_SLEEP_MS))).await;
        sqlx::query(
            "UPDATE it_numbering_rows SET committed_at = clock_timestamp()
             WHERE run = $1 AND stream = $2 AND seq = $3",
        )
        .bind(&run)
        .bind(stream)
        .bind(seq)
        .execute(&mut *tx)
        .await
        .expect("stamp commit time");
        tx.commit().await.expect("commit");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn it_numbering_is_gapless_under_concurrency() {
    let pool = pool().await;
    agmen_hub::MIGRATOR
        .run(&pool)
        .await
        .expect("run hub migrations");
    create_scratch_table(&pool).await;

    let run = run_id();
    let streams: Vec<String> = (0..STREAMS)
        .map(|i| format!("it-numbering/{run}/{i}"))
        .collect();

    let started = Instant::now();
    let mut tasks = JoinSet::new();
    for task in 0..TASKS {
        tasks.spawn(run_task(pool.clone(), run.clone(), streams.clone(), task));
    }
    while let Some(joined) = tasks.join_next().await {
        joined.expect("task finished without panicking");
    }
    let elapsed = started.elapsed();

    // Microseconds since the epoch: Postgres timestamps have microsecond
    // resolution, so this loses nothing and needs no time crate.
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT stream, seq, (extract(epoch FROM committed_at) * 1000000)::bigint
         FROM it_numbering_rows
         WHERE run = $1
         ORDER BY stream, committed_at, seq",
    )
    .bind(&run)
    .fetch_all(&pool)
    .await
    .expect("read scratch rows");

    // Clean up before asserting, so a failed run leaves nothing behind.
    sqlx::query("DELETE FROM it_numbering_rows WHERE run = $1")
        .bind(&run)
        .execute(&pool)
        .await
        .expect("delete scratch rows");
    sqlx::query("DELETE FROM stream_heads WHERE stream = ANY($1)")
        .bind(&streams)
        .execute(&pool)
        .await
        .expect("delete this run's stream heads");

    let total = TASKS * TXS_PER_TASK;
    assert_eq!(
        rows.len(),
        usize::try_from(total).unwrap(),
        "one row per committed transaction"
    );

    // Seqs per stream, in commit-time order, plus each stream's commit span.
    let mut by_stream: BTreeMap<&str, (Vec<i64>, i64, i64)> = BTreeMap::new();
    for (stream, seq, committed_us) in &rows {
        let entry = by_stream
            .entry(stream.as_str())
            .or_insert_with(|| (Vec::new(), *committed_us, *committed_us));
        entry.0.push(*seq);
        entry.2 = *committed_us;
    }

    println!(
        "numbering: {total} transactions on {STREAMS} streams by {TASKS} tasks in {:.2} s ({:.0} tx/s overall)",
        elapsed.as_secs_f64(),
        f64::from(total) / elapsed.as_secs_f64(),
    );
    for (stream, (seqs, first_us, last_us)) in &by_stream {
        let count = i64::try_from(seqs.len()).unwrap();
        assert!(
            seqs.is_sorted(),
            "{stream}: seq decreases in commit order: {seqs:?}"
        );
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        let expected: Vec<i64> = (1..=count).collect();
        assert_eq!(
            sorted, expected,
            "{stream}: seqs are not exactly 1..={count}"
        );

        let span_s = (last_us - first_us) as f64 / 1_000_000.0;
        let rate = (count - 1) as f64 / span_s;
        println!(
            "numbering: {stream}: {count} transactions committed over {span_s:.2} s ({rate:.0} tx/s on one stream)"
        );
    }
}

/// Whether `name` (schema qualified) names a table or other relation.
async fn relation_exists(pool: &PgPool, name: &str) -> bool {
    sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass($1)::text")
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("look up relation")
        .is_some()
}

/// The hub's migrations apply and revert cleanly. Runs in a schema of its own,
/// so the shared database's `stream_heads` and migration history stay as they
/// are, then drops that schema.
#[tokio::test]
async fn it_numbering_migration_reverts() {
    let schema = format!("it_numbering_mig_{}", run_id());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&pg_url())
        .await
        .expect("connect to AGMEN_IT_PG");
    // The schema name is a fixed prefix plus digits from run_id, so it is safe
    // to splice into DDL, which takes no bind parameters.
    sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\"")))
        .execute(&admin)
        .await
        .expect("create scratch schema");

    // Unqualified names, including sqlx's _sqlx_migrations, resolve to the
    // scratch schema on every connection in this pool.
    let options = PgConnectOptions::from_str(&pg_url())
        .expect("parse AGMEN_IT_PG")
        .options([("search_path", schema.as_str())]);
    let scoped = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("connect with the scratch search_path");
    let heads = format!("\"{schema}\".stream_heads");

    agmen_hub::MIGRATOR
        .run(&scoped)
        .await
        .expect("apply hub migrations");
    let after_up = relation_exists(&admin, &heads).await;
    agmen_hub::MIGRATOR
        .undo(&scoped, 0)
        .await
        .expect("revert hub migrations");
    let after_down = relation_exists(&admin, &heads).await;
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&scoped)
        .await
        .expect("count applied migrations");

    // Clean up before asserting, so a failed run leaves nothing behind.
    scoped.close().await;
    sqlx::query(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE")))
        .execute(&admin)
        .await
        .expect("drop scratch schema");

    assert!(after_up, "stream_heads missing after the up migrations");
    assert!(
        !after_down,
        "stream_heads still exists after the down migrations"
    );
    assert_eq!(applied, 0, "migration history not empty after undo to 0");
}
