//! A fake agmend for tests: a std `UnixListener` in a temporary folder
//! that speaks the local API's frames (Task 6), with the roster and chat
//! the backend's and the window's tests open on (Task 7).

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agmen_local::console::{
    ConsoleEvent, ConsoleSnapshot, GitFactsEntry, Me, RoleFileInfo, RoleLayer, RosterEntry, Status,
    WorktreeEntry,
};
use agmen_local::{ErrorCode, Request, Response, frame};
use agmen_proto::{IdentityId, IdentityKind, StampedEnvelope};
use serde_json::{Value, json};

/// The project the fake's snapshots name.
pub(crate) const PROJECT: &str = "pr-3331";

pub(crate) fn ident(id: &str) -> IdentityId {
    id.parse().unwrap()
}

fn entry(id: &str, handle: &str, kind: IdentityKind, status: Status) -> RosterEntry {
    RosterEntry {
        identity_id: ident(id),
        handle: handle.into(),
        kind,
        status,
        agent: None,
        model: None,
        delegated_by: None,
    }
}

/// The roster as the hub orders it: by handle.
pub(crate) fn roster() -> Vec<RosterEntry> {
    vec![
        entry("idandy00", "andy", IdentityKind::Human, Status::Online),
        RosterEntry {
            agent: Some("claude-code/2.1.310".into()),
            delegated_by: Some(ident("idlisten")),
            ..entry("idfx1abc", "andy/fx1", IdentityKind::Agent, Status::Online)
        },
        RosterEntry {
            agent: Some("codex".into()),
            ..entry(
                "idlisten",
                "andy/listen",
                IdentityKind::Listener,
                Status::Away,
            )
        },
        entry("idsean00", "sean", IdentityKind::Human, Status::Offline),
        entry(
            "idseanl1",
            "sean/listen",
            IdentityKind::Listener,
            Status::Online,
        ),
        entry("idseanp1", "sean/plan", IdentityKind::Agent, Status::Online),
    ]
}

/// A feed message from sean with `seq`, as the hub stamps it.
pub(crate) fn message(seq: u64) -> StampedEnvelope {
    message_with(seq, "FYI")
}

pub(crate) fn message_with(seq: u64, verb: &str) -> StampedEnvelope {
    let data = if verb == "ASK" {
        json!({"urgency": "normal"})
    } else {
        json!({})
    };
    serde_json::from_value(json!({
        "v": 1,
        "id": format!("01JBX6Q4T7W3C2M8Y9K{seq:07}"),
        "body": format!("message {seq}"),
        "sent_at": "2026-10-25T22:00:00Z",
        "verb": verb,
        "data": data,
        "stream": "feed",
        "seq": seq,
        "from": "idsean00",
        "from_handle": "sean",
        "from_kind": "human",
        "hub_at": "2026-10-25T22:00:00Z",
    }))
    .unwrap()
}

pub(crate) fn console(messages: Vec<StampedEnvelope>) -> ConsoleSnapshot {
    ConsoleSnapshot {
        project: PROJECT.into(),
        me: Me {
            id: ident("idandy00"),
            handle: "andy".into(),
        },
        roster: roster(),
        messages,
        older: false,
        roles: Vec::new(),
        worktrees: Vec::new(),
        git: GitFactsEntry::default(),
    }
}

/// The checkout root the window found, as Task 8's tests name it.
pub(crate) const ROOT: &str = "/src/agmen";

/// Task 8: a snapshot with agmend's local data: the built-in roles, two
/// worktrees of [`ROOT`] (fx1's, and a detached one with no identity
/// file) and their git facts.
pub(crate) fn console_with_checkout(messages: Vec<StampedEnvelope>) -> ConsoleSnapshot {
    let worktrees: Vec<WorktreeEntry> = serde_json::from_value(json!([
        {
            "path": ROOT,
            "identity": {"id": "idfx1abc", "handle": "andy/fx1"},
            "head": {"kind": "branch", "name": "main"},
            "upstream": {"ahead": 1, "behind": 0},
            "files": ["src/main.rs"],
            "changes": [{"path": "src/main.rs", "kind": "modified", "added": 1, "removed": 0,
                         "hunks": [{"old_start": 1, "new_start": 1, "lines": [
                             {"kind": "context", "text": "fn live() {}"},
                             {"kind": "added", "text": "fn more() {}"}]}]}],
            "commits": [{"sha": "9e2a71c0000000000000000000000000000000aa", "short": "9e2a71c",
                         "subject": "Resolve mod list", "author": "Andy",
                         "at": "2026-10-25T21:31:00Z"}],
        },
        {
            "path": "/src/review",
            "head": {"kind": "detached", "sha": "4d9fdc8000000000000000000000000000000000"},
            "files": ["src/main.rs"],
            "changes": [],
            "commits": [],
        },
    ]))
    .unwrap();
    ConsoleSnapshot {
        roles: agmen_local::roles::BUILT_IN
            .iter()
            .map(|(name, text)| RoleFileInfo {
                name: (*name).into(),
                layer: RoleLayer::BuiltIn,
                text: (*text).into(),
            })
            .collect(),
        worktrees,
        git: GitFactsEntry {
            branches: vec!["main".into()],
            commits: vec!["9e2a71c0000000000000000000000000000000aa".into()],
            files: vec!["src/main.rs".into()],
        },
        ..console(messages)
    }
}

