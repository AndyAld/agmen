use std::future::ready;
use std::time::{Duration, Instant};

use agmen_proto::{
    ClientEnvelope, DeliveryReply, IdentityId, IdentityKind, NackReason, Payload, Seq, Stream,
    Timestamp, Topics, Verb,
};

use crate::{Core, Gate, MemStore, ReplyRoute, Sender, Store, StoreError, StoreTx};

#[derive(Clone)]
struct ErrorLog(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl tracing::field::Visit for ErrorLog {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        const MAX_ERRORS: usize = 1;
        if field.name() == "error" {
            let mut errors = self.0.lock().unwrap();
            assert!(errors.len() < MAX_ERRORS);
            errors.push(format!("{value:?}"));
        }
    }
}

impl tracing::Subscriber for ErrorLog {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut self.clone());
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn store_failure_logs_keep_the_hosted_error_texts() {
    for fault in [
        "sender",
        "recorded",
        "roster",
        "ref_targets",
        "begin",
        "take_quota",
        "lock_head",
        "next_seq",
        "insert_message",
        "enqueue_publish",
        "commit",
    ] {
        let (core, store, topics, from) = setup().await;
        store
            .seed_item(topics.project(), &"B-1".parse().unwrap())
            .await
            .unwrap();
        store.fail_next(fault);
        let errors = ErrorLog(std::sync::Arc::default());
        let _guard = tracing::subscriber::set_default(errors.clone());
        let payload = if fault == "lock_head" {
            Payload::Claim {
                item: "B-1".parse().unwrap(),
            }
        } else {
            Payload::Fyi {}
        };
        let handled = core
            .ingest(
                &topics,
                &from,
                &raw(&envelope(1, payload)),
                Gate::Quota { limit: 1 },
            )
            .await
            .unwrap();
        nack(&handled.reply, NackReason::HubBusy);
        let context = if matches!(fault, "lock_head" | "next_seq") {
            "numbering failed"
        } else {
            "database error"
        };
        assert_eq!(
            *errors.0.lock().unwrap(),
            vec![format!("{context}: injected {fault} failure")],
            "{fault}"
        );
    }
}

async fn setup() -> (Core<MemStore>, MemStore, Topics, IdentityId) {
    let store = MemStore::default();
    let topics = Topics::new("tenant01".parse().unwrap(), "project1".parse().unwrap());
    let from: IdentityId = "aaaaaaaa".parse().unwrap();
    store
        .seed_sender(
            &topics,
            Sender {
                id: from.clone(),
                kind: IdentityKind::Agent,
                handle: "sol".to_owned(),
                owner: Some("human001".parse().unwrap()),
                verbs: None,
            },
            None,
        )
        .await
        .unwrap();
    (
        Core::new(store.clone(), Duration::from_secs(60)),
        store,
        topics,
        from,
    )
}

fn envelope(n: u32, payload: Payload) -> ClientEnvelope {
    ClientEnvelope {
        v: 1,
        id: format!("01JBX6Q4T7W3C2M8Y9K5{n:06}").parse().unwrap(),
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: "hi".try_into().unwrap(),
        valid_until: matches!(payload, Payload::Claim { .. }).then(|| {
            Timestamp::now()
                .checked_add(Duration::from_secs(60))
                .unwrap()
        }),
        sent_at: Timestamp::now(),
        payload,
    }
}

fn raw(env: &ClientEnvelope) -> Vec<u8> {
    serde_json::to_vec(env).unwrap()
}

fn nack(reply: &DeliveryReply, reason: NackReason) {
    assert!(
        matches!(reply, DeliveryReply::Nack { reason: actual, retryable, .. }
        if *actual == reason && *retryable == reason.is_retryable()),
        "{reply:?}"
    );
}

