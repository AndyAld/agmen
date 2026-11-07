use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

// The id of this start's mint, and the id of someone else's live listener that a
// confused or hostile hub names in its answer instead.
const MINTED: &str = "listen01";
const DECOY: &str = "decoy001";

#[derive(Clone, Copy)]
enum Answer {
    // 200 with a body that is not JSON.
    NotJson,
    // JSON with a field of the wrong type.
    Misshapen,
    // A valid answer for another handle, naming the decoy.
    OtherHandle,
    // A valid answer for another project, naming the decoy.
    OtherProject,
    // A valid answer; the test blocks storing its credential.
    #[cfg(feature = "test-hooks")]
    Valid,
}

#[derive(Clone)]
enum Listed {
    // The roster lists the mint as a listener that never connected.
    Offline,
    // The roster lists a connected listener with the minted handle.
    Online,
    // The roster does not list the mint.
    Absent,
    // The roster answers 503.
    Unreadable,
    Custom(Value),
}

fn warnings() -> (
    Arc<StdMutex<Vec<u8>>>,
    impl tracing::Subscriber + Send + Sync,
) {
    let bytes = Arc::new(StdMutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(TraceBytes(bytes.clone()))
        .finish();
    (bytes, subscriber)
}

// The hub keeps a mint live until its DELETE, so a retry proves the rollback.
// Returns the ids it was asked to revoke.
async fn mint_hub(f: &mut Fixture, answer: Answer, listed: Listed) -> Arc<StdMutex<Vec<String>>> {
    let live = Arc::new(AtomicBool::new(false));
    let first = Arc::new(AtomicBool::new(true));
    let revokes = Arc::new(StdMutex::new(Vec::new()));
    let (minted, listing, revoked, calls) =
        (live.clone(), live.clone(), live.clone(), revokes.clone());
    let app = axum::Router::new()
        .route(
            "/v1/me",
            axum::routing::get(|| async {
                axum::Json(json!({"broker_url":"mqtt://127.0.0.1:9"}))
            }),
        )
        .route(
            "/v1/identities",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let exists = minted.swap(true, Ordering::SeqCst);
                let first = first.swap(false, Ordering::SeqCst);
                async move {
                    if exists {
                        return (
                            axum::http::StatusCode::CONFLICT,
                            axum::Json(json!({"error":"listener_exists"})),
                        )
                            .into_response();
                    }
                    let mut response = json!({"id":MINTED,"handle":body["handle"],"project":"project1","tenant":"tenant01","token":"fake-hub-credential"});
                    if first {
                        match answer {
                            Answer::NotJson => {
                                return "{\"token\":\"fake-hub-credential\",\"id\":".into_response();
                            }
                            Answer::Misshapen => response["tenant"] = json!(false),
                            Answer::OtherHandle => {
                                response["id"] = json!(DECOY);
                                response["handle"] = json!("other/listener");
                            }
                            Answer::OtherProject => {
                                response["id"] = json!(DECOY);
                                response["project"] = json!("project2");
                            }
                            #[cfg(feature = "test-hooks")]
                            Answer::Valid => (),
                        }
                    }
                    axum::Json(response).into_response()
                }
            }),
        )
        .route(
            "/v1/identities/{id}",
            axum::routing::delete(move |axum::extract::Path(id): axum::extract::Path<String>| {
                if id == MINTED {
                    revoked.store(false, Ordering::SeqCst);
                }
                calls.lock().unwrap().push(id);
                async { axum::http::StatusCode::NO_CONTENT }
            }),
        )
        .route(
            "/v1/projects/{project}/state",
            axum::routing::get(move || {
                let live = listing.load(Ordering::SeqCst);
                let listed = listed.clone();
                async move {
                    if let Listed::Custom(roster) = &listed {
                        return axum::Json(json!({"value":roster})).into_response();
                    }
                    if matches!(listed, Listed::Unreadable) {
                        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    let mut roster = vec![
                        json!({"identity_id":"coord001","handle":"adam/coord","kind":"coordinator","status":"online"}),
                        json!({"identity_id":DECOY,"handle":"other/listener","kind":"listener","status":"offline"}),
                    ];
                    let status = match listed {
                        Listed::Offline => Some("offline"),
                        Listed::Online => Some("online"),
                        Listed::Absent | Listed::Unreadable | Listed::Custom(_) => None,
                    };
                    if let (true, Some(status)) = (live, status) {
                        roster.push(json!({"identity_id":MINTED,"handle":"owner/listener","kind":"listener","status":status}));
                    }
                    axum::Json(json!({"value":roster})).into_response()
                }
            }),
        );
    let login = f.listeners().hub.credentials().login().await.unwrap();
    f.hub_task.abort();
    assert!((&mut f.hub_task).await.unwrap_err().is_cancelled());
    let socket = tokio::net::TcpListener::bind(("127.0.0.1", login.hub_url.port().unwrap()))
        .await
        .unwrap();
    f.hub_task = tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
    revokes
}

