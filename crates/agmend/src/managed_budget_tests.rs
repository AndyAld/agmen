use super::*;
use crate::budget::Money;
use std::collections::BTreeMap;

async fn budget_root(fixture: &Fixture, cap: f64) {
    worker_call(fixture, "agent_launch", json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo"),"role":"chain","budget_usd":cap})).await.unwrap();
    tier(fixture, "agent001", Some(Tier::Owner)).await;
}

async fn budget_child(fixture: &Fixture, name: &str, amount: f64) -> Result<Value, ApiError> {
    child_call(
        fixture,
        "agent001",
        "child_launch",
        json!({"name":name,"role":"chain","budget_usd":amount}),
    )
    .await
}

async fn listed(fixture: &Fixture, id: &str) -> Value {
    let list = worker_call(fixture, "agent_list", json!({})).await.unwrap();
    list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["identity"] == id)
        .unwrap()
        .clone()
}

async fn child_quarter(fixture: &Fixture) {
    fixture.runner.mode.store(1, Ordering::SeqCst);
    enqueue(fixture, 1, "kid00001").await;
    batch(&fixture.daemon.core.managed, "kid00001").await;
}

#[tokio::test]
async fn shared_child_budget_comes_out_of_the_parent() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.500000"
    );
    enqueue(&fixture, 1, "agent001").await;
    batch(&fixture.daemon.core.managed, "agent001").await;
    assert_eq!(
        fixture.runner.runs.lock().unwrap()[0].budget_remaining,
        "1.500000"
    );
    fixture.runner.mode.store(1, Ordering::SeqCst);
    enqueue(&fixture, 2, "kid00001").await;
    batch(&fixture.daemon.core.managed, "kid00001").await;
    let parent = listed(&fixture, "agent001").await;
    assert_eq!(parent["children_spent_usd"], "0.250000");
    assert_eq!(parent["reserved_usd"], "0.250000");
    assert_eq!(parent["spent_usd"], "1.000000");
    assert_eq!(parent["tree_spent_usd"], "1.250000");
}

#[tokio::test]
async fn default_shared_amount_keeps_the_floor() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    assert_eq!(listed(&fixture, "kid00001").await["budget_usd"], "1.500000");
    let error = child(&fixture, "agent001", "k2").await.unwrap_err();
    assert!(
        error
            .message
            .contains("would leave the parent under 0.50 USD")
    );
}

#[tokio::test]
async fn shared_child_that_breaks_the_floor_is_refused() {
    let (fixture, mints) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    for amount in [1.6, 0.0, 0.0000001] {
        let error = budget_child(&fixture, "k1", amount).await.unwrap_err();
        assert!(
            error
                .message
                .contains("would leave the parent under 0.50 USD")
        );
    }
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.000000"
    );
}

#[tokio::test]
async fn failed_child_launch_returns_the_reservation() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    assert!(budget_child(&fixture, "reject", 0.5).await.is_err());
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.000000"
    );
    budget_child(&fixture, "k1", 1.5).await.unwrap();
}

#[tokio::test]
async fn stopped_child_returns_its_unspent_reservation() {
    for method in ["agent_stop", "agent_remove"] {
        let (fixture, _) = tree_fixture().await;
        budget_root(&fixture, 2.0).await;
        budget_child(&fixture, "k1", 0.5).await.unwrap();
        child_quarter(&fixture).await;
        worker_call(&fixture, method, json!({"identity":"kid00001"}))
            .await
            .unwrap();
        let parent = listed(&fixture, "agent001").await;
        assert_eq!(parent["reserved_usd"], "0.000000");
        assert_eq!(parent["children_spent_usd"], "0.250000");
        if method == "agent_stop" {
            worker_call(&fixture, method, json!({"identity":"kid00001"}))
                .await
                .unwrap();
            assert_eq!(
                listed(&fixture, "agent001").await["reserved_usd"],
                "0.000000"
            );
        }
    }
}

#[tokio::test]
async fn subtree_stop_returns_each_reservation_once_and_keeps_spend() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 10.0).await;
    budget_child(&fixture, "k1", 6.0).await.unwrap();
    grandchild(&fixture, "kid00001", "k2", 3.0).await;
    grandchild(&fixture, "kid00002", "k3", 1.0).await;
    child_quarter_of(&fixture, "kid00003").await;
    for _ in 0..3 {
        let answer = worker_call(&fixture, "agent_stop", json!({"identity":"kid00001"}))
            .await
            .unwrap();
        assert_eq!(
            answer["stopped"],
            json!(["kid00003", "kid00002", "kid00001"])
        );
        for id in ["agent001", "kid00001", "kid00002"] {
            assert_eq!(reserved(&fixture, id).await, "0.000000", "{id}");
            assert_eq!(
                listed(&fixture, id).await["children_spent_usd"],
                "0.250000",
                "{id}"
            );
        }
    }
    assert_eq!(listed(&fixture, "agent001").await["state"], "idle");
    worker_call(&fixture, "agent_remove", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "0.000000");
}

#[tokio::test]
async fn parent_stop_without_subtree_keeps_child_caps() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    budget_child(&fixture, "k1", 3.0).await.unwrap();
    grandchild(&fixture, "kid00001", "k2", 1.0).await;
    worker_call(
        &fixture,
        "agent_stop",
        json!({"identity":"kid00001","subtree":false}),
    )
    .await
    .unwrap();
    assert_eq!(listed(&fixture, "kid00002").await["state"], "idle");
    assert_eq!(listed(&fixture, "kid00002").await["budget_usd"], "1.000000");
    assert_eq!(reserved(&fixture, "agent001").await, "1.000000");
    assert_eq!(reserved(&fixture, "kid00001").await, "1.000000");
}

