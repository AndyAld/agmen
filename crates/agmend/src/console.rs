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
use agmen_local::console::{
    ConsoleEvent, ConsoleSnapshot, ListenerEntry, LocalData, Me, RosterEntry, SettingsChangeEntry,
    Status, change_card_id,
};
use agmen_local::{ApiError, ErrorCode, MAX_FRAME, Response, frame};
use agmen_proto::{ClientEnvelope, IdentityId, IdentityKind, ProjectId, Seq, StampedEnvelope};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::broadcast::error::RecvError;

use crate::feed::Feeds;
use crate::hub::{Hub, HubError};
use crate::listener::ListenerCaps;
use crate::local::{self, Local};

/// What every console method needs.
#[derive(Debug)]
pub(crate) struct Console {
    pub(crate) hub: Arc<Hub>,
    pub(crate) feeds: Arc<Feeds>,
    pub(crate) local: Local,
    /// How often a watch with a checkout re-reads its local data.
    pub(crate) local_every: Duration,
    /// Listener status. Change cards are published from the settings store.
    pub(crate) listeners: Arc<crate::listener::Listeners>,
    /// The settings file the listener caps are read from.
    pub(crate) settings_file: PathBuf,
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

/// How many listener rows and change cards one snapshot keeps.
const MAX_STATUS_ROWS: usize = 64;
const MAX_CHANGE_CARDS: usize = 256;
/// Messages scanned for `apply:<id>` / `undo:<id>`.
const MAX_CARD_MESSAGES: usize = 500;
/// How many `accept_from` handles the listener row names.
const MAX_ACCEPT_SHOWN: usize = 64;

#[derive(Debug, Deserialize)]
struct StatusRow {
    #[serde(default)]
    identity_id: String,
    #[serde(default)]
    handle: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    today_usd: String,
    #[serde(default)]
    daily_usd: String,
}

fn person_of(handle: &str) -> &str {
    handle.split_once('/').map_or(handle, |(person, _)| person)
}

fn presence(status: Status) -> &'static str {
    match status {
        Status::Online => "online",
        Status::Away => "away",
        Status::Offline => "offline",
    }
}

/// The person's listener when a status row exists, or when the roster
/// lists one and the settings file has that project's table. Otherwise
/// none, so an empty status does not invent caps.
///
/// A running listener works to the settings `load` reads, defaults
/// included, so it reports those. One that is not running reports caps
/// only when the person's table names them.
fn listener_entry(
    rows: &[StatusRow],
    caps: Option<&ListenerCaps>,
    roster: &[RosterEntry],
    me: &Me,
) -> Option<ListenerEntry> {
    let mine = |handle: &str| person_of(handle) == me.handle;
    let row = rows
        .iter()
        .take(MAX_STATUS_ROWS)
        .find(|row| mine(&row.handle));
    let roster_listener = roster
        .iter()
        .find(|entry| entry.kind == IdentityKind::Listener && mine(&entry.handle));
    let caps = caps.filter(|caps| row.is_some() || caps.configured);
    if row.is_none() && (roster_listener.is_none() || caps.is_none()) {
        return None;
    }
    let identity_id = row
        .and_then(|row| row.identity_id.parse::<IdentityId>().ok())
        .or_else(|| roster_listener.map(|entry| entry.identity_id.clone()))?;
    let handle = row
        .map(|row| row.handle.clone())
        .filter(|handle| !handle.is_empty())
        .or_else(|| roster_listener.map(|entry| entry.handle.clone()))
        .unwrap_or_default();
    let state = row
        .map(|row| row.state.clone())
        .filter(|state| !state.is_empty())
        .unwrap_or_else(|| {
            roster_listener
                .map(|entry| presence(entry.status).to_owned())
                .unwrap_or_default()
        });
    let model = row
        .map(|row| row.model.clone())
        .filter(|model| !model.is_empty())
        .or_else(|| caps.map(|caps| caps.model.clone()))
        .unwrap_or_default();
    let today_usd = row
        .map(|row| row.today_usd.clone())
        .filter(|amount| !amount.is_empty())
        .unwrap_or_else(|| "0.00".into());
    let daily_usd = row
        .map(|row| row.daily_usd.clone())
        .filter(|amount| !amount.is_empty())
        .or_else(|| caps.map(|caps| caps.listener_daily_usd.clone()))
        .unwrap_or_else(|| "0.00".into());
    let takes_work_from = caps.map(|caps| takes_work_from(me, caps));
    Some(ListenerEntry {
        identity_id,
        handle,
        state,
        model,
        today_usd,
        daily_usd,
        per_delegation_usd: caps.map(|caps| caps.per_delegation_usd.clone()),
        workers_daily_usd: caps.map(|caps| caps.workers_daily_usd.clone()),
        max_workers: caps.map(|caps| caps.max_workers),
        workers_running: None,
        workers_today_usd: None,
        can_use: Vec::new(),
        never: Vec::new(),
        takes_work_from,
        role: Some("listener".into()),
        tokens_in: None,
        tokens_out: None,
        uptime_minutes: None,
    })
}

