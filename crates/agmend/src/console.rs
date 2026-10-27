//! The console methods: `whoami`, `console_snapshot`, `chat_history`,
//! `console_watch`, `post` and `file_text`, all as the signed-in person.
//!
//! Messages are the project feed, newest last. Feed seqs commit with no
//! gaps (protocol 5.2), so the page before seq `n` starts after
//! `n - 1 - limit`, and replay, which only pages forward, can serve it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agmen_client::PAGE_LIMIT;
use agmen_local::console::{ConsoleEvent, ConsoleSnapshot, LocalData, Me};
use agmen_local::{ApiError, ErrorCode, MAX_FRAME, Response, frame};
use agmen_proto::{ClientEnvelope, ProjectId, Seq, StampedEnvelope};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncReadExt as _;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::broadcast::error::RecvError;

use crate::feed::Feeds;
use crate::hub::{Hub, HubError};
use crate::local::{self, Local};

/// What every console method needs.
#[derive(Debug)]
pub(crate) struct Console {
    pub(crate) hub: Arc<Hub>,
    pub(crate) feeds: Arc<Feeds>,
    pub(crate) local: Local,
    /// How often a watch with a checkout re-reads its local data.
    pub(crate) local_every: Duration,
}

/// `params` as `T`, or `bad_request`.
pub(crate) fn params<T: DeserializeOwned>(params: Value) -> Result<T, ApiError> {
    // A method without params may get `null`; treat it as `{}`.
    let params = if params.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        params
    };
    serde_json::from_value(params).map_err(|err| bad_request(format!("bad params: {err}")))
}

pub(crate) fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError {
        code: ErrorCode::BadRequest,
        message: message.into(),
    }
}

/// Room left in a frame for the `{"id": n, "result": ...}` wrapper, with
/// slack for any difference between sizing and the final encoding.
const FRAME_SLACK: usize = 4096;

/// The JSON length of `value`.
fn json_len(value: &impl Serialize) -> Result<usize, ApiError> {
    serde_json::to_vec(value)
        .map(|json| json.len())
        .map_err(encode_error)
}

/// The newest of `messages` (oldest first) that fit one frame next to a
/// result of `base` bytes, and whether any were dropped. Dropped ones are
/// the oldest, so a console reaches them with `chat_history` from the
/// oldest one kept. A message that cannot fit even alone is an error.
fn fit(
    mut messages: Vec<StampedEnvelope>,
    base: usize,
) -> Result<(Vec<StampedEnvelope>, bool), ApiError> {
    let budget = usize::try_from(MAX_FRAME)
        .unwrap_or(usize::MAX)
        .saturating_sub(base.saturating_add(FRAME_SLACK));
    let mut used = 0usize;
    let mut keep = 0usize;
    for message in messages.iter().rev() {
        // One more byte for the comma between messages.
        let len = json_len(message)?.saturating_add(1);
        if used.saturating_add(len) > budget {
            break;
        }
        used += len;
        keep += 1;
    }
    if keep == 0 && !messages.is_empty() {
        return Err(ApiError {
            code: ErrorCode::Hub,
            message: format!("the newest message does not fit the {MAX_FRAME} byte frame cap"),
        });
    }
    let dropped = messages.len() - keep;
    messages.drain(..dropped);
    Ok((messages, dropped > 0))
}

fn encode_error(err: serde_json::Error) -> ApiError {
    ApiError {
        code: ErrorCode::Hub,
        message: format!("encoding the answer failed: {err}"),
    }
}