#[tokio::test]
async fn starting_a_shared_child_reserves_again() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    child_quarter(&fixture).await;
    worker_call(&fixture, "agent_stop", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    worker_call(&fixture, "agent_start", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.250000"
    );
    worker_call(&fixture, "agent_stop", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    tier(&fixture, "agent001", Some(Tier::Owner)).await;
    budget_child(&fixture, "k2", 1.25).await.unwrap();
    let error = worker_call(&fixture, "agent_start", json!({"identity":"kid00001"}))
        .await
        .unwrap_err();
    assert!(
        error
            .message
            .contains("would leave the parent under 0.50 USD")
    );
    assert_eq!(listed(&fixture, "kid00001").await["state"], "offline");
}

#[tokio::test]
async fn raising_a_shared_child_draws_from_the_parent() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    worker_call(
        &fixture,
        "agent_budget",
        json!({"identity":"kid00001","add_usd":0.5}),
    )
    .await
    .unwrap();
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "1.000000"
    );
    let error = worker_call(
        &fixture,
        "agent_budget",
        json!({"identity":"kid00001","add_usd":100}),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("add to the parent first"));
    assert_eq!(listed(&fixture, "kid00001").await["budget_usd"], "1.000000");
    worker_call(
        &fixture,
        "agent_budget",
        json!({"identity":"agent001","add_usd":1}),
    )
    .await
    .unwrap();
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "1.000000"
    );
}

#[tokio::test]
async fn separate_child_budget_uses_the_setting() {
    let (fixture, _) = tree_fixture().await;
    settings(
        &fixture,
        "child_budgets = 'separate'\nseparate_child_usd = 2.5",
    );
    budget_root(&fixture, 2.0).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    assert_eq!(listed(&fixture, "kid00001").await["budget_usd"], "2.500000");
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.000000"
    );
    child_quarter(&fixture).await;
    assert_eq!(
        listed(&fixture, "agent001").await["children_spent_usd"],
        "0.000000"
    );
    assert_eq!(
        listed(&fixture, "agent001").await["tree_spent_usd"],
        "0.250000"
    );
}

#[tokio::test]
async fn budgets_off_runs_past_the_cap_and_still_counts() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "budgets = false\nseparate_child_usd = 2.5");
    budget_root(&fixture, 0.5).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    assert_eq!(listed(&fixture, "kid00001").await["budget_usd"], "2.500000");
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.000000"
    );
    for seq in 1..=2 {
        enqueue(&fixture, seq, "agent001").await;
        batch(&fixture.daemon.core.managed, "agent001").await;
    }
    assert_eq!(fixture.runner.runs.lock().unwrap().len(), 2);
    assert!(
        fixture
            .runner
            .runs
            .lock()
            .unwrap()
            .iter()
            .all(|spec| !spec.cap_budget)
    );
    assert_eq!(listed(&fixture, "agent001").await["spent_usd"], "2.000000");
    assert!(
        !worker_of(&fixture.daemon.core.managed, "kid00001")
            .await
            .lock()
            .await
            .record
            .shared_budget
    );
}

#[tokio::test]
async fn children_spend_survives_restart() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    child_quarter(&fixture).await;
    let restored = restarted(&fixture).await;
    let list = restored.list(json!({})).await.unwrap();
    assert_eq!(list["agents"][0]["children_spent_usd"], "0.250000");
    assert_eq!(list["agents"][0]["reserved_usd"], "0.000000");
}

#[tokio::test]
async fn invalid_table_is_reported_in_the_list() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 0.5).await;
    settings(&fixture, "budgets = 'yes'");
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let list = worker_call(&fixture, "agent_list", json!({}))
        .await
        .unwrap();
    assert!(
        list["settings_error"]
            .as_str()
            .unwrap()
            .contains("settings.toml [agents]")
    );
    for seq in 1..=2 {
        enqueue(&fixture, seq, "agent001").await;
        let managed = &fixture.daemon.core.managed;
        let worker = worker_of(managed, "agent001").await;
        worker.lock().await.last_run = None;
        managed.tick().await;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            for _ in 0..2000 {
                if worker
                    .lock()
                    .await
                    .task
                    .as_ref()
                    .is_none_or(tokio::task::JoinHandle::is_finished)
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            panic!("invalid-table batch did not finish");
        })
        .await
        .unwrap();
    }
    assert_eq!(fixture.runner.runs.lock().unwrap().len(), 1);
    let restored = restarted(&fixture).await;
    restored
        .start(json!({"identity":"agent001"}))
        .await
        .unwrap();
    batch(&restored, "agent001").await;
    assert_eq!(fixture.runner.runs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_settings_keep_the_last_good_budget_switch() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    budget_root(&fixture, 0.5).await;
    settings(&fixture, "budgets = 'yes'");
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    for seq in 1..=2 {
        enqueue(&fixture, seq, "agent001").await;
        batch(&fixture.daemon.core.managed, "agent001").await;
    }
    assert_eq!(fixture.runner.runs.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn concurrent_child_launches_cannot_reserve_the_same_dollars() {
    let (fixture, mints) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    let (a, b) = tokio::join!(
        budget_child(&fixture, "k1", 1.0),
        budget_child(&fixture, "k2", 1.0)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let error = a.err().or_else(|| b.err()).unwrap();
    assert!(
        error
            .message
            .contains("would leave the parent under 0.50 USD")
    );
    assert_eq!(mints.lock().unwrap().len(), 2);
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "1.000000"
    );
}

#[tokio::test]
async fn unknown_agents_setting_is_reported_and_ignored() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "max_agent = 32");
    root(&fixture).await;
    let list = worker_call(&fixture, "agent_list", json!({}))
        .await
        .unwrap();
    assert_eq!(
        list["settings_error"],
        "unknown setting max_agent in [agents], ignored"
    );
    let loaded = crate::safeguards::load(Some(&fixture.dir.path().join("settings.toml"))).unwrap();
    assert_eq!(loaded.max_agents, 16);
}

