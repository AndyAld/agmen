//! An identity's authenticated calls to the local origin through agmend.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use agmen_client::{ClientEvent, IdentityFile, Page, TokenStore, read_local_credential};
use agmen_local::ErrorCode;
use agmen_proto::{ClientEnvelope, IdentityId, ProjectId, Stream};
use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::agmend::{Agmend, AgmendError, LaunchCredential, LocalToken, Watch};
use crate::client::blocking;
use crate::cmd::lanes::{ItemList, LaneList, PathCheck};
use crate::joined::Recorded;
use crate::output::{Code, Fail};

const WATCH_CAPACITY: usize = 256;
const WATCH_MAX_EVENTS: usize = 1_000_000;
const WATCH_RECONNECT_ATTEMPTS: usize = 5;
const WATCH_RECONNECT_WAIT: Duration = Duration::from_secs(1);
const WATCH_SEND_WAIT: Duration = Duration::from_secs(5);

/// Credentials stay here and are never sent to a hub or broker.
#[derive(Debug)]
pub(crate) struct LocalLink {
    pub(crate) socket: PathBuf,
    identity: IdentityId,
    project: ProjectId,
    token: LocalToken,
    watch_task: Mutex<Option<JoinHandle<()>>>,
}

impl LocalLink {
    /// Uses a matching launch credential, otherwise the saved local credential.
    pub(crate) async fn load(identity: &IdentityFile, tokens: &TokenStore) -> Result<Self> {
        let launch = if std::env::var_os("AGMEN_IDENTITY").as_deref()
            == Some(std::ffi::OsStr::new(identity.identity_id.as_str()))
        {
            LaunchCredential::from_env()?
        } else {
            None
        };
        let token = match launch {
            Some(launch) => launch.token,
            None => {
                let (tokens, id) = (tokens.clone(), identity.identity_id.clone());
                let credential = blocking(move || Ok(read_local_credential(&tokens, &id)?))
                    .await?
                    .ok_or_else(|| {
                        Fail::new(
                            Code::NotLoggedIn,
                            format!("start agmend: no local token for {}", identity.handle),
                        )
                    })?;
                if credential.origin != "local" || credential.owner_id != identity.owner_id {
                    anyhow::bail!("the local identity credential does not match this worktree");
                }
                credential.local_token.parse()?
            }
        };
        Ok(Self {
            socket: agmen_client::agmend_socket()?,
            identity: identity.identity_id.clone(),
            project: identity.project.clone(),
            token,
            watch_task: Mutex::new(None),
        })
    }

    /// Reads authenticated local events without opening a broker connection.
    /// Dropping the receiver or this link closes the watch and its presence.
    pub(crate) async fn watch(&self) -> Result<mpsc::Receiver<ClientEvent>> {
        let watch = open_watch(&self.socket, &self.identity, &self.project, &self.token)
            .await
            .map_err(|error| local_error(redact_error(error, &self.token)))?;
        let (tx, rx) = mpsc::channel(WATCH_CAPACITY);
        let (socket, identity, project, token) = (
            self.socket.clone(),
            self.identity.clone(),
            self.project.clone(),
            self.token.clone(),
        );
        let mut held = self
            .watch_task
            .lock()
            .map_err(|_| anyhow::anyhow!("local watch task lock failed"))?;
        if let Some(previous) = held.take() {
            previous.abort();
        }
        *held = Some(tokio::spawn(async move {
            follow_watch(watch, tx, socket, identity, project, token).await;
        }));
        Ok(rx)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut daemon = Agmend::connect_checked(&self.socket).await?;
        daemon
            .call_as(&self.identity, &self.token, method, params)
            .await
            .map_err(|error| local_error(redact_error(error, &self.token)))
    }

    pub(crate) async fn post(&self, envelope: &ClientEnvelope) -> Result<Recorded> {
        #[derive(Deserialize)]
        struct Posted {
            stream: Stream,
            seq: u64,
        }
        let posted: Posted =
            serde_json::from_value(self.call("post", json!({"envelope": envelope})).await?)
                .context("agmend's post answer is invalid")?;
        Ok(Recorded {
            id: envelope.id.clone(),
            stream: posted.stream,
            seq: posted.seq,
        })
    }

    pub(crate) async fn page(&self, stream: &Stream, after: u64, limit: u32) -> Result<Page> {
        serde_json::from_value(
            self.call(
                "page",
                json!({"stream": stream, "after": after, "limit": limit}),
            )
            .await?,
        )
        .context("agmend's page answer is invalid")
    }

    pub(crate) async fn state(&self, key: &str) -> Result<Value> {
        #[derive(Deserialize)]
        struct State {
            value: Value,
        }
        let answer: State = serde_json::from_value(
            self.call("state", json!({"project": self.project, "key": key}))
                .await?,
        )
        .context("agmend's state answer is invalid")?;
        Ok(answer.value)
    }

