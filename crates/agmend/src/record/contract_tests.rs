//! The shared backend contract and transaction checks on SQLite.

use std::sync::Arc;
use std::time::Duration;

use agmen_core::contract::{Seed, run_all};
use agmen_core::{Core, Gate, HubPost, MemberRole, NewItem, Reads, Store, StoreTx, post_in};
use agmen_proto::{
    ClientEnvelope, DeliveryReply, IdentityId, IdentityKind, ItemKey, Payload, Seq, Stream,
    Timestamp, Topics,
};

use rusqlite::params;
use tempfile::TempDir;

use super::{SqliteStore, backend, schema, test_rows};

const CASE_WAIT: Duration = Duration::from_secs(10);
const CLAIM_LEASE: Duration = Duration::from_secs(60);
const SHORT_LEASE: Duration = Duration::from_millis(200);
const EXPIRE_WAIT: Duration = Duration::from_millis(400);
const CLAIM_ATTEMPTS: usize = 2;
const INBOX_LIMIT: u32 = 4;
const FEED_LIMIT: u32 = 8;

#[derive(Debug)]
struct SqliteSeed {
    store: SqliteStore,
}

impl Seed for SqliteSeed {
    async fn project(&self) -> Topics {
        self.store.create_person("contract").await.unwrap();
        let name = agmen_core::new_message_id(Timestamp::now()).unwrap();
        self.store.create_project(name.as_str()).await.unwrap()
    }

    async fn identity(&self, topics: &Topics, kind: IdentityKind) -> IdentityId {
        assert!(matches!(
            kind,
            IdentityKind::Agent | IdentityKind::Coordinator | IdentityKind::Listener
        ));
        let owner = self.human(topics, kind == IdentityKind::Coordinator).await;
        let topics = topics.clone();
        self.store
            .write(move |conn, _| {
                let number: i64 = conn
                    .query_row("SELECT count(*) FROM identities", [], |row| row.get(0))
                    .map_err(backend)?;
                // Local minting only serves the local person. Contracts need
                // identities owned by distinct members, as on the hub.
                let id = test_rows::identity(conn, &topics, kind.as_str(), &owner);
                conn.execute(
                    "UPDATE identities
                     SET handle=(SELECT handle FROM humans WHERE id=?1) || '/c' || ?2
                     WHERE id=?3",
                    params![owner.as_str(), number, id.as_str()],
                )
                .map_err(backend)?;
                Ok(id)
            })
            .await
            .unwrap()
    }

    async fn human(&self, topics: &Topics, coordinator: bool) -> IdentityId {
        self.store
            .add_member(
                topics,
                if coordinator {
                    MemberRole::Coordinator
                } else {
                    MemberRole::Member
                },
            )
            .await
            .unwrap()
    }
}

async fn seed() -> (TempDir, SqliteSeed) {
    tokio::time::timeout(
        CASE_WAIT,
        tokio::task::spawn_blocking(|| {
            let dir = tempfile::tempdir().unwrap();
            let store = schema::open(
                &dir.path().join(schema::RECORD_DB),
                Arc::new(Timestamp::now),
            )
            .unwrap();
            (dir, SqliteSeed { store })
        }),
    )
    .await
    .expect("SQLite contract fixture deadline")
    .unwrap()
}

fn claim(item: ItemKey) -> ClientEnvelope {
    let now = Timestamp::now();
    ClientEnvelope {
        v: 1,
        id: agmen_core::new_message_id(now).unwrap(),
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: "contract claim".try_into().unwrap(),
        valid_until: Some(now + CASE_WAIT),
        sent_at: now,
        payload: Payload::Claim { item },
    }
}