#[tokio::test]
async fn unknown_agents_settings_report_only_the_first_sixteen() {
    let (fixture, _) = tree_fixture().await;
    let text = (0..20)
        .map(|n| format!("unknown_{n:02} = true\n"))
        .collect::<String>();
    settings(&fixture, &text);
    root(&fixture).await;
    let list = worker_call(&fixture, "agent_list", json!({}))
        .await
        .unwrap();
    let error = list["settings_error"].as_str().unwrap();
    assert_eq!(error.matches("unknown setting").count(), 16);
    assert!(error.contains("unknown_15"));
    assert!(!error.contains("unknown_16"));
}

#[tokio::test]
async fn grandchild_spend_is_charged_to_each_shared_ancestor() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    budget_child(&fixture, "k1", 3.0).await.unwrap();
    tier(&fixture, "kid00001", Some(Tier::Owner)).await;
    child_call(
        &fixture,
        "kid00001",
        "child_launch",
        json!({"name":"k2","role":"chain","budget_usd":1}),
    )
    .await
    .unwrap();
    fixture.runner.mode.store(1, Ordering::SeqCst);
    enqueue(&fixture, 1, "kid00002").await;
    batch(&fixture.daemon.core.managed, "kid00002").await;
    for (id, reserved) in [("agent001", "2.750000"), ("kid00001", "0.750000")] {
        let row = listed(&fixture, id).await;
        assert_eq!(row["children_spent_usd"], "0.250000");
        assert_eq!(row["tree_spent_usd"], "0.250000");
        assert_eq!(row["reserved_usd"], reserved);
    }
    worker_call(
        &fixture,
        "agent_stop",
        json!({"identity":"kid00001","subtree":false}),
    )
    .await
    .unwrap();
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.750000"
    );
    assert_eq!(listed(&fixture, "kid00002").await["state"], "idle");
    enqueue(&fixture, 2, "kid00002").await;
    batch(&fixture.daemon.core.managed, "kid00002").await;
    assert_eq!(
        listed(&fixture, "agent001").await["children_spent_usd"],
        "0.500000"
    );
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.500000"
    );
}

#[tokio::test]
async fn raising_an_offline_shared_child_is_not_reserved_twice_on_start() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 3.0).await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    worker_call(&fixture, "agent_stop", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    worker_call(
        &fixture,
        "agent_budget",
        json!({"identity":"kid00001","add_usd":0.5}),
    )
    .await
    .unwrap();
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.500000"
    );
    worker_call(&fixture, "agent_start", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "1.000000"
    );
}

#[tokio::test]
async fn successful_child_cost_counts_and_parent_raise_keeps_it() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 3.0).await;
    budget_child(&fixture, "k1", 1.5).await.unwrap();
    enqueue(&fixture, 1, "kid00001").await;
    batch(&fixture.daemon.core.managed, "kid00001").await;
    worker_call(
        &fixture,
        "agent_budget",
        json!({"identity":"agent001","add_usd":1}),
    )
    .await
    .unwrap();
    let row = listed(&fixture, "agent001").await;
    assert_eq!(row["reserved_usd"], "0.500000");
    assert_eq!(row["children_spent_usd"], "1.000000");
    let restored = restarted(&fixture).await;
    assert_eq!(
        restored.list(json!({})).await.unwrap()["agents"][0]["children_spent_usd"],
        "1.000000"
    );
}

#[tokio::test]
async fn removing_an_offline_child_returns_a_later_budget_raise() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 3.0).await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    worker_call(&fixture, "agent_stop", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    worker_call(
        &fixture,
        "agent_budget",
        json!({"identity":"kid00001","add_usd":0.5}),
    )
    .await
    .unwrap();
    worker_call(&fixture, "agent_remove", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.000000"
    );
}

#[tokio::test]
async fn stopping_a_grandchild_returns_funds_through_its_offline_parent() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    budget_child(&fixture, "k1", 3.0).await.unwrap();
    tier(&fixture, "kid00001", Some(Tier::Owner)).await;
    child_call(
        &fixture,
        "kid00001",
        "child_launch",
        json!({"name":"k2","role":"chain","budget_usd":1}),
    )
    .await
    .unwrap();
    worker_call(
        &fixture,
        "agent_stop",
        json!({"identity":"kid00001","subtree":false}),
    )
    .await
    .unwrap();
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "1.000000"
    );
    worker_call(&fixture, "agent_stop", json!({"identity":"kid00002"}))
        .await
        .unwrap();
    assert_eq!(
        listed(&fixture, "agent001").await["reserved_usd"],
        "0.000000"
    );
}

#[tokio::test]
async fn starting_a_grandchild_reserves_through_its_offline_parent_and_rolls_back() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    budget_child(&fixture, "k1", 3.0).await.unwrap();
    tier(&fixture, "kid00001", Some(Tier::Owner)).await;
    child_call(
        &fixture,
        "kid00001",
        "child_launch",
        json!({"name":"k2","role":"chain","budget_usd":1}),
    )
    .await
    .unwrap();
    let restored = restarted(&fixture).await;
    restored
        .start(json!({"identity":"kid00002"}))
        .await
        .unwrap();
    let list = restored.list(json!({})).await.unwrap();
    assert_eq!(list["agents"][0]["reserved_usd"], "1.000000");
    assert_eq!(list["agents"][1]["reserved_usd"], "1.000000");
    restored.stop(json!({"identity":"kid00002"})).await.unwrap();
    worker_of(&restored, "agent001")
        .await
        .lock()
        .await
        .budget
        .add_cost_usd("4.5", |_| {})
        .unwrap();
    let error = restored
        .start(json!({"identity":"kid00002"}))
        .await
        .unwrap_err();
    assert!(
        error
            .message
            .contains("would leave the parent under 0.50 USD")
    );
    let list = restored.list(json!({})).await.unwrap();
    assert_eq!(list["agents"][0]["reserved_usd"], "0.000000");
    assert_eq!(list["agents"][1]["reserved_usd"], "0.000000");
    assert_eq!(list["agents"][2]["state"], "offline");
}

async fn grandchild(fixture: &Fixture, parent: &str, name: &str, amount: f64) {
    tier(fixture, parent, Some(Tier::Owner)).await;
    child_call(
        fixture,
        parent,
        "child_launch",
        json!({"name":name,"role":"chain","budget_usd":amount}),
    )
    .await
    .unwrap();
}

