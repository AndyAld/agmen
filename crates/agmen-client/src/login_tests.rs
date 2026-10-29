use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};

use super::*;
use crate::{DeviceStart, device_start, device_wait, signed_in_as};

#[test]
fn hub_url_policy_saved_login() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login.json");
    for hub in [
        "http://localhost",
        "http://127.0.0.1",
        "http://127.2.3.4",
        "http://[::1]",
        "https://hub.example",
    ] {
        let expected = login(hub.parse().unwrap());
        std::fs::write(&path, serde_json::to_vec(&expected).unwrap()).unwrap();
        assert_eq!(Login::read(&path).unwrap(), Some(expected));
    }
    for hub in [
        "http://user:password@hub.example/private?secret=yes",
        "ftp://localhost",
    ] {
        std::fs::write(
            &path,
            serde_json::to_vec(&login(hub.parse().unwrap())).unwrap(),
        )
        .unwrap();
        let error = Login::read(&path).expect_err("unsafe saved hub");
        let text = format!("{error:?}");
        assert!(text.contains("use https"), "{text}");
        assert!(!text.contains("password"), "{text}");
    }
}

#[tokio::test]
async fn hub_url_policy_token_grant() {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(100))
        .build()
        .unwrap();
    for hub in ["http://hub.example", "ftp://localhost"] {
        let error = token_grant(&http, &hub.parse().unwrap(), &[])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("use https"), "{error:?}");
    }
}

