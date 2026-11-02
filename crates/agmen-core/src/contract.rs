//! Backend contract tests shared by the hub and future solo stores.

use std::fmt::Debug;
use std::future::Future;
use std::time::Duration;

use agmen_proto::{
    Change, ClientEnvelope, DeliveryReply, DenyReason, IdentityId, IdentityKind, ItemKey, LaneName,
    NackReason, Payload, Seq, StampedEnvelope, Stream, Timestamp, Topics,
};

use crate::{Core, Gate, LaneSpec, NewItem, ReplyRoute, Status, Store, StoreTx};

const CASE_WAIT: Duration = Duration::from_secs(10);
const CLAIMANTS: usize = 8;
const NUMBERED: usize = 4;
const PAGE_SIZE: u32 = 2;
const READ_LIMIT: u32 = 64;
const VALID_FOR: Duration = Duration::from_secs(60);

/// Backend-specific fixtures for the shared contracts. Only test code implements this.
pub trait Seed: Sync {
    /// Creates a fresh, empty project.
    fn project(&self) -> impl Future<Output = Topics> + Send;

    /// Creates a live agent, coordinator or listener with its owning human.
    fn identity(
        &self,
        topics: &Topics,
        kind: IdentityKind,
    ) -> impl Future<Output = IdentityId> + Send;

    /// Creates a human member, optionally with the coordinator membership role.
    fn human(&self, topics: &Topics, coordinator: bool) -> impl Future<Output = IdentityId> + Send;
}

/// Runs all storage contracts against fresh projects from `seed`.
///
/// Each case has a ten second deadline, including fixture creation.
///
/// # Panics
/// Panics with the case name on a timeout, storage error or failed assertion.
///
/// # Examples
/// ```no_run
/// # async fn example<S: agmen_core::Store, D: agmen_core::contract::Seed>(
/// #     core: &agmen_core::Core<S>, seed: &D,
/// # ) {
/// agmen_core::contract::run_all(core, seed).await;
/// # }
/// ```
pub async fn run_all<S: Store, D: Seed>(core: &Core<S>, seed: &D) {
    case(
        "numbering_is_gapless_per_stream",
        numbering_is_gapless_per_stream(core, seed),
    )
    .await;
    case("resend_is_idempotent", resend_is_idempotent(core, seed)).await;
    case(
        "dm_stays_out_of_the_feed",
        dm_stays_out_of_the_feed(core, seed),
    )
    .await;
    case(
        "concurrent_claims_grant_one",
        concurrent_claims_grant_one(core, seed),
    )
    .await;
    case(
        "lease_boundary_is_inclusive",
        lease_boundary_is_inclusive(core, seed),
    )
    .await;
    case(
        "lend_then_check_path_allows",
        lend_then_check_path_allows(core, seed),
    )
    .await;
    case(
        "done_outside_lanes_alerts",
        done_outside_lanes_alerts(core, seed),
    )
    .await;
    case(
        "rollcall_lists_who_is_here",
        rollcall_lists_who_is_here(core, seed),
    )
    .await;
    case(
        "page_matches_what_was_numbered",
        page_matches_what_was_numbered(core, seed),
    )
    .await;
}

async fn case(name: &str, future: impl Future<Output = ()>) {
    let mut future = std::pin::pin!(tokio::time::timeout(CASE_WAIT, future));
    let result = std::future::poll_fn(|context| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            future.as_mut().poll(context)
        })) {
            Ok(progress) => {
                progress.map(|result| result.map_err(|_| "ten second deadline exceeded"))
            }
            // The panic hook prints the original failure. Add the case name
            // even when a backend's fixture panics before its first assertion.
            Err(_) => std::task::Poll::Ready(Err("case panicked; see the original failure")),
        }
    })
    .await;
    checked(name, result);
}

#[expect(
    clippy::expect_used,
    reason = "test-only contract failures name the case"
)]
fn checked<T, E: Debug>(name: &str, result: Result<T, E>) -> T {
    result.expect(name)
}