async fn reserved(fixture: &Fixture, id: &str) -> String {
    listed(fixture, id).await["reserved_usd"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn separate_child_budget_is_clamped_to_the_setting() {
    let (fixture, mints) = tree_fixture().await;
    settings(
        &fixture,
        "child_budgets = 'separate'\nseparate_child_usd = 2.5",
    );
    budget_root(&fixture, 2.0).await;
    // An agent may ask for less, never for more than the person allows.
    for (name, asked, granted) in [
        ("k1", json!(100), "2.500000"),
        ("k2", json!(1e30), "2.500000"),
        ("k3", json!(2.5), "2.500000"),
        ("k4", json!(1.25), "1.250000"),
        ("k5", json!(0.000001), "0.000001"),
    ] {
        child_call(
            &fixture,
            "agent001",
            "child_launch",
            json!({"name":name,"role":"chain","budget_usd":asked}),
        )
        .await
        .unwrap();
        let id = format!("kid{:05}", mints.lock().unwrap().len() - 1);
        assert_eq!(listed(&fixture, &id).await["budget_usd"], granted, "{name}");
    }
    assert_eq!(reserved(&fixture, "agent001").await, "0.000000");
    // The person's own raise is not an agent's pick, and a separate child draws nothing from its parent.
    worker_call(
        &fixture,
        "agent_budget",
        json!({"identity":"kid00001","add_usd":10}),
    )
    .await
    .unwrap();
    assert_eq!(
        listed(&fixture, "kid00001").await["budget_usd"],
        "12.500000"
    );
    assert_eq!(reserved(&fixture, "agent001").await, "0.000000");
}

#[tokio::test]
async fn budgets_off_child_cap_is_clamped_for_when_budgets_come_back() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "budgets = false\nseparate_child_usd = 2.5");
    budget_root(&fixture, 2.0).await;
    child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"chain","budget_usd":1000000}),
    )
    .await
    .unwrap();
    assert_eq!(listed(&fixture, "kid00001").await["budget_usd"], "2.500000");
    assert_eq!(reserved(&fixture, "agent001").await, "0.000000");
    let child = worker_of(&fixture.daemon.core.managed, "kid00001").await;
    assert!(!child.lock().await.record.shared_budget);
}

#[tokio::test]
async fn the_person_may_launch_a_root_above_the_separate_amount() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "separate_child_usd = 2.5");
    worker_call(&fixture, "agent_launch", json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo"),"role":"chain","budget_usd":50})).await.unwrap();
    assert_eq!(listed(&fixture, "agent001").await["budget_usd"], "50");
}

#[tokio::test]
async fn shared_child_larger_than_the_parent_has_is_refused() {
    let (fixture, mints) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    for amount in [json!(2.0), json!(5), json!(100), json!(1e30)] {
        let error = child_call(
            &fixture,
            "agent001",
            "child_launch",
            json!({"name":"k1","role":"chain","budget_usd":amount}),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .message
                .contains("would leave the parent under 0.50 USD"),
            "{amount}: {error:?}"
        );
        assert!(error.message.contains("2.000000"), "{error:?}");
    }
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(reserved(&fixture, "agent001").await, "0.000000");
}

#[tokio::test]
async fn agents_cannot_raise_budgets() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    let caps = (
        listed(&fixture, "agent001").await["budget_usd"].clone(),
        listed(&fixture, "kid00001").await["budget_usd"].clone(),
    );
    for (caller, target) in [
        ("agent001", "agent001"),
        ("agent001", "kid00001"),
        ("kid00001", "kid00001"),
        ("kid00001", "agent001"),
    ] {
        tier(&fixture, caller, Some(Tier::Owner)).await;
        let error = child_call(
            &fixture,
            caller,
            "agent_budget",
            json!({"identity":target,"add_usd":1}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Forbidden, "{caller} -> {target}");
    }
    // A child launch carries no raise either.
    let error = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k2","role":"chain","add_usd":5}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert_eq!(listed(&fixture, "agent001").await["budget_usd"], caps.0);
    assert_eq!(listed(&fixture, "kid00001").await["budget_usd"], caps.1);
    assert_eq!(reserved(&fixture, "agent001").await, "0.500000");
}

#[tokio::test]
async fn racing_children_and_raises_cannot_reserve_the_same_dollars() {
    let (fixture, mints) = tree_fixture().await;
    budget_root(&fixture, 3.0).await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    // 2.5 is left: a 1.5 child and a 1.5 raise each fit, both do not.
    let (launch, raise) = tokio::join!(
        budget_child(&fixture, "k2", 1.5),
        worker_call(
            &fixture,
            "agent_budget",
            json!({"identity":"kid00001","add_usd":1.5}),
        )
    );
    assert_eq!(usize::from(launch.is_ok()) + usize::from(raise.is_ok()), 1);
    assert_eq!(reserved(&fixture, "agent001").await, "2.000000");
    assert_eq!(mints.lock().unwrap().len(), 2 + usize::from(launch.is_ok()));
    // Four children for the last dollars: one fits.
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    let (a, b, c, d) = tokio::join!(
        budget_child(&fixture, "k1", 0.9),
        budget_child(&fixture, "k2", 0.9),
        budget_child(&fixture, "k3", 0.9),
        budget_child(&fixture, "k4", 0.9)
    );
    let fitted = [a, b, c, d].iter().filter(|launch| launch.is_ok()).count();
    assert_eq!(fitted, 1);
    assert_eq!(reserved(&fixture, "agent001").await, "0.900000");
}