async fn seed_item(core: &Core<SqliteStore>, topics: &Topics, from: &IdentityId) -> ItemKey {
    let key = "B-1".parse().unwrap();
    let mut tx = core.store().begin().await.unwrap();
    tx.insert_item(&NewItem {
        project: topics.project(),
        key: &key,
        title: "contract item",
        lane: None,
        severity: None,
        created_by: from,
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    key
}

#[tokio::test]
async fn contract_on_sqlite() {
    let (_dir, seed) = seed().await;
    let core = Core::new(seed.store.clone(), CLAIM_LEASE);
    run_all(&core, &seed).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn contract_on_sqlite_multi_thread() {
    let (_dir, seed) = seed().await;
    let core = Core::new(seed.store.clone(), CLAIM_LEASE);
    run_all(&core, &seed).await;
}

#[tokio::test]
async fn lease_expires_after_wait_on_sqlite() {
    tokio::time::timeout(CASE_WAIT, async {
        let (_dir, seed) = seed().await;
        let core = Core::new(seed.store.clone(), SHORT_LEASE);
        let topics = seed.project().await;
        let claimants: [IdentityId; CLAIM_ATTEMPTS] = [
            seed.identity(&topics, IdentityKind::Agent).await,
            seed.identity(&topics, IdentityKind::Agent).await,
        ];
        let key = seed_item(&core, &topics, &claimants[0]).await;
        for (n, who) in claimants.iter().enumerate() {
            if n == 1 {
                tokio::time::sleep(EXPIRE_WAIT).await;
            }
            let handled = core
                .ingest(
                    &topics,
                    who,
                    &serde_json::to_vec(&claim(key.clone())).unwrap(),
                    Gate::Open,
                )
                .await
                .unwrap();
            assert!(matches!(handled.reply, DeliveryReply::Ack { .. }));
            let inbox = core
                .page(
                    topics.project(),
                    &Stream::Inbox(who.clone()),
                    0,
                    INBOX_LIMIT,
                )
                .await
                .unwrap();
            assert_eq!(inbox.messages.len(), 1);
            assert!(matches!(&inbox.messages[0].client.payload,
                Payload::Grant { item, .. } if item == &key));
        }
        let feed = core
            .page(topics.project(), &Stream::Feed, 0, FEED_LIMIT)
            .await
            .unwrap();
        assert_eq!(feed.messages.len(), 5);
        assert!(matches!(&feed.messages[3].client.payload,
            Payload::Release { item, reason }
                if item == &key && reason.as_deref() == Some("lease_expired")));
        assert!(matches!(&feed.messages[4].client.payload,
            Payload::Grant { item, .. } if item == &key));
    })
    .await
    .expect("lease_expires_after_wait_on_sqlite: SQLite deadline");
}

#[tokio::test]
async fn dropped_transaction_records_nothing_on_sqlite() {
    tokio::time::timeout(CASE_WAIT, async {
        let (dir, seed) = seed().await;
        let core = Core::new(seed.store.clone(), CASE_WAIT);
        let topics = seed.project().await;
        let from = seed.identity(&topics, IdentityKind::Agent).await;
        let key = seed_item(&core, &topics, &from).await;
        let mut tx = core.store().begin().await.unwrap();
        post_in(
            &mut tx,
            &topics,
            HubPost {
                payload: Payload::Status {
                    summary: "uncommitted".into(),
                    items: Vec::new(),
                    spend_usd: None,
                    hours: None,
                    tokens: None,
                },
                body: "uncommitted".try_into().unwrap(),
                reply_to: None,
                items: Vec::new(),
                lane: None,
                mentioned: Vec::new(),
                streams: vec![Stream::Feed],
            },
        )
        .await
        .unwrap();
        tx.insert_claim(topics.project(), &key, &from, CASE_WAIT)
            .await
            .unwrap();
        assert_eq!(
            tx.page(topics.project(), &Stream::Feed, 0, 1)
                .await
                .unwrap()
                .messages
                .len(),
            1
        );
        assert_eq!(
            tx.active_claim(topics.project(), &key)
                .await
                .unwrap()
                .unwrap()
                .0,
            from
        );
        drop(tx);
        let mut tx = core.store().begin().await.unwrap();
        assert!(
            tx.active_claim(topics.project(), &key)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            tx.next_seq(&format!("{}/feed", topics.project()))
                .await
                .unwrap(),
            Seq(1)
        );
        drop(tx);
        let path = dir.path().join(super::schema::RECORD_DB);
        let project = topics.project().clone();
        let counts = tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(path).unwrap();
            conn.busy_timeout(CASE_WAIT).unwrap();
            conn.query_row(
                "SELECT (SELECT count(*) FROM messages WHERE project_id=?1),
                        (SELECT count(*) FROM claims WHERE project_id=?1)",
                [project.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(counts, (0, 0), "message and claim must roll back");
        let page = core
            .page(topics.project(), &Stream::Feed, 0, 1)
            .await
            .unwrap();
        assert!(page.messages.is_empty());
        assert_eq!(page.head, 0);
    })
    .await
    .expect("dropped_transaction_records_nothing_on_sqlite: SQLite deadline");
}
