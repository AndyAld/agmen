use agmen_core::{MemberRole, StoreError};
use agmen_proto::{IdentityId, IdentityKind, Seq, Stream, Timestamp, Topics};
use rusqlite::{Connection, params};
use serde_json::json;

use super::*;
use crate::record::{presence, replay, schema::SCHEMA, test_rows};

const FEED_MESSAGES: u64 = 5;

struct Fixture {
    conn: Connection,
    topics: Topics,
    owner: IdentityId,
    agent: IdentityId,
}

impl Fixture {
    fn new() -> Self {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let topics = test_rows::project(&conn);
        let owner = test_rows::human(&conn, &topics, "admin");
        let agent = test_rows::identity(&conn, &topics, "agent", &owner);
        Self {
            conn,
            topics,
            owner,
            agent,
        }
    }

    fn row(&self, stream: Stream, seq: u64, ack_row: bool) -> OwnedMessageRow {
        let at = Timestamp::from_microsecond(i64::try_from(seq).unwrap()).unwrap();
        let id = agmen_core::new_message_id(at).unwrap();
        let envelope = json!({
            "v": 1, "id": id, "verb": "FYI", "data": {}, "body": "hello",
            "sent_at": at, "stream": stream, "seq": seq,
            "from": self.agent, "from_kind": "agent", "from_handle": "worker",
            "hub_at": at
        });
        OwnedMessageRow {
            project: self.topics.project().clone(),
            stream,
            seq: Seq(seq),
            id,
            from: self.agent.clone(),
            payload_hash: vec![1, 2, 3],
            envelope_json: envelope.to_string(),
            hub_at: at,
            ack_row,
        }
    }

    fn seed(&self, row: &OwnedMessageRow) {
        self.conn
            .execute(
                "INSERT INTO messages(project_id, stream, seq, id, from_id, payload_hash,
                                  envelope, hub_at, ack_row)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    row.project.as_str(),
                    row.stream.to_string(),
                    i64::try_from(row.seq.0).unwrap(),
                    row.id.as_str(),
                    row.from.as_str(),
                    row.payload_hash,
                    row.envelope_json,
                    row.hub_at.as_microsecond(),
                    row.ack_row
                ],
            )
            .unwrap();
    }
}

#[test]
fn insert_message_conflict_is_duplicate() {
    let f = Fixture::new();
    let row = f.row(Stream::Feed, 1, true);
    insert_message(&f.conn, 0, row.clone()).unwrap();
    assert!(matches!(
        insert_message(&f.conn, 0, row.clone()),
        Err(StoreError::Duplicate)
    ));
    let mut copy = row.clone();
    copy.seq = Seq(2);
    assert!(matches!(
        insert_message(&f.conn, 0, copy),
        Err(StoreError::Duplicate)
    ));
    let mut copy = row;
    copy.stream = Stream::Inbox(f.owner.clone());
    assert!(matches!(
        insert_message(&f.conn, 0, copy.clone()),
        Err(StoreError::Duplicate)
    ));
    copy.ack_row = false;
    insert_message(&f.conn, 0, copy).unwrap();
}

#[test]
fn insert_message_other_constraints_are_not_duplicates() {
    let f = Fixture::new();
    let row = f.row(Stream::Feed, 1, true);
    f.seed(&row);
    let collision = f.row(Stream::Feed, 1, false);
    assert!(matches!(
        insert_message(&f.conn, 0, collision),
        Err(StoreError::Backend(_))
    ));
    let mut invalid = f.row(Stream::Feed, 2, false);
    invalid.envelope_json = "invalid json".into();
    assert!(matches!(
        insert_message(&f.conn, 0, invalid),
        Err(StoreError::Backend(_))
    ));
    let mut missing_project = f.row(Stream::Feed, 2, false);
    missing_project.project = "missingproject".parse().unwrap();
    assert!(matches!(
        insert_message(&f.conn, 0, missing_project),
        Err(StoreError::Backend(_))
    ));
}

#[test]
fn page_is_the_next_run_of_seqs() {
    let f = Fixture::new();
    for seq in 1..=FEED_MESSAGES {
        f.seed(&f.row(Stream::Feed, seq, true));
    }
    f.conn
        .execute(
            "INSERT INTO stream_heads(stream,seq) VALUES (?1,?2)",
            params![
                format!("{}/feed", f.topics.project()),
                i64::try_from(FEED_MESSAGES).unwrap()
            ],
        )
        .unwrap();
    let page = replay::page(&f.conn, 0, f.topics.project().clone(), Stream::Feed, 2, 2).unwrap();
    assert_eq!(
        page.messages.iter().map(|m| m.seq.0).collect::<Vec<_>>(),
        [3, 4]
    );
    assert_eq!(page.head, 5);
    assert_eq!(page.next_after, Some(4));
    assert_eq!(page.pruned_through, 0);
    let last = replay::page(&f.conn, 0, f.topics.project().clone(), Stream::Feed, 4, 2).unwrap();
    assert_eq!(last.messages[0].seq, Seq(5));
    assert_eq!(last.next_after, None);
}