// The fake agmend: a std UnixListener that speaks the local API's frames.

#[derive(Debug, Default)]
pub(crate) struct FakeState {
    pub(crate) snapshot: Option<ConsoleSnapshot>,
    /// Sent to every watch before a snapshot is answered, as if they
    /// arrived while agmend read the hub.
    pub(crate) during_snapshot: Vec<ConsoleEvent>,
    /// What a watch hears first.
    pub(crate) opening: Vec<ConsoleEvent>,
    pub(crate) post_error: Option<String>,
    /// Final review fix 2: `console_watch` answers this error and closes,
    /// as agmend does for a person signed out or a project they lost.
    pub(crate) watch_error: Option<(ErrorCode, String)>,
    /// How long each `console_snapshot` waits before it answers, as a
    /// slow hub makes agmend wait.
    pub(crate) snapshot_delay: Duration,
    pub(crate) posts: Vec<Request>,
    pub(crate) snapshots_served: usize,
    /// Task 8: each `console_snapshot`'s params.
    pub(crate) snapshot_params: Vec<Value>,
    /// Final review fix 1: the whole feed `chat_history` pages through,
    /// oldest first; empty answers every page with nothing.
    pub(crate) history: Vec<StampedEnvelope>,
    /// Final review fix 1: each `chat_history`'s params.
    pub(crate) history_params: Vec<Value>,
    /// Task 8: each `file_text`'s params.
    pub(crate) file_text_params: Vec<Value>,
    /// Task 8 review fix 3: each `console_watch`'s params.
    pub(crate) watch_params: Vec<Value>,
    /// Each watch's connection and its request id.
    pub(crate) watchers: Vec<(UnixStream, u64)>,
    /// Every accepted connection, to drop on stop.
    pub(crate) streams: Vec<UnixStream>,
}

#[derive(Debug)]
pub(crate) struct FakeAgmend {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) state: Arc<Mutex<FakeState>>,
    stop: Arc<AtomicBool>,
}

impl FakeAgmend {
    pub(crate) fn start(snapshot: ConsoleSnapshot) -> Self {
        let state = FakeState {
            snapshot: Some(snapshot),
            opening: vec![
                ConsoleEvent::Hub {
                    connected: false,
                    error: None,
                },
                ConsoleEvent::Hub {
                    connected: true,
                    error: None,
                },
            ],
            ..FakeState::default()
        };
        let fake = Self {
            dir: tempfile::tempdir().unwrap(),
            state: Arc::new(Mutex::new(state)),
            stop: Arc::new(AtomicBool::new(false)),
        };
        fake.listen(0o600);
        fake
    }

    /// Final review fix 1: a project whose feed holds messages 1 to `n`;
    /// snapshots carry the newest 500, as agmend's do.
    pub(crate) fn with_history(n: u64) -> Self {
        let all: Vec<StampedEnvelope> = (1..=n).map(message).collect();
        let newest = all[all.len().saturating_sub(500)..].to_vec();
        let fake = Self::start(ConsoleSnapshot {
            older: n > 500,
            ..console(newest)
        });
        fake.edit(|s| s.history = all);
        fake
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.dir.path().join("agmend.sock")
    }