fn envelope(payload: Payload) -> ClientEnvelope {
    let now = Timestamp::now();
    ClientEnvelope {
        v: 1,
        id: checked("envelope", crate::new_message_id(now)),
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: checked("envelope", "contract".try_into()),
        valid_until: matches!(payload, Payload::Claim { .. }).then_some(now + VALID_FOR),
        sent_at: now,
        payload,
    }
}

async fn send<S: Store>(
    name: &str,
    core: &Core<S>,
    topics: &Topics,
    from: &IdentityId,
    env: &ClientEnvelope,
) -> DeliveryReply {
    let bytes = checked(name, serde_json::to_vec(env));
    let handled = checked(name, core.ingest(topics, from, &bytes, Gate::Open).await);
    assert!(
        matches!(handled.reply, DeliveryReply::Ack { .. }),
        "{name}: {handled:?}"
    );
    handled.reply
}

async fn page<S: Store>(
    name: &str,
    core: &Core<S>,
    topics: &Topics,
    stream: &Stream,
) -> Vec<StampedEnvelope> {
    let page = checked(
        name,
        core.page(topics.project(), stream, 0, READ_LIMIT).await,
    );
    assert_eq!(
        page.next_after, None,
        "{name}: contract fixture exceeded READ_LIMIT"
    );
    page.messages
}

async fn item<S: Store>(
    name: &str,
    core: &Core<S>,
    topics: &Topics,
    from: &IdentityId,
    lane: Option<&LaneName>,
) -> ItemKey {
    let key = checked(name, "B-1".parse());
    let mut tx = checked(name, core.store().begin().await);
    checked(
        name,
        tx.insert_item(&NewItem {
            project: topics.project(),
            key: &key,
            title: "contract item",
            lane,
            severity: None,
            created_by: from,
        })
        .await,
    );
    checked(name, tx.commit().await);
    key
}

async fn numbering_is_gapless_per_stream<S: Store, D: Seed>(core: &Core<S>, seed: &D) {
    const NAME: &str = "numbering_is_gapless_per_stream";
    let topics = seed.project().await;
    let from = seed.identity(&topics, IdentityKind::Agent).await;
    let to = seed.identity(&topics, IdentityKind::Agent).await;
    let mut rolled_back = checked(NAME, core.store().begin().await);
    assert_eq!(
        checked(
            NAME,
            rolled_back
                .next_seq(&crate::record::head_key(&topics, &Stream::Feed))
                .await
        ),
        Seq(1),
        "{NAME}"
    );
    drop(rolled_back);
    for n in 1..=NUMBERED {
        assert!(
            matches!(send(NAME, core, &topics, &from, &envelope(Payload::Fyi {})).await,
            DeliveryReply::Ack { stream: Stream::Feed, seq, .. } if seq.0 == n as u64),
            "{NAME}"
        );
        let mut dm = envelope(Payload::Dm {});
        dm.to.push(to.clone());
        assert!(
            matches!(send(NAME, core, &topics, &from, &dm).await,
            DeliveryReply::Ack { stream: Stream::Inbox(ref who), seq, .. } if who == &from && seq.0 == n as u64),
            "{NAME}"
        );
    }
    for stream in [Stream::Feed, Stream::Inbox(to), Stream::Inbox(from)] {
        let rows = page(NAME, core, &topics, &stream).await;
        assert_eq!(
            rows.iter().map(|m| m.seq.0).collect::<Vec<_>>(),
            [1, 2, 3, 4],
            "{NAME}: {stream}"
        );
    }
    let other = seed.project().await;
    let who = seed.identity(&other, IdentityKind::Agent).await;
    assert!(
        matches!(
            send(NAME, core, &other, &who, &envelope(Payload::Fyi {})).await,
            DeliveryReply::Ack { seq: Seq(1), .. }
        ),
        "{NAME}: project scope"
    );
}