/// A token endpoint that answers `status` with `body` and counts calls.
async fn token_endpoint(status: StatusCode, body: &'static str) -> (Url, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let app = Router::new().route(
        "/v1/auth/token",
        post(move |form: String| {
            counter.fetch_add(1, Ordering::SeqCst);
            assert!(form.contains("grant_type=refresh_token"), "{form}");
            assert!(form.contains("refresh_token=old-refresh"), "{form}");
            async move { (status, [("content-type", "application/json")], body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}").parse().unwrap(), calls)
}

fn login(hub: Url) -> Login {
    Login {
        hub_url: hub,
        human_id: "hmn7k2q9".parse().unwrap(),
        handle: "andy".to_owned(),
    }
}

async fn stored(store: &TokenStore, login: &Login, expires_in: i64) {
    let tokens = HumanTokens::new("old-access".into(), "old-refresh".into(), expires_in);
    store_tokens(store, login, &tokens).await.unwrap();
}

#[tokio::test]
async fn fresh_tokens_are_used_as_stored() {
    let (hub, calls) = token_endpoint(StatusCode::OK, "{}").await;
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().into());
    let login = login(hub);
    stored(&store, &login, 3600).await;
    let tokens = human_tokens(&reqwest::Client::new(), &store, &login)
        .await
        .unwrap();
    assert_eq!(tokens.access_token(), "old-access");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn expiring_tokens_are_renewed_and_stored() {
    let (hub, calls) = token_endpoint(
        StatusCode::OK,
        r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().into());
    let login = login(hub);
    stored(&store, &login, 30).await;
    let http = reqwest::Client::new();
    let tokens = human_tokens(&http, &store, &login).await.unwrap();
    assert_eq!(tokens.access_token(), "new-access");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The renewed pair was stored, so the next read does not renew again.
    let again = human_tokens(&http, &store, &login).await.unwrap();
    assert_eq!(again.access_token(), "new-access");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_refused_refresh_is_not_signed_in() {
    let (hub, _) = token_endpoint(StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#).await;
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().into());
    let login = login(hub);
    stored(&store, &login, 30).await;
    let err = human_tokens(&reqwest::Client::new(), &store, &login)
        .await
        .unwrap_err();
    assert!(matches!(err, LoginError::NotSignedIn), "{err:?}");
}

#[tokio::test]
async fn no_stored_tokens_is_not_signed_in() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().into());
    let login = login("http://127.0.0.1:9".parse().unwrap());
    let err = human_tokens(&reqwest::Client::new(), &store, &login)
        .await
        .unwrap_err();
    assert!(matches!(err, LoginError::NotSignedIn), "{err:?}");
}

#[test]
fn login_file_reads_and_misses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login.json");
    assert_eq!(Login::read(&path).unwrap(), None);
    std::fs::write(
        &path,
        r#"{"hub_url":"https://hub.example.com/","human_id":"hmn7k2q9","handle":"andy"}"#,
    )
    .unwrap();
    assert_eq!(
        Login::read(&path).unwrap(),
        Some(login("https://hub.example.com/".parse().unwrap()))
    );
    std::fs::write(&path, "not json").unwrap();
    assert!(matches!(
        Login::read(&path),
        Err(LoginError::BadFile { .. })
    ));
}

const DEVICE_CODE: &str = "device-code-for-tests";
const USER_CODE: &str = "ABCD-EFGH";
const VERIFICATION_URI: &str = "http://hub.example/login/device";
const GRANTED: &str =
    r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#;
const PENDING: &str = r#"{"error":"authorization_pending"}"#;

/// What a fake hub saw of the device flow: when each poll came.
type Polls = Arc<Mutex<Vec<Instant>>>;

/// A hub with the device endpoints and `/v1/me`. The nth poll of the token
/// endpoint gets `script[n]` (the last entry repeats).
async fn device_hub(script: &'static [(StatusCode, &'static str)]) -> (Url, Polls) {
    let polls: Polls = Arc::default();
    let seen = polls.clone();
    let app = Router::new()
        .route(
            "/v1/auth/device",
            post(|| async {
                (
                    [("content-type", "application/json")],
                    format!(
                        r#"{{"device_code":"{DEVICE_CODE}","user_code":"{USER_CODE}","verification_uri":"{VERIFICATION_URI}","expires_in":900,"interval":1}}"#
                    ),
                )
            }),
        )
        .route(
            "/v1/auth/token",
            post(move |form: String| {
                assert!(form.contains("device_code"), "{form}");
                assert!(form.contains(DEVICE_CODE), "form lacks the device code");
                let mut seen = seen.lock().unwrap();
                seen.push(Instant::now());
                let (status, body) = script[(seen.len() - 1).min(script.len() - 1)];
                async move { (status, [("content-type", "application/json")], body) }
            }),
        )
        .route(
            "/v1/me",
            get(|headers: HeaderMap| async move {
                if headers.get("authorization").and_then(|v| v.to_str().ok())
                    != Some("Bearer new-access")
                {
                    return (StatusCode::UNAUTHORIZED, "{}".to_owned());
                }
                (
                    StatusCode::OK,
                    r#"{"id":"hmn7k2q9","handle":"andy","broker_url":"x","projects":[],"extra":1}"#
                        .to_owned(),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}").parse().unwrap(), polls)
}

#[tokio::test]
async fn device_flow_polls_until_granted() {
    let (hub, polls) = device_hub(&[
        (StatusCode::BAD_REQUEST, PENDING),
        (StatusCode::BAD_REQUEST, PENDING),
        (StatusCode::OK, GRANTED),
    ])
    .await;
    let http = reqwest::Client::new();
    let start = device_start(&http, &hub).await.unwrap();
    assert_eq!(start.user_code, USER_CODE);
    assert_eq!(start.verification_uri.as_str(), VERIFICATION_URI);
    assert_eq!((start.expires_in, start.interval), (900, 1));
    let tokens = device_wait(&http, &hub, &start).await.unwrap();
    assert_eq!(tokens.access_token(), "new-access");
    assert_eq!(polls.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn device_flow_slow_down_adds_five_seconds() {
    let (hub, polls) = device_hub(&[
        (StatusCode::BAD_REQUEST, r#"{"error":"slow_down"}"#),
        (StatusCode::OK, GRANTED),
    ])
    .await;
    let http = reqwest::Client::new();
    let start = device_start(&http, &hub).await.unwrap();
    device_wait(&http, &hub, &start).await.unwrap();
    let polls = polls.lock().unwrap();
    assert_eq!(polls.len(), 2);
    // The interval was 1 s; slow_down makes the next wait 6 s.
    let gap = polls[1].duration_since(polls[0]);
    assert!(gap >= Duration::from_secs(6), "{gap:?}");
}

#[tokio::test]
async fn device_flow_declined_is_declined() {
    let (hub, _) = device_hub(&[(StatusCode::BAD_REQUEST, r#"{"error":"access_denied"}"#)]).await;
    let http = reqwest::Client::new();
    let start = device_start(&http, &hub).await.unwrap();
    let error = device_wait(&http, &hub, &start).await.unwrap_err();
    assert!(matches!(error, LoginError::Declined), "{error:?}");
}

#[tokio::test]
async fn device_flow_expired_is_expired() {
    let (hub, _) = device_hub(&[(StatusCode::BAD_REQUEST, r#"{"error":"expired_token"}"#)]).await;
    let http = reqwest::Client::new();
    let start = device_start(&http, &hub).await.unwrap();
    let error = device_wait(&http, &hub, &start).await.unwrap_err();
    assert!(matches!(error, LoginError::Expired), "{error:?}");
}

#[tokio::test]
async fn device_flow_other_refusals_are_passed_on() {
    let (hub, _) = device_hub(&[(StatusCode::INTERNAL_SERVER_ERROR, r#"{"error":"boom"}"#)]).await;
    let http = reqwest::Client::new();
    let start = device_start(&http, &hub).await.unwrap();
    let error = device_wait(&http, &hub, &start).await.unwrap_err();
    assert!(
        matches!(&error, LoginError::Refused { status: 500, code, .. } if code == "boom"),
        "{error:?}"
    );
}

#[tokio::test]
async fn device_flow_stops_at_the_hubs_expiry() {
    let (hub, polls) = device_hub(&[(StatusCode::BAD_REQUEST, PENDING)]).await;
    let http = reqwest::Client::new();
    let mut start = device_start(&http, &hub).await.unwrap();
    start.expires_in = 2;
    let error = device_wait(&http, &hub, &start).await.unwrap_err();
    assert!(matches!(error, LoginError::Expired), "{error:?}");
    assert!(polls.lock().unwrap().len() <= 2);
}

#[test]
fn device_start_debug_hides_the_device_code() {
    let start: DeviceStart = serde_json::from_value(serde_json::json!({
        "device_code": DEVICE_CODE,
        "user_code": USER_CODE,
        "verification_uri": VERIFICATION_URI,
        "expires_in": 900,
        "interval": 5,
    }))
    .unwrap();
    let text = format!("{start:?}");
    assert!(!text.contains(DEVICE_CODE), "{text}");
    assert!(!text.contains("device_code"), "{text}");
    assert!(text.contains(USER_CODE), "{text}");
}

#[tokio::test]
async fn device_start_follows_the_hub_url_policy() {
    let error = device_start(
        &reqwest::Client::new(),
        &"http://hub.example".parse().unwrap(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("use https"), "{error:?}");
}

#[tokio::test]
async fn signed_in_as_reads_the_id_and_handle() {
    let (hub, _) = device_hub(&[(StatusCode::OK, GRANTED)]).await;
    let http = reqwest::Client::new();
    let who = signed_in_as(&http, &hub, "new-access").await.unwrap();
    assert_eq!(who, login(hub.clone()));
    let error = signed_in_as(&http, &hub, "wrong-access").await.unwrap_err();
    assert!(
        matches!(error, LoginError::Refused { status: 401, .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn save_login_replaces_the_old_logins_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("config").join("login.json");
    let old = login("http://127.0.0.1:1".parse().unwrap());
    let mut new = login("http://127.0.0.1:2".parse().unwrap());
    new.handle = "andy2".to_owned();
    let old_tokens = HumanTokens::new("old-access".into(), "old-refresh".into(), 3600);
    let new_tokens = HumanTokens::new("new-access".into(), "new-refresh".into(), 3600);

    save_login(&store, &file, &old, &old_tokens).await.unwrap();
    assert_eq!(Login::read(&file).unwrap(), Some(old.clone()));
    assert!(store.get(&old.key().unwrap()).unwrap().is_some());

    save_login(&store, &file, &new, &new_tokens).await.unwrap();
    assert_eq!(Login::read(&file).unwrap(), Some(new.clone()));
    assert_eq!(store.get(&old.key().unwrap()).unwrap(), None);
    let stored = store.get(&new.key().unwrap()).unwrap().unwrap();
    let stored: HumanTokens = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored.access_token(), "new-access");

    // The same person again keeps their entry (replaced, not deleted).
    save_login(&store, &file, &new, &old_tokens).await.unwrap();
    let again = store.get(&new.key().unwrap()).unwrap().unwrap();
    let again: HumanTokens = serde_json::from_str(&again).unwrap();
    assert_eq!(again.access_token(), "old-access");
}