#[tokio::test]
async fn stopping_an_offline_intermediate_again_keeps_its_live_children_reserved() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    budget_child(&fixture, "k1", 3.0).await.unwrap();
    grandchild(&fixture, "kid00001", "k2", 1.0).await;
    assert_eq!(reserved(&fixture, "agent001").await, "3.000000");
    for _ in 0..3 {
        worker_call(
            &fixture,
            "agent_stop",
            json!({"identity":"kid00001","subtree":false}),
        )
        .await
        .unwrap();
        // k2 still runs on funds that k1 holds in the root.
        assert_eq!(reserved(&fixture, "agent001").await, "1.000000");
        assert_eq!(reserved(&fixture, "kid00001").await, "1.000000");
    }
    // A raise while offline is held too, and a later stop returns only that.
    worker_call(
        &fixture,
        "agent_budget",
        json!({"identity":"kid00001","add_usd":0.5}),
    )
    .await
    .unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "1.500000");
    for _ in 0..2 {
        worker_call(
            &fixture,
            "agent_stop",
            json!({"identity":"kid00001","subtree":false}),
        )
        .await
        .unwrap();
        assert_eq!(reserved(&fixture, "agent001").await, "1.000000");
    }
    worker_call(&fixture, "agent_stop", json!({"identity":"kid00002"}))
        .await
        .unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "0.000000");
}

#[tokio::test]
async fn two_stops_at_once_release_once() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    budget_child(&fixture, "k1", 1.0).await.unwrap();
    let (a, b) = tokio::join!(
        worker_call(&fixture, "agent_stop", json!({"identity":"kid00001"})),
        worker_call(&fixture, "agent_stop", json!({"identity":"kid00001"}))
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "0.000000");
    // Launching the freed dollars again must leave exactly what it should.
    budget_child(&fixture, "k2", 1.5).await.unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "1.500000");
}

#[tokio::test]
async fn an_agent_that_kept_its_reservation_starts_without_new_funds() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    budget_child(&fixture, "k1", 1.5).await.unwrap();
    // The person raises the child until the parent has nothing left.
    worker_call(
        &fixture,
        "agent_budget",
        json!({"identity":"kid00001","add_usd":0.5}),
    )
    .await
    .unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "2.000000");
    // `tick` can take an agent offline without releasing (a role that changed).
    worker_of(&fixture.daemon.core.managed, "kid00001")
        .await
        .lock()
        .await
        .state = "offline";
    worker_call(&fixture, "agent_start", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "2.000000");
    assert_eq!(listed(&fixture, "kid00001").await["state"], "idle");
}

#[tokio::test]
async fn a_restart_then_stops_and_starts_never_refund_twice() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    child_quarter(&fixture).await;
    let restored = restarted(&fixture).await;
    let parent = |list: &Value| (list["agents"][0].clone(), list["agents"][1].clone());
    let (root, kid) = parent(&restored.list(json!({})).await.unwrap());
    assert_eq!(root["reserved_usd"], "0.000000");
    assert_eq!(root["children_spent_usd"], "0.250000");
    assert_eq!(kid["spent_usd"], "0.250000");
    // Offline after a restart: stopping holds nothing, so it returns nothing.
    for _ in 0..2 {
        restored.stop(json!({"identity":"kid00001"})).await.unwrap();
        let (root, _) = parent(&restored.list(json!({})).await.unwrap());
        assert_eq!(root["reserved_usd"], "0.000000");
        assert_eq!(root["children_spent_usd"], "0.250000");
    }
    for _ in 0..3 {
        restored
            .start(json!({"identity":"kid00001"}))
            .await
            .unwrap();
        let (root, _) = parent(&restored.list(json!({})).await.unwrap());
        assert_eq!(root["reserved_usd"], "0.250000");
        for _ in 0..2 {
            restored.stop(json!({"identity":"kid00001"})).await.unwrap();
            let (root, _) = parent(&restored.list(json!({})).await.unwrap());
            assert_eq!(root["reserved_usd"], "0.000000");
            assert_eq!(root["children_spent_usd"], "0.250000");
        }
    }
    restored
        .remove(json!({"identity":"kid00001"}))
        .await
        .unwrap();
    let list = restored.list(json!({})).await.unwrap();
    assert_eq!(list["agents"][0]["reserved_usd"], "0.000000");
    assert_eq!(list["agents"][0]["children_spent_usd"], "0.250000");
}

#[tokio::test]
async fn spend_rolls_up_through_four_levels_and_funds_return_one_level_at_a_time() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 10.0).await;
    budget_child(&fixture, "k1", 6.0).await.unwrap();
    grandchild(&fixture, "kid00001", "k2", 3.0).await;
    grandchild(&fixture, "kid00002", "k3", 1.0).await;
    assert_eq!(listed(&fixture, "kid00003").await["depth"], 4);
    child_quarter_of(&fixture, "kid00003").await;
    for (id, spent, children, held, tree_spent) in [
        ("agent001", "0.000000", "0.250000", "5.750000", "0.250000"),
        ("kid00001", "0.000000", "0.250000", "2.750000", "0.250000"),
        ("kid00002", "0.000000", "0.250000", "0.750000", "0.250000"),
        ("kid00003", "0.250000", "0.000000", "0.000000", "0.250000"),
    ] {
        let row = listed(&fixture, id).await;
        assert_eq!(row["spent_usd"], spent, "{id}");
        assert_eq!(row["children_spent_usd"], children, "{id}");
        assert_eq!(row["reserved_usd"], held, "{id}");
        assert_eq!(row["tree_spent_usd"], tree_spent, "{id}");
    }
    // Each stop returns its unspent cap to its own parent only; a running
    // parent keeps what it holds in its own parent.
    for (stopped, expected) in [
        ("kid00003", ["5.750000", "2.750000", "0.000000"]),
        ("kid00002", ["5.750000", "0.000000", "0.000000"]),
        ("kid00001", ["0.000000", "0.000000", "0.000000"]),
    ] {
        worker_call(&fixture, "agent_stop", json!({"identity":stopped}))
            .await
            .unwrap();
        for (id, held) in ["agent001", "kid00001", "kid00002"].iter().zip(expected) {
            assert_eq!(reserved(&fixture, id).await, held, "{stopped}: {id}");
        }
    }
    assert_eq!(
        listed(&fixture, "agent001").await["children_spent_usd"],
        "0.250000"
    );
}

