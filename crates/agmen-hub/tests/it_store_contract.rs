//! The shared backend contract against each test's own Postgres database.
#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_core::contract::Seed;
use agmen_core::{Core, Gate, HubPost, NewItem, Reads, Store, StoreTx, post_in};
use agmen_proto::{
    ClientEnvelope, DeliveryReply, IdentityId, IdentityKind, ItemKey, Payload, Seq, Stream,
    Timestamp, Topics,
};

const CASE_WAIT: Duration = Duration::from_secs(10);
const SHORT_LEASE: Duration = Duration::from_millis(200);
const EXPIRE_WAIT: Duration = Duration::from_millis(400);

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

async fn seed_item<S: Store>(core: &Core<S>, topics: &Topics, from: &IdentityId) -> ItemKey {
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
async fn it_store_contract_on_postgres() {
    support::run(|ctx| async move {
        let hub = &ctx.hub.hub;
        let core = Core::new(hub.store_for_tests(), Duration::from_secs(60));
        agmen_core::contract::run_all(&core, hub).await;
    })
    .await;
}

#[tokio::test]
async fn it_store_contract_lease_expires_after_wait() {
    support::run(|ctx| async move {
        tokio::time::timeout(CASE_WAIT, async {
            let hub = &ctx.hub.hub;
            let core = Core::new(hub.store_for_tests(), SHORT_LEASE);
            let topics = hub.project().await;
            let first = hub.identity(&topics, IdentityKind::Agent).await;
            let second = hub.identity(&topics, IdentityKind::Agent).await;
            let key = seed_item(&core, &topics, &first).await;
            for (n, who) in [&first, &second].into_iter().enumerate() {
                if n == 1 {
                    tokio::time::sleep(EXPIRE_WAIT).await;
                }
                let env = claim(key.clone());
                let handled = core
                    .ingest(&topics, who, &serde_json::to_vec(&env).unwrap(), Gate::Open)
                    .await
                    .unwrap();
                assert!(matches!(handled.reply, DeliveryReply::Ack { .. }));
                let inbox = core
                    .page(topics.project(), &Stream::Inbox(who.clone()), 0, 4)
                    .await
                    .unwrap();
                assert_eq!(inbox.messages.len(), 1);
                assert!(matches!(&inbox.messages[0].client.payload,
                    Payload::Grant { item, .. } if item == &key));
            }
            let feed = core.page(topics.project(), &Stream::Feed, 0, 8).await.unwrap();
            assert_eq!(feed.messages.len(), 5);
            assert!(matches!(&feed.messages[3].client.payload,
                Payload::Release { item, reason } if item == &key && reason.as_deref() == Some("lease_expired")));
            assert!(matches!(&feed.messages[4].client.payload,
                Payload::Grant { item, .. } if item == &key));
        })
        .await
        .expect("lease_boundary_is_inclusive: Postgres deadline");
    })
    .await;
}

#[tokio::test]
async fn it_store_contract_dropped_transaction_records_nothing() {
    support::run(|ctx| async move {
        tokio::time::timeout(CASE_WAIT, async {
            let hub = &ctx.hub.hub;
            let core = Core::new(hub.store_for_tests(), CASE_WAIT);
            let topics = hub.project().await;
            let from = hub.identity(&topics, IdentityKind::Agent).await;
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
            // A new transaction waits for the dropped one's rollback, including
            // when sqlx returns that connection to the pool asynchronously.
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
            let counts: (i64, i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM messages WHERE project_id = $1),
                        (SELECT count(*) FROM publish_outbox WHERE project_id = $1),
                        (SELECT count(*) FROM claims WHERE project_id = $1)",
            )
            .bind(topics.project().as_str())
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
            assert_eq!(
                counts,
                (0, 0, 0),
                "message, outbox and claim must roll back"
            );
            let page = core
                .page(topics.project(), &Stream::Feed, 0, 1)
                .await
                .unwrap();
            assert!(page.messages.is_empty());
            assert_eq!(page.head, 0);
        })
        .await
        .expect("dropped_transaction_records_nothing: Postgres deadline");
    })
    .await;
}