#[tokio::test]
async fn fyi_is_numbered_and_acked() {
    let (core, store, topics, from) = setup().await;
    let env = envelope(1, Payload::Fyi {});
    let handled = core
        .ingest(&topics, &from, &raw(&env), Gate::Open)
        .await
        .unwrap();
    assert_eq!(
        handled.reply,
        DeliveryReply::Ack {
            id: env.id.clone(),
            stream: Stream::Feed,
            seq: Seq(1)
        }
    );
    assert_eq!(handled.route, ReplyRoute::Outbox);
    let meta = handled.recorded.unwrap();
    assert_eq!(
        (meta.verb, meta.from, meta.from_kind, meta.id),
        (Verb::Fyi, from.clone(), IdentityKind::Agent, env.id)
    );
    assert_eq!(store.recorded_messages().await.unwrap().len(), 1);
    let queued = store.queued_publishes().await.unwrap();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].0, topics.feed());
    assert_eq!(queued[1].0, topics.ack(&from));
    assert_eq!(
        serde_json::from_slice::<DeliveryReply>(&queued[1].1).unwrap(),
        handled.reply
    );
}

#[tokio::test]
async fn resend_gets_the_same_ack_and_changed_resend_conflicts() {
    let (core, store, topics, from) = setup().await;
    let mut env = envelope(1, Payload::Fyi {});
    let first = core
        .ingest(&topics, &from, &raw(&env), Gate::Quota { limit: 1 })
        .await
        .unwrap();
    let again = core
        .ingest(&topics, &from, &raw(&env), Gate::ReadOnly)
        .await
        .unwrap();
    assert_eq!(again.reply, first.reply);
    assert_eq!(again.route, ReplyRoute::Caller);
    assert_eq!(again.recorded, None);
    assert_eq!(again.quota_warning, None);
    env.body = "changed".try_into().unwrap();
    nack(
        &core
            .ingest(&topics, &from, &raw(&env), Gate::ReadOnly)
            .await
            .unwrap()
            .reply,
        NackReason::Conflict,
    );
    assert_eq!(store.recorded_messages().await.unwrap().len(), 1);
    assert_eq!(store.queued_publishes().await.unwrap().len(), 2);
}

#[tokio::test]
async fn duplicate_commit_answers_as_resend() {
    let (core, store, topics, from) = setup().await;
    let bytes = raw(&envelope(1, Payload::Fyi {}));
    let first = core
        .ingest(&topics, &from, &bytes, Gate::Open)
        .await
        .unwrap();
    store.hide_next_recorded();
    store.fail_next("insert_message");
    let again = core
        .ingest(&topics, &from, &bytes, Gate::Open)
        .await
        .unwrap();
    assert_eq!(again.reply, first.reply);
    assert_eq!(again.route, ReplyRoute::Caller);
    assert_eq!(again.recorded, None);
    assert_eq!(store.recorded_messages().await.unwrap().len(), 1);
    assert_eq!(store.queued_publishes().await.unwrap().len(), 2);
}

#[tokio::test]
async fn revoked_sender_is_refused() {
    let (core, store, topics, from) = setup().await;
    store.revoke_sender(topics.project(), &from).await;
    let handled = core
        .ingest(
            &topics,
            &from,
            &raw(&envelope(1, Payload::Fyi {})),
            Gate::Open,
        )
        .await
        .unwrap();
    nack(&handled.reply, NackReason::IdentityRevoked);
    assert!(store.recorded_messages().await.unwrap().is_empty());
}

#[tokio::test]
async fn secret_shaped_body_is_refused() {
    let (core, store, topics, from) = setup().await;
    let mut env = envelope(1, Payload::Fyi {});
    env.body = format!("{}{}", "ghp_", "a".repeat(36)).try_into().unwrap();
    nack(
        &core
            .ingest(&topics, &from, &raw(&env), Gate::Open)
            .await
            .unwrap()
            .reply,
        NackReason::SecretDetected,
    );
    assert!(store.recorded_messages().await.unwrap().is_empty());
    assert!(store.queued_publishes().await.unwrap().is_empty());
}