    fn listen(&self, mode: u32) {
        let path = self.path();
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let state = self.state.clone();
        let stop = self.stop.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { break };
                state
                    .lock()
                    .unwrap()
                    .streams
                    .push(stream.try_clone().unwrap());
                let state = state.clone();
                std::thread::spawn(move || serve(stream, &state));
            }
        });
    }

    /// agmend exits: the socket goes, and every connection drops.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accept loop, which then sees the flag.
        drop(UnixStream::connect(self.path()));
        std::fs::remove_file(self.path()).unwrap();
        let mut state = self.state.lock().unwrap();
        state.watchers.clear();
        for stream in state.streams.drain(..) {
            drop(stream.shutdown(std::net::Shutdown::Both));
        }
    }

    /// agmend starts again on the same socket.
    pub(crate) fn restart(&mut self) {
        self.stop = Arc::new(AtomicBool::new(false));
        self.listen(0o600);
    }

    pub(crate) fn edit(&self, change: impl FnOnce(&mut FakeState)) {
        change(&mut self.state.lock().unwrap());
    }

    pub(crate) fn push(&self, event: &ConsoleEvent) {
        push(&mut self.state.lock().unwrap(), event);
    }

    /// Final review fix 2: ends every watch with an error frame, as agmend
    /// does for a console that fell behind.
    pub(crate) fn end_watches(&self, code: ErrorCode, message: &str) {
        let mut state = self.state.lock().unwrap();
        for (mut stream, id) in state.watchers.drain(..) {
            drop(frame::write(&mut stream, &Response::err(id, code, message)));
            drop(stream.shutdown(std::net::Shutdown::Both));
        }
    }

    pub(crate) fn posts(&self) -> Vec<Request> {
        self.state.lock().unwrap().posts.clone()
    }
}

fn push(state: &mut FakeState, event: &ConsoleEvent) {
    let value = serde_json::to_value(event).unwrap();
    state
        .watchers
        .retain_mut(|(stream, id)| frame::write(stream, &Response::ok(*id, value.clone())).is_ok());
}

fn serve(mut stream: UnixStream, state: &Mutex<FakeState>) {
    while let Ok(request) = frame::read::<Request>(&mut stream) {
        let id = request.id;
        let answer = match request.method.as_str() {
            "hello" => Response::ok(id, json!({"api": 1})),
            "whoami" => Response::ok(id, json!({"id": "idandy00", "handle": "andy"})),
            "console_snapshot" => {
                let delay = state.lock().unwrap().snapshot_delay;
                std::thread::sleep(delay);
                let shared = state;
                let mut state = shared.lock().unwrap();
                state.snapshots_served += 1;
                state.snapshot_params.push(request.params.clone());
                let early = state.during_snapshot.clone();
                for event in &early {
                    push(&mut state, event);
                }
                if !early.is_empty() {
                    // The watch reads these before this answer lands.
                    drop(state);
                    std::thread::sleep(Duration::from_millis(100));
                    state = shared.lock().unwrap();
                }
                match &state.snapshot {
                    Some(snapshot) => Response::ok(id, serde_json::to_value(snapshot).unwrap()),
                    None => Response::err(id, ErrorCode::Hub, "the hub cannot be reached"),
                }
            }
            "post" => {
                let mut state = state.lock().unwrap();
                state.posts.push(request.clone());
                match &state.post_error {
                    Some(why) => Response::err(id, ErrorCode::Hub, why.clone()),
                    None => Response::ok(id, json!({"stream": "feed", "seq": 99})),
                }
            }
            // Final review fix 1: up to `limit` messages before `before`,
            // as agmend's console.rs answers.
            "chat_history" => {
                let mut state = state.lock().unwrap();
                state.history_params.push(request.params.clone());
                let before = request.params["before"].as_u64().unwrap_or(u64::MAX);
                let limit = request.params["limit"].as_u64().unwrap_or(500);
                let earlier: Vec<&StampedEnvelope> =
                    state.history.iter().filter(|m| m.seq.0 < before).collect();
                let skip = earlier
                    .len()
                    .saturating_sub(usize::try_from(limit).unwrap());
                let page: Vec<&StampedEnvelope> = earlier[skip..].to_vec();
                let older = page.first().is_some_and(|m| m.seq.0 > 1);
                Response::ok(id, json!({"messages": page, "older": older}))
            }
            // Task 8: the fixture's main.rs, with a second line added.
            "file_text" => {
                state
                    .lock()
                    .unwrap()
                    .file_text_params
                    .push(request.params.clone());
                Response::ok(
                    id,
                    json!({"text": "fn live() {}\nfn more() {}", "truncated": false,
                           "marks": [{"kind": "added", "from": 2, "to": 3}]}),
                )
            }
            "console_watch" => {
                let mut state = state.lock().unwrap();
                state.watch_params.push(request.params.clone());
                if let Some((code, why)) = state.watch_error.clone() {
                    drop(frame::write(&mut stream, &Response::err(id, code, why)));
                    return;
                }
                let mut watch = stream.try_clone().unwrap();
                for event in state.opening.clone() {
                    let value = serde_json::to_value(&event).unwrap();
                    frame::write(&mut watch, &Response::ok(id, value)).unwrap();
                }
                state.watchers.push((watch, id));
                drop(state);
                // A watch takes no more requests; any read ends it.
                drop(frame::read::<Value>(&mut stream));
                return;
            }
            other => Response::err(id, ErrorCode::NotFound, format!("no method {other}")),
        };
        if frame::write(&mut stream, &answer).is_err() {
            return;
        }
    }
}