async fn resend_is_idempotent<S: Store, D: Seed>(core: &Core<S>, seed: &D) {
    const NAME: &str = "resend_is_idempotent";
    let topics = seed.project().await;
    let from = seed.identity(&topics, IdentityKind::Agent).await;
    let mut env = envelope(Payload::Fyi {});
    let first = send(NAME, core, &topics, &from, &env).await;
    let bytes = checked(NAME, serde_json::to_vec(&env));
    let resend = checked(
        NAME,
        core.ingest(&topics, &from, &bytes, Gate::ReadOnly).await,
    );
    assert_eq!(resend.reply, first, "{NAME}");
    assert_eq!(resend.route, ReplyRoute::Caller, "{NAME}");
    assert_eq!(resend.recorded, None, "{NAME}");
    env.body = checked(NAME, "changed".try_into());
    let changed = checked(
        NAME,
        core.ingest(
            &topics,
            &from,
            &checked(NAME, serde_json::to_vec(&env)),
            Gate::Open,
        )
        .await,
    );
    assert!(
        matches!(
            changed.reply,
            DeliveryReply::Nack {
                reason: NackReason::Conflict,
                retryable: false,
                ..
            }
        ),
        "{NAME}"
    );
    assert_eq!(
        page(NAME, core, &topics, &Stream::Feed).await.len(),
        1,
        "{NAME}"
    );

    let racing = envelope(Payload::Fyi {});
    let bytes = checked(NAME, serde_json::to_vec(&racing));
    let barrier = tokio::sync::Barrier::new(2);
    let copy = || async {
        barrier.wait().await;
        checked(NAME, core.ingest(&topics, &from, &bytes, Gate::Open).await)
    };
    let (a, b) = tokio::join!(copy(), copy());
    assert_eq!(a.reply, b.reply, "{NAME}: racing copies");
    assert!(
        matches!(a.reply, DeliveryReply::Ack { seq: Seq(2), .. }),
        "{NAME}"
    );
    assert_eq!(
        usize::from(a.recorded.is_some()) + usize::from(b.recorded.is_some()),
        1,
        "{NAME}"
    );
    assert_eq!(
        page(NAME, core, &topics, &Stream::Feed).await.len(),
        2,
        "{NAME}"
    );
    assert!(
        matches!(
            send(NAME, core, &topics, &from, &envelope(Payload::Fyi {})).await,
            DeliveryReply::Ack { seq: Seq(3), .. }
        ),
        "{NAME}: duplicate returned its number"
    );
}

async fn dm_stays_out_of_the_feed<S: Store, D: Seed>(core: &Core<S>, seed: &D) {
    const NAME: &str = "dm_stays_out_of_the_feed";
    let topics = seed.project().await;
    let from = seed.identity(&topics, IdentityKind::Agent).await;
    let to = seed.identity(&topics, IdentityKind::Agent).await;
    let stranger = seed.identity(&topics, IdentityKind::Agent).await;
    let mut env = envelope(Payload::Dm {});
    env.to.push(to.clone());
    send(NAME, core, &topics, &from, &env).await;
    for who in [from.clone(), to.clone()] {
        let rows = page(NAME, core, &topics, &Stream::Inbox(who)).await;
        assert_eq!(rows.len(), 1, "{NAME}");
        assert_eq!(rows[0].client.id, env.id, "{NAME}");
        assert_eq!(rows[0].from, from, "{NAME}");
    }
    for stream in [Stream::Feed, Stream::Inbox(stranger)] {
        assert!(
            page(NAME, core, &topics, &stream).await.is_empty(),
            "{NAME}"
        );
    }
}

