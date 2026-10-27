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
async fn hello_version_validation_precedes_identity_authentication() {
    let daemon = Daemon::start().await;
    let mut client = Client::connect(&daemon.home.socket()).await;
    client
        .send(&Request::new(
            1,
            "hello",
            json!({"api":2,"local_token":"invalid"}),
        ))
        .await;
    assert_eq!(error(client.read().await).code, ErrorCode::Version);
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
async fn post_requires_identity_token() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    let mut request = Request::new(
        9,
        "post",
        json!({"project":support::PROJECT,"envelope":support::envelope(1,"hi")}),
    );
    request.as_ = Some("agent001".parse().unwrap());
    client.send(&request).await;
    let err = error(client.read().await);
    assert_eq!(err.code, ErrorCode::Unauthorized);
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}

async fn identity_call(
    client: &mut Client,
    id: Option<&str>,
    method: &str,
    params: serde_json::Value,
) -> agmen_local::Response {
    let mut request = Request::new(99, method, params);
    request.as_ = id.map(|id| id.parse().unwrap());
    client.send(&request).await;
    client.read().await
}

#[tokio::test]
async fn sibling_token_is_unauthorized() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    for (id, token) in [
        ("agent001", "b".repeat(64)),
        ("agent003", "a".repeat(64)),
        ("agent001", "x".repeat(64)),
    ] {
        assert_eq!(
            error(identity_call(&mut client, Some(id), "post", json!({"local_token":token})).await)
                .code,
            ErrorCode::Unauthorized
        );
    }
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn identity_token_is_not_a_human_credential() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    for token in [json!("a".repeat(64)), json!(null), json!(7)] {
        assert_eq!(
            error(client.call("post", json!({"local_token":token})).await).code,
            ErrorCode::Unauthorized
        );
    }
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn identity_origin_and_owner_must_match_login() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    let original = std::fs::read(daemon.home.login_file()).unwrap();
    for field in ["hub_url", "human_id"] {
        let mut login: serde_json::Value = serde_json::from_slice(&original).unwrap();
        login[field] = if field == "hub_url" {
            json!("https://foreign.example")
        } else {
            json!("other001")
        };
        std::fs::write(
            daemon.home.login_file(),
            serde_json::to_vec(&login).unwrap(),
        )
        .unwrap();
        assert_eq!(
            error(
                identity_call(
                    &mut client,
                    Some("agent001"),
                    "post",
                    json!({"local_token":"a".repeat(64)})
                )
                .await
            )
            .code,
            ErrorCode::Unauthorized
        );
    }
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn identity_authorization_is_per_request() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    assert_eq!(
        error(
            identity_call(
                &mut client,
                Some("agent001"),
                "whoami",
                json!({"local_token":"a".repeat(64)})
            )
            .await
        )
        .code,
        ErrorCode::Forbidden
    );
    assert_eq!(
        error(identity_call(&mut client, Some("agent001"), "post", json!({})).await).code,
        ErrorCode::Unauthorized
    );
    assert_eq!(result(client.call("whoami", json!({})).await)["id"], HUMAN);
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn agent_cannot_mint() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    for method in [
        "identity_mint",
        "identity_list",
        "console_watch",
        "console_snapshot",
        "chat_history",
        "file_text",
    ] {
        assert_eq!(
            error(
                identity_call(
                    &mut client,
                    Some("agent001"),
                    method,
                    json!({"local_token":"a".repeat(64)})
                )
                .await
            )
            .code,
            ErrorCode::Forbidden
        );
    }
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
    assert_eq!(
        daemon
            .hub
            .state
            .mint_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn human_post_is_unchanged() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    assert_eq!(
        result(
            client
                .call(
                    "post",
                    json!({"project":support::PROJECT,"envelope":support::envelope(2,"human")})
                )
                .await
        ),
        json!({"stream":"feed","seq":1})
    );
    let posts = daemon.hub.state.posts.lock().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].0, format!("Bearer {}", support::ACCESS));
}
