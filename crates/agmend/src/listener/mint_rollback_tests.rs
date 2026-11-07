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
    Valid,
}

#[derive(Clone, Copy)]
enum Listed {
    // The roster lists the mint as a listener that never connected.
    Offline,
    // The roster lists a connected listener with the minted handle.
    Online,
    // The roster does not list the mint.
    Absent,
    // The roster answers 503.
    Unreadable,
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
                async move {
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
                        Listed::Absent | Listed::Unreadable => None,
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
        let key = agmen_client::TokenKey::new(&login.hub_url, &MINTED.parse().unwrap()).unwrap();
        let name = format!(
            "tok-{}",
            key.as_str()
                .bytes()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let blocked = f.dir.path().join("tokens").join(format!("{name}.tmp"));
        if matches!(answer, Answer::Valid) {
            tokio::fs::create_dir(&blocked).await.unwrap();
        }
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
        if matches!(answer, Answer::Valid) {
            tokio::fs::remove_dir(&blocked).await.unwrap();
        }
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
