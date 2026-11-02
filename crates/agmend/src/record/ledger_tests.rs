use std::sync::{Arc, Mutex};
use std::time::Duration;

use agmen_core::{
    MAX_GLOBS_PER_LANE, MAX_LANES, MAX_LISTED, NewItem, NewLend, QuotaTaken, Reads, ReleaseBy,
    ResolvedLane, Store, StoreError, StoreTx,
};
use agmen_proto::{IdentityId, IdentityKind, ItemKey, LaneName, Severity, Timestamp, Topics};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::super::{SqliteStore, backend, micros, schema, test_clock, test_rows};

const ITEM_COUNT: usize = 5;
const LIVE_FOR: Duration = Duration::from_secs(60);
const AT: &str = "2026-11-02T12:00:00Z";

struct Fixture {
    _dir: TempDir,
    store: SqliteStore,
    topics: Topics,
    other: Topics,
    owner: IdentityId,
    a: IdentityId,
    b: IdentityId,
    coordinator: IdentityId,
    time: Arc<Mutex<Timestamp>>,
}

async fn fixture() -> Fixture {
    tokio::task::spawn_blocking(|| {
        let dir = tempfile::tempdir().unwrap();
        let (clock, time) = test_clock(AT.parse().unwrap());
        let store = schema::open(&dir.path().join(schema::RECORD_DB), clock).unwrap();
        let conn = store.conn.try_lock().unwrap();
        let topics = test_rows::project(&conn);
        let other = test_rows::project(&conn);
        let owner = test_rows::human(&conn, &topics, "admin");
        let a = test_rows::identity(&conn, &topics, "agent", &owner);
        let b = test_rows::identity(&conn, &topics, "agent", &owner);
        let coordinator = test_rows::identity(&conn, &topics, "coordinator", &owner);
        for number in 1..=ITEM_COUNT {
            conn.execute(
                "INSERT INTO items(project_id, key, title, lane, created_by, created_at)
                 VALUES (?1, ?2, ?2, 'L-RUST', ?3, ?4)",
                params![
                    topics.project().as_str(),
                    format!("B-{number}"),
                    a.as_str(),
                    number as i64
                ],
            )
            .unwrap();
        }
        drop(conn);
        Fixture {
            _dir: dir,
            store,
            topics,
            other,
            owner,
            a,
            b,
            coordinator,
            time,
        }
    })
    .await
    .unwrap()
}

fn key(number: usize) -> ItemKey {
    format!("B-{number}").parse().unwrap()
}

