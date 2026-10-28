use super::*;
use serde_json::json;

const ORIGIN: &str = "https://hub.example";

fn identity() -> ManagedIdentity {
    ManagedIdentity {
        hub_url: ORIGIN.parse().unwrap(),
        broker_url: "mqtts://broker.example:8883".parse().unwrap(),
        tenant: "tenant01".parse().unwrap(),
        project: "project1".parse().unwrap(),
        identity_id: "agent001".parse().unwrap(),
        owner_id: "owner001".parse().unwrap(),
        handle: "owner/agent".into(),
        kind: IdentityKind::Agent,
    }
}

fn message(seq: u64) -> StampedEnvelope {
    serde_json::from_value(json!({
        "v":1, "id":format!("{seq:026}"), "verb":"DM", "data":{},
        "to":["agent001"], "body":"hello", "sent_at":"2026-10-27T12:00:00Z",
        "stream":"feed", "seq":seq, "from":"sender01", "from_handle":"sender",
        "from_kind":"human", "hub_at":"2026-10-27T12:00:00Z"
    }))
    .unwrap()
}

fn setup() -> (tempfile::TempDir, Store, ManagedIdentity) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("agmend.db")).unwrap();
    let who = identity();
    store.register(&who).unwrap();
    (dir, store, who)
}

#[test]
fn pending_routes_plugin_input_shapes() {
    let cases = [
        (
            "ASK",
            json!({}),
            "feed",
            json!(["agent001"]),
            "sender01",
            true,
        ),
        (
            "ASK",
            json!({}),
            "feed",
            json!(["other001"]),
            "sender01",
            false,
        ),
        (
            "GRANT",
            json!({"item":"B-1","lease_until":"2026-10-28T12:00:00Z"}),
            "inbox:agent001",
            json!([]),
            "sender01",
            true,
        ),
        (
            "DENY",
            json!({"item":"B-1","reason":"held_by"}),
            "inbox:agent001",
            json!([]),
            "sender01",
            true,
        ),
        (
            "GRANT",
            json!({"item":"B-1","lease_until":"2026-10-28T12:00:00Z"}),
            "feed",
            json!([]),
            "sender01",
            false,
        ),
        (
            "ALERT",
            json!({"need":"owner request"}),
            "feed",
            json!([]),
            "owner001",
            true,
        ),
        (
            "ALERT",
            json!({"need":"other request"}),
            "feed",
            json!([]),
            "sender01",
            false,
        ),
    ];
    for (verb, data, stream, to, from, expected) in cases {
        let (_dir, mut store, who) = setup();
        let mut raw = serde_json::to_value(message(1)).unwrap();
        raw["verb"] = json!(verb);
        raw["data"] = data;
        raw["stream"] = json!(stream);
        raw["to"] = to;
        raw["from"] = json!(from);
        let event = serde_json::from_value(raw).unwrap();
        store.ingest(ORIGIN, &who.project, &event).unwrap();
        assert_eq!(
            !store
                .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
                .unwrap()
                .is_empty(),
            expected,
            "{verb} {stream} {from}"
        );
    }
}

