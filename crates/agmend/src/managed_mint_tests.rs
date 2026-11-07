use super::*;

use agmen_local::ApiError;
use agmen_proto::IdentityId;

const MINT_TOKEN: &str = "fake-private-mint-token";
const DECOY: &str = "decoy001";

#[derive(Clone, Copy)]
enum Answer {
    Valid,
    Unparsable,
    WrongShape,
    HandleMismatch,
    ProjectMismatch,
    TenantMismatch,
    MissingVerbs,
    /// The hub's 409 for a handle a live identity already holds.
    Taken,
}

async fn mint_hub(fixture: &mut Fixture, answer: Answer, revoke_fails: bool) {
    fixture.hub.abort();
    assert!((&mut fixture.hub).await.unwrap_err().is_cancelled());
    let revoked = fixture.revoked.clone();
    let roster = fixture.roster.clone();
    let app = axum::Router::new()
        .route("/v1/identities", axum::routing::post(move |axum::Json(body): axum::Json<Value>| async move {
            if matches!(answer, Answer::Unparsable) {
                return (axum::http::StatusCode::CREATED, format!("{MINT_TOKEN} {DECOY} {{")).into_response();
            }
            if matches!(answer, Answer::Taken) {
                return (axum::http::StatusCode::CONFLICT, axum::Json(json!({"error":"handle_taken"}))).into_response();
            }
            let mut minted = json!({"id":"agent002","handle":body["handle"],"tenant":"tenant01","project":"project1","token":MINT_TOKEN,"verbs":["FYI"]});
            match answer {
                Answer::Valid | Answer::Unparsable | Answer::Taken => {}
                Answer::WrongShape => {
                    minted["id"] = json!(DECOY);
                    minted["handle"] = json!([MINT_TOKEN]);
                }
                Answer::HandleMismatch => {
                    minted["id"] = json!(DECOY);
                    minted["handle"] = json!("teammate/agent");
                }
                Answer::ProjectMismatch => {
                    minted["id"] = json!(DECOY);
                    minted["project"] = json!("project2");
                }
                Answer::TenantMismatch => {
                    minted["id"] = json!(DECOY);
                    minted["tenant"] = json!("tenant02");
                }
                Answer::MissingVerbs => {
                    minted["id"] = json!(DECOY);
                    minted.as_object_mut().unwrap().remove("verbs");
                }
            }
            (axum::http::StatusCode::CREATED, axum::Json(minted)).into_response()
        }))
        .route("/v1/identities/{id}", axum::routing::delete(move |axum::extract::Path(id): axum::extract::Path<String>| async move {
            revoked.lock().unwrap().push(id);
            if revoke_fails {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({"error":format!("{MINT_TOKEN} {DECOY}"),"error_description":MINT_TOKEN}))).into_response()
            } else {
                axum::http::StatusCode::NO_CONTENT.into_response()
            }
        }))
        .route("/v1/projects/{project}/state", axum::routing::get(move |axum::extract::Path(project): axum::extract::Path<String>| async move {
            assert_eq!(project, "project1");
            let value = roster.lock().unwrap().clone();
            if value.is_null() {
                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({"error":format!("{MINT_TOKEN} {DECOY}")}))).into_response();
            }
            axum::Json(json!({"value":value})).into_response()
        }));
    let listener = tokio::net::TcpListener::bind(fixture.hub_url.socket_addrs(|| None).unwrap()[0])
        .await
        .unwrap();
    fixture.hub = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
}

fn entry(id: &str, handle: &str, kind: &str, status: &str) -> Value {
    json!({"identity_id":id,"handle":handle,"kind":kind,"status":status})
}

fn offline_agent() -> Value {
    entry("agent002", "owner/other", "agent", "offline")
}

fn capture() -> (LogBytes, tracing::subscriber::DefaultGuard) {
    let logs = LogBytes::default();
    let guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(logs.clone())
            .finish(),
    );
    (logs, guard)
}

