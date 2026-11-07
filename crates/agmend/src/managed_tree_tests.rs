use super::*;
use crate::managed::{Source, Tier};
use agmen_local::{ApiError, ErrorCode};

const TIER_REFUSAL: &str = "launching, stopping or briefing agents needs a batch with only your owner's messages; this batch includes other senders";

#[path = "managed_budget_tests.rs"]
mod budgets;

#[path = "managed_stop_tests.rs"]
mod stop;

#[path = "managed_recents_tests.rs"]
mod recents;

#[path = "managed_child_security_tests.rs"]
mod security;

fn settings(fixture: &Fixture, text: &str) {
    std::fs::write(
        fixture.dir.path().join("settings.toml"),
        format!("[agents]\n{text}\n"),
    )
    .unwrap();
}

async fn root(fixture: &Fixture) -> Value {
    let answer = worker_call(fixture, "agent_launch", json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo"),"role":"chain"})).await.unwrap();
    tier(fixture, "agent001", Some(Tier::Owner)).await;
    answer
}

async fn tier(fixture: &Fixture, id: &str, tier: Option<Tier>) {
    worker_of(&fixture.daemon.core.managed, id)
        .await
        .lock()
        .await
        .batch_tier = tier;
}

async fn child_call(
    fixture: &Fixture,
    id: &str,
    method: &str,
    params: Value,
) -> Result<Value, ApiError> {
    child_call_with_token_of(fixture, id, id, method, params).await
}

/// A framed request that names `id` and carries the local token of `token_of`.
async fn child_call_with_token_of(
    fixture: &Fixture,
    id: &str,
    token_of: &str,
    method: &str,
    params: Value,
) -> Result<Value, ApiError> {
    let token = fixture
        .daemon
        .core
        .identities
        .local_token(&token_of.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    framed_call(fixture, id, token.expose(), method, params).await
}

/// A framed request that names `id` and carries `token` as its local token,
/// whatever the store holds.
async fn framed_call(
    fixture: &Fixture,
    id: &str,
    token: &str,
    method: &str,
    mut params: Value,
) -> Result<Value, ApiError> {
    use agmen_local::{Outcome, Request, Response, frame};
    let identity = id.parse().unwrap();
    params["local_token"] = json!(token);
    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let task = tokio::spawn(crate::api::serve_with_peer(
        server,
        fixture.daemon.core.clone(),
        |_| Ok(4242),
    ));
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        frame::write_async(&mut client, &Request::new(1, "hello", json!({"api":1})))
            .await
            .unwrap();
        let hello: Response = frame::read_async(&mut client).await.unwrap();
        assert!(matches!(hello.outcome, Outcome::Ok { .. }));
        let mut request = Request::new(2, method, params);
        request.as_ = Some(identity);
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

async fn child(fixture: &Fixture, id: &str, name: &str) -> Result<Value, ApiError> {
    child_call(
        fixture,
        id,
        "child_launch",
        json!({"name":name,"role":"chain"}),
    )
    .await
}

/// Starts the agent's real batch through `tick` and waits until it runs in `tier`.
/// The fake runner holds the batch open until the agent is stopped.
async fn running_batch(fixture: &Fixture, id: &str, tier: Tier) {
    fixture.runner.mode.store(2, Ordering::SeqCst);
    fixture.daemon.core.managed.tick().await;
    for _ in 0..200 {
        let worker = worker_of(&fixture.daemon.core.managed, id).await;
        let held = worker.lock().await;
        if held.batch_tier == Some(tier)
            && fixture
                .runner
                .runs
                .lock()
                .unwrap()
                .iter()
                .any(|run| run.cwd == held.record.worktree)
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("{tier:?} batch did not start");
}

#[tokio::test]
async fn child_launch_records_parent_and_depth() {
    let (fixture, mints) = tree_fixture().await;
    let first = root(&fixture).await;
    assert_eq!(first["depth"], 1);
    assert_eq!(first["parent"], Value::Null);
    let answer = child(&fixture, "agent001", "k1").await.unwrap();
    assert_eq!(answer["parent"], "agent001");
    assert_eq!(answer["depth"], 2);
    let listed = worker_call(&fixture, "agent_list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["agents"][1]["parent"], answer["parent"]);
    assert_eq!(listed["agents"][1]["depth"], answer["depth"]);
    assert!(mints.lock().unwrap()[0].get("delegated_by").is_none());
    assert_eq!(mints.lock().unwrap()[1]["delegated_by"], "agent001");
    let restored = restarted(&fixture).await;
    assert_eq!(
        restored.list(json!({})).await.unwrap()["agents"][1]["parent"],
        "agent001"
    );
    assert_eq!(
        restored.tree.lock().await.get(&"kid00001".parse().unwrap()),
        Some(&Some("agent001".parse().unwrap()))
    );
}

#[tokio::test]
async fn tree_methods_are_refused_from_an_others_tier_batch() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let posts = fixture.posts.lock().unwrap().len();
    for batch_tier in [Some(Tier::Others), None] {
        tier(&fixture, "agent001", batch_tier).await;
        for params in [
            json!({"name":"k1","role":"chain"}),
            json!({"name":"k1","role":"chain","brief":"review src/a.rs"}),
        ] {
            let error = child_call(&fixture, "agent001", "child_launch", params)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Forbidden);
            assert_eq!(error.message, TIER_REFUSAL);
        }
    }
    enqueue_from(&fixture, 1, "sender01", "agent001").await;
    running_batch(&fixture, "agent001", Tier::Others).await;
    assert!(!fixture.runner.runs.lock().unwrap()[0].owner_only);
    let error = child(&fixture, "agent001", "k1").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(error.message, TIER_REFUSAL);
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(fixture.posts.lock().unwrap().len(), posts);
    fixture.daemon.core.managed.shutdown().await;
    assert_eq!(
        worker_of(&fixture.daemon.core.managed, "agent001")
            .await
            .lock()
            .await
            .batch_tier,
        None
    );
}

#[tokio::test]
async fn child_stop_is_refused_from_an_others_tier_batch() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    for batch_tier in [Some(Tier::Others), None] {
        tier(&fixture, "agent001", batch_tier).await;
        let error = child_call(
            &fixture,
            "agent001",
            "child_stop",
            json!({"identity":"kid00001"}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Forbidden);
        assert_eq!(error.message, TIER_REFUSAL);
    }
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn ancestor_messages_run_in_the_others_tier() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    enqueue_from_agent(&fixture, 1, "agent001", "kid00001").await;
    batch(&fixture.daemon.core.managed, "kid00001").await;
    let runs = fixture.runner.runs.lock().unwrap();
    assert_eq!(runs.len(), 1);
    assert!(!runs[0].owner_only);
    assert!(
        runs[0]
            .args()
            .windows(2)
            .any(|args| args == ["--disallowedTools", "Bash,Edit,Write,NotebookEdit"])
    );
}

#[tokio::test]
async fn parent_dm_from_an_others_tier_batch_keeps_the_child_in_the_others_tier() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    enqueue_from(&fixture, 1, "sender01", "agent001").await;
    running_batch(&fixture, "agent001", Tier::Others).await;
    assert!(!fixture.runner.runs.lock().unwrap()[0].owner_only);
    enqueue_from_agent(&fixture, 2, "agent001", "kid00001").await;
    running_batch(&fixture, "kid00001", Tier::Others).await;
    assert_eq!(
        worker_of(&fixture.daemon.core.managed, "kid00001")
            .await
            .lock()
            .await
            .batch_tier,
        Some(Tier::Others)
    );
    fixture.daemon.core.managed.shutdown().await;
    let runs = fixture.runner.runs.lock().unwrap();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| !run.owner_only));
}