#[tokio::test]
async fn read_only_gate_lets_release_and_bye_through() {
    let (core, store, topics, from) = setup().await;
    for (n, payload) in [
        (1, Payload::Fyi {}),
        (2, Payload::Heartbeat { items: Vec::new() }),
    ] {
        nack(
            &core
                .ingest(&topics, &from, &raw(&envelope(n, payload)), Gate::ReadOnly)
                .await
                .unwrap()
                .reply,
            NackReason::ReadOnly,
        );
    }
    for (n, payload) in [
        (
            3,
            Payload::Release {
                item: "B-1".parse().unwrap(),
                reason: Some("done".to_owned()),
            },
        ),
        (4, Payload::Bye {}),
    ] {
        assert!(matches!(
            core.ingest(&topics, &from, &raw(&envelope(n, payload)), Gate::ReadOnly)
                .await
                .unwrap()
                .reply,
            DeliveryReply::Ack { .. }
        ));
    }
    assert_eq!(store.recorded_messages().await.unwrap().len(), 2);
}

#[tokio::test]
async fn quota_exhausted_is_final_with_retry_after() {
    let (core, store, topics, from) = setup().await;
    let gate = Gate::Quota { limit: 1 };
    assert!(matches!(
        core.ingest(&topics, &from, &raw(&envelope(1, Payload::Fyi {})), gate)
            .await
            .unwrap()
            .reply,
        DeliveryReply::Ack { .. }
    ));
    let handled = core
        .ingest(&topics, &from, &raw(&envelope(2, Payload::Fyi {})), gate)
        .await
        .unwrap();
    nack(&handled.reply, NackReason::QuotaExceeded);
    assert!(
        matches!(handled.reply, DeliveryReply::Nack { retry_after_ms: Some(ms), .. } if (1..=86_400_001).contains(&ms))
    );
    assert_eq!(handled.quota_warning, None);
    assert_eq!(store.recorded_messages().await.unwrap().len(), 1);
    assert_eq!(store.queued_publishes().await.unwrap().len(), 2);
}

#[tokio::test]
async fn rate_limit_nacks_the_31st_burst_message() {
    let (core, store, topics, from) = setup().await;
    let instant = Instant::now();
    for n in 1..=31 {
        let handled = core
            .ingest_at(
                &topics,
                &from,
                &raw(&envelope(n, Payload::Fyi {})),
                ready(Ok(Gate::Open)),
                || instant,
            )
            .await
            .unwrap();
        if n <= 30 {
            assert!(matches!(handled.reply, DeliveryReply::Ack { .. }));
        } else {
            nack(&handled.reply, NackReason::RateLimited);
            assert!(matches!(
                handled.reply,
                DeliveryReply::Nack {
                    retry_after_ms: Some(101),
                    ..
                }
            ));
        }
    }
    assert_eq!(store.recorded_messages().await.unwrap().len(), 30);
}

#[tokio::test]
async fn limiter_samples_time_while_holding_bucket_lock() {
    let (core, _, topics, from) = setup().await;
    let bytes = raw(&envelope(1, Payload::Fyi {}));
    let handled = core
        .ingest_at(&topics, &from, &bytes, ready(Ok(Gate::Open)), || {
            assert!(core.limiter.try_lock().is_err());
            Instant::now()
        })
        .await
        .unwrap();
    assert!(matches!(handled.reply, DeliveryReply::Ack { .. }));
}

#[tokio::test]
async fn quota_warning_fires_once_and_resets_at_utc_midnight() {
    let (core, store, topics, from) = setup().await;
    store.set_now("2026-10-29T23:59:59Z".parse().unwrap());
    for n in 1..=5 {
        let handled = core
            .ingest(
                &topics,
                &from,
                &raw(&envelope(n, Payload::Fyi {})),
                Gate::Quota { limit: 5 },
            )
            .await
            .unwrap();
        assert_eq!(handled.quota_warning, (n == 4).then_some(5));
    }
    store.set_now("2026-10-30T00:00:00Z".parse().unwrap());
    let handled = core
        .ingest(
            &topics,
            &from,
            &raw(&envelope(6, Payload::Fyi {})),
            Gate::Quota { limit: 1 },
        )
        .await
        .unwrap();
    assert!(matches!(
        handled.reply,
        DeliveryReply::Ack { seq: Seq(6), .. }
    ));
    assert_eq!(handled.quota_warning, Some(1));
}