#[test]
fn empty_reads_and_invalid_page_values() {
    let f = Fixture::new();
    let page = replay::page(&f.conn, 0, f.topics.project().clone(), Stream::Feed, 0, 2).unwrap();
    assert!(page.messages.is_empty());
    assert_eq!(
        (page.head, page.pruned_through, page.next_after),
        (0, 0, None)
    );
    assert!(
        replay::page(
            &f.conn,
            0,
            f.topics.project().clone(),
            Stream::Feed,
            u64::MAX,
            2
        )
        .is_err()
    );
    let row = f.row(Stream::Feed, 1, true);
    assert_eq!(
        recorded(&f.conn, 0, f.agent.clone(), row.id.clone()).unwrap(),
        None
    );
    assert_eq!(
        parent(
            &f.conn,
            0,
            f.topics.project().clone(),
            f.agent.clone(),
            row.id
        )
        .unwrap(),
        None
    );
    assert!(
        presence::latest_hellos(&f.conn, 0, f.topics.project().clone())
            .unwrap()
            .is_empty()
    );
    let mut bad = f.row(Stream::Feed, 1, true);
    bad.envelope_json = "{}".into();
    f.seed(&bad);
    assert!(replay::page(&f.conn, 0, f.topics.project().clone(), Stream::Feed, 0, 2).is_err());
}

#[test]
fn recorded_returns_the_ack_copy() {
    let f = Fixture::new();
    let row = f.row(Stream::Feed, 1, false);
    f.seed(&row);
    let mut copy = row.clone();
    copy.stream = Stream::Inbox(f.owner.clone());
    copy.seq = Seq(7);
    copy.ack_row = true;
    f.seed(&copy);
    assert_eq!(
        recorded(&f.conn, 0, f.agent.clone(), row.id).unwrap(),
        Some((copy.stream, Seq(7), copy.payload_hash))
    );
}

#[test]
fn sender_reads_identities_and_members() {
    let f = Fixture::new();
    f.conn
        .execute(
            "UPDATE identities SET handle='worker', role='engineer',
        verbs='[\"FYI\",\"DONE\"]', edits='{}', role_sha256='digest' WHERE id=?1",
            [f.agent.as_str()],
        )
        .unwrap();
    let agent = sender(&f.conn, 0, f.topics.clone(), f.agent.clone())
        .unwrap()
        .unwrap();
    assert_eq!(agent.id, f.agent);
    assert_eq!(agent.kind, IdentityKind::Agent);
    assert_eq!(agent.handle, "worker");
    assert_eq!(agent.owner, Some(f.owner.clone()));
    assert_eq!(agent.verbs, Some(vec!["FYI".into(), "DONE".into()]));
    let human = sender(&f.conn, 0, f.topics.clone(), f.owner.clone())
        .unwrap()
        .unwrap();
    assert_eq!(human.kind, IdentityKind::Human);
    assert_eq!(human.owner, None);
    assert_eq!(human.verbs, None);
    let elsewhere = test_rows::project(&f.conn);
    let outsider = test_rows::identity(&f.conn, &elsewhere, "agent", &f.owner);
    assert_eq!(
        sender(&f.conn, 0, f.topics.clone(), outsider).unwrap(),
        None
    );
    f.conn
        .execute(
            "UPDATE identities SET revoked_at=1 WHERE id=?1",
            [f.agent.as_str()],
        )
        .unwrap();
    assert_eq!(
        sender(&f.conn, 0, f.topics.clone(), f.agent.clone()).unwrap(),
        None
    );
    assert_eq!(sender(&f.conn, 0, elsewhere, f.owner).unwrap(), None);
}

#[test]
fn project_exists_checks_tenant_and_project() {
    let f = Fixture::new();
    let elsewhere = test_rows::project(&f.conn);
    assert!(project_exists(&f.conn, 0, f.topics.clone()).unwrap());
    let wrong_tenant = Topics::new(elsewhere.tenant().clone(), f.topics.project().clone());
    assert!(!project_exists(&f.conn, 0, wrong_tenant).unwrap());
    let absent = Topics::new(f.topics.tenant().clone(), "missingproject".parse().unwrap());
    assert!(!project_exists(&f.conn, 0, absent).unwrap());
}