async fn concurrent_claims_grant_one<S: Store, D: Seed>(core: &Core<S>, seed: &D) {
    const NAME: &str = "concurrent_claims_grant_one";
    let topics = seed.project().await;
    let mut agents = Vec::with_capacity(CLAIMANTS);
    for _ in 0..CLAIMANTS {
        agents.push(seed.identity(&topics, IdentityKind::Agent).await);
    }
    let key = item(NAME, core, &topics, &agents[0], None).await;
    let barrier = tokio::sync::Barrier::new(CLAIMANTS);
    // Joined tasks borrow the same core; the barrier admits all eight together.
    let claim = |n: usize| {
        let from = &agents[n];
        let topics = &topics;
        let key = &key;
        let barrier = &barrier;
        async move {
            let env = envelope(Payload::Claim { item: key.clone() });
            barrier.wait().await;
            send(NAME, core, topics, from, &env).await;
            env.id
        }
    };
    let ids = tokio::join!(
        claim(0),
        claim(1),
        claim(2),
        claim(3),
        claim(4),
        claim(5),
        claim(6),
        claim(7)
    );
    let ids = [ids.0, ids.1, ids.2, ids.3, ids.4, ids.5, ids.6, ids.7];
    let feed = page(NAME, core, &topics, &Stream::Feed).await;
    assert_eq!(feed.len(), CLAIMANTS * 2, "{NAME}");
    let mut grants = 0;
    for (agent, id) in agents.iter().zip(ids) {
        let inbox = page(NAME, core, &topics, &Stream::Inbox(agent.clone())).await;
        assert_eq!(inbox.len(), 1, "{NAME}");
        assert_eq!(inbox[0].client.reply_to.as_ref(), Some(&id), "{NAME}");
        let answer = &inbox[0];
        assert!(
            matches!(&answer.client.payload,
            Payload::Grant { item, .. } | Payload::Deny { item, reason: DenyReason::HeldBy }
            if item == &key),
            "{NAME}: unexpected answer {answer:?}"
        );
        grants += usize::from(matches!(answer.client.payload, Payload::Grant { .. }));
        assert_eq!(
            feed.iter()
                .filter(|m| m.client.id == answer.client.id)
                .count(),
            1,
            "{NAME}"
        );
    }
    assert_eq!(grants, 1, "{NAME}: seven held_by denials required");
    let items = checked(NAME, core.items(topics.project()).await);
    assert_eq!(items.items.len(), 1, "{NAME}");
    assert!(items.items[0].holder.is_some(), "{NAME}");
}

async fn lease_boundary_is_inclusive<S: Store, D: Seed>(core: &Core<S>, seed: &D) {
    const NAME: &str = "lease_boundary_is_inclusive";
    let topics = seed.project().await;
    let from = seed.identity(&topics, IdentityKind::Agent).await;
    let key = item(NAME, core, &topics, &from, None).await;
    let mut tx = checked(NAME, core.store().begin().await);
    checked(
        NAME,
        tx.insert_claim(topics.project(), &key, &from, VALID_FOR)
            .await,
    );
    assert!(
        checked(NAME, tx.release_lapsed(topics.project(), &key).await).is_none(),
        "{NAME}: a future lease stays live"
    );
    // Both backends hold their transaction clock fixed, so a zero extension
    // exercises equality without relying on wall-clock scheduling precision.
    checked(
        NAME,
        tx.extend_claim(topics.project(), &key, Duration::ZERO)
            .await,
    );
    let released = checked(NAME, tx.release_lapsed(topics.project(), &key).await);
    assert_eq!(
        released.map(|claim| claim.holder),
        Some(from),
        "{NAME}: equality releases"
    );
    assert!(
        checked(NAME, tx.release_lapsed(topics.project(), &key).await).is_none(),
        "{NAME}: release only once"
    );
    checked(NAME, tx.commit().await);
    let next = seed.identity(&topics, IdentityKind::Agent).await;
    send(
        NAME,
        core,
        &topics,
        &next,
        &envelope(Payload::Claim { item: key.clone() }),
    )
    .await;
    let inbox = page(NAME, core, &topics, &Stream::Inbox(next)).await;
    assert!(
        matches!(&inbox[0].client.payload, Payload::Grant { item, .. } if item == &key),
        "{NAME}"
    );
}

async fn lanes<S: Store>(
    name: &str,
    core: &Core<S>,
    topics: &Topics,
    owner: &IdentityId,
) -> LaneName {
    let lane: LaneName = checked(name, "L-UI".parse());
    checked(
        name,
        core.replace_lanes(
            topics.project(),
            IdentityKind::Coordinator,
            vec![LaneSpec {
                name: lane.clone(),
                globs: vec!["ui/**".into()],
                owner: Some(owner.to_string()),
            }],
        )
        .await,
    );
    lane
}