    pub(crate) async fn lanes(&self) -> Result<LaneList> {
        serde_json::from_value(self.call("lanes", json!({})).await?)
            .context("agmend's lanes answer is invalid")
    }

    pub(crate) async fn items(&self) -> Result<ItemList> {
        serde_json::from_value(self.call("items", json!({})).await?)
            .context("agmend's items answer is invalid")
    }

    pub(crate) async fn check_path(&self, paths: Vec<String>) -> Result<Vec<PathCheck>> {
        #[derive(Deserialize)]
        struct Checks {
            checks: Vec<PathCheck>,
        }
        let answer: Checks =
            serde_json::from_value(self.call("check_path", json!({"paths": paths})).await?)
                .context("agmend's check_path answer is invalid")?;
        Ok(answer.checks)
    }
}

#[cfg(test)]
impl LocalLink {
    /// A link to a fake agmend at `socket`, for tests whose token is not real.
    pub(crate) fn at(socket: PathBuf, identity: &IdentityFile, token: LocalToken) -> Self {
        Self {
            socket,
            identity: identity.identity_id.clone(),
            project: identity.project.clone(),
            token,
            watch_task: Mutex::new(None),
        }
    }
}

impl Drop for LocalLink {
    fn drop(&mut self) {
        let task = self
            .watch_task
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(task) = task.take() {
            task.abort();
        }
    }
}

async fn open_watch(
    socket: &std::path::Path,
    identity: &IdentityId,
    project: &ProjectId,
    token: &LocalToken,
) -> Result<Watch, AgmendError> {
    Agmend::connect_checked(socket)
        .await?
        .watch_as(identity, token, json!({"project": project}))
        .await
}

async fn watch_send(tx: &mpsc::Sender<ClientEvent>, event: ClientEvent) -> bool {
    matches!(
        tokio::time::timeout(WATCH_SEND_WAIT, tx.send(event)).await,
        Ok(Ok(()))
    )
}

async fn follow_watch(
    watch: Watch,
    tx: mpsc::Sender<ClientEvent>,
    socket: PathBuf,
    identity: IdentityId,
    project: ProjectId,
    token: LocalToken,
) {
    let mut watch = Some(watch);
    if !watch_send(&tx, ClientEvent::Connection { connected: true }).await {
        return;
    }
    for _ in 0..WATCH_MAX_EVENTS {
        let Some(current) = watch.as_mut() else {
            return;
        };
        let result = tokio::select! {
            () = tx.closed() => return,
            result = current.recv() => result,
        };
        match result {
            Ok(event) => {
                if !watch_send(&tx, event).await {
                    return;
                }
                continue;
            }
            Err(error @ (AgmendError::Refused { .. } | AgmendError::Protocol(_))) => {
                tracing::warn!(error = %local_error(redact_error(error, &token)), "local watch stopped");
                return;
            }
            Err(error) => {
                tracing::warn!(error = %redact_error(error, &token), "local watch disconnected");
            }
        }
        // Drop a partially read frame before any reconnect, including a stalled one.
        watch = None;
        if !watch_send(&tx, ClientEvent::Connection { connected: false }).await {
            return;
        }
        for _ in 0..WATCH_RECONNECT_ATTEMPTS {
            let result = tokio::select! {
                () = tx.closed() => return,
                result = async {
                    tokio::time::sleep(WATCH_RECONNECT_WAIT).await;
                    open_watch(&socket, &identity, &project, &token).await
                } => result,
            };
            match result {
                Ok(next) => {
                    watch = Some(next);
                    break;
                }
                Err(error) => {
                    let fatal = matches!(
                        error,
                        AgmendError::Refused { .. } | AgmendError::Protocol(_)
                    );
                    tracing::warn!(error = %local_error(redact_error(error, &token)), "local watch reconnect failed");
                    if fatal {
                        return;
                    }
                }
            }
        }
        if watch.is_none() || !watch_send(&tx, ClientEvent::Connection { connected: true }).await {
            return;
        }
    }
    tracing::warn!("local watch reached its event limit");
}

fn redact_error(error: AgmendError, token: &LocalToken) -> AgmendError {
    match error {
        AgmendError::Refused { code, message } => AgmendError::Refused {
            code,
            message: token.redact(&message),
        },
        other => other,
    }
}

/// Keep daemon messages intact; only a final protocol rejection exits 4.
fn local_error(error: AgmendError) -> anyhow::Error {
    match error {
        AgmendError::Refused {
            code: ErrorCode::Hub,
            message,
        } if message.starts_with("hub rejected identity message: ") => {
            Fail::rejected(message.trim_start_matches("hub rejected identity message: ")).into()
        }
        AgmendError::Refused { message, .. } => anyhow::Error::msg(message),
        other => other.into(),
    }
}
