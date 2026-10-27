# Desktop live data implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Connect the agmen desktop app to real data through agmend's local API, replacing the fixture for every panel that has a real source, so a person can read and post in their project's chat and see their roster, roles, worktrees and local agents live.

**Architecture:** The desktop talks only to agmend over the local API and holds no tokens or hub session (design.md "My calls": the desktop app section and the Secrets row). A new `agmen-local` crate holds the frame codec and the console types that agmend, the CLI and the desktop share. agmend reads the person's chat and roster from the hub over HTTP (server-sent events, replay, state) and posts as the person over HTTP, since a person has no broker identity. The desktop gains an `AgmendBackend` behind the existing `Backend` trait (ADR-0011); the fixture stays for `--screen` and tests.

**Tech Stack:** Rust 1.99.1, edition 2024; tokio and reqwest in agmend (as agmen-client); std `UnixStream` with no tokio in the desktop; serde_json; keyring 4.2.0 (agmend only); the existing gpui pin (ADR-0010).

**Spec:** [../design.md](../design.md) sections 6.3, 6.7, 6.10 and "My calls"; [../protocol.md](../protocol.md) sections 2 and 6; [../specs/2026-10-25-roles-listener-consoles-design.md](../specs/2026-10-25-roles-listener-consoles-design.md) sections 4, 5 and 9; [m0-m1.md](m0-m1.md) Task 13 (local API frames and base methods); [roles-listener.md](roles-listener.md) Tasks 5 and 6 (agmend's launcher, workers, activity, listener and settings).

## Preconditions and ownership

- Andy decided on 2026-10-26: the desktop goes through agmend, and this one plan covers both agmend's console side and the desktop. That lifts the read-only rule for `crates/agmend` and `crates/agmen-local`. Tasks 2 to 4 change `crates/agmen-hub`, and Andy must approve them separately before they start.
- Base: build on `mvp/impl` after `desktop/gpui` is merged into it. The desktop branch carries older copies of agmen-client, agmen-hub and agmen-cli (65 commits behind `mvp/impl`). Tasks that need `mvp/impl` code (`state.rs`, `/v1/me`, the CLI's login) cannot run on `desktop/gpui`.
- roles-listener Task 5 also creates `crates/agmend`. Whichever plan runs first creates the crate; the other extends it. This plan builds the socket, `hello`, `whoami`, the console methods and human posting. Task 5 adds `launch`, `workers`, `activity`, worktree creation and budgets, and Tasks 9 and 10 here wait for them.

## Global Constraints

- Everything in [mvp.md Global constraints](mvp.md#global-constraints), [m0-m1.md Global Constraints](m0-m1.md#global-constraints), and the desktop plan's Global Constraints ([desktop.md](desktop.md)).
- Socket: `$XDG_STATE_HOME/agmen/agmend.sock`; macOS `~/Library/Application Support/agmen/agmend.sock`; mode 0600 (roles-listener Global Constraints). Windows (S6) is out.
- Frames (m0-m1 Task 13): a 4-byte big-endian length, then UTF-8 JSON; request `{ "v": 1, "id": n, "method": str, "params": {...}, "as": identity_id? }`, response `{ "id": n, "result": ... }` or `{ "id": n, "error": { "code": str, "message": str } }`; the first frame is `hello {api: 1}`, and a mismatch answers `error.code = "version"`. This plan fixes the length prefix and a 16 MiB frame cap, which m0-m1 left to the builder.
- Requests without `as` act as the human (m0-m1 Task 13). The desktop never sends `as`.
- The desktop holds no tokens and no hub session (design "My calls"; keyring row "the desktop app holds none").
- Every loop, retry and buffer has a bound: replay pages of at most 500, at most 2000 messages held per project in the desktop, reconnect backoff 1, 2, 4 ... capped at 30 s.

## Review Focus

1. **agmend is not running.** The app opens to a clear "agmend isn't running" state with the command to start it, never the fixture's fake people. Test: Task 7 `live_without_agmend_says_so`.
2. **agmend restarts while the app is open.** The app reconnects, takes a fresh snapshot, and shows each message once. Test: Task 6 `reconnect_resnapshots_without_duplicates`.
3. **A socket that is not the person's own.** A socket file with mode other than 0600, or owned by another user, is refused on both sides. Tests: Task 5 `socket_is_0600` and Task 6 `refuses_foreign_socket`.
4. **Sending while disconnected.** Send fails with a visible error and the draft stays in the composer. Test: Task 6 `send_while_disconnected_keeps_the_draft`.
5. **A large backlog.** A project with 10,000 messages opens with the newest page, pages older ones on scroll-back, and never holds more than 2000. Tests: Task 5 `history_pages_at_500`, Task 6 `holds_at_most_2000`.

---

## Phase A: chat and roster

### Task 1: `agmen-local`: frames and console types

**Files:**
- Create: `crates/agmen-local/Cargo.toml`, `crates/agmen-local/src/{lib.rs,frame.rs,console.rs}`, `crates/agmen-local/tests/frame.rs`
- Modify: `Cargo.toml` (workspace member)

**Interfaces:**
- Produces:
  - `frame::write(w: &mut impl std::io::Write, value: &impl Serialize) -> Result<(), FrameError>` and `frame::read<T: DeserializeOwned>(r: &mut impl std::io::Read) -> Result<T, FrameError>`, blocking. A tokio variant sits behind feature `tokio` for agmend.
  - `pub const MAX_FRAME: u32 = 16 * 1024 * 1024`; `pub const API: u32 = 1`.
  - `Request { v, id, method, params, as_ }` (serde rename `as`), `Response { id, result | error }`, `ApiError { code, message }`, with codes `version`, `not_found`, `unauthorized`, `hub`, `bad_request`.
  - `console::ConsoleSnapshot { project: String, me: Me, roster: Vec<RosterEntry>, messages: Vec<StampedEnvelope>, older: bool }`, where `Me { id: IdentityId, handle: String }`. `RosterEntry` mirrors protocol 2's `state/roster` row: `{identity_id, handle, kind, role?, agent?, model?, status, delegated_by?}`.
  - `console::ConsoleEvent { Message(StampedEnvelope), Roster(Vec<RosterEntry>), Hub { connected: bool } }`.
- [ ] **Step 1: Write the failing tests:** `frame_round_trips`, `frame_over_cap_is_refused` (a length of `MAX_FRAME + 1` returns `FrameError::TooLarge` without reading the body), `truncated_frame_is_an_error`, `request_serializes_as_with_the_wire_name` (JSON has `"as"`), `console_snapshot_round_trips`.
- [ ] **Step 2:** Run `cargo nextest run -p agmen-local`. Expected: FAIL (crate missing).
- [ ] **Step 3:** Implement. The desktop uses the blocking functions only, so the default features pull in no tokio.
- [ ] **Step 4:** Run the tests and the gate. Expected: PASS.
- [ ] **Step 5:** Commit: `Add agmen-local with the local API frames and console types`

### Task 2: Hub: post as the human (needs Andy's approval for agmen-hub)

**Files:**
- Modify: `crates/agmen-hub/src/{http.rs,ingest.rs}`, plus a new `crates/agmen-hub/src/human_post.rs`
- Test: `crates/agmen-hub/tests/it_human_post.rs`

**Interfaces:**
- Produces: `POST /v1/projects/{project}/messages` (protocol section 6) with a `ClientEnvelope` body and a human bearer token. The hub stamps `from` as the human, runs the same ingest checks as MQTT (protocol 5.2 order: rate limit, schema and size, `valid_until`, sender, verb, secrets), and answers `{stream, seq}` or the protocol's NACK reason as JSON with a 4xx status.
- [ ] **Step 1: Write the failing it tests:** `it_human_post_is_stamped_as_the_human`, `it_human_post_runs_ingest_checks` (a 70 KiB body gets `too_large`, a secret gets `secret_detected`), `it_human_post_needs_membership` (another project's member gets 403).
- [ ] **Step 2:** Run `cargo nextest run -p agmen-hub --profile it`. Expected: FAIL (404).
- [ ] **Step 3:** Implement by reusing `admit` and the stamping path rather than a second ingest.
- [ ] **Step 4:** Run the it tests and the gate. Expected: PASS.
- [ ] **Step 5:** Commit: `Let a member post to the chat over HTTP as themselves`

### Task 3: Hub: server-sent events for the human (needs Andy's approval)

**Files:**
- Create: `crates/agmen-hub/src/events.rs`
- Modify: `crates/agmen-hub/src/http.rs`
- Test: `crates/agmen-hub/tests/it_events.rs`

**Interfaces:**
- Produces: `GET /v1/projects/{project}/events` (protocol section 6): server-sent events for the feed and the human's readable inboxes. Each event's id is `<stream>:<seq>` and its data is a `StampedEnvelope`. On reconnect the hub replays everything after `Last-Event-ID`, then goes live. A keep-alive comment every 15 s.
- [ ] **Step 1: Write the failing it tests:** `it_events_stream_live_messages`, `it_events_resume_after_last_event_id` (no gap and no repeat), `it_events_cover_owned_inboxes_only`.
- [ ] **Steps 2 to 5:** as Task 2. Commit: `Stream a member's chat to them as server-sent events`

### Task 4: Hub: read retained state over HTTP (needs Andy's approval)

**Files:**
- Modify: `crates/agmen-hub/src/{http.rs,presence.rs}`
- Test: `crates/agmen-hub/tests/it_state.rs`

**Interfaces:**
- Produces: `GET /v1/projects/{project}/state?key=roster|head` (protocol section 6), returning the retained JSON the hub publishes to `state/{key}`; members only.
- [ ] **Step 1: Write the failing it test:** `it_state_roster_matches_the_retained_topic`.
- [ ] **Steps 2 to 5:** as Task 2. Commit: `Serve retained project state over HTTP`

### Task 5: agmend: socket, `hello`, `whoami`, console methods

**Files:**
- Create (or extend, per Preconditions): `crates/agmend/src/{main.rs,api.rs,socket.rs,login.rs,console.rs}`, `crates/agmend/tests/{api.rs,console.rs}`

**Interfaces:**
- Consumes: Task 1 (frames, console types), Tasks 2 to 4 (hub routes), the CLI's login on `mvp/impl` (`login.json` in the config dir, tokens in the keychain under service `agmen`).
- Produces methods:
  - `hello {api}`
  - `whoami` returns `Me`
  - `console_snapshot {project}` returns `ConsoleSnapshot` (newest 500 messages, roster)
  - `chat_history {project, before: Seq, limit <= 500}` returns `{messages, older}`
  - `console_watch {project}` holds the connection and streams `ConsoleEvent` frames
  - `post {project, envelope}`, as the human, returns `{stream, seq}` or the hub's error as `error.code = "hub"`
- Behavior:
  - One SSE connection per watched project, shared by every console watching it.
  - The roster is re-read from `state?key=roster` every 15 s, and a `Roster` event is sent only when it changed (`shortcut:` until the hub streams state).
  - The lock file sits beside the socket, so one agmend runs per user.
- [ ] **Step 1: Write the failing tests** against an in-process agmend with a fake hub:
  - `socket_is_0600`
  - `second_agmend_exits` (the lock is held)
  - `hello_with_api_2_is_version_error`
  - `whoami_reads_the_cli_login`
  - `console_snapshot_has_the_newest_page_and_roster`
  - `history_pages_at_500`
  - `watch_streams_messages_in_seq_order`
  - `watch_sends_roster_only_on_change`
  - `post_goes_out_as_the_human`
  - `post_reports_the_hub_nack` (code `hub`, and the message names the reason)
- [ ] **Step 2:** Run `cargo nextest run -p agmend`. Expected: FAIL.
- [ ] **Step 3:** Implement on tokio. Each connection gets one task, and a `console_watch` connection serves nothing else.
- [ ] **Step 4:** Run the tests and the gate. Expected: PASS.
- [ ] **Step 5:** Commit: `Add agmend's socket and console methods for chat and roster`

### Task 6: Desktop: `AgmendBackend`

**Files:**
- Create: `crates/agmen-desktop/src/backend/agmend.rs`, `crates/agmen-desktop/src/backend/agmend_tests.rs`
- Modify: `crates/agmen-desktop/src/backend/mod.rs`, `Cargo.toml` (add `agmen-local` without tokio)

**Interfaces:**
- Consumes: Task 1, Task 5's methods. The `Backend` trait (ADR-0011) is unchanged.
- Produces:
  - `AgmendBackend::connect(socket: &Path, project: &str) -> Result<AgmendBackend, BackendError>`, with new variants `BackendError::Agmend(String)` and `BackendError::NotConnected`.
  - A reader thread owns the `console_watch` connection and pushes `Update`s into the `futures` channel that `subscribe` returns.
  - `send` and `act` use a second connection, request by request, on the background thread.
  - The snapshot is built in `fn snapshot_from(console: ConsoleSnapshot) -> Snapshot`:
    - `messages`, `me`, `me_id`, `project` come straight from the console snapshot.
    - The roster becomes `PersonGroup`s, grouped by person in `state/roster`'s order.
    - `listener_id` is the roster's `listener` kind.
    - Every field with no live source stays empty (Task 7 adds the empty states).
  - Messages are deduplicated by `(stream, seq)`, and at most 2000 are held.
  - `Action`s with no live source return `BackendError::Unsupported`.
- [ ] **Step 1: Write the failing tests** against a fake agmend on a temporary socket:
  - `snapshot_maps_roster_into_person_groups`
  - `watch_messages_arrive_as_updates`
  - `reconnect_resnapshots_without_duplicates` (the fake drops and restarts)
  - `send_posts_as_the_human`
  - `send_while_disconnected_keeps_the_draft` (Err, and the composer keeps its text)
  - `holds_at_most_2000`
  - `refuses_foreign_socket` (wrong mode or owner)
  - `unsupported_actions_say_so`
- [ ] **Step 2:** Run `cargo nextest run -p agmen-desktop agmend`. Expected: FAIL.
- [ ] **Step 3:** Implement with std `UnixStream` and threads (no tokio in the desktop).
  - Updates cross to the main thread through the existing `futures` channel.
  - Reconnect backoff follows Global Constraints.
- [ ] **Step 4:** Run the tests and the gate. Expected: PASS.
- [ ] **Step 5:** Commit: `Add the agmend backend for live chat and roster`

### Task 7: Desktop: live by default, connection and empty states

**Files:**
- Modify: `crates/agmen-desktop/src/{main.rs,backend/mod.rs}`, `src/ui/{status_bar.rs,listener_panel.rs,agent_panel.rs,project_panel.rs,git_panel.rs,roles_panel.rs}`, plus their tests
- Modify: `docs/adr/0011-desktop-backend-trait.md` (a Consequences line for the live backend)

**Interfaces:**
- Launch:
  - Plain `agmen-desktop` tries `AgmendBackend` for the project named by the current directory's `.agmen/identity.json`, or the last project in `~/.config/agmen/settings.toml`.
  - `--fixture` keeps today's behavior, and so does `--screen`.
  - If agmend can't be reached, the window opens to a "agmend isn't running" view with `agmend service install` and a Retry button.
- Snapshot:
  - `listener` becomes `Option<ListenerInfo>`.
  - The panels whose data is missing show plain empty states: "No listener for this project", "No local agents", "No worktrees yet".
  - None of these show fixture data.
- Status bar: shows connected, reconnecting or offline, from `ConsoleEvent::Hub` and the socket.
- [ ] **Step 1: Write the failing tests:**
  - `live_without_agmend_says_so`
  - `fixture_flag_keeps_the_fixture`
  - `empty_listener_panel_says_none`
  - `status_bar_shows_reconnecting`
- [ ] **Steps 2 to 4:** Red, implement, then green and the gate.
- [ ] **Step 5:** Shoot: `--screen chat` (fixture) next to a live window against the fake agmend. Compare the chat pane layout.
- [ ] **Step 6:** Commit: `Open the desktop on live data through agmend`

## Phase B: local data

### Task 8: agmend and desktop: roles, worktrees, git facts, file text

**Files:**
- Modify: `crates/agmend/src/console.rs`, `crates/agmen-local/src/console.rs`, `crates/agmen-desktop/src/backend/agmend.rs`
- Test: `crates/agmend/tests/local_data.rs`, `crates/agmen-desktop/src/backend/agmend_tests.rs`

**Interfaces:**
- `ConsoleSnapshot` gains:
  - `roles: Vec<RoleFileInfo>`: the built-in, project and personal layers, as spec 3.5.
  - `worktrees: Vec<WorktreeEntry>`: from `git worktree list --porcelain` of the project's checkout, with changes from `git status --porcelain=v2` and commits from `git log -n 20`.
  - `git: GitFactsEntry`: branches, recent commits and tracked files.
- New method `file_text {worktree, path}` returns `{text, marks}`.
  - It refuses a path that leaves the worktree root, including through a symlink (ADR-0011 C297).
  - It caps the answer at 512 KiB.
- The desktop maps these into `Snapshot.roles`, `worktrees` and `git`. FileView asks `file_text` when a file opens.
- [ ] **Step 1: Write the failing tests:**
  - `worktrees_list_branch_changes_and_commits` (a temp repo with two worktrees)
  - `file_text_refuses_symlink_escape`
  - `file_text_caps_at_512k`
  - `roles_list_three_layers`
  - Desktop: `file_view_reads_through_agmend`
- [ ] **Steps 2 to 5:** Red, implement, green and the gate, then commit. Commit: `Serve roles, worktrees and file text to the desktop`

## Phase C: local agents (after roles-listener Task 5)

### Task 9: Agent sessions, workers and budgets in the Agents and Agent panels

**Files:**
- Modify: `crates/agmend/src/console.rs`, `crates/agmen-local/src/console.rs`, `crates/agmen-desktop/src/backend/agmend.rs`

**Interfaces:**
- Consumes: roles-listener Task 5's `workers {project}` and `activity {identity_id, after?, follow}`.
- `ConsoleEvent` gains `Activity { identity_id, row }`, where row is a Tool, Out, Say or Cost line, mapped from stream-json in agmend.
- `ConsoleSnapshot` gains `agents: Vec<AgentEntry>`, carrying spend, budget, tokens, worktree, role enforcement and `tools_lossy`.
- The desktop fills `Snapshot.agents` and `sessions`.
- `Action::SpawnWorker{agent: "claude"}` maps to `launch`. Other agents return Unsupported until ACP (a later plan).
- [ ] **Step 1: Write the failing tests:**
  - `activity_rows_map_tool_out_say_cost`
  - `session_follows_new_rows`
  - `agent_panel_shows_spend_against_cap`
  - `spawn_worker_launches_claude`
  - `spawn_other_agent_is_unsupported`
- [ ] **Steps 2 to 5:** Red, implement, green and the gate, then commit. Commit: `Show local agents' sessions, workers and budgets live`

## Phase D: the listener and change cards (after roles-listener Task 6)

### Task 10: Listener panel and change cards

**Files:**
- Modify: `crates/agmend/src/{console.rs,listener/settings.rs}`, `crates/agmen-local/src/console.rs`, `crates/agmen-desktop/src/backend/agmend.rs`

**Interfaces:**
- Consumes: roles-listener Task 6's listener status and `settings_decide {id, apply}`.
- New method `settings_undo {id}`, because `settings_decide` takes only a bool and the plan's CLI has `agmen settings undo`.
- `ConsoleSnapshot` gains:
  - `listener: Option<ListenerEntry>`
  - `changes: Vec<ChangeEntry>`, keyed by the message id they belong to.
- The desktop maps `Action::Apply`, `Cancel` and `Undo` onto `settings_decide` and `settings_undo`.
- [ ] **Step 1: Write the failing tests:**
  - `apply_widens_through_settings_decide`
  - `cancel_leaves_settings_unchanged`
  - `undo_restores_the_before_values`
  - `listener_panel_shows_live_caps`
- [ ] **Steps 2 to 5:** Red, implement, green and the gate, then commit. Commit: `Drive the listener panel and change cards from agmend`

---

## Out of this plan

Each item stays on the fixture, or shows its empty state, until its own plan lands.

- Take-over and switching agents (spike S10's PTY, ACP).
- Agent scans and model lists for agents other than Claude.
- Per-message models: no wire field exists, and agmend would report them beside the message.
- Planning rooms: PLAN handling, share approval, kick-off, spec text at a commit.
- Urgent-alert routing beyond what the ALERT envelope already carries. The desktop's alert logic reads envelopes, so it works on live messages once Task 6 lands.
- The GitHub App, for merges and PR cards.
- Windows local IPC (S6).