#[test]
fn pending_routes_answers_to_own_messages_only() {
    let (_dir, mut store, who) = setup();
    let mut own = message(1);
    own.from = who.identity_id.clone();
    own.client.payload = Payload::Ask { of: None };
    own.client.to.clear();
    store.ingest(ORIGIN, &who.project, &own).unwrap();
    let mut answer = message(2);
    answer.client.payload = Payload::Answer {};
    answer.client.to.clear();
    answer.client.reply_to = Some(own.client.id.clone());
    store.ingest(ORIGIN, &who.project, &answer).unwrap();
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .len(),
        1
    );
    answer.seq = agmen_proto::Seq(3);
    answer.client.id = "00000000000000000000000003".parse().unwrap();
    answer.client.reply_to = Some("00000000000000000000000099".parse().unwrap());
    store.ingest(ORIGIN, &who.project, &answer).unwrap();
    assert!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pending_answer_uses_delivery_id_without_an_own_feed_copy() {
    let (_dir, mut store, who) = setup();
    let own = "00000000000000000000000099".parse().unwrap();
    store.remember_own(ORIGIN, &who.identity_id, &own).unwrap();
    let mut answer = message(1);
    answer.client.payload = Payload::Answer {};
    answer.client.to.clear();
    answer.client.reply_to = Some(own);
    store.ingest(ORIGIN, &who.project, &answer).unwrap();
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn state_opens_private_database() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("private");
    let path = parent.join("agmend.db");
    let _store = Store::open(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for name in ["agmend.db", "agmend.db-wal", "agmend.db-shm"] {
        assert_eq!(
            std::fs::metadata(parent.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn state_reopen_preserves_identities_and_positions() {
    let (dir, mut store, who) = setup();
    store.ingest(ORIGIN, &who.project, &message(1)).unwrap();
    drop(store);
    let mut store = Store::open(&dir.path().join("agmend.db")).unwrap();
    assert_eq!(
        store.identities(ORIGIN, &who.owner_id).unwrap(),
        vec![who.clone()]
    );
    assert_eq!(
        store.positions(ORIGIN, &who.project).unwrap()[&Stream::Feed],
        1
    );
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .identities("https://other.example", &who.owner_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pending_store_deduplicates_stream_seq() {
    let (_dir, mut store, who) = setup();
    for _ in 0..2 {
        store.ingest(ORIGIN, &who.project, &message(1)).unwrap();
    }
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn duplicate_pending_copy_is_a_no_op_and_advances_position() {
    let (_dir, mut store, who) = setup();
    let mut event = message(1);
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    event.stream = Stream::Inbox(who.identity_id.clone());
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    assert_eq!(
        store.positions(ORIGIN, &who.project).unwrap()[&event.stream],
        1
    );
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn pending_store_deduplicates_cross_stream_message() {
    let (_dir, mut store, who) = setup();
    let mut event = message(1);
    event.client.payload = Payload::Assign {
        item: "B-4".parse().unwrap(),
        to: who.identity_id.clone(),
    };
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .len(),
        1
    );
    event.stream = Stream::Inbox(who.identity_id.clone());
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    assert!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pending_store_transaction_rolls_back_on_full() {
    let (_dir, mut store, who) = setup();
    for seq in 1..=10_000 {
        store.ingest(ORIGIN, &who.project, &message(seq)).unwrap();
    }
    assert!(
        store
            .ingest(ORIGIN, &who.project, &message(10_001))
            .is_err()
    );
    assert_eq!(
        store.positions(ORIGIN, &who.project).unwrap()[&Stream::Feed],
        10_000
    );
    store
        .drain(ORIGIN, &who.identity_id, 1, usize::MAX)
        .unwrap();
    store
        .ingest(ORIGIN, &who.project, &message(10_001))
        .unwrap();
    let batch = store
        .drain(ORIGIN, &who.identity_id, u32::MAX, usize::MAX)
        .unwrap();
    assert_eq!(batch.len(), 500);
    assert_eq!(batch.first().unwrap().seq.0, 2);
    assert_eq!(batch.last().unwrap().seq.0, 501);
}

#[test]
fn pending_store_drain_is_atomic() {
    let (dir, mut store, who) = setup();
    for seq in 1..=4 {
        store.ingest(ORIGIN, &who.project, &message(seq)).unwrap();
    }
    let path = dir.path().join("agmend.db");
    let other = who.clone();
    let task = std::thread::spawn(move || {
        Store::open(&path)
            .unwrap()
            .drain(ORIGIN, &other.identity_id, 2, usize::MAX)
            .unwrap()
    });
    let first = store
        .drain(ORIGIN, &who.identity_id, 2, usize::MAX)
        .unwrap();
    let second = task.join().unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    assert!(
        first
            .iter()
            .all(|a| second.iter().all(|b| a.client.id != b.client.id))
    );
}

#[test]
fn state_rejects_future_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agmend.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 3)
        .unwrap();
    assert!(matches!(Store::open(&path), Err(StoreError::Version(3))));
}

/// Copies `name` and its rollback journal from `source` into `target` while a
/// write transaction is open: what a crash during that transaction leaves.
fn crash_copy(source: &Path, target: &Path, name: &str) {
    let path = source.join(name);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE; PRAGMA cache_size=1; CREATE TABLE pad(payload BLOB); BEGIN IMMEDIATE;",
    )
    .unwrap();
    for _ in 0..40 {
        conn.execute("INSERT INTO pad VALUES (zeroblob(4096))", [])
            .unwrap();
    }
    conn.pragma_update(None, "user_version", 1).unwrap();
    fs::create_dir_all(target).unwrap();
    for suffix in ["", "-journal"] {
        fs::copy(
            format!("{}{suffix}", path.display()),
            format!("{}{suffix}", target.join(name).display()),
        )
        .unwrap();
    }
    conn.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn startup_recovers_stores_left_with_hot_rollback_journals() {
    let source = tempfile::tempdir().unwrap();
    let identity_dir = source.path().join("identities/agent001");
    fs::create_dir_all(&identity_dir).unwrap();
    drop(Store::open(&source.path().join("agmend.db")).unwrap());
    drop(agmen_client::Outbox::open(identity_dir.join(agmen_client::STATE_DB)).unwrap());
    let state = tempfile::tempdir().unwrap();
    crash_copy(source.path(), state.path(), "agmend.db");
    crash_copy(
        &identity_dir,
        &state.path().join("identities/agent001"),
        agmen_client::STATE_DB,
    );
    drop(open_stores(&state.path().join("agmend.db")).unwrap());
    let versions = schemas(state.path(), Instant::now() + Duration::from_secs(5)).unwrap();
    assert_eq!(
        versions,
        BTreeMap::from([
            ("agmend.db".to_owned(), 2),
            ("identities/agent001/agmen.db".to_owned(), 2),
        ])
    );
}

#[test]
fn two_senders_with_same_id_both_deliver() {
    for assign in [false, true] {
        let (_dir, mut store, who) = setup();
        let mut event = message(1);
        if assign {
            event.client.payload = Payload::Assign {
                item: "B-4".parse().unwrap(),
                to: who.identity_id.clone(),
            };
        }
        store.ingest(ORIGIN, &who.project, &event).unwrap();
        event.from = "sender02".parse().unwrap();
        event.seq.0 = 2;
        store.ingest(ORIGIN, &who.project, &event).unwrap();
        let rows = store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].from, rows[1].from);
    }
}

#[test]
fn version_one_migrates_pending_and_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agmend.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE identities (origin TEXT NOT NULL, identity TEXT NOT NULL, owner TEXT NOT NULL, project TEXT NOT NULL, metadata TEXT NOT NULL, PRIMARY KEY(origin,identity)) STRICT;
        CREATE TABLE positions (origin TEXT NOT NULL, project TEXT NOT NULL, stream TEXT NOT NULL, seq INTEGER NOT NULL CHECK(seq >= 0), PRIMARY KEY(origin,project,stream)) STRICT;
        CREATE TABLE pending (n INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, project TEXT NOT NULL, identity TEXT NOT NULL, id TEXT NOT NULL, message TEXT NOT NULL, UNIQUE(origin,project,identity,id)) STRICT;
        CREATE INDEX pending_identity ON pending(origin,identity,n);
        CREATE TABLE receipts (origin TEXT NOT NULL, project TEXT NOT NULL, identity TEXT NOT NULL, id TEXT NOT NULL, feed INTEGER, inbox INTEGER, PRIMARY KEY(origin,project,identity,id)) STRICT;
        CREATE INDEX receipts_identity ON receipts(origin,identity);
        PRAGMA user_version=1;").unwrap();
    let who = identity();
    let event = message(1);
    conn.execute(
        "INSERT INTO pending VALUES (7,?1,?2,?3,?4,?5)",
        params![
            ORIGIN,
            who.project.as_str(),
            who.identity_id.as_str(),
            event.client.id.as_str(),
            serde_json::to_string(&event).unwrap()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO receipts VALUES (?1,?2,?3,?4,1,NULL)",
        params![
            ORIGIN,
            who.project.as_str(),
            who.identity_id.as_str(),
            event.client.id.as_str()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO identities VALUES (?1,?2,?3,?4,?5)",
        params![
            ORIGIN,
            who.identity_id.as_str(),
            who.owner_id.as_str(),
            who.project.as_str(),
            serde_json::to_string(&who).unwrap()
        ],
    )
    .unwrap();
    // These legacy receipts outlived their drained pending messages.
    conn.execute(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<19999)
        INSERT INTO receipts SELECT ?1,?2,?3,printf('old-%d',x),x,NULL FROM n",
        params![ORIGIN, who.project.as_str(), who.identity_id.as_str()],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    let sender: String = store
        .conn
        .query_row(
            "SELECT sender FROM receipts WHERE id=?1",
            [event.client.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sender, event.from.as_str());
    assert_eq!(
        store
            .conn
            .query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        20000
    );
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap(),
        vec![event]
    );
    assert_eq!(
        store.identities(ORIGIN, &who.owner_id).unwrap(),
        vec![who.clone()]
    );
    let mut next = message(1);
    next.client.id = message(2).client.id;
    next.client.payload = Payload::Assign {
        item: "B-4".parse().unwrap(),
        to: who.identity_id.clone(),
    };
    store.ingest(ORIGIN, &who.project, &next).unwrap();
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap(),
        vec![next]
    );
}

#[test]
fn maximum_seq_and_malformed_json_are_errors() {
    let (_dir, mut store, who) = setup();
    assert!(
        store
            .ingest(ORIGIN, &who.project, &message(u64::MAX))
            .is_err()
    );
    store.ingest(ORIGIN, &who.project, &message(1)).unwrap();
    store
        .conn
        .execute("UPDATE pending SET message = '{}'", [])
        .unwrap();
    assert!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .is_err()
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT count(*) FROM pending", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn gaps_qualifiers_and_byte_budget() {
    let (_dir, mut store, who) = setup();
    assert!(store.ingest(ORIGIN, &who.project, &message(2)).is_err());
    let mut event = message(1);
    event.client.payload = Payload::Answer {};
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    event = message(2);
    event.from = who.identity_id.clone();
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    event = message(3);
    event.client.payload = Payload::Answer {};
    event.mentioned.push(who.identity_id.clone());
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    let size = serde_json::to_vec(&event).unwrap().len();
    assert!(
        store
            .drain(ORIGIN, &who.identity_id, 500, size + 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, size + 2)
            .unwrap(),
        vec![event]
    );
}

#[test]
fn assign_receipts_prune_after_both_streams_and_survive_reopen() {
    for first_stream in [Stream::Feed, Stream::Inbox(identity().identity_id)] {
        let (dir, mut store, who) = setup();
        let mut event = message(1);
        event.client.to.clear();
        event.client.payload = Payload::Assign {
            item: "B-4".parse().unwrap(),
            to: who.identity_id.clone(),
        };
        event.stream = first_stream.clone();
        store.ingest(ORIGIN, &who.project, &event).unwrap();
        assert_eq!(
            store
                .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
                .unwrap(),
            vec![event.clone()]
        );
        drop(store);
        let mut store = Store::open(&dir.path().join("agmend.db")).unwrap();
        assert_eq!(
            store
                .conn
                .query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        event.stream = match first_stream {
            Stream::Feed => Stream::Inbox(who.identity_id.clone()),
            Stream::Inbox(_) => Stream::Feed,
        };
        store.ingest(ORIGIN, &who.project, &event).unwrap();
        assert_eq!(
            store
                .conn
                .query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(
            store
                .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
                .unwrap()
                .is_empty()
        );
        store.ingest(ORIGIN, &who.project, &event).unwrap();
        event.stream = first_stream;
        store.ingest(ORIGIN, &who.project, &event).unwrap();
        assert!(
            store
                .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn feed_mentions_do_not_create_receipts() {
    let (_dir, mut store, who) = setup();
    let mut event = message(1);
    event.client.to.clear();
    event.client.payload = Payload::Fyi {};
    event.mentioned.push(who.identity_id.clone());
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 1, usize::MAX)
            .unwrap(),
        vec![event]
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn inbox_dms_do_not_accumulate_receipts() {
    let (_dir, mut store, who) = setup();
    let stream = Stream::Inbox(who.identity_id.clone());
    for seq in 1..=20_001 {
        let mut event = message(seq);
        event.stream = stream.clone();
        store.ingest(ORIGIN, &who.project, &event).unwrap();
        assert_eq!(
            store
                .drain(ORIGIN, &who.identity_id, 1, usize::MAX)
                .unwrap(),
            vec![event]
        );
    }
    assert_eq!(
        store.positions(ORIGIN, &who.project).unwrap()[&stream],
        20_001
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn stamped_client_near_envelope_cap_ingests_and_drains() {
    let (dir, mut store, who) = setup();
    let mut event = message(1);
    event.client.to.clear();
    event.client.payload = Payload::Fyi {};
    event.client.mentions = vec!["all".parse().unwrap()];
    event.client.body = "".try_into().unwrap();
    let remaining = 65_487 - serde_json::to_vec(&event.client).unwrap().len();
    event.client.body = format!(
        "{}{}",
        "\n".repeat(remaining / 2),
        "x".repeat(remaining % 2)
    )
    .try_into()
    .unwrap();
    let client_raw = serde_json::to_vec(&event.client).unwrap();
    assert_eq!(client_raw.len(), 65_487);
    let client = agmen_proto::ClientEnvelope::from_json(&client_raw).unwrap();
    agmen_proto::validate(&client, event.hub_at).unwrap();
    event.mentioned = (0..99)
        .map(|n| format!("peer{n:05}").parse().unwrap())
        .collect();
    event.mentioned.push(who.identity_id.clone());
    event.from_handle.clear();
    event.from_handle = "h".repeat(66_928 - serde_json::to_vec(&event).unwrap().len());
    assert_eq!(event.mentioned.len(), 100);
    assert_eq!(serde_json::to_vec(&event).unwrap().len(), 66_928);
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    assert_eq!(
        store.positions(ORIGIN, &who.project).unwrap()[&Stream::Feed],
        1
    );
    drop(store);
    let mut store = Store::open(&dir.path().join("agmend.db")).unwrap();
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap(),
        vec![event]
    );
}

#[test]
fn nonqualifying_oversized_message_advances_position() {
    let (_dir, mut store, who) = setup();
    let mut event = message(1);
    event.client.to.clear();
    event.client.payload = Payload::Fyi {};
    event.from_handle = "h".repeat(crate::sse::MAX_EVENT);
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    assert_eq!(
        store.positions(ORIGIN, &who.project).unwrap()[&Stream::Feed],
        1
    );
    store.ingest(ORIGIN, &who.project, &message(2)).unwrap();
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap(),
        vec![message(2)]
    );
}

#[test]
fn stored_rows_use_receive_cap() {
    let (_dir, mut store, who) = setup();
    let mut event = message(1);
    event.from_handle.clear();
    event.from_handle =
        "h".repeat(crate::sse::MAX_EVENT - serde_json::to_vec(&event).unwrap().len());
    assert_eq!(
        serde_json::to_vec(&event).unwrap().len(),
        crate::sse::MAX_EVENT
    );
    event.from_handle.push('h');
    assert!(matches!(
        store.ingest(ORIGIN, &who.project, &event),
        Err(StoreError::Full(_))
    ));
    assert!(store.positions(ORIGIN, &who.project).unwrap().is_empty());
    event.from_handle.pop();
    store.ingest(ORIGIN, &who.project, &event).unwrap();
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap(),
        vec![event]
    );
}

#[test]
fn receipt_cap_refuses_without_cursor_advance() {
    let (_dir, mut store, who) = setup();
    let mut event = message(1);
    event.client.to.clear();
    event.client.payload = Payload::Assign {
        item: "B-4".parse().unwrap(),
        to: who.identity_id.clone(),
    };
    store
        .conn
        .execute(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<20000)
         INSERT INTO receipts SELECT ?1, ?2, ?3, 'sender01', printf('old-%d',x), x, NULL FROM n",
            params![ORIGIN, who.project.as_str(), who.identity_id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        store.ingest(ORIGIN, &who.project, &event),
        Err(StoreError::Full(_))
    ));
    assert!(store.positions(ORIGIN, &who.project).unwrap().is_empty());
    assert!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn registration_and_stream_counts_are_bounded() {
    let (_dir, mut store, who) = setup();
    for n in 1..64 {
        let mut next = who.clone();
        next.identity_id = format!("peers{n:03}").parse().unwrap();
        next.project = format!("proj{n:04}").parse().unwrap();
        store.register(&next).unwrap();
    }
    let mut extra = who.clone();
    extra.identity_id = "extra001".parse().unwrap();
    assert!(matches!(store.register(&extra), Err(StoreError::Full(_))));
    store.register(&who).unwrap();
    extra = who.clone();
    extra.owner_id = "other001".parse().unwrap();
    assert!(store.register(&extra).is_err());
    for n in 0..256 {
        let mut event = message(1);
        event.stream = Stream::Inbox(format!("inbox{n:03}").parse().unwrap());
        store.ingest(ORIGIN, &who.project, &event).unwrap();
    }
    assert!(matches!(
        store.ingest(ORIGIN, &who.project, &message(1)),
        Err(StoreError::Full(_))
    ));
    assert_eq!(store.positions(ORIGIN, &who.project).unwrap().len(), 256);
}

#[test]
fn drain_errors_roll_back_the_whole_selection() {
    let (_dir, mut store, who) = setup();
    store.ingest(ORIGIN, &who.project, &message(1)).unwrap();
    store.ingest(ORIGIN, &who.project, &message(2)).unwrap();
    store
        .conn
        .execute("UPDATE pending SET message='{}' WHERE n=2", [])
        .unwrap();
    assert!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .is_err()
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT count(*) FROM pending", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    store.conn.pragma_update(None, "query_only", true).unwrap();
    assert!(
        store
            .drain(ORIGIN, &who.identity_id, 1, usize::MAX)
            .is_err()
    );
}

#[test]
fn origin_and_project_isolate_pending_and_positions() {
    let (_dir, mut store, who) = setup();
    let other_origin = "https://other.example";
    let mut other = who.clone();
    other.hub_url = other_origin.parse().unwrap();
    store.register(&other).unwrap();
    store.ingest(ORIGIN, &who.project, &message(1)).unwrap();
    assert!(
        store
            .positions(other_origin, &who.project)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .drain(other_origin, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .is_empty()
    );
    store
        .ingest(other_origin, &who.project, &message(1))
        .unwrap();
    assert_eq!(
        store
            .drain(other_origin, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .len(),
        1
    );
    let project = "project2".parse().unwrap();
    store.ingest(ORIGIN, &project, &message(1)).unwrap();
    assert_eq!(store.positions(ORIGIN, &project).unwrap()[&Stream::Feed], 1);
    assert!(
        store
            .drain(ORIGIN, &who.identity_id, 500, usize::MAX)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn owner_position_namespaces_preserve_project_quota() {
    let (_dir, mut store, who) = setup();
    let other: IdentityId = "other001".parse().unwrap();
    for seq in 0..256 {
        let mut event = message(1);
        event.stream = Stream::Inbox(format!("stream{seq:03}").parse().unwrap());
        let owner = if seq % 2 == 0 { &who.owner_id } else { &other };
        store
            .ingest_for_owner(ORIGIN, &who.project, &event, Some(owner))
            .unwrap();
    }
    assert_eq!(
        store
            .positions_for_owner(ORIGIN, &who.owner_id, &who.project)
            .unwrap()
            .len(),
        128
    );
    assert_eq!(
        store
            .positions_for_owner(ORIGIN, &other, &who.project)
            .unwrap()
            .len(),
        128
    );
    assert!(matches!(
        store.ingest_for_owner(ORIGIN, &who.project, &message(1), Some(&other)),
        Err(StoreError::Full("256 stream positions"))
    ));
}

#[test]
fn launch_batches_prioritize_owner_and_preserve_other_senders() {
    let (_dir, mut store, who) = setup();
    for seq in 1..=4 {
        let mut row = message(seq);
        if seq >= 2 {
            row.from = who.owner_id.clone();
        }
        if seq == 3 {
            row.from_kind = IdentityKind::Agent;
        }
        store.ingest(ORIGIN, &who.project, &row).unwrap();
    }
    let owner = store.drain_launch(ORIGIN, &who).unwrap();
    assert_eq!(owner.iter().map(|m| m.seq.0).collect::<Vec<_>>(), [2, 4]);
    let others = store.drain_launch(ORIGIN, &who).unwrap();
    assert_eq!(others.iter().map(|m| m.seq.0).collect::<Vec<_>>(), [1, 3]);
    assert!(store.drain_launch(ORIGIN, &who).unwrap().is_empty());
}

#[test]
fn unregister_forgets_the_identity_and_its_waiting_messages() {
    let (_dir, mut store, who) = setup();
    store.ingest(ORIGIN, &who.project, &message(1)).unwrap();
    store.unregister(ORIGIN, &who.identity_id).unwrap();
    assert!(store.identities(ORIGIN, &who.owner_id).unwrap().is_empty());
    let pending: i64 = store
        .conn
        .query_row("SELECT count(*) FROM pending", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pending, 0);
    // Registering again starts from an empty queue.
    store.register(&who).unwrap();
    assert!(
        store
            .drain(ORIGIN, &who.identity_id, 500, 1 << 20)
            .unwrap()
            .is_empty()
    );
}