/// Who the listener takes work from: the person, coordinators, and the
/// handles the settings add. These are the senders its intake accepts.
fn takes_work_from(me: &Me, caps: &ListenerCaps) -> String {
    let mut who = vec![me.handle.as_str(), "coordinators"];
    who.extend(
        caps.accept_from
            .iter()
            .map(String::as_str)
            .filter(|handle| *handle != me.handle)
            .take(MAX_ACCEPT_SHOWN),
    );
    who.join(", ")
}

/// The identities of the person's own listener, from the roster.
fn own_listeners(roster: &[RosterEntry], me: &Me) -> Vec<IdentityId> {
    roster
        .iter()
        .filter(|entry| {
            entry.kind == IdentityKind::Listener && person_of(&entry.handle) == me.handle
        })
        .map(|entry| entry.identity_id.clone())
        .take(MAX_STATUS_ROWS)
        .collect()
}

/// Points each card at the newest message from one of `listeners` whose
/// last line names it. The card's id is on the shared feed, so anyone can
/// end an FYI with it: a message from anyone else never takes a card, and
/// cannot move one off the listener's own message.
fn attach_message_ids(
    changes: &mut [SettingsChangeEntry],
    messages: &[StampedEnvelope],
    listeners: &[IdentityId],
) {
    let start = messages.len().saturating_sub(MAX_CARD_MESSAGES);
    let messages = &messages[start..];
    for card in changes.iter_mut().take(MAX_CHANGE_CARDS) {
        card.message_id = messages.iter().rev().find_map(|message| {
            (listeners.contains(&message.from)
                && change_card_id(message.client.body.as_str()) == Some(card.id.as_str()))
            .then(|| message.client.id.clone())
        });
    }
}

