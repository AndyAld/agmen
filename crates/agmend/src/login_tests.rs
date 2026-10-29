use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use serde_json::{Value, json};

use super::*;

const GRANTED: &str =
    r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#;
const PENDING: &str = r#"{"error":"authorization_pending"}"#;
const DENIED: &str = r#"{"error":"access_denied"}"#;
const EXPIRED: &str = r#"{"error":"expired_token"}"#;

/// What the fake hub answers a poll of one device code: the code's number
/// (from 1) and how many polls it has had (from 1).
type Script = fn(usize, usize) -> (StatusCode, &'static str);

/// A fake hub: the device endpoints and `/v1/me`.
struct FakeHub {
    url: Url,
    /// Polls seen, by device code.
    polls: Arc<Mutex<HashMap<String, usize>>>,
}

impl FakeHub {
    async fn start(script: Script, me: StatusCode) -> Self {
        let starts = Arc::new(AtomicUsize::new(0));
        let polls: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let seen = polls.clone();
        let app = Router::new()
            .route(
                "/v1/auth/device",
                post(move || {
                    let n = starts.fetch_add(1, Ordering::SeqCst) + 1;
                    async move {
                        axum::Json(json!({
                            "device_code": device_code(n),
                            "user_code": user_code(n),
                            "verification_uri": "http://hub.example/login/device",
                            "expires_in": 900,
                            "interval": 1,
                        }))
                    }
                }),
            )
            .route(
                "/v1/auth/token",
                post(move |form: String| {
                    let code = form
                        .split('&')
                        .find_map(|pair| pair.strip_prefix("device_code="))
                        .unwrap_or_default()
                        .to_owned();
                    let n = code
                        .strip_prefix("device-code-")
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                    let count = {
                        let mut seen = seen.lock().unwrap();
                        let count = seen.entry(code).or_default();
                        *count += 1;
                        *count
                    };
                    let (status, body) = script(n, count);
                    async move { (status, [("content-type", "application/json")], body) }
                }),
            )
            .route(
                "/v1/me",
                get(move |headers: HeaderMap| async move {
                    let bearer = headers.get("authorization").and_then(|v| v.to_str().ok());
                    if bearer != Some("Bearer new-access") {
                        return (StatusCode::UNAUTHORIZED, "{}".to_owned());
                    }
                    if me != StatusCode::OK {
                        return (me, r#"{"error":"boom"}"#.to_owned());
                    }
                    (
                        StatusCode::OK,
                        r#"{"id":"hmn7k2q9","handle":"andy","broker_url":"x","projects":[]}"#
                            .to_owned(),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url: format!("http://{addr}").parse().unwrap(),
            polls,
        }
    }

    fn polls(&self, n: usize) -> usize {
        self.polls
            .lock()
            .unwrap()
            .get(&device_code(n))
            .copied()
            .unwrap_or(0)
    }
}

fn device_code(n: usize) -> String {
    format!("device-code-{n}")
}

fn user_code(n: usize) -> String {
    format!("CODE-{n:04}")
}

/// Credentials over a temp home: a login file in a folder that does not
/// exist yet, and a file token store.
struct Home {
    dir: tempfile::TempDir,
    credentials: Credentials,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let credentials = Credentials::new(
            dir.path().join("config").join("login.json"),
            TokenStore::Dir(dir.path().join("tokens")),
            crate::hub::Hub::http().unwrap(),
        );
        Self { dir, credentials }
    }

    fn login_file(&self) -> PathBuf {
        self.dir.path().join("config").join("login.json")
    }

    fn store(&self) -> TokenStore {
        TokenStore::Dir(self.dir.path().join("tokens"))
    }

    /// Polls `login_status` until the state is not `pending`.
    async fn settled(&self) -> Value {
        for _ in 0..80 {
            let status = self.credentials.login_status(json!({})).await.unwrap();
            if status["state"] != "pending" {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the sign-in stayed pending");
    }
}

fn grant_on_second_poll(_: usize, poll: usize) -> (StatusCode, &'static str) {
    if poll < 2 {
        (StatusCode::BAD_REQUEST, PENDING)
    } else {
        (StatusCode::OK, GRANTED)
    }
}

#[tokio::test]
async fn login_status_starts_idle() {
    let home = Home::new();
    let status = home.credentials.login_status(json!({})).await.unwrap();
    assert_eq!(status, json!({"state": "idle"}));
}

#[tokio::test]
async fn login_status_refuses_unknown_params() {
    let home = Home::new();
    let error = home
        .credentials
        .login_status(json!({"extra": 1}))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
}

#[tokio::test]
async fn login_start_refuses_unknown_params() {
    let home = Home::new();
    let error = home
        .credentials
        .login_start(json!({"hub": "https://hub.example", "extra": 1}))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
}

#[tokio::test]
async fn login_start_without_a_hub_or_login_is_refused() {
    let home = Home::new();
    for params in [json!({}), json!(null), json!({"hub": null})] {
        let error = home.credentials.login_start(params).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert_eq!(error.message, "name the hub to sign in at");
    }
    // Nothing started.
    let status = home.credentials.login_status(json!({})).await.unwrap();
    assert_eq!(status["state"], "idle");
}

#[tokio::test]
async fn login_start_refuses_an_unsafe_hub() {
    let home = Home::new();
    for hub in ["http://hub.example", "not a url"] {
        let error = home
            .credentials
            .login_start(json!({"hub": hub}))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest, "{hub}");
    }
}

#[tokio::test]
async fn login_start_answers_the_code_not_the_device_code() {
    let hub = FakeHub::start(|_, _| (StatusCode::BAD_REQUEST, PENDING), StatusCode::OK).await;
    let home = Home::new();
    let answer = home
        .credentials
        .login_start(json!({"hub": hub.url}))
        .await
        .unwrap();
    assert_eq!(answer["user_code"], user_code(1));
    assert_eq!(
        answer["verification_uri"],
        "http://hub.example/login/device"
    );
    assert_eq!(answer["expires_in"], 900);
    assert_eq!(answer["interval"], 1);
    let status = home.credentials.login_status(json!({})).await.unwrap();
    for text in [answer.to_string(), status.to_string()] {
        assert!(!text.contains("device_code"), "{text}");
        assert!(!text.contains(&device_code(1)), "{text}");
    }
    assert!(
        format!("{:?}", home.credentials)
            .find(&device_code(1))
            .is_none(),
        "Debug shows the device code"
    );
}

#[tokio::test]
async fn login_status_reports_pending_then_signed_in() {
    let hub = FakeHub::start(grant_on_second_poll, StatusCode::OK).await;
    let home = Home::new();
    home.credentials
        .login_start(json!({"hub": hub.url}))
        .await
        .unwrap();

    let pending = home.credentials.login_status(json!({})).await.unwrap();
    assert_eq!(pending["state"], "pending");
    assert_eq!(pending["user_code"], user_code(1));
    assert_eq!(
        pending["verification_uri"],
        "http://hub.example/login/device"
    );
    let expires_at: Timestamp = pending["expires_at"].as_str().unwrap().parse().unwrap();
    let left = expires_at.duration_since(Timestamp::now());
    assert!(
        left.as_secs() > 800 && left.as_secs() <= 900,
        "expires_at is {left:?} away"
    );

    let done = home.settled().await;
    assert_eq!(
        done,
        json!({"state": "signed_in", "hub": hub.url.as_str(), "handle": "andy"})
    );
    // The login file and the tokens are written; the next hub call reads
    // the new person.
    let login = home.credentials.login().await.unwrap();
    assert_eq!(login.handle, "andy");
    assert_eq!(login.hub_url, hub.url);
    assert_eq!(
        Login::read(&home.login_file()).unwrap(),
        Some(login.clone())
    );
    let stored = home.store().get(&login.key().unwrap()).unwrap().unwrap();
    let stored: HumanTokens = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored.access_token(), "new-access");
    let (seen, access) = home.credentials.access().await.unwrap();
    assert_eq!((seen, access.as_str()), (login, "new-access"));
}

#[tokio::test]
async fn login_start_defaults_to_the_current_logins_hub() {
    let hub = FakeHub::start(|_, _| (StatusCode::OK, GRANTED), StatusCode::OK).await;
    let home = Home::new();
    let old = Login {
        hub_url: hub.url.clone(),
        human_id: "hmn0old1".parse().unwrap(),
        handle: "before".to_owned(),
    };
    let tokens = HumanTokens::new("old-access".into(), "old-refresh".into(), 3600);
    agmen_client::save_login(&home.store(), &home.login_file(), &old, &tokens)
        .await
        .unwrap();
    home.credentials.login_start(json!({})).await.unwrap();
    let done = home.settled().await;
    assert_eq!(done["state"], "signed_in");
    assert_eq!(done["handle"], "andy");
    // The earlier person's tokens went with their login.
    assert_eq!(home.store().get(&old.key().unwrap()).unwrap(), None);
}

#[tokio::test]
async fn login_status_reports_declined() {
    let hub = FakeHub::start(|_, _| (StatusCode::BAD_REQUEST, DENIED), StatusCode::OK).await;
    let home = Home::new();
    home.credentials
        .login_start(json!({"hub": hub.url}))
        .await
        .unwrap();
    assert_eq!(home.settled().await, json!({"state": "declined"}));
    assert!(!home.login_file().exists());
}

#[tokio::test]
async fn login_status_reports_expired() {
    let hub = FakeHub::start(|_, _| (StatusCode::BAD_REQUEST, EXPIRED), StatusCode::OK).await;
    let home = Home::new();
    home.credentials
        .login_start(json!({"hub": hub.url}))
        .await
        .unwrap();
    assert_eq!(home.settled().await, json!({"state": "expired"}));
}

#[tokio::test]
async fn login_status_reports_failed_without_secrets() {
    let hub = FakeHub::start(
        |_, _| (StatusCode::OK, GRANTED),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    let home = Home::new();
    home.credentials
        .login_start(json!({"hub": hub.url}))
        .await
        .unwrap();
    let done = home.settled().await;
    assert_eq!(done["state"], "failed");
    let message = done["message"].as_str().unwrap();
    assert!(!message.is_empty());
    let text = done.to_string();
    for secret in [device_code(1).as_str(), "new-access", "new-refresh"] {
        assert!(!text.contains(secret), "{text}");
    }
    assert!(!home.login_file().exists());
}

#[tokio::test]
async fn a_second_login_start_replaces_the_first() {
    // Code 1 is never confirmed; code 2 is granted on its first poll.
    let hub = FakeHub::start(
        |n, _| {
            if n == 1 {
                (StatusCode::BAD_REQUEST, PENDING)
            } else {
                (StatusCode::OK, GRANTED)
            }
        },
        StatusCode::OK,
    )
    .await;
    let home = Home::new();
    home.credentials
        .login_start(json!({"hub": hub.url}))
        .await
        .unwrap();
    for _ in 0..60 {
        if hub.polls(1) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(hub.polls(1) > 0, "the first flow never polled");

    let second = home
        .credentials
        .login_start(json!({"hub": hub.url}))
        .await
        .unwrap();
    assert_eq!(second["user_code"], user_code(2));
    let pending = home.credentials.login_status(json!({})).await.unwrap();
    assert_eq!(pending["user_code"], user_code(2));

    assert_eq!(home.settled().await["state"], "signed_in");
    let first_polls = hub.polls(1);
    // The first interval is 1 s: two more would have come by now.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(hub.polls(1), first_polls, "the first flow kept polling");
}

#[tokio::test]
async fn a_replaced_flow_that_wakes_late_saves_nothing() {
    // Code 1 is granted on its first poll; a newer start takes the flow
    // before the first flow gets to save.
    let hub = FakeHub::start(|_, _| (StatusCode::OK, GRANTED), StatusCode::OK).await;
    let home = Home::new();
    home.credentials
        .login_start(json!({"hub": hub.url}))
        .await
        .unwrap();
    {
        let mut flow = home.credentials.flow.lock().await;
        // Past the first poll and the grant: the task now waits for the lock.
        tokio::time::sleep(Duration::from_millis(2500)).await;
        // What a second `login_start` does, minus the abort.
        flow.generation += 1;
        flow.status = Status::Idle;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!home.login_file().exists(), "the replaced flow saved");
    assert_eq!(
        home.credentials.login_status(json!({})).await.unwrap()["state"],
        "idle"
    );
}