async fn start_error(f: &Fixture) -> ApiError {
    f.human(
        "listener_start",
        json!({"project":"project1","root":f.repo()}),
    )
    .await
    .unwrap_err()
}

// The hub accepted the mint and this start fails. Only the mint is revoked,
// never the decoy, its credential is gone, and a retry mints again.
async fn assert_mint_rolled_back(answer: Answer, expected_error: &str) {
    let (bytes, subscriber) = warnings();
    async {
        let mut f = Fixture::new(FAKE).await;
        let revokes = mint_hub(&mut f, answer, Listed::Offline).await;
        let credentials = f.listeners().hub.credentials();
        let login = credentials.login().await.unwrap();
        // Block only the atomic write's temporary path, leaving owner auth usable.
        #[cfg(feature = "test-hooks")]
        let blocked = {
            let key =
                agmen_client::TokenKey::new(&login.hub_url, &MINTED.parse().unwrap()).unwrap();
            let name = format!(
                "tok-{}",
                key.as_str()
                    .bytes()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            matches!(answer, Answer::Valid)
                .then(|| agmen_client::fail_token_writes(f.dir.path().join("tokens").join(name)))
        };
        let error = start_error(&f).await;
        assert_eq!(error.message, expected_error);
        assert_eq!(*revokes.lock().unwrap(), [MINTED]);
        for id in [MINTED, DECOY] {
            assert_eq!(
                credentials
                    .token(&login.hub_url, &id.parse().unwrap())
                    .await
                    .unwrap(),
                None
            );
        }
        let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("WARN"), "rollback must be observable: {logs}");
        assert!(logs.contains(MINTED));
        for hidden in ["fake-hub-credential", "other/listener", "project2", DECOY] {
            assert!(!logs.contains(hidden), "{hidden} in {logs}");
            assert!(!error.message.contains(hidden), "{hidden} in {error:?}");
        }
        #[cfg(feature = "test-hooks")]
        drop(blocked);
        assert_eq!(f.start().await["identity_id"], MINTED);
        assert_eq!(*revokes.lock().unwrap(), [MINTED]);
        f.stop().await;
    }
    .with_subscriber(subscriber)
    .await;
}

#[tokio::test]
async fn unparsable_hub_listener_response_revokes_the_mint_and_allows_retry() {
    assert_mint_rolled_back(Answer::NotJson, "invalid listener mint response").await;
}

#[tokio::test]
async fn invalid_hub_listener_response_revokes_the_mint_and_allows_retry() {
    assert_mint_rolled_back(Answer::Misshapen, "invalid listener mint response").await;
}

#[tokio::test]
async fn mismatched_hub_listener_handle_revokes_only_the_mint_and_allows_retry() {
    assert_mint_rolled_back(
        Answer::OtherHandle,
        "hub minted a different listener than requested",
    )
    .await;
}