#[test]
fn parent_finds_the_earliest_visible_copy() {
    let f = Fixture::new();
    let mut hidden = f.row(Stream::Inbox(f.owner.clone()), 1, false);
    hidden.envelope_json = json!({"verb":"DM", "from_kind":"agent"}).to_string();
    f.seed(&hidden);
    assert_eq!(
        parent(
            &f.conn,
            0,
            f.topics.project().clone(),
            f.agent.clone(),
            hidden.id.clone()
        )
        .unwrap(),
        None
    );
    let mut visible = hidden.clone();
    visible.stream = Stream::Inbox(f.agent.clone());
    visible.hub_at = Timestamp::from_microsecond(2).unwrap();
    visible.envelope_json = json!({"verb":"ASK", "from_kind":"agent"}).to_string();
    f.seed(&visible);
    let mut feed = hidden.clone();
    feed.stream = Stream::Feed;
    feed.hub_at = Timestamp::from_microsecond(3).unwrap();
    feed.envelope_json = json!({"verb":"FYI", "from_kind":"agent"}).to_string();
    f.seed(&feed);
    // A different author with the same message id must not add streams.
    let mut other_author = hidden.clone();
    other_author.stream = Stream::Inbox("someoneelse".parse().unwrap());
    other_author.from = f.owner.clone();
    f.seed(&other_author);
    let parent = parent(
        &f.conn,
        0,
        f.topics.project().clone(),
        f.agent.clone(),
        hidden.id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(parent.from, f.agent);
    assert_eq!(parent.from_kind, IdentityKind::Agent);
    assert_eq!(parent.verb, "ASK");
    assert_eq!(
        parent.streams,
        [hidden.stream, visible.stream, feed.stream]
            .into_iter()
            .collect()
    );
}

#[test]
fn is_delegate_needs_a_live_worker() {
    let f = Fixture::new();
    let listener = test_rows::identity(&f.conn, &f.topics, "listener", &f.owner);
    let elsewhere = test_rows::project(&f.conn);
    assert!(
        !is_delegate(
            &f.conn,
            0,
            f.topics.project().clone(),
            f.agent.clone(),
            listener.clone()
        )
        .unwrap()
    );
    f.conn
        .execute(
            "UPDATE identities SET delegated_by=?1 WHERE id=?2",
            params![listener.as_str(), f.agent.as_str()],
        )
        .unwrap();
    assert!(
        is_delegate(
            &f.conn,
            0,
            f.topics.project().clone(),
            f.agent.clone(),
            listener.clone()
        )
        .unwrap()
    );
    assert!(
        !is_delegate(
            &f.conn,
            0,
            elsewhere.project().clone(),
            f.agent.clone(),
            listener.clone()
        )
        .unwrap()
    );
    assert!(
        !is_delegate(
            &f.conn,
            0,
            f.topics.project().clone(),
            f.agent.clone(),
            f.owner
        )
        .unwrap()
    );
    f.conn
        .execute(
            "UPDATE identities SET revoked_at=1 WHERE id=?1",
            [f.agent.as_str()],
        )
        .unwrap();
    assert!(
        !is_delegate(
            &f.conn,
            0,
            f.topics.project().clone(),
            f.agent.clone(),
            listener
        )
        .unwrap()
    );
}

#[test]
fn roster_rows_order_by_handle() {
    let f = Fixture::new();
    let revoked = test_rows::identity(&f.conn, &f.topics, "agent", &f.owner);
    f.conn
        .execute(
            "UPDATE identities SET revoked_at=1 WHERE id=?1",
            [revoked.as_str()],
        )
        .unwrap();
    f.conn
        .execute(
            "UPDATE identities SET handle='z-worker', agent='codex', delegated_by=?1,
        role='engineer', verbs='[]', edits='{}', role_sha256='digest' WHERE id=?2",
            params![revoked.as_str(), f.agent.as_str()],
        )
        .unwrap();
    f.conn
        .execute(
            "UPDATE humans SET handle='a-owner' WHERE id=?1",
            [f.owner.as_str()],
        )
        .unwrap();
    let rows = presence::roster_rows(&f.conn, 0, f.topics.project().clone()).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.handle.as_str()).collect::<Vec<_>>(),
        ["a-owner", "z-worker"]
    );
    assert_eq!(rows[0].kind, "human");
    assert_eq!(
        (&rows[0].role, &rows[0].agent, &rows[0].delegated_by),
        (&None, &None, &None)
    );
    assert_eq!(rows[1].role.as_deref(), Some("engineer"));
    assert_eq!(rows[1].agent.as_deref(), Some("codex"));
    assert_eq!(rows[1].delegated_by.as_deref(), Some(revoked.as_str()));
    let roster = roster(&f.conn, 0, f.topics.project().clone()).unwrap();
    assert_eq!(roster.0.len(), 2);
    assert_eq!(roster.get(&f.owner).unwrap().role, Some(MemberRole::Admin));
    let agent = roster.get(&f.agent).unwrap();
    assert_eq!(agent.owner.as_ref(), Some(&f.owner));
    assert_eq!(agent.identity_role.as_deref(), Some("engineer"));
    assert_eq!(agent.kind, IdentityKind::Agent);
}