async fn lend_then_check_path_allows<S: Store, D: Seed>(core: &Core<S>, seed: &D) {
    const NAME: &str = "lend_then_check_path_allows";
    let topics = seed.project().await;
    let owner = seed.identity(&topics, IdentityKind::Agent).await;
    let borrower = seed.identity(&topics, IdentityKind::Agent).await;
    let lane = lanes(NAME, core, &topics, &owner).await;
    let checks = || {
        core.check_path(
            topics.project(),
            &borrower,
            vec!["ui/a.rs".into(), "ui/b.rs".into()],
        )
    };
    let before = checked(NAME, checks().await);
    assert!(
        before.is_some_and(|rows| rows.len() == 2 && rows.iter().all(|row| !row.allowed)),
        "{NAME}"
    );
    send(
        NAME,
        core,
        &topics,
        &owner,
        &envelope(Payload::Lend {
            lane,
            paths: vec!["ui/a.rs".into()],
            to: borrower.clone(),
            until: Timestamp::now() + VALID_FOR,
        }),
    )
    .await;
    let after = checked(NAME, checks().await);
    assert!(
        after.is_some_and(|rows| rows.len() == 2 && rows[0].allowed && !rows[1].allowed),
        "{NAME}"
    );
}

async fn done_outside_lanes_alerts<S: Store, D: Seed>(core: &Core<S>, seed: &D) {
    const NAME: &str = "done_outside_lanes_alerts";
    let topics = seed.project().await;
    let coordinator = seed.human(&topics, true).await;
    let from = seed.identity(&topics, IdentityKind::Agent).await;
    let owner = seed.identity(&topics, IdentityKind::Agent).await;
    lanes(NAME, core, &topics, &owner).await;
    let key = item(NAME, core, &topics, &from, None).await;
    send(
        NAME,
        core,
        &topics,
        &from,
        &envelope(Payload::Claim { item: key.clone() }),
    )
    .await;
    let done = envelope(Payload::Done {
        item: key.clone(),
        branch: "contract".into(),
        commit: checked(NAME, "a".repeat(40).parse()),
        base_sha: checked(NAME, "b".repeat(40).parse()),
        checks: Vec::new(),
        changes: vec![Change {
            status: "M".into(),
            path: "ui/a.rs".into(),
        }],
    });
    send(NAME, core, &topics, &from, &done).await;
    let feed = page(NAME, core, &topics, &Stream::Feed).await;
    let alerts: Vec<_> = feed
        .iter()
        .filter(|m| matches!(m.client.payload, Payload::Alert { .. }))
        .collect();
    assert_eq!(alerts.len(), 1, "{NAME}");
    assert_eq!(alerts[0].client.reply_to.as_ref(), Some(&done.id), "{NAME}");
    assert!(alerts[0].client.body.as_str().contains("ui/a.rs"), "{NAME}");
    assert!(alerts[0].mentioned.contains(&coordinator), "{NAME}");
    let items = checked(NAME, core.items(topics.project()).await);
    assert_eq!(items.items[0].state, "done", "{NAME}");
    assert!(items.items[0].holder.is_none(), "{NAME}");
}