#[tokio::test]
async fn exhausted_quota_rechecks_a_concurrent_resend() {
    let (core, store, topics, from) = setup().await;
    let bytes = raw(&envelope(1, Payload::Fyi {}));
    let gate = Gate::Quota { limit: 1 };
    let first = core.ingest(&topics, &from, &bytes, gate).await.unwrap();
    store.hide_next_recorded();
    let again = core.ingest(&topics, &from, &bytes, gate).await.unwrap();
    assert_eq!(again.reply, first.reply);
    assert_eq!(again.route, ReplyRoute::Caller);
    assert_eq!(again.quota_warning, None);
    assert_eq!(store.recorded_messages().await.unwrap().len(), 1);
}

#[tokio::test]
async fn quota_exempts_heartbeat_release_and_bye() {
    let (core, store, topics, from) = setup().await;
    store.fail_next("take_quota");
    for (n, payload) in [
        (1, Payload::Heartbeat { items: Vec::new() }),
        (
            2,
            Payload::Release {
                item: "B-1".parse().unwrap(),
                reason: Some("done".to_owned()),
            },
        ),
        (3, Payload::Bye {}),
    ] {
        assert!(matches!(
            core.ingest(
                &topics,
                &from,
                &raw(&envelope(n, payload)),
                Gate::Quota { limit: 0 }
            )
            .await
            .unwrap()
            .reply,
            DeliveryReply::Ack { .. }
        ));
    }
    nack(
        &core
            .ingest(
                &topics,
                &from,
                &raw(&envelope(4, Payload::Fyi {})),
                Gate::Quota { limit: 1 },
            )
            .await
            .unwrap()
            .reply,
        NackReason::HubBusy,
    );
}

#[tokio::test]
async fn quota_is_taken_before_the_feed_head_is_locked() {
    let (core, store, topics, from) = setup().await;
    store
        .seed_item(topics.project(), &"B-1".parse().unwrap())
        .await
        .unwrap();
    store.fail_next("lock_head");
    let claim = |n| {
        raw(&envelope(
            n,
            Payload::Claim {
                item: "B-1".parse().unwrap(),
            },
        ))
    };
    // With no message left, the refusal comes from the usage row, which every
    // transaction takes before the feed head (the hub's lock order).
    nack(
        &core
            .ingest(&topics, &from, &claim(1), Gate::Quota { limit: 0 })
            .await
            .unwrap()
            .reply,
        NackReason::QuotaExceeded,
    );
    // The armed fault was never reached: the next claim hits it.
    nack(
        &core
            .ingest(&topics, &from, &claim(2), Gate::Open)
            .await
            .unwrap()
            .reply,
        NackReason::HubBusy,
    );
}