#[tokio::test]
async fn brief_reaches_the_child_as_a_dm_from_the_parent() {
    let (mut fixture, _) = tree_fixture_with_verbs(json!(["FYI", "DM"])).await;
    Arc::get_mut(&mut Arc::get_mut(&mut fixture.daemon.core).unwrap().managed)
        .unwrap()
        .identities
        .take();
    let posted = Arc::new(Mutex::new(Vec::new()));
    let captured = posted.clone();
    let messages = fixture.posts.clone();
    Arc::get_mut(&mut Arc::get_mut(&mut fixture.daemon.core).unwrap().identities)
        .unwrap()
        .publisher = Some(Arc::new(move |sender, envelope| {
        messages.lock().unwrap().push(envelope.clone());
        captured.lock().unwrap().push((sender, envelope));
        Box::pin(async {
            Ok(OutboxState::Recorded {
                stream: "inbox:kid00001".parse().unwrap(),
                seq: agmen_proto::Seq(1),
            })
        })
    }));
    fixture
        .daemon
        .core
        .managed
        .attach(&fixture.daemon.core.identities)
        .await;
    root(&fixture).await;
    child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"chain","brief":"review src/a.rs"}),
    )
    .await
    .unwrap();
    let (sender, envelope) = posted.lock().unwrap()[2].clone();
    assert_eq!(posted.lock().unwrap().len(), 3);
    assert_eq!(sender.identity_id.as_str(), "agent001");
    assert!(matches!(envelope.payload, agmen_proto::Payload::Dm {}));
    assert_eq!(envelope.to, vec!["kid00001".parse().unwrap()]);
    assert_eq!(envelope.body.as_str(), "review src/a.rs");
    let mut stamped = serde_json::to_value(envelope).unwrap();
    let fields = stamped.as_object_mut().unwrap();
    fields.extend(serde_json::from_value::<serde_json::Map<String, Value>>(json!({"from":"agent001","from_handle":"owner/worker","from_kind":"agent","mentioned":["kid00001"],"stream":"inbox:kid00001","seq":1,"hub_at":"2026-10-25T16:04:12Z"})).unwrap());
    fixture
        .daemon
        .core
        .identities
        .enqueue_test(serde_json::from_value(stamped).unwrap())
        .await;
    batch(&fixture.daemon.core.managed, "kid00001").await;
    let runs = fixture.runner.runs.lock().unwrap();
    assert_eq!(runs.len(), 1);
    assert!(!runs[0].owner_only);
    assert!(runs[0].prompt.contains("review src/a.rs"));
}