async fn rollcall_lists_who_is_here<S: Store, D: Seed>(core: &Core<S>, seed: &D) {
    const NAME: &str = "rollcall_lists_who_is_here";
    let topics = seed.project().await;
    let asker = seed.human(&topics, false).await;
    let online = seed.identity(&topics, IdentityKind::Agent).await;
    let away = seed.identity(&topics, IdentityKind::Listener).await;
    let offline = seed.identity(&topics, IdentityKind::Coordinator).await;
    let empty = envelope(Payload::Rollcall {});
    send(NAME, core, &topics, &asker, &empty).await;
    checked(
        NAME,
        core.answer_rollcall(&topics, &empty.id, &asker, IdentityKind::Human)
            .await,
    );
    let rows = page(NAME, core, &topics, &Stream::Feed).await;
    assert!(
        matches!(&rows[1].client.payload, Payload::Status { summary, .. } if summary == "Nobody is online."),
        "{NAME}"
    );
    core.statuses()
        .set(topics.project(), &online, Status::Online);
    core.statuses().set(topics.project(), &away, Status::Away);
    let roster = checked(NAME, core.roster(&topics).await);
    let here: Vec<_> = roster
        .iter()
        .filter(|entry| entry.status != Status::Offline)
        .collect();
    assert_eq!(here.len(), 2, "{NAME}");
    let ask = envelope(Payload::Rollcall {});
    send(NAME, core, &topics, &asker, &ask).await;
    checked(
        NAME,
        core.answer_rollcall(&topics, &ask.id, &asker, IdentityKind::Human)
            .await,
    );
    let rows = page(NAME, core, &topics, &Stream::Feed).await;
    let answer = &rows[3];
    assert_eq!(answer.client.reply_to.as_ref(), Some(&ask.id), "{NAME}");
    assert_eq!(answer.mentioned, vec![asker], "{NAME}");
    let names: Vec<_> = here
        .iter()
        .map(|entry| {
            if entry.status == Status::Away {
                format!("{} (away)", entry.handle)
            } else {
                entry.handle.clone()
            }
        })
        .collect();
    assert!(
        matches!(&answer.client.payload, Payload::Status { summary, .. } if summary == &format!("2 online: {}", names.join(", "))),
        "{NAME}"
    );
    for who in [online, away] {
        assert!(answer.client.body.as_str().contains(who.as_str()), "{NAME}");
    }
    assert!(
        !answer.client.body.as_str().contains(offline.as_str()),
        "{NAME}"
    );
}

async fn page_matches_what_was_numbered<S: Store, D: Seed>(core: &Core<S>, seed: &D) {
    const NAME: &str = "page_matches_what_was_numbered";
    let topics = seed.project().await;
    let from = seed.identity(&topics, IdentityKind::Agent).await;
    let mut expected = Vec::with_capacity(NUMBERED);
    for _ in 0..NUMBERED {
        let env = envelope(Payload::Fyi {});
        send(NAME, core, &topics, &from, &env).await;
        expected.push(env.id);
    }
    let mut actual = Vec::with_capacity(NUMBERED);
    let mut after = 0;
    for n in 0..NUMBERED / PAGE_SIZE as usize {
        let page = checked(
            NAME,
            core.page(topics.project(), &Stream::Feed, after, PAGE_SIZE)
                .await,
        );
        assert_eq!(page.head, NUMBERED as u64, "{NAME}");
        assert_eq!(page.pruned_through, 0, "{NAME}");
        assert_eq!(page.messages.len(), PAGE_SIZE as usize, "{NAME}");
        for message in page.messages {
            after += 1;
            assert_eq!(message.seq.0, after, "{NAME}");
            assert_eq!(message.from, from, "{NAME}");
            assert_eq!(message.stream, Stream::Feed, "{NAME}");
            actual.push(message.client.id);
        }
        assert_eq!(page.next_after, (n == 0).then_some(after), "{NAME}");
    }
    assert_eq!(actual, expected, "{NAME}");
    let empty = checked(
        NAME,
        core.page(topics.project(), &Stream::Feed, after, PAGE_SIZE)
            .await,
    );
    assert!(empty.messages.is_empty(), "{NAME}");
    assert_eq!(empty.head, NUMBERED as u64, "{NAME}");
    assert_eq!(empty.next_after, None, "{NAME}");
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::task::{Context, Waker};
    use std::time::Duration;

    use crate::{Core, MemStore};

    #[tokio::test]
    async fn contract_on_mem_store() {
        let store = MemStore::default();
        let core = Core::new(store.clone(), Duration::from_secs(60));
        super::run_all(&core, &store).await;
    }

    #[tokio::test]
    async fn fixture_panics_name_the_contract_case() {
        let mut future = std::pin::pin!(super::case("failing_fixture_case", async {
            super::checked::<(), _>("fixture", Err("injected fixture failure"));
        }));
        let mut context = Context::from_waker(Waker::noop());
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            future.as_mut().poll(&mut context)
        }))
        .unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("failing_fixture_case"), "{message}");
    }
}
