use super::*;
use crate::{FAKE_GITHUB_CLIENT_ID, FakeGithub};

async fn github() -> (FakeGithub, Github) {
    let fake = FakeGithub::start().await.unwrap();
    let config = GithubConfig {
        client_id: FAKE_GITHUB_CLIENT_ID.to_owned(),
        url: fake.base().clone(),
        api_url: fake.base().clone(),
    };
    let github = Github::new(&config, crate::http::upstream_client().unwrap()).unwrap();
    (fake, github)
}

#[tokio::test]
async fn start_device_sends_only_the_client_id() {
    let (fake, github) = github().await;
    let code = github.start_device().await.unwrap();
    assert_eq!(
        fake.last_device_form().unwrap(),
        [("client_id".into(), FAKE_GITHUB_CLIENT_ID.into())]
    );
    assert_eq!(
        fake.last_device_accept().as_deref(),
        Some("application/json")
    );
    assert_eq!(code.user_code.len(), 9);
    assert_eq!(
        code.verification_uri,
        fake.base().join("login/device").unwrap()
    );
    assert_eq!(code.expires_in, 900);
    assert_eq!(code.interval, 1);
}

#[tokio::test]
async fn poll_sends_the_device_grant_without_a_secret() {
    let (fake, github) = github().await;
    let code = github.start_device().await.unwrap();
    assert!(matches!(
        github.poll_device(&code.device_code).await.unwrap(),
        DevicePoll::Pending
    ));
    let form = fake.last_token_form().unwrap();
    assert_eq!(form.len(), 3);
    for pair in [
        ("client_id".into(), FAKE_GITHUB_CLIENT_ID.into()),
        ("device_code".into(), code.device_code),
        (
            "grant_type".into(),
            "urn:ietf:params:oauth:grant-type:device_code".into(),
        ),
    ] {
        assert!(form.contains(&pair), "{form:?}");
    }
    assert!(!form.iter().any(|(key, _)| key == "client_secret"));
}

#[tokio::test]
async fn poll_maps_every_github_answer() {
    let (fake, github) = github().await;
    let code = github.start_device().await.unwrap();
    assert!(matches!(
        github.poll_device(&code.device_code).await.unwrap(),
        DevicePoll::Pending
    ));
    fake.slow_down_next(&code.user_code);
    assert!(matches!(
        github.poll_device(&code.device_code).await.unwrap(),
        DevicePoll::SlowDown { interval: 6 }
    ));
    fake.approve(&code.user_code, 4242, "Andy");
    assert!(matches!(
        github.poll_device(&code.device_code).await.unwrap(),
        DevicePoll::Approved(_)
    ));
    let code = github.start_device().await.unwrap();
    fake.deny(&code.user_code);
    assert!(matches!(
        github.poll_device(&code.device_code).await.unwrap(),
        DevicePoll::Denied
    ));
    let code = github.start_device().await.unwrap();
    fake.expire(&code.user_code);
    assert!(matches!(
        github.poll_device(&code.device_code).await.unwrap(),
        DevicePoll::Expired
    ));
}

#[tokio::test]
async fn user_reads_the_numeric_id() {
    let (fake, github) = github().await;
    let code = github.start_device().await.unwrap();
    fake.approve(&code.user_code, 4242, "Andy");
    let DevicePoll::Approved(token) = github.poll_device(&code.device_code).await.unwrap() else {
        panic!("approved token required")
    };
    let user = github.user(&token).await.unwrap();
    assert_eq!(user.id.get(), 4242);
    assert_eq!(user.login, "Andy");
    assert_eq!(format!("{token:?}"), "GithubToken(<redacted>)");
}

#[tokio::test]
async fn other_github_errors_are_refusals() {
    let (fake, github) = github().await;
    let code = github.start_device().await.unwrap();
    fake.refuse_next_poll("device_flow_disabled");
    assert!(
        matches!(github.poll_device(&code.device_code).await, Err(GithubError::Refused { code }) if code == "device_flow_disabled")
    );
}

#[tokio::test]
async fn unreachable_github_is_a_request_error() {
    let config = GithubConfig {
        client_id: FAKE_GITHUB_CLIENT_ID.into(),
        url: "http://127.0.0.1:9/".parse().unwrap(),
        api_url: "http://127.0.0.1:9/".parse().unwrap(),
    };
    let github = Github::new(&config, crate::http::upstream_client().unwrap()).unwrap();
    assert!(matches!(
        github.start_device().await,
        Err(GithubError::Request(_))
    ));
}

#[test]
fn github_token_debug_is_redacted() {
    let token = GithubToken(format!("gho_{}", "a".repeat(36)));
    assert_eq!(format!("{token:?}"), "GithubToken(<redacted>)");
}

#[test]
fn human_handles_follow_github_logins() {
    for good in ["andy", "adam-k", "a", "x1-2"] {
        assert!(is_human_handle(good), "{good}");
    }
    for bad in ["", "-andy", "andy/sol", "Andy", "a".repeat(40).as_str()] {
        assert!(!is_human_handle(bad), "{bad}");
    }
}

#[test]
fn github_ids_are_positive_decimals() {
    assert_eq!("583231".parse::<GithubId>().unwrap().to_string(), "583231");
    for bad in ["0", "-1", "", "1e3", "18446744073709551616"] {
        assert!(bad.parse::<GithubId>().is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn github_responses_have_a_byte_limit() {
    let response = axum::http::Response::new(vec![b' '; MAX_GITHUB_RESPONSE_BYTES + 1]);
    assert!(matches!(
        read_json::<serde_json::Value>(response.into(), GithubError::Request).await,
        Err(GithubError::Refused { code }) if code == "response_too_large"
    ));
    let mut body = vec![b' '; MAX_GITHUB_RESPONSE_BYTES - 2];
    body.extend_from_slice(b"{}");
    let response = axum::http::Response::new(body);
    let answer: serde_json::Value = read_json(response.into(), GithubError::Request)
        .await
        .unwrap();
    assert_eq!(answer, serde_json::json!({}));
}
