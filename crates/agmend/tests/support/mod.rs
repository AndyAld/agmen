//! An in-process agmend on a socket in a temporary folder, a fake hub that
//! serves the four routes agmend calls, and a local API client.

#![allow(dead_code, reason = "each test binary uses part of the support module")]

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agmen_client::{HumanTokens, Login, TokenStore, store_tokens};
use agmen_local::{Request, Response, frame};
use agmend::{Agmend, Config};
use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;

pub const PROJECT: &str = "projectp1";
pub const HUMAN: &str = "hmn7k2q9";
pub const HANDLE: &str = "andy";
pub const ACCESS: &str = "access-1";

/// One feed message as the hub stamps it.
pub fn stamped(seq: u64, body: &str) -> Value {
    json!({
        "v": 1,
        "id": format!("01JBX6Q4T7W3C2M8Y9K5N{seq:05}"),
        "verb": "FYI",
        "data": {},
        "body": body,
        "sent_at": "2026-10-25T16:04:11Z",
        "stream": "feed",
        "seq": seq,
        "from": "idn7k2q9",
        "from_handle": "andy/sol",
        "from_kind": "agent",
        "hub_at": "2026-10-25T16:04:12Z",
    })
}

/// A client envelope the human posts.
pub fn envelope(id_tail: u32, body: &str) -> Value {
    json!({
        "v": 1,
        "id": format!("01JBX6Q4T7W3C2M8Y9K5P{id_tail:05}"),
        "verb": "FYI",
        "data": {},
        "body": body,
        "sent_at": "2026-10-25T16:04:11Z",
    })
}

pub fn roster(status: &str) -> Value {
    json!([
        {"identity_id": HUMAN, "handle": HANDLE, "kind": "human", "status": "online"},
        {"identity_id": "idn7k2q9", "handle": "andy/sol", "kind": "agent", "status": status,
         "agent": "claude", "model": "opus"},
    ])
}

/// What the fake hub holds and saw.
#[derive(Default)]
pub struct HubState {
    pub feed: Mutex<Vec<Value>>,
    pub roster: Mutex<Value>,
    /// Bodies posted to `messages`, with their Authorization header.
    pub posts: Mutex<Vec<(String, Value)>>,
    /// When set, `messages` answers this status and body.
    pub post_answer: Mutex<Option<(StatusCode, Value)>>,
    /// How many `events` connections were opened.
    pub event_streams: AtomicUsize,
    /// When set, every route answers 401, as after a logout.
    pub revoked: AtomicBool,
    /// When set, `events` answers this status and error code.
    pub events_refusal: Mutex<Option<(StatusCode, &'static str)>>,
    /// The `after` query of each `events` connection.
    pub event_afters: Mutex<Vec<Option<String>>>,
    live: Mutex<Option<broadcast::Sender<Value>>>,
}

impl HubState {
    /// Appends a feed message and sends it to every open events stream.
    pub fn publish(&self, body: &str) -> u64 {
        let mut feed = self.feed.lock().unwrap();
        let seq = feed.len() as u64 + 1;
        let message = stamped(seq, body);
        feed.push(message.clone());
        if let Some(live) = self.live.lock().unwrap().as_ref() {
            // No stream open is fine: the message is in the feed for replay.
            let _receivers = live.send(message);
        }
        seq
    }

    /// Ends every open events stream, as a hub restart would.
    pub fn drop_streams(&self) {
        self.live.lock().unwrap().take();
    }

    /// Fills the feed to `count` messages that each carry `body`.
    pub fn fill_with(&self, count: u64, body: &str) {
        let mut feed = self.feed.lock().unwrap();
        for seq in feed.len() as u64 + 1..=count {
            feed.push(stamped(seq, body));
        }
    }

    pub fn fill(&self, count: u64) {
        let mut feed = self.feed.lock().unwrap();
        for seq in feed.len() as u64 + 1..=count {
            feed.push(stamped(seq, &format!("message {seq}")));
        }
    }
}

pub struct FakeHub {
    pub url: String,
    pub state: Arc<HubState>,
}

fn authorized(hub: &HubState, headers: &HeaderMap) -> Result<(), Box<HttpResponse>> {
    let want = format!("Bearer {ACCESS}");
    if !hub.revoked.load(Ordering::SeqCst)
        && headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == want)
    {
        Ok(())
    } else {
        Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": "unauthorized"})),
            )
                .into_response(),
        ))
    }
}

#[derive(Deserialize)]
struct KeyQuery {
    key: String,
}

async fn state_route(
    State(hub): State<Arc<HubState>>,
    UrlPath(project): UrlPath<String>,
    headers: HeaderMap,
    Query(query): Query<KeyQuery>,
) -> HttpResponse {
    if let Err(denied) = authorized(&hub, &headers) {
        return *denied;
    }
    assert_eq!(project, PROJECT);
    let head = hub.feed.lock().unwrap().len() as u64;
    let value = match query.key.as_str() {
        "head" => json!({"feed_seq": head, "rules_v": 0, "lanes_v": 0, "items_v": 0,
                         "hub_at": "2026-10-25T16:04:12Z"}),
        "roster" => hub.roster.lock().unwrap().clone(),
        _ => return (StatusCode::BAD_REQUEST, "bad key").into_response(),
    };
    axum::Json(json!({"key": query.key, "as_of_seq": head, "value": value})).into_response()
}