/// Charges `cost` to a running agent as a finished batch does, whatever its cap.
async fn spend(fixture: &Fixture, id: &str, cost: &str) {
    let managed = &fixture.daemon.core.managed;
    let worker = worker_of(managed, id).await;
    let accounting = managed.accounting.lock().await;
    let held = worker.lock().await;
    managed.record_cost(held, cost).await.unwrap();
    drop(accounting);
}

/// What `id` still holds in its parent's reservation.
async fn held_in_parent(fixture: &Fixture, id: &str) -> String {
    worker_of(&fixture.daemon.core.managed, id)
        .await
        .lock()
        .await
        .reserved_from_parent
        .usd()
}

/// A batch can cost more than the cap its child was given, since
/// `--max-budget-usd` ends a run after the response that passes it.
#[tokio::test]
async fn an_overshooting_child_does_not_release_its_siblings_reservation() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 5.0).await;
    budget_child(&fixture, "k1", 1.0).await.unwrap();
    budget_child(&fixture, "k2", 1.0).await.unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "2.000000");
    spend(&fixture, "kid00001", "1.5").await;
    let parent = listed(&fixture, "agent001").await;
    assert_eq!(parent["reserved_usd"], "1.000000");
    assert_eq!(parent["children_spent_usd"], "1.500000");
    assert_eq!(held_in_parent(&fixture, "kid00001").await, "0.000000");
    assert_eq!(held_in_parent(&fixture, "kid00002").await, "1.000000");
    assert_eq!(listed(&fixture, "kid00001").await["spent_usd"], "1.500000");
    // The parent has 5 - 1.5 spent - 1 still held for the other child.
    enqueue(&fixture, 1, "agent001").await;
    batch(&fixture.daemon.core.managed, "agent001").await;
    assert_eq!(
        fixture.runner.runs.lock().unwrap()[0].budget_remaining,
        "2.500000"
    );
}

/// The same holds one level up, where the parent's own parent holds funds
/// for it: only what the parent can spare goes back.
#[tokio::test]
async fn an_overshoot_under_a_stopped_parent_keeps_its_other_children_backed() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 10.0).await;
    budget_child(&fixture, "k1", 6.0).await.unwrap();
    grandchild(&fixture, "kid00001", "k2", 1.0).await;
    grandchild(&fixture, "kid00001", "k3", 1.0).await;
    worker_call(
        &fixture,
        "agent_stop",
        json!({"identity":"kid00001","subtree":false}),
    )
    .await
    .unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "2.000000");
    spend(&fixture, "kid00002", "1.5").await;
    assert_eq!(reserved(&fixture, "kid00001").await, "1.000000");
    assert_eq!(held_in_parent(&fixture, "kid00001").await, "1.000000");
    assert_eq!(reserved(&fixture, "agent001").await, "1.000000");
    for id in ["agent001", "kid00001"] {
        assert_eq!(
            listed(&fixture, id).await["children_spent_usd"],
            "1.500000",
            "{id}"
        );
    }
}

async fn child_quarter_of(fixture: &Fixture, id: &str) {
    fixture.runner.mode.store(1, Ordering::SeqCst);
    enqueue(fixture, 1, id).await;
    batch(&fixture.daemon.core.managed, id).await;
}

/// A tiny seeded generator, so a failing sequence replays.
struct Dice(u64);

impl Dice {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % u64::try_from(bound).unwrap()).unwrap()
    }
}

struct Books {
    parent: Option<String>,
    shared: bool,
    online: bool,
    cap: Money,
    spent: Money,
    children_spent: Money,
    reserved: Money,
    held: Money,
    unspent: Money,
}

async fn books(managed: &Managed) -> BTreeMap<String, Books> {
    let mut rows = BTreeMap::new();
    for (id, worker) in managed.workers.lock().await.iter() {
        let worker = worker.lock().await;
        rows.insert(
            id.to_string(),
            Books {
                parent: worker.record.parent.as_ref().map(ToString::to_string),
                shared: worker.record.shared_budget,
                online: worker.state != "offline",
                cap: Money::from_usd(&worker.record.budget_usd).unwrap(),
                spent: worker.budget.spent(),
                children_spent: worker.budget.children_spent(),
                reserved: worker.budget.reserved(),
                held: worker.reserved_from_parent,
                unspent: worker.budget.unspent(),
            },
        );
    }
    rows
}

/// What each agent's descendants amount to in `ledger`, at most 16 levels up.
fn rolled_up<'a>(
    ledger: &'a BTreeMap<String, Money>,
    parents: &'a BTreeMap<String, Option<String>>,
) -> BTreeMap<&'a str, Money> {
    let mut below: BTreeMap<&str, Money> = BTreeMap::new();
    for (id, cost) in ledger {
        let mut at = parents.get(id).and_then(Option::as_deref);
        for _ in 0..16 {
            let Some(parent) = at else { break };
            let total = below.entry(parent).or_insert(Money::ZERO);
            *total = total.saturating_add(*cost);
            at = parents.get(parent).and_then(Option::as_deref);
        }
    }
    below
}