#[tokio::test]
async fn oversized_brief_is_refused_before_minting() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    assert!(
        child_call(
            &fixture,
            "agent001",
            "child_launch",
            json!({"name":"k1","role":"chain","brief":"x".repeat(agmen_proto::MAX_BODY_BYTES + 1)})
        )
        .await
        .is_err()
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn child_launch_refuses_a_role_not_in_the_list() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let error = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"reviewer"}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.message,
        "role chain may not launch reviewer; it may launch: chain"
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn role_without_a_list_launches_nothing() {
    for role in [Some("reviewer"), None] {
        let (fixture, mints) = tree_fixture().await;
        let mut params =
            json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo")});
        if let Some(role) = role {
            params["role"] = json!(role);
        }
        worker_call(&fixture, "agent_launch", params).await.unwrap();
        tier(&fixture, "agent001", Some(Tier::Owner)).await;
        let error = child(&fixture, "agent001", "k1").await.unwrap_err();
        assert_eq!(
            error.message,
            format!(
                "role {} may not launch chain; it may launch: nothing",
                role.unwrap_or("(none)")
            )
        );
        assert_eq!(mints.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn inherited_launch_list_is_effective() {
    let (fixture, _) = tree_fixture().await;
    std::fs::write(fixture.dir.path().join("repo/.agmen/roles/inherited.xml"), "<role name=\"inherited\" extends=\"chain\" version=\"1\"><summary>inherited</summary></role>").unwrap();
    worker_call(&fixture, "agent_launch", json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo"),"role":"inherited"})).await.unwrap();
    tier(&fixture, "agent001", Some(Tier::Owner)).await;
    assert!(child(&fixture, "agent001", "k1").await.is_ok());
}

#[tokio::test]
async fn child_role_must_be_an_agent_role() {
    for kind in ["coordinator", "listener"] {
        let (fixture, mints) = tree_fixture().await;
        let roles = fixture.dir.path().join("personal-roles");
        std::fs::create_dir(&roles).unwrap();
        std::fs::write(roles.join("boss.xml"), format!("<role name=\"boss\" extends=\"base\" kind=\"{kind}\" tier=\"standard\" version=\"1\"><summary>boss</summary></role>")).unwrap();
        std::fs::write(fixture.dir.path().join("repo/.agmen/roles/chain.xml"), "<role name=\"chain\" extends=\"base\" kind=\"agent\" tier=\"standard\" version=\"1\"><summary>c</summary><launches roles=\"boss\"/></role>").unwrap();
        let mut fixture = fixture;
        Arc::get_mut(&mut Arc::get_mut(&mut fixture.daemon.core).unwrap().managed)
            .unwrap()
            .personal_roles = Some(roles);
        root(&fixture).await;
        let error = child_call(
            &fixture,
            "agent001",
            "child_launch",
            json!({"name":"k1","role":"boss"}),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.message,
            "a launched agent's role must be an agent role"
        );
        assert_eq!(mints.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn depth_limit_counts_levels() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "max_tree_depth = 2");
    assert_eq!(root(&fixture).await["depth"], 1);
    assert_eq!(child(&fixture, "agent001", "k1").await.unwrap()["depth"], 2);
    tier(&fixture, "kid00001", Some(Tier::Owner)).await;
    let error = child(&fixture, "kid00001", "k2").await.unwrap_err();
    assert_eq!(
        error.message,
        "tree depth limit 2 reached: owner/k1 is at depth 2. Raise or turn off Tree depth in Settings, Agents"
    );
    settings(&fixture, "max_tree_depth = 2\nlimit_tree_depth = false");
    assert_eq!(child(&fixture, "kid00001", "k2").await.unwrap()["depth"], 3);
}

#[tokio::test]
async fn agent_count_counts_running_agents() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "max_agents = 2");
    root(&fixture).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    let error = child(&fixture, "agent001", "k2").await.unwrap_err();
    assert_eq!(
        error.message,
        "2 agents are already running for you. Raise or turn off Agents running per person in Settings, Agents"
    );
    worker_call(&fixture, "agent_stop", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    assert!(child(&fixture, "agent001", "k2").await.is_ok());
}

#[tokio::test]
async fn concurrent_child_launches_respect_the_count() {
    let (fixture, mints) = tree_fixture().await;
    settings(&fixture, "max_agents = 2");
    root(&fixture).await;
    let (a, b) = tokio::join!(
        child(&fixture, "agent001", "k1"),
        child(&fixture, "agent001", "k2")
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(mints.lock().unwrap().len(), 2);
    assert_eq!(fixture.daemon.core.managed.workers.lock().await.len(), 2);
}

#[tokio::test]
async fn human_launch_applies_the_count() {
    let (fixture, mints) = tree_fixture().await;
    settings(&fixture, "max_agents = 1");
    root(&fixture).await;
    let error = worker_call(
        &fixture,
        "agent_launch",
        json!({"project":"project1","name":"other","root":fixture.dir.path().join("repo")}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.message,
        "1 agents are already running for you. Raise or turn off Agents running per person in Settings, Agents"
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
}

/// A chain that may launch its own role never gets past the registry bound.
///
/// 256 real launches take two minutes (about fifteen git runs each), so most of
/// the registry is filled with offline stand-ins and the chain runs for real
/// into the bound. The store's own bound, the same constant, is pinned by
/// `registration_and_stream_counts_are_bounded`.
#[tokio::test]
async fn launch_chain_stops_at_the_registry_bound_with_limits_off() {
    const REAL: usize = 5;
    let max = agmen_local::safeguards::MAX_MANAGED;
    let (fixture, mints) = tree_fixture().await;
    settings(&fixture, "limit_tree_depth = false\nlimit_agents = false");
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    let template = worker_of(managed, "agent001")
        .await
        .lock()
        .await
        .record
        .clone();
    for n in 0..max - 1 - REAL {
        let mut record = template.clone();
        record.identity.identity_id = format!("fill{n:04}").parse().unwrap();
        let budget = crate::budget::Budget::new(crate::budget::Money::from_usd("5").unwrap());
        managed
            .insert_worker(crate::managed::Worker::offline(record, budget))
            .await
            .unwrap();
    }
    let mut caller = "agent001".to_owned();
    let mut refused = false;
    for n in 1..=REAL + 5 {
        tier(&fixture, &caller, Some(Tier::Owner)).await;
        match child(&fixture, &caller, &format!("k{n}")).await {
            Ok(answer) => {
                assert_eq!(answer["depth"], n + 1);
                caller = answer["identity"].as_str().unwrap().to_owned();
            }
            Err(error) => {
                assert_eq!(n, REAL + 1);
                assert_eq!(
                    error.message,
                    "256 managed agents are already registered. agmend stops at 256 agents"
                );
                refused = true;
                break;
            }
        }
    }
    assert!(refused);
    assert_eq!(managed.workers.lock().await.len(), max);
    assert_eq!(managed.tree.lock().await.len(), max);
    assert_eq!(mints.lock().unwrap().len(), 1 + REAL);
}

#[tokio::test]
async fn stopped_hub_agent_still_counts_toward_the_cap() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    let managed = &fixture.daemon.core.managed;
    assert_eq!(managed.workers.lock().await.len(), 1);
    let identities = managed
        .identities()
        .unwrap()
        .managed(
            agmen_client::origin_key(&fixture.hub_url),
            "owner001".parse().unwrap(),
        )
        .await
        .unwrap();
    assert!(
        identities
            .iter()
            .any(|identity| identity.identity_id.as_str() == "agent001")
    );
}

#[tokio::test]
async fn invalid_agents_table_refuses_launch() {
    let (fixture, mints) = tree_fixture().await;
    settings(&fixture, "max_agents = \"many\"");
    let params = json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo"),"role":"chain"});
    assert!(
        worker_call(&fixture, "agent_launch", params.clone())
            .await
            .unwrap_err()
            .message
            .contains("settings.toml [agents]")
    );
    assert!(mints.lock().unwrap().is_empty());
    settings(&fixture, "");
    root(&fixture).await;
    settings(&fixture, "max_agents = \"many\"");
    assert!(
        child(&fixture, "agent001", "k1")
            .await
            .unwrap_err()
            .message
            .contains("settings.toml [agents]")
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn child_launch_refuses_a_supplied_parent() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    for field in ["parent", "delegated_by"] {
        let mut params = json!({"name":"k1","role":"chain"});
        params[field] = json!("sender01");
        assert_eq!(
            child_call(&fixture, "agent001", "child_launch", params)
                .await
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
    }
    assert_eq!(mints.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn worker_peer_may_call_child_methods_only_with_identity() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    let identity = worker_of(&fixture.daemon.core.managed, "agent001")
        .await
        .lock()
        .await
        .record
        .identity
        .clone();
    for method in ["child_launch", "child_stop"] {
        assert!(
            crate::api::owner_peer_check(
                &fixture.daemon.core.managed,
                &crate::identities::Caller::Identity(identity.clone()),
                method,
                || Ok(4242)
            )
            .is_ok()
        );
        let error = crate::api::owner_peer_check(
            &fixture.daemon.core.managed,
            &crate::identities::Caller::Human,
            method,
            || Ok(4242),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Forbidden);
        assert_eq!(error.message, "agents cannot call owner-only methods");
        assert_eq!(
            worker_call(&fixture, method, json!({}))
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
}

#[tokio::test]
async fn only_launched_agents_of_this_login_can_launch() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    let original = worker_of(managed, "agent001")
        .await
        .lock()
        .await
        .record
        .clone();
    let mut unknown = original.identity.clone();
    unknown.identity_id = "unknown1".parse().unwrap();
    assert_eq!(
        managed
            .launch_child(&unknown, json!({"name":"k1","role":"chain"}))
            .await
            .unwrap_err()
            .message,
        "only agents agmen launched can launch agents"
    );
    worker_of(managed, "agent001")
        .await
        .lock()
        .await
        .record
        .source = Source::Joined;
    assert_eq!(
        child(&fixture, "agent001", "k1").await.unwrap_err().message,
        "only agents agmen launched can launch agents"
    );
    worker_of(managed, "agent001").await.lock().await.record = original.clone();
    let mut login: Login = serde_json::from_str(
        &std::fs::read_to_string(fixture.dir.path().join("login.json")).unwrap(),
    )
    .unwrap();
    login.human_id = "other001".parse().unwrap();
    std::fs::write(
        fixture.dir.path().join("login.json"),
        serde_json::to_vec(&login).unwrap(),
    )
    .unwrap();
    assert_eq!(
        managed
            .launch_child(&original.identity, json!({"name":"k1","role":"chain"}))
            .await
            .unwrap_err()
            .message,
        "only agents agmen launched can launch agents"
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stored_record_without_tree_fields_loads_as_root() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    let file = fixture
        .daemon
        .core
        .managed
        .agent_dir(&"agent001".parse().unwrap())
        .join("agent.json");
    let mut saved: Value = serde_json::from_str(&role_file(&file).unwrap()).unwrap();
    for key in ["parent", "depth", "shared_budget"] {
        saved.as_object_mut().unwrap().remove(key);
    }
    std::fs::write(file, serde_json::to_vec(&saved).unwrap()).unwrap();
    let restored = restarted(&fixture).await;
    let held = worker_of(&restored, "agent001").await;
    let held = held.lock().await;
    assert_eq!(held.record.parent, None);
    assert_eq!(held.record.depth, 1);
    assert!(!held.record.shared_budget);
    drop(held);
    assert_eq!(
        restored.tree.lock().await.get(&"agent001".parse().unwrap()),
        Some(&None)
    );
}

#[tokio::test]
async fn child_worktree_branches_from_the_parent_head() {
    let (fixture, _) = tree_fixture().await;
    let parent = root(&fixture).await;
    let folder = std::path::Path::new(parent["worktree"].as_str().unwrap());
    std::fs::write(folder.join("parent-change"), "parent commit\n").unwrap();
    git(folder, &["add", "parent-change"]);
    git(
        folder,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "parent change",
        ],
    );
    let answer = child(&fixture, "agent001", "k1").await.unwrap();
    let folder_child = std::path::Path::new(answer["worktree"].as_str().unwrap());
    git(
        folder,
        &[
            "merge-base",
            "--is-ancestor",
            "HEAD",
            answer["branch"].as_str().unwrap(),
        ],
    );
    assert_eq!(
        std::fs::read_to_string(folder_child.join("parent-change")).unwrap(),
        "parent commit\n"
    );
    assert_eq!(folder_child.parent(), folder.parent());
}

#[tokio::test]
async fn stopped_worker_has_no_tree_authority() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    let error = child(&fixture, "agent001", "k1").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(error.message, TIER_REFUSAL);
}

#[tokio::test]
async fn failed_brief_does_not_leave_a_registered_child() {
    let (mut fixture, _) = tree_fixture_with_verbs(json!(["FYI", "DM"])).await;
    Arc::get_mut(&mut Arc::get_mut(&mut fixture.daemon.core).unwrap().managed)
        .unwrap()
        .identities
        .take();
    Arc::get_mut(&mut Arc::get_mut(&mut fixture.daemon.core).unwrap().identities)
        .unwrap()
        .publisher = Some(Arc::new(|_, envelope| {
        Box::pin(async move {
            if matches!(envelope.payload, agmen_proto::Payload::Dm {}) {
                Err(ApiError {
                    code: ErrorCode::Hub,
                    message: "brief outbox failed".into(),
                })
            } else {
                Ok(OutboxState::Pending { attempts: 0 })
            }
        })
    }));
    fixture
        .daemon
        .core
        .managed
        .attach(&fixture.daemon.core.identities)
        .await;
    root(&fixture).await;
    let before = git_state(&fixture);
    std::fs::create_dir_all(
        fixture
            .daemon
            .core
            .managed
            .state_dir
            .join("identities/kid00001"),
    )
    .unwrap();
    let error = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"chain","brief":"review src/a.rs"}),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("brief outbox failed"));
    assert_discarded(&fixture, "kid00001", &before).await;
    assert!(
        !fixture
            .daemon
            .core
            .managed
            .state_dir
            .join("identities/kid00001")
            .exists()
    );
}

/// The checkout's worktrees and branches, to compare around a failed launch.
fn git_state(fixture: &Fixture) -> (Vec<u8>, Vec<u8>) {
    let repo = fixture.dir.path().join("repo");
    (
        listing(&repo, &["worktree", "list", "--porcelain"]),
        listing(&repo, &["branch", "--list"]),
    )
}

/// Everything a launch made for `id` after the hub minted it is gone, and the
/// parent is as it was.
async fn assert_discarded(fixture: &Fixture, id: &str, before: &(Vec<u8>, Vec<u8>)) {
    assert_discarded_but_the_row(fixture, id, before, 1).await;
    assert!(
        fixture
            .daemon
            .core
            .identities
            .local_token(&id.parse().unwrap())
            .await
            .unwrap()
            .is_none(),
        "store row"
    );
}

/// As `assert_discarded`, with `kept` workers left in the registry, and without
/// the store row, which a stopped daemon cannot be asked about.
async fn assert_discarded_but_the_row(
    fixture: &Fixture,
    id: &str,
    before: &(Vec<u8>, Vec<u8>),
    kept: usize,
) {
    let managed = &fixture.daemon.core.managed;
    let identity: agmen_proto::IdentityId = id.parse().unwrap();
    assert_eq!(*fixture.revoked.lock().unwrap(), [id.to_owned()]);
    let credential = |origin: &url::Url| {
        fixture
            .tokens
            .get(&agmen_client::TokenKey::new(origin, &identity).unwrap())
            .unwrap()
    };
    assert!(credential(&fixture.hub_url).is_none(), "hub credential");
    assert!(
        credential(&"https://agmend.invalid".parse().unwrap()).is_none(),
        "local credential"
    );
    assert!(!managed.agent_dir(&identity).exists(), "record folder");
    assert!(
        !managed.state_dir.join("agents").join(id).exists(),
        "role prompt folder"
    );
    assert_eq!(managed.workers.lock().await.len(), kept);
    assert_eq!(managed.tree.lock().await.len(), kept);
    assert_eq!(&git_state(fixture), before, "worktree and branch");
}

/// The guard must let the owner through, so the tier comes from a real batch.
#[tokio::test]
async fn an_owner_batch_may_launch_while_it_runs() {
    let (fixture, mints) = tree_fixture().await;
    let params = json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo"),"role":"chain"});
    worker_call(&fixture, "agent_launch", params).await.unwrap();
    enqueue(&fixture, 1, "agent001").await;
    running_batch(&fixture, "agent001", Tier::Owner).await;
    assert!(fixture.runner.runs.lock().unwrap()[0].owner_only);
    let answer = child(&fixture, "agent001", "k1").await.unwrap();
    assert_eq!(answer["parent"], "agent001");
    assert_eq!(mints.lock().unwrap().len(), 2);
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn a_batch_that_ends_while_the_launch_waits_is_refused() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let managed = fixture.daemon.core.managed.clone();
    let busy = managed.launching.lock().await;
    let end_batch = async {
        // By now the call has passed the first tier check and waits for the lock.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        tier(&fixture, "agent001", None).await;
        drop(busy);
    };
    let (result, ()) = tokio::join!(child(&fixture, "agent001", "k1"), end_batch);
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(error.message, TIER_REFUSAL);
    assert_eq!(mints.lock().unwrap().len(), 1);
}

/// A parent can write its own worktree. A child must not take its broker, or
/// any other part of its identity, from a file the parent can change.
#[tokio::test]
async fn child_launch_refuses_a_tampered_parent_identity_file() {
    let (fixture, mints) = tree_fixture().await;
    let parent = root(&fixture).await;
    let file =
        std::path::Path::new(parent["worktree"].as_str().unwrap()).join(".agmen/identity.json");
    let original = std::fs::read_to_string(&file).unwrap();
    let before = git_state(&fixture);
    for (key, value) in [
        ("broker_url", "mqtt://evil.example:1883"),
        ("handle", "owner/other"),
        ("identity_id", "other001"),
    ] {
        let mut changed: Value = serde_json::from_str(&original).unwrap();
        changed[key] = json!(value);
        std::fs::write(&file, serde_json::to_vec(&changed).unwrap()).unwrap();
        let error = child(&fixture, "agent001", "k1").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest, "{key}");
        assert!(error.message.contains(".agmen/identity.json"), "{key}");
    }
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(git_state(&fixture), before);
    std::fs::write(&file, original).unwrap();
    assert!(child(&fixture, "agent001", "k1").await.is_ok());
}

/// The list a parent may launch from is the one it was launched with.
#[tokio::test]
async fn child_launch_refuses_a_parent_whose_role_changed_since_launch() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let file = fixture.dir.path().join("repo/.agmen/roles/chain.xml");
    let edited = std::fs::read_to_string(&file)
        .unwrap()
        .replace("<summary>c</summary>", "<summary>changed</summary>");
    std::fs::write(&file, edited).unwrap();
    let error = child(&fixture, "agent001", "k1").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert_eq!(
        error.message,
        "role chain changed since launch; relaunch the agent to use it"
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
}

/// The store and the registry share one bound, so a stale row can fill the
/// store first. The launch then fails after the mint and must undo it.
#[tokio::test]
async fn a_full_store_after_the_mint_leaves_no_identity_behind() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let before = git_state(&fixture);
    std::fs::create_dir_all(
        fixture
            .daemon
            .core
            .managed
            .state_dir
            .join("identities/kid00001"),
    )
    .unwrap();
    let template = worker_of(&fixture.daemon.core.managed, "agent001")
        .await
        .lock()
        .await
        .record
        .identity
        .clone();
    for n in 0..agmen_local::safeguards::MAX_MANAGED - 1 {
        let mut stale = template.clone();
        stale.identity_id = format!("stale{n:04}").parse().unwrap();
        fixture
            .daemon
            .core
            .identities
            .register(stale)
            .await
            .unwrap();
    }
    let error = child(&fixture, "agent001", "k1").await.unwrap_err();
    assert!(
        error.message.contains("daemon state operation failed"),
        "{}",
        error.message
    );
    assert_eq!(mints.lock().unwrap().len(), 2);
    assert_discarded(&fixture, "kid00001", &before).await;
    assert!(
        !fixture
            .daemon
            .core
            .managed
            .state_dir
            .join("identities/kid00001")
            .exists()
    );
}

/// The same goes for the git pointer in a parent's worktree: a child made
/// through it would land in whatever repository the parent names.
#[tokio::test]
async fn child_launch_refuses_a_parent_worktree_that_points_at_another_repository() {
    let (fixture, mints) = tree_fixture().await;
    let parent = root(&fixture).await;
    let other = fixture.dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.invalid"],
    ] {
        git(&other, &args);
    }
    std::fs::write(other.join(".gitignore"), ".agmen/\n.worktrees/\n").unwrap();
    git(&other, &["add", ".gitignore"]);
    git(
        &other,
        &["-c", "commit.gpgsign=false", "commit", "-m", "initial"],
    );
    let pointer = std::path::Path::new(parent["worktree"].as_str().unwrap()).join(".git");
    let original = std::fs::read_to_string(&pointer).unwrap();
    std::fs::write(
        &pointer,
        format!("gitdir: {}\n", other.join(".git").display()),
    )
    .unwrap();
    let error = child(&fixture, "agent001", "k1").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert_eq!(
        error.message,
        "the parent's worktree no longer belongs to the checkout it was launched from"
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert!(!other.join(".worktrees").exists());
    std::fs::write(&pointer, original).unwrap();
    assert!(child(&fixture, "agent001", "k1").await.is_ok());
}

/// A corrupt record can name its own descendant as its parent. Nothing walks
/// the links today: a depth comes from the stored number. A walk added later
/// needs its own bound.
#[tokio::test]
async fn parent_links_that_form_a_cycle_on_disk_load_and_launch_without_looping() {
    let (fixture, _mints) = tree_fixture().await;
    root(&fixture).await;
    let params = json!({"project":"project1","name":"other","root":fixture.dir.path().join("repo"),"role":"chain"});
    worker_call(&fixture, "agent_launch", params).await.unwrap();
    let managed = &fixture.daemon.core.managed;
    for (id, parent) in [("agent001", "kid00001"), ("kid00001", "agent001")] {
        let file = managed.agent_dir(&id.parse().unwrap()).join("agent.json");
        let mut saved: Value = serde_json::from_str(&role_file(&file).unwrap()).unwrap();
        saved["parent"] = json!(parent);
        std::fs::write(file, serde_json::to_vec(&saved).unwrap()).unwrap();
    }
    let restored = restarted(&fixture).await;
    let limit = std::time::Duration::from_secs(10);
    let listed = tokio::time::timeout(limit, restored.list(json!({})))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listed["agents"].as_array().unwrap().len(), 2);
    assert_eq!(restored.tree.lock().await.len(), 2);
    let worker = worker_of(&restored, "agent001").await;
    let identity = {
        let mut held = worker.lock().await;
        held.batch_tier = Some(Tier::Owner);
        held.record.identity.clone()
    };
    let launched = tokio::time::timeout(
        limit,
        restored.launch_child(&identity, json!({"name":"k1","role":"chain"})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(launched["parent"], "agent001");
    assert_eq!(launched["depth"], 2);
}

/// An agent knows only its own token, so it cannot launch as another agent.
#[tokio::test]
async fn child_launch_as_another_agent_needs_that_agents_token() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    tier(&fixture, "kid00001", Some(Tier::Owner)).await;
    let error = child_call_with_token_of(
        &fixture,
        "kid00001",
        "agent001",
        "child_launch",
        json!({"name":"k2","role":"chain"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unauthorized);
    assert_eq!(mints.lock().unwrap().len(), 2);
}