fn lanes(conn: &Connection, topics: &Topics, owner: &IdentityId) {
    for (name, globs, holder) in [
        ("L-RUST", r#"["src/**"]"#, Some(owner.as_str())),
        ("L-DOCS", r#"["docs/**"]"#, None),
    ] {
        conn.execute(
            "INSERT INTO lanes(project_id, name, globs, owner_identity_id, v)
             VALUES (?1, ?2, ?3, ?4, 1)",
            params![topics.project().as_str(), name, globs, holder],
        )
        .unwrap();
    }
}

impl Fixture {
    async fn claim(&self, number: usize, holder: &IdentityId, offset_us: i64) {
        let project = self.topics.project().clone();
        let holder = holder.clone();
        self.store
            .write(move |conn, now| {
                conn.execute(
                    "INSERT INTO claims(project_id, item_key, identity_id, granted_at, lease_until)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        project.as_str(),
                        key(number).as_str(),
                        holder.as_str(),
                        now - 10_000_000,
                        now + offset_us
                    ],
                )
                .map_err(backend)?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn lanes(&self) {
        let topics = self.topics.clone();
        let owner = self.a.clone();
        self.store
            .write(move |conn, _| {
                lanes(conn, &topics, &owner);
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn revoke(&self, id: &IdentityId) {
        let id = id.clone();
        self.store
            .write(move |conn, now| {
                conn.execute(
                    "UPDATE identities SET revoked_at=?1 WHERE id=?2",
                    params![now, id.as_str()],
                )
                .map_err(backend)?;
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn lease_until_equal_to_now_is_lapsed() {
    let f = fixture().await;
    let mut tx = f.store.begin().await.unwrap();
    let until = tx
        .insert_claim(f.topics.project(), &key(1), &f.a, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(until, AT.parse::<Timestamp>().unwrap());
    let lapsed = tx
        .release_lapsed(f.topics.project(), &key(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lapsed.holder, f.a);
    assert_eq!(lapsed.handle, f.a.as_str());
    assert_eq!(
        tx.release_lapsed(f.topics.project(), &key(1))
            .await
            .unwrap(),
        None
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn renew_skips_lapsed_claims() {
    let f = fixture().await;
    f.claim(1, &f.a, 1).await;
    f.claim(2, &f.a, 0).await;
    f.claim(3, &f.b, 1).await;
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.renew_claims(
            f.topics.project(),
            &f.a,
            &[key(1), key(1), key(2), key(3)],
            LIVE_FOR
        )
        .await
        .unwrap(),
        1
    );
    let leases: Vec<i64> = tx
        .call(|conn, _| {
            let mut stmt = conn
                .prepare("SELECT lease_until FROM claims ORDER BY item_key LIMIT 3")
                .map_err(backend)?;
            stmt.query_map([], |row| row.get(0))
                .map_err(backend)?
                .collect::<rusqlite::Result<_>>()
                .map_err(backend)
        })
        .await
        .unwrap();
    let now = micros(AT.parse().unwrap());
    assert_eq!(leases, [now + 60_000_000, now, now + 1]);
    assert_eq!(
        tx.renew_claims(f.topics.project(), &f.a, &[], LIVE_FOR)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn release_by_holder_or_coordinator() {
    let f = fixture().await;
    f.claim(1, &f.a, 1).await;
    f.claim(2, &f.a, 1).await;
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.release_claim(f.topics.project(), &key(1), &f.b, ReleaseBy::Holder)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        tx.release_claim(f.topics.project(), &key(1), &f.b, ReleaseBy::Coordinator)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        tx.release_claim(f.topics.project(), &key(1), &f.b, ReleaseBy::Coordinator)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        tx.release_claim(f.topics.project(), &key(2), &f.a, ReleaseBy::Holder)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn item_numbers_count_per_prefix() {
    let f = fixture().await;
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.next_item_number(f.topics.project(), "B").await.unwrap(),
        1
    );
    assert_eq!(
        tx.next_item_number(f.topics.project(), "B").await.unwrap(),
        2
    );
    assert_eq!(
        tx.next_item_number(f.topics.project(), "S").await.unwrap(),
        1
    );
    assert_eq!(
        tx.next_item_number(f.other.project(), "B").await.unwrap(),
        1
    );
    drop(tx);
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.next_item_number(f.topics.project(), "B").await.unwrap(),
        1
    );
}

#[tokio::test]
async fn replace_lanes_bumps_one_version() {
    let f = fixture().await;
    let lanes = [
        ResolvedLane {
            name: "L-RUST".parse().unwrap(),
            globs: vec!["src/**".into()],
            owner: Some(f.a.clone()),
        },
        ResolvedLane {
            name: "L-DOCS".parse().unwrap(),
            globs: vec!["docs/**".into()],
            owner: None,
        },
    ];
    let mut tx = f.store.begin().await.unwrap();
    tx.replace_lanes(f.topics.project(), &lanes).await.unwrap();
    tx.replace_lanes(f.topics.project(), &lanes).await.unwrap();
    tx.commit().await.unwrap();
    let rows = f
        .store
        .reader()
        .lane_list(f.topics.project())
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| (row.name.as_str(), row.v))
            .collect::<Vec<_>>(),
        [("L-DOCS", 2), ("L-RUST", 2)]
    );
    assert_eq!(rows[0].owner, None);
    assert_eq!(rows[1].owner.as_deref(), Some(f.a.as_str()));
    assert_eq!(rows[1].owner_handle.as_deref(), Some(f.a.as_str()));
    assert_eq!(rows[1].globs, ["src/**"]);
}

#[tokio::test]
async fn lends_count_only_unexpired() {
    let f = fixture().await;
    f.lanes().await;
    let mut tx = f.store.begin().await.unwrap();
    let lane: LaneName = "L-RUST".parse().unwrap();
    for (message, seconds) in [
        ("00000000000000000000000001", 0),
        ("00000000000000000000000002", 1),
    ] {
        tx.insert_lend(&NewLend {
            project: f.topics.project(),
            lane: &lane,
            message_id: &message.parse().unwrap(),
            from: &f.a,
            to: &f.b,
            paths: &["src/**".into()],
            until: AT.parse::<Timestamp>().unwrap() + Duration::from_secs(seconds),
        })
        .await
        .unwrap();
    }
    assert_eq!(
        tx.active_lends(f.topics.project(), &lane, &f.b)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        tx.active_lends(f.other.project(), &lane, &f.b)
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    assert_eq!(
        f.store
            .reader()
            .lends_to(f.topics.project(), &f.b)
            .await
            .unwrap(),
        [(lane, vec!["src/**".into()])]
    );
}

#[tokio::test]
async fn expire_claims_takes_at_most_limit() {
    let f = fixture().await;
    for number in 1..=ITEM_COUNT {
        f.claim(number, &f.a, -(number as i64)).await;
    }
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.expire_claims(0).await.unwrap().is_empty());
    let rows = tx.expire_claims(2).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().map(|row| row.item.as_str()).collect::<Vec<_>>(),
        ["B-5", "B-4"]
    );
    assert!(
        rows.iter()
            .all(|row| row.tenant == f.topics.tenant().as_str()
                && row.project == f.topics.project().as_str()
                && row.holder == f.a.as_str()
                && row.handle == f.a.as_str()
                && row.lane.as_deref() == Some("L-RUST")
                && row.open)
    );
    tx.commit().await.unwrap();
    let remaining: i64 = f
        .store
        .read(|conn, _| {
            conn.query_row(
                "SELECT count(*) FROM claims WHERE released_at IS NULL",
                [],
                |row| row.get(0),
            )
            .map_err(backend)
        })
        .await
        .unwrap();
    assert_eq!(remaining, 3);
}

#[tokio::test]
async fn quota_warns_once_at_eighty_percent() {
    const LIMIT: u32 = 10;
    let f = fixture().await;
    let mut tx = f.store.begin().await.unwrap();
    for messages in 1..=LIMIT {
        assert_eq!(
            tx.take_quota(f.topics.tenant(), LIMIT).await.unwrap(),
            QuotaTaken::Counted {
                messages,
                warn: messages == 8
            }
        );
    }
    assert_eq!(
        tx.take_quota(f.topics.tenant(), LIMIT).await.unwrap(),
        QuotaTaken::Exhausted
    );
    assert_eq!(
        tx.take_quota(f.other.tenant(), 0).await.unwrap(),
        QuotaTaken::Exhausted
    );
    assert_eq!(
        tx.take_quota(f.other.tenant(), 1).await.unwrap(),
        QuotaTaken::Counted {
            messages: 1,
            warn: true
        }
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn items_show_only_strictly_live_leases() {
    let f = fixture().await;
    f.claim(1, &f.a, 0).await;
    f.claim(2, &f.a, 1_000).await;
    let rows = f.store.reader().items(f.topics.project()).await.unwrap();
    assert_eq!(rows.len(), ITEM_COUNT);
    assert_eq!((&rows[0].7, &rows[0].8, rows[0].9), (&None, &None, None));
    assert_eq!(rows[1].7.as_deref(), Some(f.a.as_str()));
    assert_eq!(rows[1].8.as_deref(), Some(f.a.as_str()));
    assert_eq!(
        rows[1].9,
        Some(AT.parse::<Timestamp>().unwrap().as_millisecond() + 1)
    );
    *f.time.lock().unwrap() += Duration::from_millis(1);
    assert_eq!(
        f.store.reader().items(f.topics.project()).await.unwrap()[1].7,
        None
    );
}

#[tokio::test]
async fn identity_live_ignores_revoked() {
    let f = fixture().await;
    f.revoke(&f.a).await;
    let mut tx = f.store.begin().await.unwrap();
    assert!(!tx.identity_live(f.topics.project(), &f.a).await.unwrap());
    assert!(tx.identity_live(f.topics.project(), &f.b).await.unwrap());
    assert!(!tx.identity_live(f.other.project(), &f.b).await.unwrap());
    assert!(
        !tx.identity_live(f.topics.project(), &f.owner)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn assign_item_accepts_a_revoked_identity() {
    let f = fixture().await;
    f.revoke(&f.b).await;
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.assign_item(f.topics.project(), &key(1), &f.b)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        tx.lock_item(f.topics.project(), &key(1))
            .await
            .unwrap()
            .unwrap()
            .2
            .as_deref(),
        Some(f.b.as_str())
    );
    assert_eq!(
        tx.assign_item(f.topics.project(), &key(1), &f.owner)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        tx.assign_item(f.other.project(), &key(1), &f.b)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        tx.assign_item(f.topics.project(), &key(99), &f.b)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn active_claim_includes_lapsed_until_released() {
    let f = fixture().await;
    f.claim(1, &f.a, 0).await;
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.active_claim(f.topics.project(), &key(1)).await.unwrap(),
        Some((f.a.clone(), f.a.to_string()))
    );
    assert_eq!(
        tx.active_claim(f.other.project(), &key(1)).await.unwrap(),
        None
    );
    tx.release_lapsed(f.topics.project(), &key(1))
        .await
        .unwrap();
    assert_eq!(
        tx.active_claim(f.topics.project(), &key(1)).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn extend_and_close_use_the_transaction_clock() {
    let f = fixture().await;
    f.claim(1, &f.a, 1).await;
    f.claim(2, &f.a, 0).await;
    let mut tx = f.store.begin().await.unwrap();
    *f.time.lock().unwrap() += LIVE_FOR;
    assert_eq!(
        tx.extend_claim(f.topics.project(), &key(1), LIVE_FOR)
            .await
            .unwrap(),
        AT.parse::<Timestamp>().unwrap() + LIVE_FOR
    );
    assert_eq!(
        tx.release_lapsed(f.topics.project(), &key(1))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        tx.close_claim(f.topics.project(), &key(1), &f.b)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        tx.close_claim(f.topics.project(), &key(2), &f.a)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        tx.close_claim(f.topics.project(), &key(1), &f.a)
            .await
            .unwrap(),
        1
    );
    assert!(
        tx.extend_claim(f.topics.project(), &key(1), LIVE_FOR)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn items_insert_lock_and_mark_done() {
    let f = fixture().await;
    f.lanes().await;
    let mut tx = f.store.begin().await.unwrap();
    let key: ItemKey = "S-1".parse().unwrap();
    let lane: LaneName = "L-RUST".parse().unwrap();
    tx.insert_item(&NewItem {
        project: f.topics.project(),
        key: &key,
        title: "A finding",
        lane: Some(&lane),
        severity: Some(Severity::Security),
        created_by: &f.a,
    })
    .await
    .unwrap();
    assert_eq!(
        tx.lock_item(f.topics.project(), &key).await.unwrap(),
        Some((
            "open".into(),
            Some("L-RUST".into()),
            None,
            Some(f.a.to_string())
        ))
    );
    tx.mark_done(f.topics.project(), &key).await.unwrap();
    assert_eq!(
        tx.lock_item(f.topics.project(), &key)
            .await
            .unwrap()
            .unwrap()
            .0,
        "done"
    );
    assert_eq!(tx.lock_item(f.other.project(), &key).await.unwrap(), None);
    tx.commit().await.unwrap();
    let rows = f.store.reader().items(f.topics.project()).await.unwrap();
    let row = rows.iter().find(|row| row.0 == "S-1").unwrap();
    assert_eq!(
        (&row.1, row.3.as_deref(), row.5.as_str()),
        (&"A finding".into(), Some("security"), f.a.as_str())
    );
}

#[tokio::test]
async fn lane_reads_distinguish_missing_unowned_and_live_holders() {
    let f = fixture().await;
    f.lanes().await;
    f.claim(1, &f.a, 1).await;
    f.claim(2, &f.b, 0).await;
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.lock_lane_owner(f.topics.project(), &"L-MISSING".parse().unwrap())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        tx.lock_lane_owner(f.topics.project(), &"L-DOCS".parse().unwrap())
            .await
            .unwrap(),
        Some(None)
    );
    assert_eq!(
        tx.lock_lane_owner(f.topics.project(), &"L-RUST".parse().unwrap())
            .await
            .unwrap(),
        Some(Some(f.a.clone()))
    );
    let globs = tx.lane_globs(f.topics.project()).await.unwrap();
    assert_eq!(
        globs,
        [
            ("L-DOCS".parse().unwrap(), vec!["docs/**".into()]),
            ("L-RUST".parse().unwrap(), vec!["src/**".into()])
        ]
    );
    let holders = tx.lane_holders(f.topics.project()).await.unwrap();
    assert_eq!(holders.len(), 1);
    assert_eq!(holders[0].identity, f.a);
    assert_eq!(holders[0].lane.as_str(), "L-RUST");
}

#[tokio::test]
async fn identity_scope_and_owner_resolution_respect_project_and_revocation() {
    let f = fixture().await;
    let a = f.a.clone();
    let edits = serde_json::json!(["src/**"]);
    let saved = edits.clone();
    f.store.write(move |conn, _| {
        conn.execute("UPDATE identities SET role='engineer', verbs='[]', edits=?1, role_sha256='hash', handle='engineer' WHERE id=?2", params![saved.to_string(), a.as_str()]).map_err(backend)?;
        Ok(())
    }).await.unwrap();
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.resolve_owner(f.topics.project(), "engineer")
            .await
            .unwrap(),
        Some(f.a.clone())
    );
    assert_eq!(
        tx.resolve_owner(f.topics.project(), f.a.as_str())
            .await
            .unwrap(),
        Some(f.a.clone())
    );
    assert_eq!(
        tx.resolve_owner(f.other.project(), "engineer")
            .await
            .unwrap(),
        None
    );
    assert_eq!(tx.identity_edits(&f.a).await.unwrap(), Some(edits.clone()));
    assert_eq!(tx.identity_edits(&f.b).await.unwrap(), None);
    assert_eq!(
        tx.live_edit_scope(f.topics.project(), &f.a).await.unwrap(),
        Some((IdentityKind::Agent, Some(edits.clone())))
    );
    tx.commit().await.unwrap();
    f.revoke(&f.a).await;
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.resolve_owner(f.topics.project(), "engineer")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        tx.live_edit_scope(f.topics.project(), &f.a).await.unwrap(),
        None
    );
    assert_eq!(
        tx.live_edit_scope(f.other.project(), &f.b).await.unwrap(),
        None
    );
    assert_eq!(tx.identity_edits(&f.a).await.unwrap(), Some(edits));
}

#[tokio::test]
async fn lends_require_a_live_owner_or_coordinator() {
    let f = fixture().await;
    f.lanes().await;
    let project = f.topics.project().clone();
    let from = [f.a.clone(), f.b.clone(), f.coordinator.clone()];
    let to = f.b.clone();
    f.store.write(move |conn, now| {
        for id in &from {
            conn.execute("INSERT INTO lane_lends(project_id,lane,message_id,from_id,to_id,paths,until_at) VALUES (?1,'L-RUST',?2,?2,?3,'[\"src/**\"]',?4)", params![project.as_str(), id.as_str(), to.as_str(), now + 1]).map_err(backend)?;
        }
        Ok(())
    }).await.unwrap();
    assert_eq!(
        f.store
            .reader()
            .lends_to(f.topics.project(), &f.b)
            .await
            .unwrap()
            .len(),
        2
    );
    f.revoke(&f.a).await;
    assert_eq!(
        f.store
            .reader()
            .lends_to(f.topics.project(), &f.b)
            .await
            .unwrap()
            .len(),
        1
    );
    f.revoke(&f.coordinator).await;
    assert!(
        f.store
            .reader()
            .lends_to(f.topics.project(), &f.b)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn quota_rolls_back_and_changes_at_utc_midnight() {
    let f = fixture().await;
    *f.time.lock().unwrap() = "2026-11-02T23:59:59.999999Z".parse().unwrap();
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.take_quota(f.topics.tenant(), 1).await.unwrap(),
        QuotaTaken::Counted {
            messages: 1,
            warn: true
        }
    );
    drop(tx);
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.take_quota(f.topics.tenant(), 1).await.unwrap(),
        QuotaTaken::Counted {
            messages: 1,
            warn: true
        }
    );
    *f.time.lock().unwrap() += Duration::from_micros(1);
    assert_eq!(
        tx.take_quota(f.topics.tenant(), 1).await.unwrap(),
        QuotaTaken::Exhausted
    );
    tx.commit().await.unwrap();
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.take_quota(f.topics.tenant(), u32::MAX).await.unwrap(),
        QuotaTaken::Counted {
            messages: 1,
            warn: false
        }
    );
    tx.commit().await.unwrap();
    let days: Vec<String> = f
        .store
        .read(|conn, _| {
            let mut stmt = conn
                .prepare("SELECT day FROM tenant_usage ORDER BY day LIMIT 2")
                .map_err(backend)?;
            stmt.query_map([], |row| row.get(0))
                .map_err(backend)?
                .collect::<rusqlite::Result<_>>()
                .map_err(backend)
        })
        .await
        .unwrap();
    assert_eq!(days, ["2026-11-02", "2026-11-03"]);
}

#[tokio::test]
async fn stored_lane_caps_and_invalid_names_return_errors() {
    let f = fixture().await;
    assert!(
        f.store
            .reader()
            .lane_list(f.other.project())
            .await
            .unwrap()
            .is_empty()
    );
    let project = f.topics.project().clone();
    f.store.write(move |conn, _| {
        conn.execute("INSERT INTO lanes(project_id,name,globs,v) VALUES (?1,'invalid name','[\"**\"]',1)", [project.as_str()]).map_err(backend)?;
        Ok(())
    }).await.unwrap();
    assert!(matches!(
        f.store.reader().lane_list(f.topics.project()).await,
        Err(StoreError::Stored(_))
    ));
    assert!(matches!(
        f.store.reader().lane_globs(f.topics.project()).await,
        Err(StoreError::Stored(_))
    ));
    let project = f.topics.project().clone();
    f.store
        .write(move |conn, _| {
            conn.execute("DELETE FROM lanes WHERE project_id=?1", [project.as_str()])
                .map_err(backend)?;
            conn.execute(
                "INSERT INTO lanes(project_id,name,globs,v) VALUES (?1,'L-RUST',?2,1)",
                params![
                    project.as_str(),
                    serde_json::to_string(&vec!["**"; MAX_GLOBS_PER_LANE + 1]).unwrap()
                ],
            )
            .map_err(backend)?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        f.store.reader().lane_globs(f.topics.project()).await,
        Err(StoreError::Stored(_))
    ));
    let mut tx = f.store.begin().await.unwrap();
    let lane = ResolvedLane {
        name: "L-RUST".parse().unwrap(),
        globs: vec!["**".into()],
        owner: None,
    };
    assert!(matches!(
        tx.replace_lanes(f.topics.project(), &vec![lane; MAX_LANES + 1])
            .await,
        Err(StoreError::Stored(_))
    ));
}

#[tokio::test]
async fn items_are_bounded_and_ordered_by_creation_then_key() {
    let f = fixture().await;
    let project = f.topics.project().clone();
    let creator = f.a.clone();
    f.store.write(move |conn, _| {
        conn.execute("DELETE FROM items", []).map_err(backend)?;
        for number in 1..=MAX_LISTED + 1 {
            conn.execute("INSERT INTO items(project_id,key,title,created_by,created_at) VALUES (?1,?2,'title',?3,0)", params![project.as_str(), format!("B-{number}"), creator.as_str()]).map_err(backend)?;
        }
        Ok(())
    }).await.unwrap();
    let rows = f.store.reader().items(f.topics.project()).await.unwrap();
    assert_eq!(rows.len(), MAX_LISTED as usize);
    assert!(rows.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(
        f.store
            .reader()
            .items(f.other.project())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn claim_replies_round_milliseconds_and_reject_overflow() {
    let f = fixture().await;
    *f.time.lock().unwrap() += Duration::from_micros(500);
    let mut tx = f.store.begin().await.unwrap();
    let until = tx
        .insert_claim(
            f.topics.project(),
            &key(1),
            &f.a,
            Duration::from_micros(1_999),
        )
        .await
        .unwrap();
    assert_eq!(
        until,
        AT.parse::<Timestamp>().unwrap() + Duration::from_millis(2)
    );
    assert!(matches!(
        tx.extend_claim(f.topics.project(), &key(1), Duration::MAX)
            .await,
        Err(StoreError::Stored(_))
    ));
    assert!(matches!(
        tx.insert_claim(f.topics.project(), &key(2), &f.a, Duration::MAX)
            .await,
        Err(StoreError::Stored(_))
    ));
}

#[tokio::test]
async fn expire_claims_includes_a_lease_ending_exactly_now() {
    let f = fixture().await;
    f.claim(1, &f.a, 0).await;
    f.claim(2, &f.a, 1).await;
    let mut tx = f.store.begin().await.unwrap();
    let rows = tx.expire_claims(10).await.unwrap();
    assert_eq!(
        rows.iter().map(|row| row.item.as_str()).collect::<Vec<_>>(),
        ["B-1"]
    );
    assert!(tx.expire_claims(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn lane_holders_are_ordered_by_grant_then_claim_id() {
    let f = fixture().await;
    f.claim(3, &f.b, 1).await;
    f.claim(1, &f.a, 1).await;
    let holders = f
        .store
        .reader()
        .lane_holders(f.topics.project())
        .await
        .unwrap();
    assert_eq!(
        holders
            .iter()
            .map(|holder| holder.identity.clone())
            .collect::<Vec<_>>(),
        [f.b.clone(), f.a.clone()]
    );
}

#[tokio::test]
async fn two_transactions_claiming_one_item_grant_exactly_one() {
    let f = fixture().await;
    let mut first = f.store.begin().await.unwrap();
    let store = f.store.clone();
    let project = f.topics.project().clone();
    let rival = f.b.clone();
    let second = tokio::spawn(async move {
        let mut tx = store.begin().await.unwrap();
        let held = tx.active_claim(&project, &key(1)).await.unwrap();
        if held.is_none() {
            tx.insert_claim(&project, &key(1), &rival, LIVE_FOR)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        held
    });
    // Give the rival time to queue behind the open write transaction; the
    // outcome is the same if it has not, so the wait is only for coverage.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        first
            .active_claim(f.topics.project(), &key(1))
            .await
            .unwrap()
            .is_none()
    );
    first
        .insert_claim(f.topics.project(), &key(1), &f.a, LIVE_FOR)
        .await
        .unwrap();
    first.commit().await.unwrap();
    let seen = tokio::time::timeout(Duration::from_secs(10), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen, Some((f.a.clone(), f.a.to_string())));
    let active: i64 = f
        .store
        .read(|conn, _| {
            conn.query_row(
                "SELECT count(*) FROM claims WHERE released_at IS NULL",
                [],
                |row| row.get(0),
            )
            .map_err(backend)
        })
        .await
        .unwrap();
    assert_eq!(active, 1);
}