/// What every agent's books must say after any step. `over` lists what each
/// agent's batches cost beyond what it held in its parent: `--max-budget-usd`
/// ends a run only after the response that passes it. Past its cap an agent,
/// and every ancestor above it, may be that far over its own cap.
fn unbalanced(
    rows: &BTreeMap<String, Books>,
    ledger: &BTreeMap<String, Money>,
    over: &BTreeMap<String, Money>,
    parents: &BTreeMap<String, Option<String>>,
) -> Option<String> {
    let below_over = rolled_up(over, parents);
    let below = rolled_up(ledger, parents);
    for (id, row) in rows {
        let children = rows
            .values()
            .filter(|other| other.parent.as_deref() == Some(id) && other.shared)
            .fold(Money::ZERO, |total, other| total.saturating_add(other.held));
        if row.reserved != children {
            return Some(format!(
                "{id} reserves {} for its children but they hold {}",
                row.reserved.usd(),
                children.usd()
            ));
        }
        if row.shared {
            // Running, an agent holds its whole unspent cap. Stopped, it holds
            // at least what its live children need, plus any raise the person
            // made while it was stopped.
            // A raise after an overshoot reserves the whole amount in the parent
            // while only part of it becomes spendable, so the parent may hold
            // up to the agent's own overshoot more than it can use.
            let slack = over.get(id).copied().unwrap_or(Money::ZERO);
            let fits = if row.online {
                row.unspent <= row.held && row.held <= row.unspent.saturating_add(slack)
            } else {
                row.reserved <= row.held && row.held <= row.unspent.saturating_add(slack)
            };
            if !fits {
                return Some(format!(
                    "{id} holds {} in its parent; it has {} unspent and {} reserved (online: {})",
                    row.held.usd(),
                    row.unspent.usd(),
                    row.reserved.usd(),
                    row.online
                ));
            }
        } else if row.held != Money::ZERO {
            return Some(format!("{id} is not shared but holds {}", row.held.usd()));
        }
        let committed = row
            .spent
            .saturating_add(row.children_spent)
            .saturating_add(row.reserved);
        let allowed = over
            .get(id)
            .copied()
            .unwrap_or(Money::ZERO)
            .saturating_add(below_over.get(id.as_str()).copied().unwrap_or(Money::ZERO));
        if committed > row.cap.saturating_add(allowed) {
            return Some(format!(
                "{id} has committed {} of a {} cap, with {} of overshoot below it",
                committed.usd(),
                row.cap.usd(),
                allowed.usd()
            ));
        }
        let own = ledger.get(id).copied().unwrap_or(Money::ZERO);
        let descendants = below.get(id.as_str()).copied().unwrap_or(Money::ZERO);
        if row.spent != own || row.children_spent != descendants {
            return Some(format!(
                "{id} counts spent {} and children {}, the ledger says {} and {}",
                row.spent.usd(),
                row.children_spent.usd(),
                own.usd(),
                descendants.usd()
            ));
        }
    }
    None
}

/// A child launch from a batch that serves only the owner, as `child_launch` needs.
async fn launch_under(
    managed: &Arc<Managed>,
    target: &str,
    (number, amount): (usize, f64),
    parents: &mut BTreeMap<String, Option<String>>,
) -> String {
    let worker = worker_of(managed, target).await;
    worker.lock().await.batch_tier = Some(Tier::Owner);
    let identity = worker.lock().await.record.identity.clone();
    let body = json!({"name":format!("n{number}"),"role":"chain","budget_usd":amount});
    let launched = managed.launch_child(&identity, body).await;
    worker.lock().await.batch_tier = None;
    if let Ok(answer) = &launched {
        parents.insert(
            answer["identity"].as_str().unwrap().to_owned(),
            Some(target.to_owned()),
        );
    }
    format!("launch {amount} under {target}: {}", launched.is_ok())
}

/// Charges up to a quarter dollar to an online agent, as a finished batch does.
/// This never passes the agent's cap; `overshoot_at` does.
async fn spend_at(managed: &Managed, target: &str, ledger: &mut BTreeMap<String, Money>) -> String {
    let worker = worker_of(managed, target).await;
    let (online, remaining) = {
        let held = worker.lock().await;
        (held.state != "offline", held.budget.remaining_usd())
    };
    let quarter = Money::from_usd("0.25").unwrap();
    let cost = quarter.min(Money::from_usd(&remaining).unwrap());
    if !online || cost == Money::ZERO {
        return format!("skip spend at {target}");
    }
    let accounting = managed.accounting.lock().await;
    let held = worker.lock().await;
    managed.record_cost(held, &cost.usd()).await.unwrap();
    drop(accounting);
    let total = ledger.entry(target.to_owned()).or_insert(Money::ZERO);
    *total = total.saturating_add(cost);
    format!("spend {} at {target}", cost.usd())
}

/// Charges a running shared agent more than it holds in its parent, by up to a
/// quarter dollar past its cap. The agent has no live children of its own, so
/// the overshoot is all of what it spends beyond its cap, and it is kept to
/// what every ancestor still has free, so no ancestor passes its own cap by
/// more than the overshoot.
async fn overshoot_at(
    managed: &Managed,
    dice: &mut Dice,
    (ledger, over): (&mut BTreeMap<String, Money>, &mut BTreeMap<String, Money>),
) -> String {
    let rows = books(managed).await;
    let eligible: Vec<&String> = rows
        .iter()
        .filter(|(_, row)| row.online && row.shared && row.reserved == Money::ZERO)
        .map(|(id, _)| id)
        .collect();
    if eligible.is_empty() {
        return "skip overshoot".to_owned();
    }
    let target = eligible[dice.below(eligible.len())].clone();
    let row = &rows[&target];
    let mut room = Money::from_usd("0.25").unwrap();
    let mut at = row.parent.as_deref();
    for _ in 0..16 {
        let Some(parent) = at.and_then(|id| rows.get(id)) else {
            break;
        };
        room = room.min(parent.unspent.saturating_sub(parent.reserved));
        at = parent.parent.as_deref();
    }
    if room == Money::ZERO {
        return format!("skip overshoot at {target}");
    }
    let cost = row.unspent.saturating_add(room);
    let worker = worker_of(managed, &target).await;
    let accounting = managed.accounting.lock().await;
    let held = worker.lock().await;
    managed.record_cost(held, &cost.usd()).await.unwrap();
    drop(accounting);
    for (book, amount) in [(ledger, cost), (over, room)] {
        let total = book.entry(target.clone()).or_insert(Money::ZERO);
        *total = total.saturating_add(amount);
    }
    format!(
        "overshoot {} at {target}, {} past its cap",
        cost.usd(),
        room.usd()
    )
}

