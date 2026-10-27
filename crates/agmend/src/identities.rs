//! Per-request local caller authentication and identity session publishing.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agmen_client::{ClientEvent, ClientKind, HubClient, OutboxState, Session, SessionConfig};
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{ClientEnvelope, IdentityId, Topics};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;
use url::Url;

use crate::login::Credentials;
use crate::state::{ManagedIdentity, Store};

const DEADLINE: Duration = Duration::from_secs(10);
const POST_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "B T2 specifies a caller owning its managed metadata"
)]
pub(crate) enum Caller {
    Human,
    Identity(ManagedIdentity),
}

// This record deliberately has no Debug implementation: it holds a credential.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalCredential {
    origin: String,
    owner_id: IdentityId,
    local_token: String,
}

#[derive(Debug)]
struct Runtime {
    session: Weak<Session>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
type Publisher = Arc<
    dyn Fn(
            ManagedIdentity,
            ClientEnvelope,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<OutboxState, ApiError>> + Send>,
        > + Send
        + Sync,
>;

pub(crate) struct Identities {
    credentials: Arc<Credentials>,
    store: Arc<Mutex<Store>>,
    state_dir: PathBuf,
    runtimes: AsyncMutex<BTreeMap<(String, IdentityId), Runtime>>,
    #[cfg(test)]
    publisher: Option<Publisher>,
}

impl std::fmt::Debug for Identities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identities").finish_non_exhaustive()
    }
}

impl Identities {
    pub(crate) fn new(
        credentials: Arc<Credentials>,
        store: Arc<Mutex<Store>>,
        state_dir: PathBuf,
    ) -> Self {
        Self {
            credentials,
            store,
            state_dir,
            runtimes: AsyncMutex::new(BTreeMap::new()),
            #[cfg(test)]
            publisher: None,
        }
    }

    pub(crate) async fn authorize(
        &self,
        identity: Option<&IdentityId>,
        token: Option<&str>,
    ) -> Result<Caller, ApiError> {
        let Some(id) = identity else {
            return if token.is_none() {
                Ok(Caller::Human)
            } else {
                Err(unauthorized())
            };
        };
        let supplied = token.ok_or_else(unauthorized)?;
        if !valid_token(supplied) {
            return Err(unauthorized());
        }
        tokio::time::timeout(DEADLINE, async {
            let login = self.credentials.login().await.map_err(|_| unauthorized())?;
            let origin = login.hub_url.origin().ascii_serialization();
            let namespace = Url::parse("https://agmend.invalid").map_err(|_| unauthorized())?;
            let raw = self
                .credentials
                .token(&namespace, id)
                .await?
                .ok_or_else(unauthorized)?;
            let credential: LocalCredential =
                serde_json::from_str(&raw).map_err(|_| unauthorized())?;
            if credential.origin != origin
                || credential.owner_id != login.human_id
                || !tokens_match(&credential.local_token, supplied)
            {
                return Err(unauthorized());
            }
            let rows = self.managed(origin.clone(), login.human_id.clone()).await?;
            let caller = rows
                .into_iter()
                .find(|row| {
                    row.identity_id == *id
                        && row.owner_id == login.human_id
                        && row.hub_url.origin().ascii_serialization() == origin
                })
                .ok_or_else(unauthorized)?;
            Ok(Caller::Identity(caller))
        })
        .await
        .map_err(|_| unauthorized())?
    }