#[tokio::test]
async fn failed_effects_roll_back_quota_messages_outbox_and_numbers() {
    for fault in [
        "begin",
        "take_quota",
        "insert_message",
        "enqueue_publish",
        "commit",
        "lock_head",
        "insert_claim",
        "insert_lend",
    ] {
        let (core, store, topics, from) = setup().await;
        let payload = if fault == "insert_lend" {
            store
                .seed_lane(topics.project(), "L-UI", &["ui/**"])
                .await
                .unwrap();
            let mut tx = store.begin().await.unwrap();
            tx.replace_lanes(
                topics.project(),
                &[crate::ResolvedLane {
                    name: "L-UI".parse().unwrap(),
                    globs: vec!["ui/**".to_owned()],
                    owner: Some(from.clone()),
                }],
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            Payload::Lend {
                lane: "L-UI".parse().unwrap(),
                to: from.clone(),
                paths: vec!["ui/**".to_owned()],
                until: Timestamp::now()
                    .checked_add(Duration::from_secs(60))
                    .unwrap(),
            }
        } else {
            store
                .seed_item(topics.project(), &"B-1".parse().unwrap())
                .await
                .unwrap();
            Payload::Claim {
                item: "B-1".parse().unwrap(),
            }
        };
        store.fail_next(fault);
        let handled = core
            .ingest(
                &topics,
                &from,
                &raw(&envelope(1, payload)),
                Gate::Quota { limit: 1 },
            )
            .await
            .unwrap();
        nack(&handled.reply, NackReason::HubBusy);
        assert_eq!(handled.recorded, None, "{fault}");
        assert_eq!(handled.quota_warning, None, "{fault}");
        assert!(
            store.recorded_messages().await.unwrap().is_empty(),
            "{fault}"
        );
        assert!(
            store.queued_publishes().await.unwrap().is_empty(),
            "{fault}"
        );
        assert!(
            matches!(
                core.ingest(
                    &topics,
                    &from,
                    &raw(&envelope(2, Payload::Fyi {})),
                    Gate::Quota { limit: 1 }
                )
                .await
                .unwrap()
                .reply,
                DeliveryReply::Ack { seq: Seq(1), .. }
            ),
            "{fault}"
        );
    }
}

#[tokio::test]
async fn gate_lookup_is_deferred_until_after_admission_and_resends() {
    let (core, _, topics, from) = setup().await;
    let env = envelope(1, Payload::Fyi {});
    let first = core
        .ingest(&topics, &from, &raw(&env), Gate::Open)
        .await
        .unwrap();
    let again = core
        .ingest_with_gate(&topics, &from, &raw(&env), async {
            panic!("resend must not load the gate")
        })
        .await
        .unwrap();
    assert_eq!(again.reply, first.reply);
    let invalid = format!(r#"{{"id":"{}","v":2}}"#, env.id);
    let handled = core
        .ingest_with_gate(&topics, &from, invalid.as_bytes(), async {
            panic!("invalid publish must not load the gate")
        })
        .await
        .unwrap();
    nack(&handled.reply, NackReason::Schema);
    let handled = core
        .ingest_with_gate(
            &topics,
            &from,
            &raw(&envelope(2, Payload::Fyi {})),
            ready(Err(StoreError::Stored("gate unavailable".to_owned()))),
        )
        .await
        .unwrap();
    nack(&handled.reply, NackReason::HubBusy);
}

#[tokio::test]
async fn unanswerable_publishes_and_project_lookup_errors_keep_their_errors() {
    let (core, store, topics, from) = setup().await;
    assert!(matches!(
        core.ingest(&topics, &from, b"{}", Gate::Open).await,
        Err(crate::IngestError::NoMessageId)
    ));
    let other = Topics::new("tenant02".parse().unwrap(), topics.project().clone());
    assert!(matches!(
        core.ingest(
            &other,
            &from,
            &raw(&envelope(1, Payload::Fyi {})),
            Gate::Open
        )
        .await,
        Err(crate::IngestError::UnknownProject)
    ));
    store.fail_next("project_exists");
    assert!(matches!(
        core.ingest(
            &topics,
            &from,
            &raw(&envelope(1, Payload::Fyi {})),
            Gate::Open
        )
        .await,
        Err(crate::IngestError::Store(_))
    ));
}

#[tokio::test]
async fn human_ack_stays_with_caller_and_unknown_verbs_are_schema() {
    let (core, store, topics, from) = setup().await;
    store
        .seed_sender(
            &topics,
            Sender {
                id: from.clone(),
                kind: IdentityKind::Human,
                handle: "andy".to_owned(),
                owner: None,
                verbs: None,
            },
            None,
        )
        .await
        .unwrap();
    let handled = core
        .ingest(
            &topics,
            &from,
            &raw(&envelope(1, Payload::Fyi {})),
            Gate::Open,
        )
        .await
        .unwrap();
    assert_eq!(handled.route, ReplyRoute::Caller);
    assert_eq!(store.queued_publishes().await.unwrap().len(), 1);
    let payload = Payload::Unknown {
        verb: "FUTURE".to_owned(),
        data: serde_json::json!({}),
    };
    nack(
        &core
            .ingest(&topics, &from, &raw(&envelope(2, payload)), Gate::Open)
            .await
            .unwrap()
            .reply,
        NackReason::Schema,
    );
}
