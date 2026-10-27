//! The console methods: `whoami`, `console_snapshot`, `chat_history`,
//! `console_watch` and `post`, all as the signed-in person.
//!
//! Messages are the project feed, newest last. Feed seqs commit with no
//! gaps (protocol 5.2), so the page before seq `n` starts after
//! `n - 1 - limit`, and replay, which only pages forward, can serve it.

use std::sync::Arc;

use agmen_client::PAGE_LIMIT;
use agmen_local::console::{ConsoleEvent, ConsoleSnapshot, Me};
use agmen_local::{ApiError, ErrorCode, Response, frame};
use agmen_proto::{ClientEnvelope, ProjectId, Seq, StampedEnvelope};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncReadExt as _;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::broadcast::error::RecvError;

use crate::feed::Feeds;
use crate::hub::{Hub, HubError};

/// What every console method needs.
#[derive(Debug)]
pub(crate) struct Console {
    pub(crate) hub: Arc<Hub>,
    pub(crate) feeds: Arc<Feeds>,
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

fn to_value(value: &impl Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|err| ApiError {
        code: ErrorCode::Hub,
        message: format!("encoding the answer failed: {err}"),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectParams {
    project: ProjectId,
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

    /// `console_snapshot {project}`: the newest 500 feed messages and the
    /// roster.
    pub(crate) async fn snapshot(&self, raw: Value) -> Result<Value, ApiError> {
        let ProjectParams { project } = params(raw)?;
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
        to_value(&ConsoleSnapshot {
            project: project.to_string(),
            me: Me {
                id: login.human_id,
                handle: login.handle,
            },
            roster,
            messages,
            older,
        })
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
        to_value(&History { messages, older })
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
        let ProjectParams { project } = params(raw)?;
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
        loop {
            let event = tokio::select! {
                event = watch.rx.recv() => event,
                // Cancel safe. A watch connection takes no more requests,
                // so any read (bytes or EOF) ends it.
                _ = reader.read(&mut probe) => return Ok(()),
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

/// Writes `event` as a response frame with `id`.
async fn send_event(
    writer: &mut OwnedWriteHalf,
    id: u64,
    event: &ConsoleEvent,
) -> Result<(), frame::FrameError> {
    let value = serde_json::to_value(event)?;
    frame::write_async(writer, &Response::ok(id, value)).await
}