    async fn managed(
        &self,
        origin: String,
        owner: IdentityId,
    ) -> Result<Vec<ManagedIdentity>, ApiError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            store
                .lock()
                .map_err(|_| hub_error("daemon state lock failed"))?
                .identities(&origin, &owner)
                .map_err(|_| hub_error("reading managed identities failed"))
        })
        .await
        .map_err(|_| hub_error("reading managed identities failed"))?
    }

    pub(crate) async fn post(
        &self,
        caller: &ManagedIdentity,
        envelope: ClientEnvelope,
    ) -> Result<Value, ApiError> {
        let id = envelope.id.clone();
        let size = serde_json::to_vec(&envelope)
            .map_err(|_| crate::console::bad_request("invalid envelope"))?
            .len();
        if size > agmen_proto::MAX_ENVELOPE_BYTES {
            return Err(crate::console::bad_request(
                "message exceeds protocol envelope cap",
            ));
        }
        let operation = async {
            #[cfg(test)]
            if let Some(publish) = &self.publisher {
                return tokio::time::timeout(POST_WAIT, publish(caller.clone(), envelope))
                    .await
                    .unwrap_or(Ok(OutboxState::Pending { attempts: 0 }));
            }
            let deadline = tokio::time::Instant::now() + DEADLINE;
            let session = tokio::time::timeout_at(deadline, async {
                let session = self.session(caller).await?;
                session
                    .publish(envelope)
                    .await
                    .map_err(|_| hub_error("writing identity outbox failed"))?;
                Ok::<_, ApiError>(session)
            })
            .await
            .map_err(|_| hub_error("identity post timed out before enqueue completed"))??;
            let wait = async {
                // 100 reads at 50 ms bound the ACK wait independently of event volume.
                for _ in 0..100 {
                    let outbox = session.outbox().clone();
                    let id = id.clone();
                    let state = tokio::task::spawn_blocking(move || outbox.state(&id))
                        .await
                        .map_err(|_| hub_error("reading identity outbox failed"))?
                        .map_err(|_| hub_error("reading identity outbox failed"))?;
                    if let Some(state) = state
                        && !matches!(state, OutboxState::Pending { .. })
                    {
                        return Ok(state);
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Ok::<_, ApiError>(OutboxState::Pending { attempts: 0 })
            };
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::timeout(POST_WAIT.min(remaining), wait)
                .await
                .unwrap_or(Ok(OutboxState::Pending { attempts: 0 }))
        };
        let state = operation.await?;
        match state {
            OutboxState::Recorded { stream, seq } => Ok(json!({"stream":stream,"seq":seq})),
            OutboxState::Pending { .. } => Ok(json!({"queued":true,"id":id})),
            OutboxState::Rejected { reason } => Err(hub_error(&format!(
                "hub rejected identity message: {reason}"
            ))),
        }
    }

    async fn session(&self, caller: &ManagedIdentity) -> Result<Arc<Session>, ApiError> {
        let key = (
            caller.hub_url.origin().ascii_serialization(),
            caller.identity_id.clone(),
        );
        let mut runtimes = self.runtimes.lock().await;
        runtimes.retain(|_, run| !run.task.is_finished());
        if let Some(session) = runtimes.get(&key).and_then(|run| run.session.upgrade()) {
            return Ok(session);
        }
        if runtimes.len() >= 64 {
            return Err(hub_error("64 identity sessions are already running"));
        }
        let token = self
            .credentials
            .token(&caller.hub_url, &caller.identity_id)
            .await?
            .filter(|token| !token.is_empty())
            .ok_or_else(|| hub_error("managed identity has no hub token"))?;
        let hub = HubClient::new(
            caller.hub_url.clone(),
            caller.project.clone(),
            caller.identity_id.clone(),
            token,
        )
        .map_err(|_| hub_error("building identity hub client failed"))?;
        let dir = self
            .state_dir
            .join("identities")
            .join(caller.identity_id.as_str());
        let dir = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)?;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            Ok::<_, std::io::Error>(dir)
        })
        .await
        .map_err(|_| hub_error("creating identity state directory failed"))?
        .map_err(|_| hub_error("creating identity state directory failed"))?;
        let config = SessionConfig::new(
            ClientKind::Mcp,
            Topics::new(caller.tenant.clone(), caller.project.clone()),
            hub,
            caller.broker_url.clone(),
            dir,
        );
        let (session, mut events) = tokio::time::timeout(DEADLINE, Session::connect(config))
            .await
            .map_err(|_| hub_error("connecting identity session timed out"))?
            .map_err(|_| hub_error("connecting identity session failed"))?;
        let session = Arc::new(session);
        let held = session.clone();
        let credentials = self.credentials.clone();
        let store = self.store.clone();
        let identity = caller.clone();
        let task = tokio::spawn(async move {
            let mut check = tokio::time::interval(Duration::from_secs(1));
            // A run ends at the first connection loss. An explicit post can
            // start another run, without an automatic daemon restart.
            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        None | Some(ClientEvent::Connection { connected: false }) => break,
                        Some(_) => {}
                    },
                    _ = check.tick() => {
                        let valid = tokio::time::timeout(DEADLINE, still_managed(&credentials, store.clone(), &identity)).await;
                        if !matches!(valid, Ok(true)) { break; }
                    }
                }
            }
            drop(held);
        });
        runtimes.insert(
            key,
            Runtime {
                session: Arc::downgrade(&session),
                task,
            },
        );
        Ok(session)
    }
}

