use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
        Self::with_refresh(script, me, None).await
    }

    async fn with_refresh(
        script: Script,
        me: StatusCode,
        refresh: Option<Arc<DelayedRefresh>>,
    ) -> Self {
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
                    let refresh = refresh.clone();
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
                    async move {
                        let (status, body) = if let Some(refresh) = refresh
                            && form.contains("grant_type=refresh_token")
                        {
                            refresh.grant(&form).await
                        } else {
                            (status, body)
                        };
                        (status, [("content-type", "application/json")], body)
                    }
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

#[derive(Default)]
pub(crate) struct DelayedRefresh {
    pub(crate) started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: AtomicUsize,
    /// Answer the delayed first call with a server error instead of the tokens.
    fail: AtomicBool,
}

impl DelayedRefresh {
    pub(crate) fn release(&self) {
        self.release.notify_one();
    }

    async fn grant(&self, form: &str) -> (StatusCode, &'static str) {
        assert!(form.contains("refresh_token=old-refresh"));
        if self.calls.fetch_add(1, Ordering::SeqCst) != 0 {
            return (StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#);
        }
        self.started.notify_one();
        tokio::time::timeout(Duration::from_secs(30), self.release.notified())
            .await
            .expect("test must release the spent refresh grant");
        if self.fail.load(Ordering::SeqCst) {
            return (StatusCode::INTERNAL_SERVER_ERROR, "{}");
        }
        (StatusCode::OK, GRANTED)
    }
}

pub(crate) struct RefreshFixture {
    pub(crate) daemon: crate::Agmend,
    pub(crate) refresh: Arc<DelayedRefresh>,
    home: Home,
    login: Login,
}

impl RefreshFixture {
    pub(crate) async fn new() -> Self {
        let refresh = Arc::new(DelayedRefresh::default());
        let hub = FakeHub::with_refresh(
            |_, _| (StatusCode::BAD_REQUEST, PENDING),
            StatusCode::OK,
            Some(refresh.clone()),
        )
        .await;
        let home = Home::new();
        let login = Login {
            hub_url: hub.url,
            human_id: "hmn7k2q9".parse().unwrap(),
            handle: "andy".into(),
        };
        save_login(
            &home.store(),
            &home.login_file(),
            &login,
            &HumanTokens::new("old-access".into(), "old-refresh".into(), 0),
        )
        .await
        .unwrap();
        let daemon = crate::Agmend::bind(crate::Config::new(
            home.dir.path().join("agmend.sock"),
            home.login_file(),
            home.store(),
        ))
        .unwrap();
        Self {
            daemon,
            refresh,
            home,
            login,
        }
    }

    pub(crate) fn dir(&self) -> &std::path::Path {
        self.home.dir.path()
    }

    /// What the store holds for `login`, as written.
    fn stored(&self, login: &Login) -> Option<String> {
        self.home.store().get(&login.key().unwrap()).unwrap()
    }

    /// The first step of `agmen logout`, which runs in the CLI's process.
    fn delete_tokens(&self) {
        self.home
            .store()
            .delete(&self.login.key().unwrap())
            .unwrap();
    }

    /// How many files the token store's folder holds, staged ones included.
    fn store_files(&self) -> usize {
        std::fs::read_dir(self.dir().join("tokens"))
            .unwrap()
            .count()
    }

    pub(crate) async fn finish_refresh(&self) {
        assert_eq!(self.refresh.calls.load(Ordering::SeqCst), 1);
        self.refresh.release.notify_one();
        let stored = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let store = self.home.store();
                let key = self.login.key().unwrap();
                let raw = tokio::task::spawn_blocking(move || store.get(&key))
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let pair: Value = serde_json::from_str(&raw).unwrap();
                if pair["access_token"] == "new-access" {
                    break pair;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("renewed tokens must be stored after the caller times out");
        assert_eq!(stored["access_token"], "new-access");
        assert_eq!(stored["refresh_token"], "new-refresh");
        let credentials = self.daemon.core.origins.hub().credentials();
        let (login, access) = credentials.access().await.unwrap();
        assert_eq!(login, self.login);
        assert_eq!(access, "new-access");
        credentials.forget().await;
        let (_, access) = credentials.access().await.unwrap();
        assert_eq!(access, "new-access");
        assert_eq!(self.refresh.calls.load(Ordering::SeqCst), 1);
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
async fn login_start_with_an_unusable_lock_fails_before_the_hub_is_asked() {
    let home = Home::new();
    let lock = home.login_file().with_file_name("login.lock");
    std::fs::create_dir_all(&lock).unwrap();
    // Nothing listens here: asking the hub first would say it could not.
    let error = home
        .credentials
        .login_start(json!({"hub": "http://127.0.0.1:9"}))
        .await
        .unwrap_err();
    assert!(
        error.message.contains(&lock.display().to_string()),
        "{error:?}"
    );
    assert!(error.message.contains("os error"), "{error:?}");
    assert!(error.message.contains("owner"), "{error:?}");
    let status = home.credentials.login_status(json!({})).await.unwrap();
    assert_eq!(status["state"], "idle");
}

#[tokio::test]
async fn login_start_checks_the_hub_before_a_busy_lock() {
    let home = Home::new();
    let path = home.login_file().with_file_name("login.lock");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = std::fs::File::create(path).unwrap();
    lock.lock().unwrap();
    let start = std::time::Instant::now();
    let error = home
        .credentials
        .login_start(json!({"hub": "http://hub.example"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest, "{error:?}");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "waited on the lock"
    );
}

#[tokio::test]
async fn login_start_checks_the_hub_before_an_unusable_lock() {
    let home = Home::new();
    std::fs::create_dir_all(home.login_file().with_file_name("login.lock")).unwrap();
    let error = home
        .credentials
        .login_start(json!({"hub": "http://hub.example"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest, "{error:?}");
}

#[tokio::test]
async fn login_start_lock_errors_name_login_lock() {
    let home = Home::new();
    let lock = home.login_file().with_file_name("login.lock");
    std::fs::create_dir_all(&lock).unwrap();
    let error = home
        .credentials
        .login_start(json!({"hub": "http://127.0.0.1:9"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Hub);
    assert!(error.message.contains("local login.lock"), "{error:?}");
    assert!(!error.message.contains('\n'), "{error:?}");
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

#[tokio::test]
async fn a_caller_that_gives_up_in_the_queue_starts_no_refresh() {
    let fixture = RefreshFixture::new().await;
    fixture.refresh.fail.store(true, Ordering::SeqCst);
    let hub = fixture.daemon.core.origins.hub().clone();
    let first = tokio::spawn({
        let hub = hub.clone();
        async move { hub.credentials().access().await }
    });
    tokio::time::timeout(Duration::from_secs(2), fixture.refresh.started.notified())
        .await
        .expect("the hub must see the first refresh");
    // A second caller waits behind the refresh and gives up.
    let waited = tokio::time::timeout(Duration::from_millis(300), hub.credentials().access()).await;
    assert!(waited.is_err(), "the second caller must still be waiting");
    fixture.refresh.release.notify_one();
    let first = first.await.unwrap();
    assert!(
        matches!(&first, Err(LoginError::Refused { status: 500, .. })),
        "{:?}",
        first.as_ref().err()
    );
    // The next caller makes the only other attempt (the hub answers 400 to it).
    let third = hub.credentials().access().await;
    assert!(
        matches!(&third, Err(LoginError::NotSignedIn)),
        "{:?}",
        third.as_ref().err()
    );
    assert_eq!(
        fixture.refresh.calls.load(Ordering::SeqCst),
        2,
        "the caller that left started a refresh of its own"
    );
}

#[tokio::test]
async fn a_second_caller_waits_for_a_refresh_whose_caller_left() {
    let fixture = RefreshFixture::new().await;
    let hub = fixture.daemon.core.origins.hub().clone();
    let first = tokio::spawn({
        let hub = hub.clone();
        async move { hub.credentials().access().await }
    });
    tokio::time::timeout(Duration::from_secs(2), fixture.refresh.started.notified())
        .await
        .expect("the hub must see the refresh");
    // The caller leaves while the hub holds its spent grant.
    first.abort();
    let second = tokio::spawn({
        let hub = hub.clone();
        async move { hub.credentials().access().await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    fixture.finish_refresh().await;
    let (login, access) = second.await.unwrap().unwrap();
    assert_eq!(login, fixture.login);
    assert_eq!(access, "new-access");
}

/// Serves `daemon` until the returned sender fires, after a caller has
/// started a renewal that the hub holds and then given up on it.
async fn serve_with_a_renewal_in_flight(
    daemon: crate::Agmend,
    refresh: &DelayedRefresh,
) -> (tokio::sync::oneshot::Sender<()>, JoinHandle<()>) {
    let core = daemon.core.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(daemon.serve_until(async move {
        let _stopped = stopped.await;
    }));
    let caller = tokio::spawn(async move { core.origins.hub().credentials().access().await });
    tokio::time::timeout(Duration::from_secs(2), refresh.started.notified())
        .await
        .expect("the hub must see the refresh");
    caller.abort();
    (stop, serving)
}

#[tokio::test]
async fn shutdown_waits_for_a_renewal_in_flight_to_store_its_tokens() {
    let RefreshFixture {
        daemon,
        refresh,
        home,
        login,
    } = RefreshFixture::new().await;
    let (stop, mut serving) = serve_with_a_renewal_in_flight(daemon, &refresh).await;
    stop.send(()).unwrap();
    // The hub answers only after shutdown began: agmend must still be there
    // to store the answer.
    assert!(
        tokio::time::timeout(Duration::from_millis(500), &mut serving)
            .await
            .is_err(),
        "agmend exited with a renewal in flight"
    );
    refresh.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("agmend must exit once the renewal ends")
        .unwrap();
    let stored = home.store().get(&login.key().unwrap()).unwrap().unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["access_token"], "new-access");
    assert_eq!(stored["refresh_token"], "new-refresh");
}

#[tokio::test]
async fn shutdown_does_not_wait_past_its_bound_for_a_hung_hub() {
    let RefreshFixture {
        daemon,
        refresh,
        home: _home,
        login: _login,
    } = RefreshFixture::new().await;
    let (stop, serving) = serve_with_a_renewal_in_flight(daemon, &refresh).await;
    // The hub never answers.
    let began = std::time::Instant::now();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(13), serving)
        .await
        .expect("a hung hub held agmend past its shutdown bound")
        .unwrap();
    let waited = began.elapsed();
    assert!(
        waited >= Duration::from_secs(10),
        "agmend left after {waited:?} with the renewal still in flight"
    );
}

#[tokio::test]
async fn shutdown_does_not_start_a_renewal_for_expired_tokens() {
    const EMPTY_SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
    let fixture = RefreshFixture::new().await;
    tokio::time::timeout(EMPTY_SHUTDOWN_WAIT, fixture.daemon.serve_until(async {}))
        .await
        .expect("shutdown without an in-flight renewal must not wait for the floor");
    assert_eq!(fixture.refresh.calls.load(Ordering::SeqCst), 0);
}

/// Starts a renewal and waits until the hub holds its refresh grant.
async fn renewal_in_flight(
    fixture: &RefreshFixture,
) -> JoinHandle<Result<(Login, String), LoginError>> {
    let hub = fixture.daemon.core.origins.hub().clone();
    let renewal = tokio::spawn(async move { hub.credentials().access().await });
    tokio::time::timeout(Duration::from_secs(2), fixture.refresh.started.notified())
        .await
        .expect("the hub must see the refresh");
    renewal
}

async fn cache_is_empty(fixture: &RefreshFixture) -> bool {
    let hub = fixture.daemon.core.origins.hub();
    hub.credentials().cache.lock().await.is_none()
}

/// What the tracing subscriber wrote, to read back.
#[derive(Clone, Default)]
struct LogBytes(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBytes {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

impl LogBytes {
    /// Captures this thread's events up to INFO until the guard drops.
    fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
        let logs = Self::default();
        let guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .with_writer(logs.clone())
                .finish(),
        );
        (logs, guard)
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[tokio::test]
async fn a_logout_during_a_renewal_leaves_no_token_pair() {
    let (logs, _capture) = LogBytes::capture();
    let fixture = RefreshFixture::new().await;
    let renewal = renewal_in_flight(&fixture).await;
    // `agmen logout`: the tokens go, then the login file.
    fixture.delete_tokens();
    std::fs::remove_file(fixture.home.login_file()).unwrap();
    fixture.refresh.release.notify_one();
    let outcome = renewal.await.unwrap();
    assert!(
        matches!(outcome, Err(LoginError::NotSignedIn)),
        "{outcome:?}"
    );
    assert_eq!(
        fixture.stored(&fixture.login),
        None,
        "the store holds a pair after the logout"
    );
    assert_eq!(fixture.store_files(), 0, "the store folder is not empty");
    assert!(
        cache_is_empty(&fixture).await,
        "the cache holds a pair after the logout"
    );
    assert!(!fixture.home.login_file().exists());
    let text = logs.text();
    assert!(
        text.contains("INFO") && text.contains("discarded"),
        "{text}"
    );
    for secret in ["old-access", "old-refresh", "new-access", "new-refresh"] {
        assert!(!text.contains(secret), "{text}");
    }
}

#[tokio::test]
async fn a_logout_under_way_during_a_renewal_discards_it() {
    // `agmen logout` deletes the tokens before the login file, so a renewal
    // that checks in between finds the file still there.
    let fixture = RefreshFixture::new().await;
    let renewal = renewal_in_flight(&fixture).await;
    fixture.delete_tokens();
    fixture.refresh.release.notify_one();
    let outcome = renewal.await.unwrap();
    assert!(
        matches!(outcome, Err(LoginError::NotSignedIn)),
        "{outcome:?}"
    );
    assert_eq!(
        fixture.stored(&fixture.login),
        None,
        "the pair was written back after the logout began"
    );
    assert!(cache_is_empty(&fixture).await);
}

#[tokio::test]
async fn a_login_as_another_account_during_a_renewal_keeps_the_new_pair() {
    let fixture = RefreshFixture::new().await;
    let renewal = renewal_in_flight(&fixture).await;
    // `agmen login` as someone else: the old tokens go, then the new pair
    // and the login file are written.
    let other = Login {
        hub_url: fixture.login.hub_url.clone(),
        human_id: "hmn0new1".parse().unwrap(),
        handle: "other".into(),
    };
    let pair = HumanTokens::new("other-access".into(), "other-refresh".into(), 3600);
    save_login(
        &fixture.home.store(),
        &fixture.home.login_file(),
        &other,
        &pair,
    )
    .await
    .unwrap();
    let saved = fixture.stored(&other);
    assert!(saved.is_some());
    fixture.refresh.release.notify_one();
    let outcome = renewal.await.unwrap();
    assert!(
        matches!(outcome, Err(LoginError::NotSignedIn)),
        "{outcome:?}"
    );
    assert_eq!(
        fixture.stored(&fixture.login),
        None,
        "the old account's pair came back"
    );
    assert_eq!(
        fixture.stored(&other),
        saved,
        "the new account's pair changed"
    );
    assert_eq!(
        fixture.store_files(),
        1,
        "the store holds more than the new pair"
    );
    assert_eq!(
        Login::read(&fixture.home.login_file()).unwrap(),
        Some(other.clone())
    );
    assert!(cache_is_empty(&fixture).await);
    // agmend serves the new account from its untouched pair, with no second
    // refresh.
    let hub = fixture.daemon.core.origins.hub();
    let (login, access) = hub.credentials().access().await.unwrap();
    assert_eq!((login, access.as_str()), (other, "other-access"));
    assert_eq!(fixture.refresh.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_renewal_for_an_unchanged_login_is_stored_and_cached() {
    let (logs, _capture) = LogBytes::capture();
    let fixture = RefreshFixture::new().await;
    let renewal = renewal_in_flight(&fixture).await;
    fixture.refresh.release.notify_one();
    let (login, access) = renewal.await.unwrap().unwrap();
    assert_eq!(login, fixture.login);
    assert_eq!(access, "new-access");
    let stored = fixture.stored(&fixture.login).unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["access_token"], "new-access");
    assert_eq!(stored["refresh_token"], "new-refresh");
    let hub = fixture.daemon.core.origins.hub();
    let cache = hub.credentials().cache.lock().await;
    assert!(
        cache.as_ref().is_some_and(|(cached, tokens)| {
            *cached == fixture.login && tokens.access_token() == "new-access"
        }),
        "the renewed pair is not cached"
    );
    assert!(!logs.text().contains("discarded"), "{}", logs.text());
}