#[tokio::test]
async fn mismatched_hub_listener_project_revokes_only_the_mint_and_allows_retry() {
    assert_mint_rolled_back(
        Answer::OtherProject,
        "hub minted a different listener than requested",
    )
    .await;
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
async fn failed_hub_listener_token_storage_revokes_the_mint_and_allows_retry() {
    assert_mint_rolled_back(Answer::Valid, "could not read identity credential").await;
}

// The roster cannot show which listener is the mint: nothing is revoked.
async fn assert_mint_not_guessed(listed: Listed) {
    let (bytes, subscriber) = warnings();
    async {
        let mut f = Fixture::new(FAKE).await;
        let revokes = mint_hub(&mut f, Answer::NotJson, listed).await;
        let error = start_error(&f).await;
        assert_eq!(error.message, "invalid listener mint response");
        let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("WARN"));
        assert!(logs.contains("cannot revoke"), "{logs}");
        assert!(!logs.contains("fake-hub-credential"));
        // The hub still owns the mint, so a retry is refused. Never guess an id.
        assert_eq!(
            start_error(&f).await.message,
            "a listener already runs in this project"
        );
        assert!(revokes.lock().unwrap().is_empty());
    }
    .with_subscriber(subscriber)
    .await;
}

#[tokio::test]
async fn hub_listener_mint_missing_from_the_roster_is_not_revoked() {
    assert_mint_not_guessed(Listed::Absent).await;
}

#[tokio::test]
async fn connected_hub_listener_with_the_minted_handle_is_not_revoked() {
    assert_mint_not_guessed(Listed::Online).await;
}

#[tokio::test]
async fn hub_listener_mint_is_not_revoked_when_the_roster_cannot_be_read() {
    assert_mint_not_guessed(Listed::Unreadable).await;
}

fn offline_listener(id: &str) -> Value {
    json!({"identity_id":id,"handle":"owner/listener","kind":"listener","status":"offline"})
}

async fn assert_lookup_refused(f: &mut Fixture, roster: Value) {
    let revokes = mint_hub(f, Answer::Misshapen, Listed::Custom(roster)).await;
    let (bytes, subscriber) = warnings();
    let error = start_error(f).with_subscriber(subscriber).await;
    assert_eq!(error.message, "invalid listener mint response");
    assert!(revokes.lock().unwrap().is_empty());
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert_eq!(
        logs.lines()
            .filter(|line| line.contains("cannot revoke an unregistered hub listener"))
            .count(),
        1,
        "one warning must explain the skipped revoke: {logs}"
    );
    assert!(!logs.contains(MINTED), "{logs}");
    assert!(!logs.contains("fake-hub-credential"), "{logs}");
}

#[tokio::test]
async fn listener_lookup_skips_entries_that_said_hello() {
    for (field, value) in [("agent", "claude-code"), ("model", "sonnet")] {
        let mut f = Fixture::new(FAKE).await;
        let mut said = offline_listener(MINTED);
        said[field] = json!(value);
        assert_lookup_refused(&mut f, json!([said])).await;
    }
}

#[tokio::test]
async fn listener_lookup_skips_a_stored_credential() {
    let mut f = Fixture::new(FAKE).await;
    let listeners = f.listeners().clone();
    let credentials = listeners.hub.credentials();
    let login = credentials.login().await.unwrap();
    credentials
        .set_token(
            &login.hub_url,
            &MINTED.parse().unwrap(),
            "held-listener".into(),
        )
        .await
        .unwrap();
    assert_lookup_refused(&mut f, json!([offline_listener(MINTED)])).await;
    assert_eq!(
        credentials
            .token(&login.hub_url, &MINTED.parse().unwrap())
            .await
            .unwrap()
            .as_deref(),
        Some("held-listener")
    );
}

#[tokio::test]
async fn listener_lookup_finds_fresh_mint_among_known_and_said_hello_entries() {
    let mut f = Fixture::new(FAKE).await;
    let listeners = f.listeners().clone();
    let credentials = listeners.hub.credentials();
    let login = credentials.login().await.unwrap();
    let known = "known001".parse().unwrap();
    credentials
        .set_token(&login.hub_url, &known, "held-listener".into())
        .await
        .unwrap();
    let mut said = offline_listener("hello001");
    said["agent"] = json!("claude-code");
    said["model"] = json!("sonnet");
    let revokes = mint_hub(
        &mut f,
        Answer::Misshapen,
        Listed::Custom(json!([
            offline_listener("known001"),
            said,
            offline_listener(MINTED)
        ])),
    )
    .await;
    let (bytes, subscriber) = warnings();
    let error = start_error(&f).with_subscriber(subscriber).await;
    assert_eq!(error.message, "invalid listener mint response");
    assert_eq!(*revokes.lock().unwrap(), [MINTED]);
    assert_eq!(
        credentials
            .token(&login.hub_url, &known)
            .await
            .unwrap()
            .as_deref(),
        Some("held-listener")
    );
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(logs.contains(MINTED), "{logs}");
    for hidden in [
        "known001",
        "hello001",
        "held-listener",
        "fake-hub-credential",
    ] {
        assert!(!logs.contains(hidden), "{hidden} in {logs}");
    }
}

