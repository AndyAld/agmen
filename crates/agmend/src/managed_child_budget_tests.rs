use super::*;

fn batch_cap(fixture: &Fixture, id: &str) -> Option<Money> {
    let runs = fixture.runner.runs.lock().unwrap();
    let args = runs
        .iter()
        .rev()
        .find(|run| run.identity.as_str() == id)
        .unwrap()
        .args();
    args.windows(2)
        .find(|pair| pair[0] == "--max-budget-usd")
        .map(|pair| Money::from_usd(&pair[1]).unwrap())
}

#[tokio::test]
async fn shared_launching_batch_uses_half_the_remaining_budget() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    assert_eq!(batch_cap(&fixture, "agent001").unwrap().usd(), "2.500000");
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn shared_running_tree_caps_reservations_plus_batch_at_five_dollars() {
    let (fixture, mints) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    let failure = budget_child(&fixture, "over", 4.5).await.unwrap_err();
    assert_eq!(failure.code, ErrorCode::BadRequest);
    assert_eq!(mints.lock().unwrap().len(), 1);
    budget_child(&fixture, "k1", 1.0).await.unwrap();
    budget_child(&fixture, "k2", 1.5).await.unwrap();
    let held = Money::from_usd(&reserved(&fixture, "agent001").await).unwrap();
    let total = held
        .checked_add(batch_cap(&fixture, "agent001").unwrap())
        .unwrap();
    assert_eq!(held.usd(), "2.500000");
    assert_eq!(total.usd(), "5.000000");
    let failure = budget_child(&fixture, "k3", 0.000001).await.unwrap_err();
    assert_eq!(failure.code, ErrorCode::BadRequest);
    assert_eq!(reserved(&fixture, "agent001").await, "2.500000");
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn shared_mid_batch_child_within_reserve_succeeds_and_above_it_reports_budget() {
    let (fixture, mints) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    budget_child(&fixture, "k1", 1.0).await.unwrap();
    let failure = budget_child(&fixture, "k2", 1.500001).await.unwrap_err();
    assert_eq!(failure.code, ErrorCode::BadRequest);
    assert_eq!(
        failure.message,
        "launching this child would leave the parent under 0.50 USD; the parent has 4.000000 USD left"
    );
    assert_eq!(mints.lock().unwrap().len(), 2);
    assert_eq!(reserved(&fixture, "agent001").await, "1.000000");
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn shared_running_default_child_uses_only_the_reserve() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    assert_eq!(listed(&fixture, "kid00001").await["budget_usd"], "2.500000");
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn shared_batch_keeps_existing_children_and_prior_spend_backed() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    spend(&fixture, "agent001", "0.5").await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    assert_eq!(batch_cap(&fixture, "agent001").unwrap().usd(), "2.000000");
    budget_child(&fixture, "k2", 2.0).await.unwrap();
    assert!(budget_child(&fixture, "k3", 0.000001).await.is_err());
    assert_eq!(reserved(&fixture, "agent001").await, "2.500000");
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn shared_grandchild_obeys_its_parents_running_batch_cap() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    budget_child(&fixture, "k1", 4.0).await.unwrap();
    enqueue(&fixture, 1, "kid00001").await;
    running_batch(&fixture, "kid00001", Tier::Owner).await;
    assert_eq!(batch_cap(&fixture, "kid00001").unwrap().usd(), "2.000000");
    grandchild(&fixture, "kid00001", "k2", 2.0).await;
    let failure = child_call(
        &fixture,
        "kid00001",
        "child_launch",
        json!({"name":"k3","role":"chain","budget_usd":0.000001}),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, ErrorCode::BadRequest);
    assert_eq!(reserved(&fixture, "kid00001").await, "2.000000");
    assert_eq!(reserved(&fixture, "agent001").await, "4.000000");
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn shared_running_failed_child_refunds_exactly_its_reservation() {
    let (fixture, mints) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    budget_child(&fixture, "k1", 0.75).await.unwrap();
    let before = reserved(&fixture, "agent001").await;
    let failure = budget_child(&fixture, "reject", 1.25).await.unwrap_err();
    assert!(failure.message.contains("role permissions were refused"));
    assert_eq!(mints.lock().unwrap().len(), 3);
    assert_eq!(reserved(&fixture, "agent001").await, before);
    budget_child(&fixture, "k2", 1.75).await.unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "2.500000");
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn shared_nonlaunching_batch_keeps_the_full_remaining_cap() {
    let (fixture, _) = tree_fixture().await;
    let catalog = agmen_roles::Layers::default().load().unwrap();
    assert!(catalog.get("reviewer").unwrap().launches.is_empty());
    worker_call(&fixture, "agent_launch", json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo"),"role":"reviewer","budget_usd":5})).await.unwrap();
    spend(&fixture, "agent001", "0.75").await;
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    assert_eq!(batch_cap(&fixture, "agent001").unwrap().usd(), "4.250000");
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn separate_launching_batch_keeps_the_full_remaining_cap() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "child_budgets = 'separate'");
    budget_root(&fixture, 5.0).await;
    spend(&fixture, "agent001", "0.75").await;
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    assert_eq!(batch_cap(&fixture, "agent001").unwrap().usd(), "4.250000");
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn budgets_off_launching_batch_has_no_cap() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    budget_root(&fixture, 5.0).await;
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    assert_eq!(batch_cap(&fixture, "agent001"), None);
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn shared_idle_parent_keeps_the_existing_floor() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    budget_child(&fixture, "k1", 4.5).await.unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "4.500000");
    assert!(budget_child(&fixture, "k2", 0.000001).await.is_err());
}

#[tokio::test]
async fn shared_finished_batch_releases_its_cap_for_child_reservations() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    enqueue(&fixture, 1, "agent001").await;
    batch(&fixture.daemon.core.managed, "agent001").await;
    tier(&fixture, "agent001", Some(Tier::Owner)).await;
    budget_child(&fixture, "k1", 3.5).await.unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "3.500000");
}

#[tokio::test]
async fn shared_attach_aborted_batch_releases_its_cap_for_child_reservations() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    *fixture.runner.attach_error.lock().unwrap() = Some("no Terminal".into());
    let managed = &fixture.daemon.core.managed;
    // Hold settlement until Attach's deadline aborts the batch task.
    let accounting = managed.accounting.lock().await;
    let failure = managed
        .attach_claude(json!({"identity":"agent001"}))
        .await
        .unwrap_err();
    assert!(failure.message.contains("no Terminal"));
    assert_eq!(fixture.runner.cancelled.load(Ordering::SeqCst), 1);
    drop(accounting);
    assert_eq!(listed(&fixture, "agent001").await["state"], "idle");
    tier(&fixture, "agent001", Some(Tier::Owner)).await;
    budget_child(&fixture, "k1", 4.5).await.unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "4.500000");
}

#[tokio::test]
async fn shared_small_batch_preserves_the_floor_without_adding_it_to_the_cap() {
    for (cap, batch, child_cap) in [
        (0.75, "0.375000", "0.250000"),
        (1.000001, "0.500000", "0.500001"),
    ] {
        let (fixture, _) = tree_fixture().await;
        budget_root(&fixture, cap).await;
        enqueue(&fixture, 1, "agent001").await;
        running_batch(&fixture, "agent001", Tier::Owner).await;
        assert_eq!(batch_cap(&fixture, "agent001").unwrap().usd(), batch);
        child(&fixture, "agent001", "k1").await.unwrap();
        assert_eq!(listed(&fixture, "kid00001").await["budget_usd"], child_cap);
        fixture.daemon.core.managed.shutdown().await;
    }
}