#[test]
fn latest_hellos_keep_the_newest_per_sender() {
    let f = Fixture::new();
    for seq in 1..=FEED_MESSAGES {
        let stream = if seq == 4 {
            Stream::Inbox(f.agent.clone())
        } else {
            Stream::Feed
        };
        let mut row = f.row(stream, seq, true);
        let (verb, data) = match seq {
            1 => ("HELLO", json!({"agent":"old", "model":"old-model"})),
            2 => ("HELLO", json!({"agent":"codex", "model":"new-model"})),
            3 => {
                row.from = f.owner.clone();
                ("HELLO", json!({}))
            }
            4 => ("HELLO", json!({"agent":"private"})),
            _ => ("FYI", json!({})),
        };
        row.envelope_json = json!({"verb":verb, "data":data}).to_string();
        f.seed(&row);
    }
    let other = test_rows::project(&f.conn);
    let mut elsewhere = f.row(Stream::Feed, 10, true);
    elsewhere.project = other.project().clone();
    elsewhere.envelope_json = json!({"verb":"HELLO", "data":{"agent":"elsewhere"}}).to_string();
    f.seed(&elsewhere);
    let hellos = presence::latest_hellos(&f.conn, 0, f.topics.project().clone()).unwrap();
    assert_eq!(hellos.len(), 2);
    assert_eq!(
        hellos[&f.agent],
        (Some("codex".into()), Some("new-model".into()))
    );
    assert_eq!(hellos[&f.owner], (None, None));
}

#[test]
fn latest_hellos_query_uses_the_partial_index() {
    let f = Fixture::new();
    presence::latest_hellos(&f.conn, 0, f.topics.project().clone()).unwrap();
    let explain = format!("EXPLAIN QUERY PLAN {}", presence::LATEST_HELLOS_SQL);
    let mut stmt = f.conn.prepare(&explain).unwrap();
    const MAX_PLAN_ROWS: usize = 32;
    let details = stmt
        .query_map([f.topics.project().as_str()], |row| row.get::<_, String>(3))
        .unwrap()
        .take(MAX_PLAN_ROWS)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("messages_latest_hello")),
        "{details:?}"
    );
}

#[test]
fn ref_targets_are_sorted_scoped_and_filtered() {
    let f = Fixture::new();
    let other = test_rows::project(&f.conn);
    f.conn
        .execute(
            "INSERT INTO lanes(project_id,name,globs,v) VALUES
        (?1,'zebra','[\"z/**\"]',1), (?1,'alpha','[\"a/**\"]',1),
        (?2,'foreign','[\"f/**\"]',1)",
            params![f.topics.project().as_str(), other.project().as_str()],
        )
        .unwrap();
    f.conn
        .execute(
            "INSERT INTO items(project_id,key,title,created_by,created_at) VALUES
        (?1,'BUG-2','two',?3,0), (?1,'BUG-1','one',?3,0), (?2,'BUG-3','foreign',?3,0)",
            params![
                f.topics.project().as_str(),
                other.project().as_str(),
                f.agent.as_str()
            ],
        )
        .unwrap();
    let (lanes, keys) = ref_targets(
        &f.conn,
        0,
        f.topics.project().clone(),
        vec![
            "BUG-2".into(),
            "BUG-1".into(),
            "BUG-2".into(),
            "BUG-3".into(),
        ],
    )
    .unwrap();
    assert_eq!(lanes, ["alpha", "zebra"]);
    assert_eq!(
        keys.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["BUG-1", "BUG-2"]
    );
    assert!(
        ref_targets(&f.conn, 0, f.topics.project().clone(), vec![])
            .unwrap()
            .1
            .is_empty()
    );
}

