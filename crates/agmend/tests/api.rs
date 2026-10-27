//! The socket, the lock, `hello` and `whoami`.

#![cfg(test)]

mod support;

use std::os::unix::fs::PermissionsExt as _;

use agmen_local::{ErrorCode, Request};
use agmend::{Agmend, AgmendError};
use serde_json::json;
use support::{Client, Daemon, HANDLE, HUMAN, error, result};

#[tokio::test]
async fn socket_is_0600() {
    let daemon = Daemon::start().await;
    let mode = std::fs::metadata(daemon.home.socket())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
}

#[tokio::test]
async fn second_agmend_exits() {
    let daemon = Daemon::start().await;
    let err = Agmend::bind(daemon.home.config()).unwrap_err();
    assert!(matches!(err, AgmendError::AlreadyRunning { .. }), "{err:?}");
    assert!(err.to_string().contains("already running"), "{err}");
    // The first one still answers.
    let mut client = daemon.client().await;
    result(client.call("whoami", json!({})).await);
}

#[tokio::test]
async fn database_opens_after_lock_and_future_schema_is_contextual() {
    use std::error::Error as _;

    let hub = support::fake_hub().await;
    let home = support::Home::signed_in(&hub.url).await;
    let daemon = Agmend::bind(home.config()).unwrap();
    let conn = rusqlite::Connection::open(home.database()).unwrap();
    conn.pragma_update(None, "user_version", 3).unwrap();
    let error = Agmend::bind(home.config()).unwrap_err();
    assert!(matches!(error, AgmendError::AlreadyRunning { .. }));
    drop(daemon);
    let error = Agmend::bind(home.config()).unwrap_err();
    assert!(matches!(error, AgmendError::Store { .. }));
    assert!(error.to_string().contains("agmend.db"));
    assert!(error.source().unwrap().to_string().contains("schema 3"));
    assert!(!home.socket().exists());
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    let reopened = Agmend::bind(home.config()).unwrap();
    assert!(reopened.socket().exists());
}

#[tokio::test]
async fn hello_with_api_2_is_version_error() {
    let daemon = Daemon::start().await;
    let mut client = Client::connect(&daemon.home.socket()).await;
    let err = error(client.call("hello", json!({"api": 2})).await);
    assert_eq!(err.code, ErrorCode::Version);
    assert!(err.message.contains('1'), "{}", err.message);
}

#[tokio::test]
async fn the_first_request_must_be_hello() {
    let daemon = Daemon::start().await;
    let mut client = Client::connect(&daemon.home.socket()).await;
    let err = error(client.call("whoami", json!({})).await);
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("hello"), "{}", err.message);
}

#[tokio::test]
async fn whoami_reads_the_cli_login() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    let me = result(client.call("whoami", json!({})).await);
    assert_eq!(me, json!({"id": HUMAN, "handle": HANDLE}));
}

#[tokio::test]
async fn whoami_signed_out_is_unauthorized() {
    let daemon = Daemon::start().await;
    std::fs::remove_file(daemon.home.login_file()).unwrap();
    let mut client = daemon.client().await;
    let err = error(client.call("whoami", json!({})).await);
    assert_eq!(err.code, ErrorCode::Unauthorized);
    assert!(err.message.contains("agmen login"), "{}", err.message);
}

#[tokio::test]
async fn unknown_methods_are_not_found() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    let err = error(client.call("launch", json!({})).await);
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn a_request_with_as_is_refused_for_now() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    let mut request = Request::new(9, "whoami", json!({}));
    request.as_ = Some("idn7k2q9".parse().unwrap());
    client.send(&request).await;
    let err = error(client.read().await);
    assert_eq!(err.code, ErrorCode::BadRequest);
}
