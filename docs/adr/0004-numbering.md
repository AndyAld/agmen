# 0004: Gapless per-stream numbering

Status: accepted
Date: 2026-10-25

## Context

Readers detect lost messages by seq: a gap (`seq > last + 1`) triggers a replay (protocol 5.3). That only works if the hub hands out seqs with no gaps and commits them in seq order, so a reader never sees seq 8 committed before seq 7 and replays past a message that is about to appear. A Postgres sequence does neither: `nextval` is not rolled back, and two transactions can commit in the opposite order to the values they drew. Spike S5 checks the row-lock approach from protocol 5.2 under concurrency.

## Decision

The hub numbers messages with one row per stream in `stream_heads (stream text primary key, seq bigint not null)`, created by `crates/agmen-hub/migrations/0001_streams.up.sql`. `agmen_hub::next_seq(tx, stream)` runs, inside the ingest transaction:

```sql
INSERT INTO stream_heads (stream, seq) VALUES ($1, 1)
ON CONFLICT (stream) DO UPDATE SET seq = stream_heads.seq + 1
RETURNING seq
```

The upsert creates a new stream's row with seq 1, so no step registers streams in advance. It takes the row lock that protocol 5.2's `UPDATE ... RETURNING` takes, and holds it until commit or rollback. A second transaction on the same stream waits for the first to finish, so seqs commit in order, and a rollback returns its seq to the next writer. Writers on different streams do not block each other.

The hub embeds its migrations with `sqlx::migrate!` as `agmen_hub::MIGRATOR` and runs it before other queries. Hub migrations are reversible: each is a `NNNN_name.up.sql` and `NNNN_name.down.sql` pair, and `0001_streams.down.sql` drops `stream_heads`. sqlx reads that suffix as a reversible migration and runs the down file on `Migrator::undo`. Later hub migrations follow the same form. `crates/agmen-hub/build.rs` makes Cargo rebuild when a file is added to `migrations/`, which the macro cannot track on stable Rust. Queries are runtime-checked `sqlx::query` calls with binds (ruling R23).

## Results

`crates/agmen-hub/tests/it_numbering.rs`, `it_numbering_is_gapless_under_concurrency`: 50 tasks each run 40 transactions (2,000 in all) against the dev stack's Postgres 16.15 on an Apple M2 Max, with a pool of 50 connections so every task can hold a transaction open at once. Each transaction calls `next_seq` on one of 3 streams picked at random, inserts `(stream, seq, task, n)` into a scratch table, sleeps a random 0 to 5 ms, stamps `clock_timestamp()`, and commits. Per stream, the test checks that the seqs are exactly `1..=count` and that ordering rows by the commit stamp gives non-decreasing seq.

- Run 20 times with `--retries 0`: 20 passes.
- Mutation check: changing the increment to `+ 2` fails the test with "seqs are not exactly 1..=680".
- `it_numbering_migration_reverts` applies the migrations and undoes them to version 0 in a scratch schema, and checks that `stream_heads` exists after the up step and is gone after the down step. A no-op down file fails it with "stream_heads still exists after the down migrations".
- Throughput on one stream, from the test output (`committed / commit span` per stream, 60 samples): median 186 tx/s, range 97 to 202. 17 of 20 runs measured 160 to 202 tx/s per stream; runs 7, 8 and 10 measured 97 to 107 on all three streams at once. The cause is unknown: other worktrees share this machine and this Postgres, so a load spike there is likely but unverified. Across all 3 streams: median 557 tx/s, range 298 to 591.

The per-stream rate measures how long each transaction holds the row lock, not the cost of the upsert. Here that hold is four round trips plus the test's sleep: about 5.4 ms per transaction at 186 tx/s.

## Consequences

- One stream's throughput is 1 divided by the time ingest holds the lock. Ingest must call `next_seq` late in its transaction, after validation and dedupe, and do no network calls or waits between `next_seq` and commit. Even with the spike's deliberate 0 to 5 ms hold, 186 tx/s per stream is about 18 times the chat rate limit in design.md (10 per second, burst 30).
- Busy streams do not slow other streams, because each stream has its own row.
- A dropped connection rolls back its transaction and releases the lock, so a crashed ingest never leaves a gap.
- Revisit if one stream needs more than a few hundred messages per second, or if ingest has to hold the transaction open across a slow step: then number in batches, or move the seq out of the critical section with a single writer per stream.