async fn model_run(seed: u64) {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "limit_tree_depth = false\nlimit_agents = false");
    budget_root(&fixture, 20.0).await;
    let mut managed = fixture.daemon.core.managed.clone();
    let mut dice = Dice(seed);
    let mut ledger = BTreeMap::new();
    let mut over = BTreeMap::new();
    let mut parents = BTreeMap::from([("agent001".to_owned(), None)]);
    let mut log = Vec::new();
    let mut launches = 0;
    for step in 0..80 {
        if step % 20 == 19 {
            managed = restarted(&fixture).await;
            log.push("restart".to_owned());
        }
        let rows = books(&managed).await;
        // Half the time pick an agent that has children: stopping and raising
        // those is where the books are hardest to keep.
        let parents_now: Vec<&String> = rows
            .values()
            .filter_map(|row| row.parent.as_ref())
            .collect();
        let present: Vec<&String> = if dice.below(2) == 0 && !parents_now.is_empty() {
            parents_now
        } else {
            rows.keys().collect()
        };
        let target = present[dice.below(present.len())].clone();
        let action = match dice.below(12) {
            0 | 1 if launches < 8 => {
                launches += 1;
                let amount = [0.5, 1.0, 1.5, 2.5, 4.0][dice.below(5)];
                launch_under(&managed, &target, (launches, amount), &mut parents).await
            }
            2..=4 => {
                let subtree = dice.below(2) == 0;
                let stopped = managed
                    .stop(json!({"identity":target,"subtree":subtree}))
                    .await;
                format!("stop {target}, subtree {subtree}: {}", stopped.is_ok())
            }
            5 | 6 => {
                let started = managed.start(json!({"identity":target})).await;
                format!("start {target}: {}", started.is_ok())
            }
            7 => {
                let leaf = !books(&managed)
                    .await
                    .values()
                    .any(|row| row.parent.as_deref() == Some(&target));
                if target != "agent001" && leaf {
                    let removed = managed.remove(json!({"identity":target})).await;
                    format!("remove {target}: {}", removed.is_ok())
                } else {
                    format!("skip remove {target}")
                }
            }
            8 => {
                let raised = managed
                    .agent_budget(json!({"identity":target,"add_usd":0.5}))
                    .await;
                if raised.is_ok() {
                    ledger.entry(target.clone()).or_insert(Money::ZERO);
                }
                format!("raise {target}: {}", raised.is_ok())
            }
            9 => spend_at(&managed, &target, &mut ledger).await,
            _ => overshoot_at(&managed, &mut dice, (&mut ledger, &mut over)).await,
        };
        log.push(action);
        if let Some(wrong) = unbalanced(&books(&managed).await, &ledger, &over, &parents) {
            panic!("seed {seed}, step {step}: {wrong}\n{}", log.join("\n"));
        }
    }
}

#[tokio::test]
async fn random_sequences_keep_the_books_balanced_a() {
    model_run(0x9e37_79b9_7f4a_7c15).await;
}

#[tokio::test]
async fn random_sequences_keep_the_books_balanced_b() {
    model_run(0xbf58_476d_1ce4_e5b9).await;
}

#[tokio::test]
async fn random_sequences_keep_the_books_balanced_c() {
    model_run(0x94d0_49bb_1331_11eb).await;
}

#[tokio::test]
async fn random_sequences_keep_the_books_balanced_d() {
    model_run(0x2545_f491_4f6c_dd1d).await;
}

/// A framed request that names `id` and carries its local token, from a peer
/// that is not one of agmend's own agent processes.
async fn identity_request_from_outside(
    fixture: &Fixture,
    id: &str,
    method: &str,
    mut params: Value,
) -> Result<Value, ApiError> {
    use agmen_local::{Outcome, Request, Response, frame};
    let token = fixture
        .daemon
        .core
        .identities
        .local_token(&id.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    params["local_token"] = json!(token.expose());
    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let task = tokio::spawn(crate::api::serve_with_peer(
        server,
        fixture.daemon.core.clone(),
        |_| Ok(12345),
    ));
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        frame::write_async(&mut client, &Request::new(1, "hello", json!({"api":1})))
            .await
            .unwrap();
        let _hello: Response = frame::read_async(&mut client).await.unwrap();
        let mut request = Request::new(2, method, params);
        request.as_ = Some(id.parse().unwrap());
        frame::write_async(&mut client, &request).await.unwrap();
        match frame::read_async::<Response>(&mut client)
            .await
            .unwrap()
            .outcome
        {
            Outcome::Ok { result } => Ok(result),
            Outcome::Err { error } => Err(error),
        }
    })
    .await;
    drop(client);
    task.abort();
    let _joined = task.await;
    result.unwrap()
}

#[tokio::test]
async fn an_agent_outside_agmends_process_tree_still_cannot_raise_budgets() {
    let (fixture, _) = tree_fixture().await;
    budget_root(&fixture, 2.0).await;
    budget_child(&fixture, "k1", 0.5).await.unwrap();
    let caps = (
        listed(&fixture, "agent001").await["budget_usd"].clone(),
        listed(&fixture, "kid00001").await["budget_usd"].clone(),
    );
    for (caller, target) in [
        ("agent001", "agent001"),
        ("agent001", "kid00001"),
        ("kid00001", "kid00001"),
        ("kid00001", "agent001"),
    ] {
        let error = identity_request_from_outside(
            &fixture,
            caller,
            "agent_budget",
            json!({"identity":target,"add_usd":1}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Forbidden, "{caller} -> {target}");
    }
    assert_eq!(listed(&fixture, "agent001").await["budget_usd"], caps.0);
    assert_eq!(listed(&fixture, "kid00001").await["budget_usd"], caps.1);
    assert_eq!(reserved(&fixture, "agent001").await, "0.500000");
    // The person's own raise, on a human connection, still works.
    worker_call(
        &fixture,
        "agent_budget",
        json!({"identity":"kid00001","add_usd":0.5}),
    )
    .await
    .unwrap();
    assert_eq!(reserved(&fixture, "agent001").await, "1.000000");
}