fn to_value(value: &impl Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(encode_error)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotParams {
    project: ProjectId,
    /// The person's checkout of the project, which the desktop found;
    /// without it the snapshot has no worktrees, git facts or project
    /// roles (Task 8).
    #[serde(default)]
    root: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchParams {
    project: ProjectId,
    /// The checkout whose local data the watch re-reads and sends as
    /// `Local` events when it changes (Task 8 review fix 3).
    #[serde(default)]
    root: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTextParams {
    project: ProjectId,
    root: PathBuf,
    worktree: PathBuf,
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryParams {
    project: ProjectId,
    /// Left out for the newest page, as m0-m1's `chat_history` allows.
    #[serde(default)]
    before: Option<Seq>,
    #[serde(default = "page_limit")]
    limit: u32,
}

const fn page_limit() -> u32 {
    PAGE_LIMIT
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PostParams {
    project: ProjectId,
    envelope: ClientEnvelope,
}

/// `chat_history`'s answer.
#[derive(Debug, Serialize)]
struct History {
    messages: Vec<StampedEnvelope>,
    older: bool,
}

/// `post`'s answer.
#[derive(Debug, Serialize)]
struct Posted {
    stream: agmen_proto::Stream,
    seq: Seq,
}

impl Console {
    /// `whoami`: the person in the CLI's login file.
    pub(crate) async fn whoami(&self) -> Result<Value, ApiError> {
        let login = self
            .hub
            .credentials()
            .login()
            .await
            .map_err(HubError::from)?;
        to_value(&Me {
            id: login.human_id,
            handle: login.handle,
        })
    }

    /// `console_snapshot {project, root?}`: the newest 500 feed messages,
    /// the roster, the role files, and, for a trusted checkout at `root`,
    /// its worktrees and git facts.
    pub(crate) async fn snapshot(&self, raw: Value) -> Result<Value, ApiError> {
        let SnapshotParams { project, root } = params(raw)?;
        let login = self
            .hub
            .credentials()
            .login()
            .await
            .map_err(HubError::from)?;
        let head = self.hub.head(&project).await?;
        let (messages, older) = self
            .before(&project, head.saturating_add(1), PAGE_LIMIT)
            .await?;
        let roster = self.hub.roster(&project).await?;
        let local = self.local.read(&project, root.as_deref()).await;
        let mut snapshot = ConsoleSnapshot {
            project: project.to_string(),
            me: Me {
                id: login.human_id,
                handle: login.handle,
            },
            roster,
            messages: Vec::new(),
            older,
            roles: local.roles,
            worktrees: local.worktrees,
            git: local.git,
        };
        let (messages, trimmed) = fit(messages, json_len(&snapshot)?)?;
        snapshot.messages = messages;
        snapshot.older |= trimmed;
        to_value(&snapshot)
    }

    /// `chat_history {project, before?, limit?}`: up to `limit` (default
    /// and cap 500) feed messages before seq `before`, or the newest ones.
    pub(crate) async fn history(&self, raw: Value) -> Result<Value, ApiError> {
        let HistoryParams {
            project,
            before,
            limit,
        } = params(raw)?;
        if !(1..=PAGE_LIMIT).contains(&limit) {
            return Err(bad_request(format!(
                "limit must be 1 to {PAGE_LIMIT}, not {limit}"
            )));
        }
        let before = match before {
            Some(before) => before.0,
            None => self.hub.head(&project).await?.saturating_add(1),
        };
        let (messages, older) = self.before(&project, before, limit).await?;
        let base = json_len(&History {
            messages: Vec::new(),
            older,
        })?;
        let (messages, trimmed) = fit(messages, base)?;
        to_value(&History {
            messages,
            older: older || trimmed,
        })
    }

    /// Up to `limit` feed messages with seqs below `before`, and whether
    /// older ones exist.
    async fn before(
        &self,
        project: &ProjectId,
        before: u64,
        limit: u32,
    ) -> Result<(Vec<StampedEnvelope>, bool), HubError> {
        let last = before.saturating_sub(1);
        let after = last.saturating_sub(u64::from(limit));
        let count = last - after;
        if count == 0 {
            return Ok((Vec::new(), false));
        }
        // count <= limit <= 500, so it fits.
        let count = u32::try_from(count).unwrap_or(PAGE_LIMIT);
        let page = self.hub.page(project, after, count).await?;
        let mut messages = page.messages;
        messages.retain(|m| m.seq.0 < before);
        Ok((messages, after > 0))
    }

    /// `file_text {project, root, worktree, path}`: one file's text, at
    /// most 512 KiB, and its gutter marks.
    pub(crate) async fn file_text(&self, raw: Value) -> Result<Value, ApiError> {
        let FileTextParams {
            project,
            root,
            worktree,
            path,
        } = params(raw)?;
        to_value(&local::file_text(&project, &root, &worktree, &path).await?)
    }

    /// `post {project, envelope}`: the envelope as the person, over HTTP.
    pub(crate) async fn post(&self, raw: Value) -> Result<Value, ApiError> {
        let PostParams { project, envelope } = params(raw)?;
        let (stream, seq) = self.hub.post(&project, &envelope).await?;
        to_value(&Posted { stream, seq })
    }

    /// `console_watch {project}`: streams `ConsoleEvent` frames, each a
    /// response with the request's id, until the console hangs up or falls
    /// behind. The connection serves nothing else.
    pub(crate) async fn watch(
        &self,
        id: u64,
        raw: Value,
        mut reader: OwnedReadHalf,
        writer: &mut OwnedWriteHalf,
    ) -> Result<(), ApiError> {
        let WatchParams { project, root } = params(raw)?;
        // A project the person may not read fails here, not as a stream
        // that never connects. An unreachable hub is not an error: the
        // watch starts disconnected and connects when the hub is back.
        if let Err(err) = self.hub.head(&project).await
            && err.is_final()
        {
            return Err(err.into());
        }
        let mut watch = self.feeds.watch(&project);
        if send_event(writer, id, &watch.first).await.is_err() {
            return Ok(());
        }
        let mut probe = [0u8; 1];
        let mut local = LocalWatch::new(self, &project, root, self.local_every);
        loop {
            let event = tokio::select! {
                event = watch.rx.recv() => event,
                // Cancel safe. A watch connection takes no more requests,
                // so any read (bytes or EOF) ends it.
                _ = reader.read(&mut probe) => return Ok(()),
                changed = local.next() => match changed {
                    Some(data) => Ok(ConsoleEvent::Local(Box::new(data))),
                    None => continue,
                },
            };
            match event {
                Ok(event) => {
                    if send_event(writer, id, &event).await.is_err() {
                        return Ok(());
                    }
                }
                Err(RecvError::Lagged(missed)) => {
                    return Err(ApiError {
                        code: ErrorCode::Hub,
                        message: format!(
                            "this console fell {missed} events behind; take a new snapshot and watch again"
                        ),
                    });
                }
                Err(RecvError::Closed) => return Ok(()),
            }
        }
    }
}

/// Task 8 review fix 3: a watch's re-reads of its checkout. One read runs
/// at a time, every `every` from the watch's start; a tick while one runs
/// is skipped. The first read is always sent, since the snapshot before
/// the watch may be stale; later ones only when they differ.
struct LocalWatch {
    reader: Option<(Local, ProjectId, PathBuf)>,
    ticks: tokio::time::Interval,
    running: Option<tokio::task::JoinHandle<LocalData>>,
    last: Option<LocalData>,
}

impl LocalWatch {
    fn new(console: &Console, project: &ProjectId, root: Option<PathBuf>, every: Duration) -> Self {
        let mut ticks = tokio::time::interval(every);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Self {
            reader: root.map(|root| (console.local.clone(), project.clone(), root)),
            ticks,
            running: None,
            last: None,
        }
    }

    /// The next read that changed; `None` when a step finished with
    /// nothing to send. Pends forever for a watch without a checkout.
    /// Cancel safe: the tick and the join handle both are, and a read
    /// keeps running in its task.
    async fn next(&mut self) -> Option<LocalData> {
        let Some((local, project, root)) = &self.reader else {
            return std::future::pending().await;
        };
        if let Some(running) = &mut self.running {
            let read = running.await;
            self.running = None;
            let data = match read {
                Ok(data) => data,
                Err(err) => {
                    tracing::debug!(error = %err, "re-reading the checkout stopped");
                    return None;
                }
            };
            if self.last.as_ref() == Some(&data) {
                return None;
            }
            self.last = Some(data.clone());
            return Some(data);
        }
        self.ticks.tick().await;
        let (local, project, root) = (local.clone(), project.clone(), root.clone());
        self.running = Some(tokio::spawn(async move {
            local.read(&project, Some(&root)).await
        }));
        None
    }
}

impl Drop for LocalWatch {
    fn drop(&mut self) {
        // A read for a watch that ended has no one to tell.
        if let Some(running) = self.running.take() {
            running.abort();
        }
    }
}

/// Writes `event` as a response frame with `id`.
async fn send_event(
    writer: &mut OwnedWriteHalf,
    id: u64,
    event: &ConsoleEvent,
) -> Result<(), frame::FrameError> {
    let value = serde_json::to_value(event)?;
    frame::write_async(writer, &Response::ok(id, value)).await
}