fn assert_private(logs: &LogBytes, failure: &ApiError) -> String {
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    for text in [&logs, &serde_json::to_string(failure).unwrap()] {
        assert!(!text.contains(MINT_TOKEN), "mint token was exposed");
        assert!(!text.contains(DECOY), "untrusted identity was exposed");
    }
    logs
}

async fn assert_no_credentials(fixture: &Fixture) {
    for origin in [&fixture.hub_url, &"https://agmend.invalid".parse().unwrap()] {
        for id in ["agent002", DECOY] {
            assert!(
                fixture
                    .daemon
                    .core
                    .origins
                    .hub()
                    .credentials()
                    .token(origin, &id.parse().unwrap())
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }
    assert!(
        fixture
            .daemon
            .core
            .identities
            .local_token(&"agent002".parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

async fn other(fixture: &Fixture, role: Option<&str>) -> Result<Value, ApiError> {
    fixture.daemon.core.managed.launch(json!({"project":"project1","name":"other","root":fixture.dir.path().join("repo"),"role":role})).await
}

async fn invalid_answer(answer: Answer) {
    let mut fixture = Fixture::new().await;
    *fixture.roster.lock().unwrap() = json!([
        offline_agent(),
        entry(DECOY, "teammate/agent", "agent", "offline")
    ]);
    mint_hub(&mut fixture, answer, false).await;
    let (logs, _guard) = capture();
    let failure = other(&fixture, Some("reviewer")).await.unwrap_err();
    let logs = assert_private(&logs, &failure);
    assert_eq!(*fixture.revoked.lock().unwrap(), ["agent002"]);
    assert_no_credentials(&fixture).await;
    assert!(
        logs.contains("agent002"),
        "rollback must name the trusted roster id"
    );
    assert!(
        fixture.daemon.core.managed.list(json!({})).await.unwrap()["agents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn mint_unparsable_success_revokes_only_roster_match() {
    invalid_answer(Answer::Unparsable).await;
}

#[tokio::test]
async fn mint_wrong_shape_revokes_only_roster_match() {
    invalid_answer(Answer::WrongShape).await;
}

#[tokio::test]
async fn mint_handle_mismatch_revokes_only_roster_match() {
    invalid_answer(Answer::HandleMismatch).await;
}

#[tokio::test]
async fn mint_project_mismatch_revokes_only_roster_match() {
    invalid_answer(Answer::ProjectMismatch).await;
}

#[tokio::test]
async fn mint_tenant_mismatch_revokes_only_roster_match() {
    invalid_answer(Answer::TenantMismatch).await;
}

#[tokio::test]
async fn mint_missing_role_verbs_revokes_only_roster_match() {
    invalid_answer(Answer::MissingVerbs).await;
}

fn block_token_write(fixture: &Fixture, origin: &url::Url) {
    let key = agmen_client::TokenKey::new(origin, &"agent002".parse().unwrap()).unwrap();
    let hex: String = key
        .as_str()
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    // Reuse the directory-store failure injection used by identity registration tests.
    std::fs::create_dir(
        fixture
            .dir
            .path()
            .join("tokens")
            .join(format!("tok-{hex}.tmp")),
    )
    .unwrap();
}

#[tokio::test]
async fn mint_token_store_failure_revokes_validated_id() {
    let mut fixture = Fixture::new().await;
    mint_hub(&mut fixture, Answer::Valid, false).await;
    block_token_write(&fixture, &fixture.hub_url);
    let (logs, _guard) = capture();
    let failure = other(&fixture, None).await.unwrap_err();
    let logs = assert_private(&logs, &failure);
    assert_eq!(*fixture.revoked.lock().unwrap(), ["agent002"]);
    assert_no_credentials(&fixture).await;
    assert!(logs.contains("agent002"));
}

#[tokio::test]
async fn mint_register_failure_deletes_token_when_revoke_fails() {
    let mut fixture = Fixture::new().await;
    mint_hub(&mut fixture, Answer::Valid, true).await;
    block_token_write(&fixture, &"https://agmend.invalid".parse().unwrap());
    let (logs, _guard) = capture();
    let failure = other(&fixture, None).await.unwrap_err();
    assert_eq!(*fixture.revoked.lock().unwrap(), ["agent002"]);
    assert_no_credentials(&fixture).await;
    let logs = assert_private(&logs, &failure);
    assert!(logs.contains("agent002"));
    // The person is told the hub identity is still live, by id and nothing else.
    assert!(
        failure.message.contains("left behind: revoking agent002"),
        "{}",
        failure.message
    );
}

#[tokio::test]
async fn mint_register_failure_revokes_and_reports_no_leftover_identity() {
    let mut fixture = Fixture::new().await;
    mint_hub(&mut fixture, Answer::Valid, false).await;
    block_token_write(&fixture, &"https://agmend.invalid".parse().unwrap());
    let (logs, _guard) = capture();
    let failure = other(&fixture, None).await.unwrap_err();
    assert_private(&logs, &failure);
    assert_eq!(*fixture.revoked.lock().unwrap(), ["agent002"]);
    assert_no_credentials(&fixture).await;
    assert!(
        !failure.message.contains("revoking"),
        "the hub revoked it: {}",
        failure.message
    );
}

async fn refuses_lookup(roster: Value) {
    let mut fixture = Fixture::new().await;
    *fixture.roster.lock().unwrap() = roster;
    mint_hub(&mut fixture, Answer::WrongShape, false).await;
    let (logs, _guard) = capture();
    let failure = other(&fixture, None).await.unwrap_err();
    assert!(fixture.revoked.lock().unwrap().is_empty());
    assert_no_credentials(&fixture).await;
    let logs = assert_private(&logs, &failure);
    assert_eq!(
        logs.lines()
            .filter(|line| line.contains("cannot revoke an unregistered hub agent"))
            .count(),
        1,
        "one warning must explain the skipped revoke"
    );
    assert!(logs.contains("cannot revoke an unregistered hub agent"));
    assert!(!logs.contains("agent002"));
}

#[tokio::test]
async fn mint_lookup_skips_online_agent() {
    refuses_lookup(json!([entry("agent002", "owner/other", "agent", "online")])).await;
}

#[tokio::test]
async fn mint_lookup_skips_away_agent() {
    refuses_lookup(json!([entry("agent002", "owner/other", "agent", "away")])).await;
}

#[tokio::test]
async fn mint_lookup_skips_another_kind() {
    refuses_lookup(json!([entry(
        "agent002",
        "owner/other",
        "listener",
        "offline"
    )]))
    .await;
}

#[tokio::test]
async fn mint_lookup_skips_another_handle() {
    refuses_lookup(json!([entry(
        "agent002",
        "owner/someone",
        "agent",
        "offline"
    )]))
    .await;
}

#[tokio::test]
async fn mint_lookup_skips_empty_roster() {
    refuses_lookup(json!([])).await;
}

#[tokio::test]
async fn mint_lookup_skips_ambiguous_roster() {
    refuses_lookup(json!([
        offline_agent(),
        entry("agent003", "owner/other", "agent", "offline")
    ]))
    .await;
}

#[tokio::test]
async fn mint_lookup_skips_unreadable_roster() {
    refuses_lookup(Value::Null).await;
}

#[tokio::test]
async fn mint_lookup_skips_invalid_roster() {
    refuses_lookup(json!([{"identity_id":DECOY,"handle":MINT_TOKEN}])).await;
}

#[tokio::test]
async fn mint_child_failure_revokes_only_child_roster_match() {
    let (mut fixture, _) = tree_fixture().await;
    fixture.daemon.core.managed.launch(json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo"),"role":"chain"})).await.unwrap();
    let managed = &fixture.daemon.core.managed;
    let parent = worker_of(managed, "agent001").await;
    parent.lock().await.batch_tier = Some(super::super::Tier::Owner);
    let caller = parent.lock().await.record.identity.clone();
    // The parent and a sibling are listed offline too; only the child's own
    // handle may be revoked.
    *fixture.roster.lock().unwrap() = json!([
        entry("agent001", "owner/worker", "agent", "offline"),
        offline_agent(),
        entry("sibling1", "owner/sibling", "agent", "offline")
    ]);
    mint_hub(&mut fixture, Answer::HandleMismatch, false).await;
    let (logs, _guard) = capture();
    let failure = fixture
        .daemon
        .core
        .managed
        .launch_child(&caller, json!({"name":"other","role":"chain"}))
        .await
        .unwrap_err();
    let logs = assert_private(&logs, &failure);
    assert_eq!(*fixture.revoked.lock().unwrap(), ["agent002"]);
    assert_no_credentials(&fixture).await;
    assert!(logs.contains("agent002"));
    let listed = fixture.daemon.core.managed.list(json!({})).await.unwrap();
    assert_eq!(listed["agents"].as_array().unwrap().len(), 1);
    assert_eq!(listed["agents"][0]["identity"], "agent001");
    assert!(
        fixture
            .daemon
            .core
            .origins
            .hub()
            .credentials()
            .token(&fixture.hub_url, &"agent001".parse::<IdentityId>().unwrap())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn mint_refused_for_a_taken_handle_leaves_a_stopped_worker_alone() {
    let mut fixture = Fixture::new().await;
    // A stopped worker of mine, still live on the hub, holds the handle asked for.
    *fixture.roster.lock().unwrap() = json!([entry("stopped1", "owner/other", "agent", "offline")]);
    mint_hub(&mut fixture, Answer::Taken, false).await;
    let (logs, _guard) = capture();
    let failure = other(&fixture, None).await.unwrap_err();
    let logs = assert_private(&logs, &failure);
    assert!(failure.message.contains("409"), "{}", failure.message);
    assert!(fixture.revoked.lock().unwrap().is_empty());
    assert!(!logs.contains("unregistered hub agent"), "{logs}");
}

#[tokio::test]
async fn mint_child_named_like_its_parent_revokes_nothing() {
    let (mut fixture, _) = tree_fixture().await;
    fixture.daemon.core.managed.launch(json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo"),"role":"chain"})).await.unwrap();
    let managed = &fixture.daemon.core.managed;
    let parent = worker_of(managed, "agent001").await;
    parent.lock().await.batch_tier = Some(super::super::Tier::Owner);
    let caller = parent.lock().await.record.identity.clone();
    // The roster lists the parent offline under the handle the child asks for;
    // the hub refuses that handle while the parent is live.
    *fixture.roster.lock().unwrap() =
        json!([entry("agent001", "owner/worker", "agent", "offline")]);
    mint_hub(&mut fixture, Answer::Taken, false).await;
    let (logs, _guard) = capture();
    let failure = fixture
        .daemon
        .core
        .managed
        .launch_child(&caller, json!({"name":"worker","role":"chain"}))
        .await
        .unwrap_err();
    let logs = assert_private(&logs, &failure);
    assert!(fixture.revoked.lock().unwrap().is_empty());
    assert!(!logs.contains("unregistered hub agent"), "{logs}");
    let listed = fixture.daemon.core.managed.list(json!({})).await.unwrap();
    assert_eq!(listed["agents"].as_array().unwrap().len(), 1);
    assert!(
        fixture
            .daemon
            .core
            .origins
            .hub()
            .credentials()
            .token(&fixture.hub_url, &"agent001".parse::<IdentityId>().unwrap())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn mint_coordinator_lookup_matches_the_coordinator_kind() {
    let mut fixture = Fixture::new().await;
    *fixture.roster.lock().unwrap() =
        json!([entry("agent002", "owner/other", "coordinator", "offline")]);
    mint_hub(&mut fixture, Answer::WrongShape, false).await;
    let (logs, _guard) = capture();
    let failure = other(&fixture, Some("coordinator")).await.unwrap_err();
    assert_private(&logs, &failure);
    assert_eq!(*fixture.revoked.lock().unwrap(), ["agent002"]);
    assert_no_credentials(&fixture).await;
}
