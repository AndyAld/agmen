use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;

use super::*;

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
