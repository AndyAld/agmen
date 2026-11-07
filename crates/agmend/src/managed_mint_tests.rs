use super::*;

use crate::hub_mint::tests::{Calls, Failure, connect_failure_hub, mint_router};
use agmen_local::ApiError;
use agmen_proto::IdentityId;

async fn ambiguous_hub(f: &mut Fixture, failure: Failure, roster: Value) -> Arc<Calls> {
    failure.configure(f.daemon.core.origins.hub());
    let (app, calls) = mint_router(failure, "agent002", roster);
    f.hub.abort();
    assert!((&mut f.hub).await.unwrap_err().is_cancelled());
    let socket = tokio::net::TcpListener::bind(f.hub_url.socket_addrs(|| None).unwrap()[0])
        .await
        .unwrap();
    f.hub = tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
    calls
}

async fn ambiguous_agent_rollback(failure: Failure) {
    let mut f = Fixture::new().await;
    let calls = ambiguous_hub(&mut f, failure, json!([offline_agent()])).await;
    let (logs, _guard) = capture();
    let error = other(&f, None).await.unwrap_err();
    failure.assert_error(&error);
    assert_eq!(calls.mints.load(Ordering::SeqCst), 1);
    assert_eq!(calls.roster.load(Ordering::SeqCst), 1);
    assert_eq!(*calls.revoked.lock().unwrap(), ["agent002"]);
    assert!(assert_private(&logs, &error).contains("agent002"));
    assert_no_credentials(&f).await;
    assert!(f.daemon.core.managed.workers.lock().await.is_empty());
    let login = f
        .daemon
        .core
        .origins
        .hub()
        .credentials()
        .login()
        .await
        .unwrap();
    assert!(
        !f.daemon
            .core
            .managed
            .on_record(&login, &"agent002".parse().unwrap())
            .await
    );
    assert_eq!(other(&f, None).await.unwrap()["identity"], "agent002");
    assert_eq!(calls.mints.load(Ordering::SeqCst), 2);
    assert_eq!(*calls.revoked.lock().unwrap(), ["agent002"]);
    f.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn agent_ambiguous_late_headers_revokes_and_allows_retry() {
    ambiguous_agent_rollback(Failure::LateHeaders).await;
}

#[tokio::test]
async fn agent_ambiguous_late_body_revokes_and_allows_retry() {
    ambiguous_agent_rollback(Failure::LateBody).await;
}

#[tokio::test]
async fn agent_ambiguous_reset_body_revokes_and_allows_retry() {
    ambiguous_agent_rollback(Failure::ResetBody).await;
}

#[tokio::test]
async fn agent_ambiguous_oversized_body_revokes_and_allows_retry() {
    ambiguous_agent_rollback(Failure::OversizedBody).await;
}

#[tokio::test]
async fn agent_ambiguous_oversized_stream_revokes_and_allows_retry() {
    ambiguous_agent_rollback(Failure::OversizedStream).await;
}

#[tokio::test]
async fn agent_ambiguous_lookup_preserves_on_record_and_hello_entries() {
    for failure in [
        Failure::LateHeaders,
        Failure::LateBody,
        Failure::ResetBody,
        Failure::OversizedBody,
        Failure::OversizedStream,
    ] {
        for held in [true, false] {
            let mut f = Fixture::new().await;
            let mut roster = offline_agent();
            if held {
                stopped_worker(&mut f, "other").await;
            } else {
                roster["model"] = json!("sonnet");
            }
            let calls = ambiguous_hub(&mut f, failure, json!([roster])).await;
            let (logs, _guard) = capture();
            let error = other(&f, None).await.unwrap_err();
            failure.assert_error(&error);
            assert_private(&logs, &error);
            assert_eq!(calls.roster.load(Ordering::SeqCst), 1, "{failure:?}");
            assert!(calls.revoked.lock().unwrap().is_empty(), "{failure:?}");
        }
    }
}

#[tokio::test]
async fn agent_refusal_with_an_oversized_body_skips_lookup_and_revoke() {
    let mut f = Fixture::new().await;
    let failure = Failure::OversizedRefusal;
    let calls = ambiguous_hub(&mut f, failure, json!([offline_agent()])).await;
    let error = other(&f, None).await.unwrap_err();
    assert_eq!(calls.mints.load(Ordering::SeqCst), 1);
    assert_eq!(calls.roster.load(Ordering::SeqCst), 0);
    assert!(calls.revoked.lock().unwrap().is_empty());
    failure.assert_error(&error);
}

#[tokio::test]
async fn agent_mint_connect_failure_skips_lookup_and_revoke() {
    let mut f = Fixture::new().await;
    let calls = ambiguous_hub(&mut f, Failure::ResetBody, json!([offline_agent()])).await;
    let mut managed = Managed::new(
        f.daemon.core.origins.clone(),
        f.runner.clone(),
        f.dir.path().to_owned(),
        Some(f.dir.path().join("claude")),
        None,
        None,
    );
    managed.hub = connect_failure_hub(&managed.hub).await;
    managed.attach(&f.daemon.core.identities).await;
    let (logs, _guard) = capture();
    let error = Arc::new(managed)
        .launch(json!({"project":"project1","name":"other","root":f.dir.path().join("repo")}))
        .await
        .unwrap_err();
    assert_eq!(error.code, agmen_local::ErrorCode::Hub);
    assert!(error.message.contains("connect"), "{error:?}");
    assert_eq!(calls.mints.load(Ordering::SeqCst), 0);
    assert_eq!(calls.roster.load(Ordering::SeqCst), 0);
    assert!(calls.revoked.lock().unwrap().is_empty());
    assert!(
        !assert_private(&logs, &error).contains("unregistered hub"),
        "no rollback attempted"
    );
}

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

/// An offline `owner/other` whose latest HELLO set `field` to `value`.
fn said_hello(field: &str, value: &str) -> Value {
    let mut said = offline_agent();
    said[field] = json!(value);
    said
}

#[tokio::test]
async fn mint_lookup_skips_an_agent_that_said_hello_with_a_model() {
    refuses_lookup(json!([said_hello("model", "sonnet")])).await;
}

#[tokio::test]
async fn mint_lookup_skips_an_agent_that_said_hello_with_an_agent() {
    refuses_lookup(json!([said_hello("agent", "claude-code")])).await;
}

/// A worker of mine, launched as `owner/<name>` (the fake hub gives it
/// agent002) and then stopped: still live on the hub, so the roster lists it
/// offline, and agmend holds its record, row and credential.
async fn stopped_worker(fixture: &mut Fixture, name: &str) -> crate::state::ManagedIdentity {
    mint_hub(fixture, Answer::Valid, false).await;
    let managed = &fixture.daemon.core.managed;
    managed
        .launch(json!({"project":"project1","name":name,"root":fixture.dir.path().join("repo")}))
        .await
        .unwrap();
    managed.stop(json!({"identity":"agent002"})).await.unwrap();
    worker_of(managed, "agent002")
        .await
        .lock()
        .await
        .record
        .identity
        .clone()
}

async fn forget_row(fixture: &Fixture, identity: &crate::state::ManagedIdentity) {
    fixture
        .daemon
        .core
        .identities
        .forget(identity)
        .await
        .unwrap();
}

fn forget_credential(fixture: &Fixture, id: &str) {
    let key = agmen_client::TokenKey::new(&fixture.hub_url, &id.parse().unwrap()).unwrap();
    fixture.tokens.delete(&key).unwrap();
}

/// A proxy now answers 2xx junk to a mint of `owner/other` that `launcher`
/// takes: nothing may be revoked, and one warning says why.
async fn junk_refused(fixture: &mut Fixture, launcher: &Arc<Managed>) {
    mint_hub(fixture, Answer::WrongShape, false).await;
    let (logs, _guard) = capture();
    let failure = launcher
        .launch(json!({"project":"project1","name":"other","root":fixture.dir.path().join("repo")}))
        .await
        .unwrap_err();
    let logs = assert_private(&logs, &failure);
    assert!(fixture.revoked.lock().unwrap().is_empty());
    assert_eq!(
        logs.lines()
            .filter(|line| line.contains("cannot revoke an unregistered hub agent"))
            .count(),
        1,
        "one warning must explain the skipped revoke: {logs}"
    );
    assert!(!logs.contains("agent002"), "{logs}");
}

#[tokio::test]
async fn mint_lookup_skips_a_stopped_worker_on_record() {
    let mut fixture = Fixture::new().await;
    stopped_worker(&mut fixture, "other").await;
    *fixture.roster.lock().unwrap() = json!([offline_agent()]);
    let managed = fixture.daemon.core.managed.clone();
    junk_refused(&mut fixture, &managed).await;
}

#[tokio::test]
async fn mint_lookup_skips_a_record_read_back_after_a_restart() {
    let mut fixture = Fixture::new().await;
    let identity = stopped_worker(&mut fixture, "other").await;
    // Only agent.json is left for the restarted agmend to read.
    forget_row(&fixture, &identity).await;
    forget_credential(&fixture, "agent002");
    *fixture.roster.lock().unwrap() = json!([offline_agent()]);
    let managed = restarted(&fixture).await;
    junk_refused(&mut fixture, &managed).await;
}

#[tokio::test]
async fn mint_lookup_skips_an_identity_row_alone() {
    let mut fixture = Fixture::new().await;
    stopped_worker(&mut fixture, "other").await;
    let managed = fixture.daemon.core.managed.clone();
    managed.workers.lock().await.clear();
    forget_credential(&fixture, "agent002");
    *fixture.roster.lock().unwrap() = json!([offline_agent()]);
    junk_refused(&mut fixture, &managed).await;
}

#[tokio::test]
async fn mint_lookup_skips_a_stored_credential_alone() {
    let mut fixture = Fixture::new().await;
    let identity = stopped_worker(&mut fixture, "other").await;
    let managed = fixture.daemon.core.managed.clone();
    managed.workers.lock().await.clear();
    forget_row(&fixture, &identity).await;
    *fixture.roster.lock().unwrap() = json!([offline_agent()]);
    junk_refused(&mut fixture, &managed).await;
}

#[tokio::test]
async fn mint_lookup_skips_a_credential_it_cannot_read() {
    let mut fixture = Fixture::new().await;
    let key = agmen_client::TokenKey::new(&fixture.hub_url, &"agent002".parse().unwrap()).unwrap();
    let hex: String = key
        .as_str()
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    // A directory where the token file belongs makes the read fail.
    std::fs::create_dir(fixture.dir.path().join("tokens").join(format!("tok-{hex}"))).unwrap();
    *fixture.roster.lock().unwrap() = json!([offline_agent()]);
    let managed = fixture.daemon.core.managed.clone();
    junk_refused(&mut fixture, &managed).await;
}

#[tokio::test]
async fn mint_lookup_still_finds_the_fresh_mint_among_known_and_said_hello_entries() {
    let mut fixture = Fixture::new().await;
    // A stopped worker of another handle, on record.
    stopped_worker(&mut fixture, "worker").await;
    // A worker joined by hand holds the handle asked for: only its credential is on record.
    let joined =
        agmen_client::TokenKey::new(&fixture.hub_url, &"agent003".parse().unwrap()).unwrap();
    fixture.tokens.set(&joined, "joined-by-hand").unwrap();
    let mut said = entry("hello001", "owner/other", "agent", "offline");
    said["agent"] = json!("claude-code");
    said["model"] = json!("sonnet");
    *fixture.roster.lock().unwrap() = json!([
        entry("agent002", "owner/worker", "agent", "offline"),
        entry("agent003", "owner/other", "agent", "offline"),
        said,
        entry("fresh001", "owner/other", "agent", "offline")
    ]);
    mint_hub(&mut fixture, Answer::WrongShape, false).await;
    let (logs, _guard) = capture();
    let failure = other(&fixture, None).await.unwrap_err();
    let logs = assert_private(&logs, &failure);
    assert_eq!(*fixture.revoked.lock().unwrap(), ["fresh001"]);
    assert!(logs.contains("fresh001"), "{logs}");
}

#[tokio::test]
async fn mint_lookup_skips_an_agmend_db_it_cannot_read() {
    let mut fixture = Fixture::new().await;
    // A row agmend cannot parse makes its read of this person's rows fail.
    rusqlite::Connection::open(fixture.dir.path().join("agmend.db"))
        .unwrap()
        .execute(
            "INSERT INTO identities VALUES (?1, 'broken01', 'owner001', 'project1', 'not json')",
            [agmen_client::origin_key(&fixture.hub_url)],
        )
        .unwrap();
    *fixture.roster.lock().unwrap() = json!([offline_agent()]);
    let managed = fixture.daemon.core.managed.clone();
    junk_refused(&mut fixture, &managed).await;
}