#[test]
fn enqueue_publish_stores_nothing() {
    let f = Fixture::new();
    let count = || {
        f.conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    };
    let before = count();
    let changes = f.conn.total_changes();
    enqueue_publish(&f.conn, 0, f.topics.clone(), "feed".into(), vec![1, 2, 3]).unwrap();
    assert_eq!(count(), before);
    assert_eq!(f.conn.total_changes(), changes);
}

#[test]
fn parent_stream_lookup_uses_the_message_id_index() {
    let f = Fixture::new();
    let explain = format!("EXPLAIN QUERY PLAN {}", PARENT_STREAMS_SQL);
    let mut stmt = f.conn.prepare(&explain).unwrap();
    const MAX_PLAN_ROWS: usize = 32;
    let details = stmt
        .query_map(
            params![f.topics.project().as_str(), f.agent.as_str(), "id"],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .take(MAX_PLAN_ROWS)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("messages_by_id (project_id=? AND id=?)")),
        "{details:?}"
    );
}

#[test]
fn roster_rows_break_handle_ties_by_id() {
    let f = Fixture::new();
    f.conn
        .execute(
            "UPDATE identities SET handle='same', id='zz00000000' WHERE id=?1",
            [f.agent.as_str()],
        )
        .unwrap();
    f.conn
        .execute(
            "UPDATE humans SET handle='same' WHERE id=?1",
            [f.owner.as_str()],
        )
        .unwrap();
    let rows = presence::roster_rows(&f.conn, 0, f.topics.project().clone()).unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.identity_id.as_str())
            .collect::<Vec<_>>(),
        [f.owner.as_str(), "zz00000000"]
    );
}

#[test]
fn page_is_capped_and_scoped_to_its_project() {
    let f = Fixture::new();
    let total = u64::from(agmen_core::MAX_PAGE) + 1;
    for seq in 1..=total {
        f.seed(&f.row(Stream::Feed, seq, true));
    }
    let other = test_rows::project(&f.conn);
    let mut elsewhere = f.row(Stream::Feed, 1, true);
    elsewhere.project = other.project().clone();
    f.seed(&elsewhere);
    f.conn
        .execute(
            "INSERT INTO stream_heads(stream,seq) VALUES (?1,?2)",
            params![
                format!("{}/feed", f.topics.project()),
                i64::try_from(total).unwrap()
            ],
        )
        .unwrap();
    let page = replay::page(
        &f.conn,
        0,
        f.topics.project().clone(),
        Stream::Feed,
        0,
        u32::MAX,
    )
    .unwrap();
    let seqs: Vec<u64> = page.messages.iter().map(|m| m.seq.0).collect();
    assert_eq!(
        seqs,
        (1..=u64::from(agmen_core::MAX_PAGE)).collect::<Vec<_>>()
    );
    assert_eq!(page.next_after, Some(u64::from(agmen_core::MAX_PAGE)));
    assert_eq!(page.head, total);
}

#[test]
fn parent_ignores_copies_in_other_projects() {
    let f = Fixture::new();
    let other = test_rows::project(&f.conn);
    let mut foreign = f.row(Stream::Feed, 1, false);
    foreign.project = other.project().clone();
    foreign.envelope_json = json!({"verb":"FYI", "from_kind":"agent"}).to_string();
    f.seed(&foreign);
    assert_eq!(
        parent(
            &f.conn,
            0,
            f.topics.project().clone(),
            f.agent.clone(),
            foreign.id
        )
        .unwrap(),
        None
    );
}

#[test]
fn sender_needs_the_projects_own_tenant() {
    let f = Fixture::new();
    let elsewhere = test_rows::project(&f.conn);
    let wrong_tenant = Topics::new(elsewhere.tenant().clone(), f.topics.project().clone());
    assert_eq!(
        sender(&f.conn, 0, wrong_tenant.clone(), f.agent.clone()).unwrap(),
        None
    );
    assert_eq!(
        sender(&f.conn, 0, wrong_tenant, f.owner.clone()).unwrap(),
        None
    );
}

#[test]
fn rosters_leave_out_members_of_other_projects() {
    let f = Fixture::new();
    let other = test_rows::project(&f.conn);
    let stranger = test_rows::human(&f.conn, &other, "member");
    test_rows::identity(&f.conn, &other, "agent", &stranger);
    let rows = presence::roster_rows(&f.conn, 0, f.topics.project().clone()).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.identity_id != stranger.as_str()));
    let roster = roster(&f.conn, 0, f.topics.project().clone()).unwrap();
    assert_eq!(roster.0.len(), 2);
    assert!(roster.get(&stranger).is_none());
}