#[tokio::test]
async fn listener_lookup_skips_a_stopped_listener_record_alone() {
    let mut f = Fixture::new(FAKE).await;
    f.start().await;
    f.stop().await;
    let listeners = f.listeners();
    let entries = listeners.entries.lock().await;
    let identity = entries
        .get(&"project1".parse().unwrap())
        .unwrap()
        .live
        .lock()
        .await
        .identity
        .clone();
    drop(entries);
    listeners
        .identities()
        .unwrap()
        .forget(&identity)
        .await
        .unwrap();
    listeners
        .hub
        .credentials()
        .delete_token(&identity.hub_url, &identity.identity_id)
        .await
        .unwrap();
    assert_lookup_refused(&mut f, json!([offline_listener(MINTED)])).await;
}

#[tokio::test]
async fn listener_lookup_skips_a_durable_intent_alone() {
    let mut f = Fixture::new(FAKE).await;
    let folder = f.dir.path().join("listener/project1");
    tokio::fs::create_dir_all(&folder).await.unwrap();
    tokio::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
        .await
        .unwrap();
    write_intent(
        folder.join("intent.json"),
        Intent {
            project: "project1".parse().unwrap(),
            root: f.repo(),
            identity_id: MINTED.parse().unwrap(),
        },
    )
    .await
    .unwrap();
    assert_lookup_refused(&mut f, json!([offline_listener(MINTED)])).await;
}

#[tokio::test]
async fn listener_lookup_skips_an_unreadable_intent() {
    let mut f = Fixture::new(FAKE).await;
    let folder = f.dir.path().join("listener/project1");
    tokio::fs::create_dir_all(&folder).await.unwrap();
    tokio::fs::write(folder.join("intent.json"), "not json")
        .await
        .unwrap();
    assert_lookup_refused(&mut f, json!([offline_listener(MINTED)])).await;
}

#[tokio::test]
async fn listener_lookup_skips_an_identity_row_alone() {
    let mut f = Fixture::new(FAKE).await;
    let listeners = f.listeners().clone();
    let login = listeners.hub.credentials().login().await.unwrap();
    // This person's listener in another project: a row that start would not
    // reuse for project1, with no record, intent or hub credential beside it.
    let other: ManagedIdentity = serde_json::from_value(json!({
        "hub_url":login.hub_url,"broker_url":"mqtt://127.0.0.1:9","tenant":"tenant01",
        "project":"project2","identity_id":MINTED,"owner_id":login.human_id,
        "handle":"owner/listener","kind":"listener"
    }))
    .unwrap();
    listeners
        .identities()
        .unwrap()
        .register(other)
        .await
        .unwrap();
    assert_lookup_refused(&mut f, json!([offline_listener(MINTED)])).await;
}

#[tokio::test]
async fn listener_lookup_skips_a_credential_it_cannot_read() {
    let mut f = Fixture::new(FAKE).await;
    let login = f.listeners().hub.credentials().login().await.unwrap();
    let key = agmen_client::TokenKey::new(&login.hub_url, &MINTED.parse().unwrap()).unwrap();
    let hex: String = key
        .as_str()
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    // A directory where the token file belongs makes the read fail.
    tokio::fs::create_dir(f.dir.path().join("tokens").join(format!("tok-{hex}")))
        .await
        .unwrap();
    assert_lookup_refused(&mut f, json!([offline_listener(MINTED)])).await;
}