async fn still_managed(
    credentials: &Credentials,
    store: Arc<Mutex<Store>>,
    identity: &ManagedIdentity,
) -> bool {
    let Ok(login) = credentials.login().await else {
        return false;
    };
    let origin = identity.hub_url.origin().ascii_serialization();
    if login.human_id != identity.owner_id || login.hub_url.origin().ascii_serialization() != origin
    {
        return false;
    }
    let expected = identity.clone();
    // Any failed state read ends the runtime, rather than retaining authority
    // after the daemon can no longer verify its managed identity.
    matches!(
        tokio::task::spawn_blocking(move || {
            store
                .lock()
                .ok()?
                .identities(&origin, &expected.owner_id)
                .ok()
                .map(|rows| rows.contains(&expected))
        })
        .await,
        Ok(Some(true))
    )
}

fn valid_token(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn tokens_match(stored: &str, supplied: &str) -> bool {
    if stored.len() != 64 || supplied.len() != 64 {
        return false;
    }
    stored
        .bytes()
        .zip(supplied.bytes())
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

pub(crate) fn unauthorized() -> ApiError {
    ApiError {
        code: ErrorCode::Unauthorized,
        message: "invalid local identity credential".into(),
    }
}

pub(crate) fn forbidden() -> ApiError {
    ApiError {
        code: ErrorCode::Forbidden,
        message: "method is not available to identity callers".into(),
    }
}

fn hub_error(message: &str) -> ApiError {
    ApiError {
        code: ErrorCode::Hub,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agmen_client::{Login, TokenKey, TokenStore};
    use agmen_local::{Request, Response, frame};
    use agmen_proto::{NackReason, Seq, Stream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::UnixStream;

    struct Fixture {
        _dir: tempfile::TempDir,
        daemon: crate::Agmend,
        who: ManagedIdentity,
        tokens: TokenStore,
    }

    impl Fixture {
        fn new(publisher: Option<Publisher>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let tokens = TokenStore::Dir(dir.path().join("tokens"));
            let login_file = dir.path().join("login.json");
            let who = ManagedIdentity {
                hub_url: "https://hub.example".parse().unwrap(),
                broker_url: "mqtt://127.0.0.1:9".parse().unwrap(),
                tenant: "tenant01".parse().unwrap(),
                project: "project1".parse().unwrap(),
                identity_id: "agent001".parse().unwrap(),
                owner_id: "owner001".parse().unwrap(),
                handle: "owner/agent".into(),
                kind: agmen_proto::IdentityKind::Agent,
            };
            std::fs::write(
                &login_file,
                serde_json::to_vec(&Login {
                    hub_url: who.hub_url.clone(),
                    human_id: who.owner_id.clone(),
                    handle: "owner".into(),
                })
                .unwrap(),
            )
            .unwrap();
            let config =
                crate::Config::new(dir.path().join("agmend.sock"), login_file, tokens.clone());
            drop(crate::Agmend::bind(config.clone()).unwrap());
            rusqlite::Connection::open(dir.path().join("agmend.db"))
                .unwrap()
                .execute(
                    "INSERT INTO identities VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![
                        who.hub_url.origin().ascii_serialization(),
                        who.identity_id.as_str(),
                        who.owner_id.as_str(),
                        who.project.as_str(),
                        serde_json::to_string(&who).unwrap()
                    ],
                )
                .unwrap();
            tokens.set(&TokenKey::new(&"https://agmend.invalid".parse().unwrap(), &who.identity_id).unwrap(), &json!({"origin":"https://hub.example", "owner_id":"owner001", "local_token":"a".repeat(64)}).to_string()).unwrap();
            let mut daemon = crate::Agmend::bind(config).unwrap();
            Arc::get_mut(&mut Arc::get_mut(&mut daemon.core).unwrap().identities)
                .unwrap()
                .publisher = publisher;
            Self {
                _dir: dir,
                daemon,
                who,
                tokens,
            }
        }

        async fn call(&self, method: &str, raw: Value) -> Response {
            let (mut client, server) = UnixStream::pair().unwrap();
            let core = self.daemon.core.clone();
            let task = tokio::spawn(crate::api::serve(server, core));
            let response = tokio::time::timeout(Duration::from_secs(8), async {
                frame::write_async(&mut client, &Request::new(1, "hello", json!({"api":1})))
                    .await
                    .unwrap();
                let hello: Response = frame::read_async(&mut client).await.unwrap();
                assert!(matches!(hello.outcome, agmen_local::Outcome::Ok { .. }));
                let mut request = Request::new(2, method, raw);
                request.as_ = Some(self.who.identity_id.clone());
                frame::write_async(&mut client, &request).await.unwrap();
                frame::read_async(&mut client).await.unwrap()
            })
            .await
            .unwrap();
            drop(client);
            task.await.unwrap();
            response
        }
    }

    fn result(response: Response) -> Value {
        match response.outcome {
            agmen_local::Outcome::Ok { result } => result,
            other => panic!("expected result, received {other:?}"),
        }
    }

    fn error(response: Response) -> ApiError {
        match response.outcome {
            agmen_local::Outcome::Err { error } => error,
            other => panic!("expected error, received {other:?}"),
        }
    }

    fn envelope() -> Value {
        json!({"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"FYI","data":{},"body":"hello","sent_at":"2026-10-27T12:00:00Z"})
    }

    #[tokio::test]
    async fn seeded_identity_post_uses_its_session() {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let fixture = Fixture::new(Some(Arc::new(move |caller, env| {
            assert_eq!(caller.identity_id.as_str(), "agent001");
            assert_eq!(caller.project.as_str(), "project1");
            assert_eq!(env.body.as_str(), "hello");
            seen.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(OutboxState::Recorded {
                    stream: Stream::Feed,
                    seq: Seq(7),
                })
            })
        })));
        let response = fixture
            .call(
                "post",
                json!({"project":"project1","local_token":"a".repeat(64),"envelope":envelope()}),
            )
            .await;
        assert_eq!(response.id, 2);
        assert_eq!(result(response), json!({"stream":"feed","seq":7}));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn identity_post_pending_and_final_rejection() {
        for (state, expected) in [
            (OutboxState::Pending { attempts: 1 }, None),
            (
                OutboxState::Rejected {
                    reason: NackReason::RateLimited,
                },
                Some(ErrorCode::Hub),
            ),
        ] {
            let fixture = Fixture::new(Some(Arc::new(move |_, _| {
                let state = state.clone();
                Box::pin(async { Ok(state) })
            })));
            let response = fixture
                .call(
                    "post",
                    json!({"local_token":"a".repeat(64),"envelope":envelope()}),
                )
                .await;
            match expected {
                None => assert_eq!(
                    result(response),
                    json!({"queued":true,"id":envelope()["id"]})
                ),
                Some(code) => {
                    let error = error(response);
                    assert_eq!(error.code, code);
                    assert!(error.message.contains("rate_limited"));
                }
            }
        }
    }

    #[tokio::test]
    async fn identity_post_stops_waiting_after_five_seconds() {
        let fixture = Fixture::new(Some(Arc::new(|_, _| Box::pin(std::future::pending()))));
        let start = tokio::time::Instant::now();
        let response = fixture
            .call(
                "post",
                json!({"local_token":"a".repeat(64),"envelope":envelope()}),
            )
            .await;
        assert_eq!(
            result(response),
            json!({"queued":true,"id":envelope()["id"]})
        );
        assert!(start.elapsed() >= POST_WAIT);
        assert!(start.elapsed() < Duration::from_secs(7));
    }

    #[tokio::test]
    async fn identity_project_cannot_be_selected_by_caller() {
        let fixture = Fixture::new(Some(Arc::new(|_, _| {
            panic!("foreign project must not publish")
        })));
        let response = fixture
            .call(
                "post",
                json!({"project":"project2","local_token":"a".repeat(64),"envelope":envelope()}),
            )
            .await;
        assert_eq!(error(response).code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn missing_hub_token_is_contextual() {
        let fixture = Fixture::new(None);
        assert!(
            fixture
                .tokens
                .get(&TokenKey::new(&fixture.who.hub_url, &fixture.who.identity_id).unwrap())
                .unwrap()
                .is_none()
        );
        let response = fixture
            .call(
                "post",
                json!({"local_token":"a".repeat(64),"envelope":envelope()}),
            )
            .await;
        let error = error(response);
        assert_eq!(error.code, ErrorCode::Hub);
        assert!(error.message.contains("no hub token"));
        assert!(!error.message.contains(&"a".repeat(64)));
    }
}