/// Drops a card's message id when `fit` did not keep that message, so the
/// encoded snapshot stays within the size `fit` already allowed.
fn clear_dropped_cards(changes: &mut [SettingsChangeEntry], kept: &[StampedEnvelope]) {
    for card in changes.iter_mut().take(MAX_CHANGE_CARDS) {
        let present = card.message_id.as_ref().is_some_and(|id| {
            kept.iter()
                .take(MAX_CARD_MESSAGES)
                .any(|message| &message.client.id == id)
        });
        if card.message_id.is_some() && !present {
            card.message_id = None;
        }
    }
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
        let me = Me {
            id: login.human_id,
            handle: login.handle,
        };
        // After the hub reads, so this does not hold a hub lock across them.
        // A status or settings failure leaves the chat snapshot intact.
        let (listener, mut changes) = self.listener_view(&project, &roster, &me).await;
        attach_message_ids(&mut changes, &messages, &own_listeners(&roster, &me));
        let mut snapshot = ConsoleSnapshot {
            project: project.to_string(),
            me,
            roster,
            messages: Vec::new(),
            older,
            roles: local.roles,
            worktrees: local.worktrees,
            git: local.git,
            listener,
            changes,
        };
        let (messages, trimmed) = fit(messages, json_len(&snapshot)?)?;
        clear_dropped_cards(&mut snapshot.changes, &messages);
        snapshot.messages = messages;
        snapshot.older |= trimmed;
        to_value(&snapshot)
    }

    /// Status rows for `project`, or none when agmend cannot read them.
    /// One bad row is skipped so the chat snapshot still returns.
    async fn status_rows(&self, project: &ProjectId) -> Vec<StatusRow> {
        let value = match self
            .listeners
            .human_call("listener_status", json!({"project": project}))
            .await
        {
            Ok(value) => value,
            Err(err) => {
                tracing::debug!(
                    message = %err.message,
                    "listener status unavailable; the snapshot keeps the chat"
                );
                return Vec::new();
            }
        };
        let Value::Array(rows) = value else {
            tracing::debug!("listener status was not a list; the snapshot keeps the chat");
            return Vec::new();
        };
        let mut out = Vec::new();
        for row in rows.into_iter().take(MAX_STATUS_ROWS) {
            match serde_json::from_value(row) {
                Ok(row) => out.push(row),
                Err(_) => {
                    tracing::debug!("skipping a listener status row the console could not read");
                }
            }
        }
        out
    }

    async fn file_caps(&self, project: &ProjectId) -> Option<ListenerCaps> {
        let path = self.settings_file.clone();
        let project = project.clone();
        match tokio::task::spawn_blocking(move || crate::listener::listener_caps(&path, &project))
            .await
        {
            Ok(caps) => caps,
            Err(err) => {
                tracing::debug!(
                    error = %err,
                    "reading listener caps failed; the snapshot keeps the chat"
                );
                None
            }
        }
    }

    async fn listener_view(
        &self,
        project: &ProjectId,
        roster: &[RosterEntry],
        me: &Me,
    ) -> (Option<ListenerEntry>, Vec<SettingsChangeEntry>) {
        let rows = self.status_rows(project).await;
        let caps = self.file_caps(project).await;
        let listener = listener_entry(&rows, caps.as_ref(), roster, me);
        let mut changes = crate::listener::published_changes(&self.settings_file, project);
        if changes.len() > MAX_CHANGE_CARDS {
            changes.truncate(MAX_CHANGE_CARDS);
        }
        (listener, changes)
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

#[cfg(test)]
mod tests {
    use agmen_local::console::{Me, RosterEntry, SettingsChangeEntry, SettingsChangeState, Status};
    use agmen_proto::{IdentityId, IdentityKind, StampedEnvelope};

    use super::{ListenerCaps, StatusRow, attach_message_ids, listener_entry, own_listeners};

    /// Andy's listener, the sender of every card in these tests.
    const LISTENER: &str = "idlisten";

    fn ident(id: &str) -> IdentityId {
        id.parse().unwrap()
    }

    fn message_from(seq: u64, from: &str, kind: &str, body: &str) -> StampedEnvelope {
        serde_json::from_value(serde_json::json!({
            "v": 1,
            "id": format!("01JBX6Q4T7W3C2M8Y9K5N{seq:05}"),
            "verb": "FYI",
            "data": {},
            "body": body,
            "sent_at": "2026-10-25T16:04:11Z",
            "stream": "feed",
            "seq": seq,
            "from": from,
            "from_handle": "andy/listener",
            "from_kind": kind,
            "hub_at": "2026-10-25T16:04:12Z",
        }))
        .unwrap()
    }

    fn message(seq: u64, body: &str) -> StampedEnvelope {
        message_from(seq, LISTENER, "listener", body)
    }

    fn card(id: &str) -> SettingsChangeEntry {
        SettingsChangeEntry {
            id: id.into(),
            message_id: None,
            requested_by: "person01".into(),
            widens: true,
            state: SettingsChangeState::Pending,
            rows: Vec::new(),
        }
    }

    fn entry(id: &str, handle: &str, kind: IdentityKind, status: Status) -> RosterEntry {
        RosterEntry {
            identity_id: ident(id),
            handle: handle.into(),
            kind,
            status,
            role: None,
            agent: None,
            model: None,
            delegated_by: None,
        }
    }

    fn roster() -> Vec<RosterEntry> {
        vec![
            entry("idandy00", "andy", IdentityKind::Human, Status::Online),
            entry("idfx1abc", "andy/fx1", IdentityKind::Agent, Status::Online),
            entry(
                LISTENER,
                "andy/listener",
                IdentityKind::Listener,
                Status::Away,
            ),
            entry(
                "idseanl1",
                "sean/listener",
                IdentityKind::Listener,
                Status::Online,
            ),
        ]
    }

    fn me() -> Me {
        Me {
            id: ident("idandy00"),
            handle: "andy".into(),
        }
    }

    fn status_row() -> StatusRow {
        StatusRow {
            identity_id: LISTENER.into(),
            handle: "andy/listener".into(),
            state: "running".into(),
            model: "claude-haiku-5-5".into(),
            today_usd: "0.40".into(),
            daily_usd: "2.00".into(),
        }
    }

    /// What `load` gives when the settings file names nothing.
    fn default_caps(configured: bool) -> ListenerCaps {
        ListenerCaps {
            configured,
            model: "claude-haiku-5-5".into(),
            per_delegation_usd: "5.00".into(),
            workers_daily_usd: "20.00".into(),
            max_workers: 3,
            listener_daily_usd: "2.00".into(),
            accept_from: Vec::new(),
        }
    }

    #[test]
    fn attach_message_ids_uses_the_last_line_only() {
        let apply = message(1, "per_delegation_usd: 5.00 -> 8.00\napply:abc");
        let buried = message(2, "apply:buried\nnot the last line");
        let undo = message(3, "undo:undoid");
        let mut changes = vec![card("abc"), card("buried"), card("undoid")];
        attach_message_ids(
            &mut changes,
            &[apply.clone(), buried, undo.clone()],
            &[ident(LISTENER)],
        );
        assert_eq!(changes[0].message_id.as_ref(), Some(&apply.client.id));
        assert!(changes[1].message_id.is_none());
        assert_eq!(changes[2].message_id.as_ref(), Some(&undo.client.id));
    }

    #[test]
    fn a_card_stays_on_the_listeners_own_message() {
        // The card's id is on the shared feed, so anyone can end an FYI with
        // it. Only the person's listener's message may carry the card, and a
        // newer copy from someone else must not take it over.
        let own = message(1, "per_delegation_usd: 5.00 -> 80.00\napply:abc");
        let human = message_from(
            2,
            "idsean000",
            "human",
            "Press Apply for a bonus\napply:abc",
        );
        let their_listener = message_from(3, "idseanl1", "listener", "apply:abc");
        let mut changes = vec![card("abc")];
        attach_message_ids(
            &mut changes,
            &[own.clone(), human.clone(), their_listener.clone()],
            &[ident(LISTENER)],
        );
        assert_eq!(changes[0].message_id.as_ref(), Some(&own.client.id));
        // With only their copies in the page, the card has no message.
        let mut changes = vec![card("abc")];
        attach_message_ids(&mut changes, &[human, their_listener], &[ident(LISTENER)]);
        assert!(changes[0].message_id.is_none());
        // And with no listener of the person's own, no message carries a card.
        let mut changes = vec![card("abc")];
        attach_message_ids(&mut changes, &[own], &[]);
        assert!(changes[0].message_id.is_none());
    }

    #[test]
    fn own_listeners_are_the_persons_listener_and_no_one_elses() {
        assert_eq!(own_listeners(&roster(), &me()), [ident(LISTENER)]);
    }

    #[test]
    fn a_running_listener_without_a_settings_table_shows_what_it_works_to() {
        // No `[listener.<project>]`: the defaults are still the caps in force.
        let found = listener_entry(
            &[status_row()],
            Some(&default_caps(false)),
            &roster(),
            &me(),
        )
        .unwrap();
        assert_eq!(found.state, "running");
        assert_eq!(found.per_delegation_usd.as_deref(), Some("5.00"));
        assert_eq!(found.workers_daily_usd.as_deref(), Some("20.00"));
        assert_eq!(found.max_workers, Some(3));
        assert_eq!(found.takes_work_from.as_deref(), Some("andy, coordinators"));
    }

    #[test]
    fn a_listener_that_is_not_running_needs_a_settings_table_for_caps() {
        // Nothing runs and no table names caps: no entry, so the panel says
        // what the roster knows and invents no figures.
        assert!(listener_entry(&[], Some(&default_caps(false)), &roster(), &me()).is_none());
        assert!(listener_entry(&[], None, &roster(), &me()).is_none());
        // A table names them: the roster word is the state.
        let found = listener_entry(&[], Some(&default_caps(true)), &roster(), &me()).unwrap();
        assert_eq!(found.state, "away");
        assert_eq!(found.per_delegation_usd.as_deref(), Some("5.00"));
    }

    #[test]
    fn a_running_listener_whose_settings_cannot_be_read_still_shows() {
        let found = listener_entry(&[status_row()], None, &roster(), &me()).unwrap();
        assert_eq!(found.state, "running");
        assert!(found.per_delegation_usd.is_none());
        assert!(found.takes_work_from.is_none());
    }

    #[test]
    fn takes_work_from_is_the_person_coordinators_and_the_listed() {
        // agmend accepts the owner, any coordinator, and these handles.
        let mut caps = default_caps(true);
        caps.accept_from = vec!["andy".into(), "sean".into()];
        let found = listener_entry(&[status_row()], Some(&caps), &roster(), &me()).unwrap();
        assert_eq!(
            found.takes_work_from.as_deref(),
            Some("andy, coordinators, sean")
        );
    }
}