#[derive(Deserialize)]
struct PageQuery {
    #[serde(default)]
    after: u64,
    limit: Option<u64>,
}

async fn replay_route(
    State(hub): State<Arc<HubState>>,
    UrlPath((project, stream)): UrlPath<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> HttpResponse {
    if let Err(denied) = authorized(&hub, &headers) {
        return *denied;
    }
    assert_eq!((project.as_str(), stream.as_str()), (PROJECT, "feed"));
    let limit = query.limit.unwrap_or(500);
    assert!((1..=500).contains(&limit), "limit {limit}");
    let feed = hub.feed.lock().unwrap();
    let head = feed.len() as u64;
    let messages: Vec<Value> = feed
        .iter()
        .skip(usize::try_from(query.after).unwrap().min(feed.len()))
        .take(usize::try_from(limit).unwrap())
        .cloned()
        .collect();
    let last = query.after + messages.len() as u64;
    let next_after = (last < head).then_some(last);
    axum::Json(json!({"messages": messages, "next_after": next_after, "head": head}))
        .into_response()
}

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<String>,
}

async fn events_route(
    State(hub): State<Arc<HubState>>,
    UrlPath(project): UrlPath<String>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> HttpResponse {
    if let Err(denied) = authorized(&hub, &headers) {
        return *denied;
    }
    assert_eq!(project, PROJECT);
    if let Some((status, code)) = *hub.events_refusal.lock().unwrap() {
        return (status, axum::Json(json!({"error": code}))).into_response();
    }
    hub.event_streams.fetch_add(1, Ordering::SeqCst);
    hub.event_afters.lock().unwrap().push(query.after.clone());
    let after = query
        .after
        .unwrap_or_default()
        .split(',')
        .find_map(|entry| {
            entry
                .strip_prefix("feed:")
                .map(|s| s.parse::<u64>().unwrap())
        })
        .unwrap_or(0);
    let (replay, mut live) = {
        let feed = hub.feed.lock().unwrap();
        let mut slot = hub.live.lock().unwrap();
        let live = slot
            .get_or_insert_with(|| broadcast::channel(1024).0)
            .subscribe();
        let replay: Vec<Value> = feed
            .iter()
            .skip(usize::try_from(after).unwrap())
            .cloned()
            .collect();
        (replay, live)
    };
    let (tx, rx) = mpsc::channel::<Value>(1024);
    tokio::spawn(async move {
        for message in replay {
            if tx.send(message).await.is_err() {
                return;
            }
        }
        while let Ok(message) = live.recv().await {
            if tx.send(message).await.is_err() {
                return;
            }
        }
    });
    let events = ReceiverStream::new(rx).map(|message| {
        Ok::<_, Infallible>(
            Event::default()
                .id(format!("feed:{}", message["seq"]))
                .data(message.to_string()),
        )
    });
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

async fn post_route(
    State(hub): State<Arc<HubState>>,
    UrlPath(project): UrlPath<String>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> HttpResponse {
    assert_eq!(project, PROJECT);
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    hub.posts.lock().unwrap().push((auth, body.clone()));
    if let Err(denied) = authorized(&hub, &headers) {
        return *denied;
    }
    if let Some((status, answer)) = hub.post_answer.lock().unwrap().clone() {
        return (status, axum::Json(answer)).into_response();
    }
    let seq = hub.publish(body["body"].as_str().unwrap_or_default());
    axum::Json(json!({"verb": "ACK", "id": body["id"], "stream": "feed", "seq": seq}))
        .into_response()
}

/// Starts the fake hub on a free port.
pub async fn fake_hub() -> FakeHub {
    let state = Arc::new(HubState::default());
    *state.roster.lock().unwrap() = roster("online");
    let app = Router::new()
        .route("/v1/projects/{p}/state", get(state_route))
        .route("/v1/projects/{p}/streams/{s}", get(replay_route))
        .route("/v1/projects/{p}/events", get(events_route))
        .route("/v1/projects/{p}/messages", post(post_route))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    FakeHub {
        url: format!("http://{addr}/"),
        state,
    }
}

/// A temporary home: the socket, the login file and the token store.
pub struct Home {
    pub dir: tempfile::TempDir,
}

impl Home {
    /// A home where the CLI signed in at `hub_url`.
    pub async fn signed_in(hub_url: &str) -> Self {
        Self::signed_in_for(hub_url, 3600).await
    }

    /// As [`Home::signed_in`], with an access token that runs out in
    /// `expires_in` seconds (under 120 makes agmend renew it first).
    pub async fn signed_in_for(hub_url: &str, expires_in: i64) -> Self {
        // In target/tmp: short enough for a Unix socket path (104 bytes on
        // macOS), and inside the worktree.
        let dir = tempfile::Builder::new()
            .prefix("d")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .unwrap();
        let home = Self { dir };
        let login = Login {
            hub_url: hub_url.parse().unwrap(),
            human_id: HUMAN.parse().unwrap(),
            handle: HANDLE.to_owned(),
        };
        std::fs::write(home.login_file(), serde_json::to_vec(&login).unwrap()).unwrap();
        let tokens = HumanTokens::new(ACCESS.to_owned(), "refresh-1".to_owned(), expires_in);
        store_tokens(&home.tokens(), &login, &tokens).await.unwrap();
        home
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.path().join("agmend.sock")
    }

    pub fn login_file(&self) -> PathBuf {
        self.dir.path().join("login.json")
    }

    pub fn tokens(&self) -> TokenStore {
        TokenStore::Dir(self.dir.path().join("tokens"))
    }

    /// Where this home's personal roles live.
    pub fn personal_roles(&self) -> PathBuf {
        self.dir.path().join("roles")
    }

    pub fn config(&self) -> Config {
        Config::new(self.socket(), self.login_file(), self.tokens())
            .roster_every(Duration::from_millis(100))
            .personal_roles(self.personal_roles())
            .local_every(Duration::from_millis(100))
    }
}

/// A running agmend and the fake hub it talks to.
pub struct Daemon {
    pub hub: FakeHub,
    pub home: Home,
}

impl Daemon {
    pub async fn start() -> Self {
        let hub = fake_hub().await;
        let home = Home::signed_in(&hub.url).await;
        Self::run(hub, home)
    }

    /// An agmend whose login names a hub nobody listens on, with a token
    /// that needs renewing, so every renewal fails to connect.
    pub async fn start_unreachable() -> Self {
        let hub = fake_hub().await;
        let home = Home::signed_in_for("http://127.0.0.1:9/", 30).await;
        Self::run(hub, home)
    }

    fn run(hub: FakeHub, home: Home) -> Self {
        let agmend = Agmend::bind(home.config()).unwrap();
        tokio::spawn(agmend.serve());
        Self { hub, home }
    }

    /// A connection that has said hello.
    pub async fn client(&self) -> Client {
        let mut client = Client::connect(&self.home.socket()).await;
        let hello = client.call("hello", json!({"api": 1})).await;
        assert_eq!(hello.outcome, ok(json!({"api": 1})));
        client
    }
}

pub fn ok(result: Value) -> agmen_local::Outcome {
    agmen_local::Outcome::Ok { result }
}

/// The result of `response`, failing the test on an error.
pub fn result(response: Response) -> Value {
    match response.outcome {
        agmen_local::Outcome::Ok { result } => result,
        agmen_local::Outcome::Err { error } => panic!("{error:?}"),
    }
}

pub fn error(response: Response) -> agmen_local::ApiError {
    match response.outcome {
        agmen_local::Outcome::Err { error } => error,
        agmen_local::Outcome::Ok { result } => panic!("expected an error, got {result}"),
    }
}

/// A local API connection.
pub struct Client {
    stream: UnixStream,
    next: u64,
}

impl Client {
    pub async fn connect(socket: &Path) -> Self {
        Self {
            stream: UnixStream::connect(socket).await.unwrap(),
            next: 1,
        }
    }

    pub async fn send(&mut self, request: &Request) {
        frame::write_async(&mut self.stream, request).await.unwrap();
    }

    pub async fn read(&mut self) -> Response {
        frame::read_async(&mut self.stream).await.unwrap()
    }

    pub async fn call(&mut self, method: &str, params: Value) -> Response {
        let id = self.next;
        self.next += 1;
        self.send(&Request::new(id, method, params)).await;
        let response = self.read().await;
        assert_eq!(response.id, id);
        response
    }

    /// Starts `console_watch` and hands the frames to a channel, so a test
    /// can wait on them with a timeout without cutting a frame in half.
    pub async fn watch(self, project: &str) -> mpsc::Receiver<Response> {
        self.watch_with(json!({"project": project})).await
    }

    /// As [`Client::watch`], with any params, such as a checkout `root`.
    pub async fn watch_with(mut self, params: Value) -> mpsc::Receiver<Response> {
        let id = self.next;
        self.send(&Request::new(id, "console_watch", params)).await;
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(async move {
            while let Ok(response) = frame::read_async::<Response>(&mut self.stream).await {
                assert_eq!(response.id, id);
                if tx.send(response).await.is_err() {
                    return;
                }
            }
        });
        rx
    }
}

/// The next frame within 5 s.
pub async fn next(frames: &mut mpsc::Receiver<Response>) -> Value {
    let response = tokio::time::timeout(Duration::from_secs(5), frames.recv())
        .await
        .expect("a frame within 5 s")
        .expect("the watch is still open");
    result(response)
}

/// Asserts no frame arrives within `wait`.
pub async fn quiet(frames: &mut mpsc::Receiver<Response>, wait: Duration) {
    if let Ok(frame) = tokio::time::timeout(wait, frames.recv()).await {
        panic!("expected no frame, got {frame:?}");
    }
}
