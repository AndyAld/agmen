# agmend local API and OS service implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete agmend's Unix service and local API for identities it runs, with caller binding, inbox reads, watches, retained state and a durable pending queue.

**Architecture:** L7 adds the service module to the binary, leaving the library to L10. L10 extends the existing local API and reuses agmen-client sessions, outboxes and token storage. A separate agmend SQLite file commits pending messages and receive positions together, so delivery does not depend on a desktop watch being open.

**Tech Stack:** Rust edition 2024 and MSRV 1.99.1 (`Cargo.toml:20` to 21); existing tokio, reqwest and serde_json dependencies (`crates/agmend/Cargo.toml:21` to 30); approved rusqlite 0.39.0 with `bundled` (`Cargo.lock:4546` to 4547; existing usage at `crates/agmen-client/Cargo.toml:21`). Use agmen-client's TokenStore rather than introducing keyring 4.2.0: the current implementation uses keyring-core and native stores (`crates/agmen-client/Cargo.toml:18`, `crates/agmen-client/Cargo.toml:36` to 40).

**Spec:** [../design.md](../design.md) sections 6.3 and 6.10 and "My calls"; [../protocol.md](../protocol.md) section 6; [desktop-live.md](desktop-live.md) Tasks 5 and 8, as built. This replaces [m0-m1.md](m0-m1.md) Task 13 for execution in D17's scope. The source extracts are `/Users/andyaldrich/Projects/agmen/.worktrees/desktop-live/.superpowers/sdd/program/l2p2/{m0-m1-task13.md,l10-lane.md}` and `../l7/lane.md` relative to that directory.

## Preconditions and ownership

- Execute Part A as L7 T1 to T4. Execute Part B as L10 T1, T4, T2, T5, T3, in that order. Task numbers below preserve those lane identifiers.
- L7 owns `main.rs`, `service/{mod.rs,launchd.rs,systemd.rs}`, `tests/cli.rs` and the desktop offline wording and test. Declare `mod service;` from `main.rs`. L7 must leave `lib.rs` untouched (D18).
- L10 owns `api.rs`, `lib.rs`, `Cargo.toml`, `hub.rs`, `login.rs`, new `identities.rs` and `state.rs`, `tests/api.rs`, `tests/support/mod.rs` and `agmen-local/src/message.rs`. Tests embedded in the new modules stay with their owner. A dependency edit also updates Cargo.lock's agmend dependency list.
- B T3 waits for M3a to be merged into `mvp/impl` and integrated into the execution checkout. That merge is an integration prerequisite, not a command in this plan. It must supply the roles mint body and the shared EditScope type. D14 places EditScope in `agmen-proto/src/roles.rs`; this checkout does not yet declare that module (`crates/agmen-proto/src/lib.rs:39` to 44). Do not create a competing type here.
- The current mint handler accepts only handle, project and kind and denies unknown fields (`crates/agmen-hub/src/identities.rs:140` to 149). B T3 must not ship against it or silently retry with a smaller body.
- Already built: hello, whoami, console_snapshot, chat_history, human post and file_text dispatch (`crates/agmend/src/api.rs:97` to 109); console_watch takes its own connection (`crates/agmend/src/api.rs:47` to 53). Task 8's local roles, worktrees and git data are included in snapshots (`crates/agmend/src/console.rs:194` to 225), and file_text delegates to the local reader (`crates/agmend/src/console.rs:280` to 287). Extend these paths without rebuilding them.

## Global Constraints

- D14: add `Forbidden` and `StaleRequest` to agmen-local ErrorCode. Keep `#[serde(other)] Unknown` last (`crates/agmen-local/src/message.rs:42` to 57). StaleRequest is a local error, never a wire NackReason. This overrides roles-listener.md's proposed wire variant (`docs/plans/roles-listener.md:86`). Do not invent a stale check on unrelated read methods.
- D17: devices are deferred. The rusqlite 0.39.0 bundled edge is approved. Keep the daemon database separate from the identity database named `agmen.db` (`crates/agmen-client/src/session.rs:44` to 45). Store it as `agmend.db` beside the socket. Reuse the client's WAL, FULL synchronous and 5 s busy-timeout pattern (`crates/agmen-client/src/db.rs:62` to 72), without changing the client's schema.
- D18: service files are `~/Library/LaunchAgents/dev.agmen.agmend.plist` and `~/.config/systemd/user/agmend.service`. Replace the negative CLI test at `crates/agmend/tests/cli.rs:37` to 46 and the old offline copy assertions at `crates/agmen-desktop/src/ui/live_root_tests.rs:74` to 81. Those behavior changes are deliberate and authorized by this plan.
- D36: Windows S6 is out. Put `// shortcut: Unix-only service; revisit when S6 Windows support is approved.` on service platform selection, and `// shortcut: Unix-only local API; revisit when S6 Windows IPC is approved.` at the API transport boundary. No windows.rs and no named-pipe work.
- Keep the existing four-byte frame prefix and 16 MiB cap (`crates/agmen-local/src/frame.rs:43` to 68; `crates/agmen-local/src/lib.rs:17`). Keep hello first, version errors and response ids (`crates/agmend/src/api.rs:42` to 46, `crates/agmend/src/api.rs:64` to 95). Preserve the human post result `{stream,seq}` (`crates/agmend/src/console.rs:170` to 174, `crates/agmend/src/console.rs:291` to 295).
- Requests without `as` remain human calls. New identity calls use the existing top-level `as` field (`crates/agmen-local/src/message.rs:20` to 22) plus `params.local_token`. Remove that credential before decoding method params. Do not add a public Request field or log the credential. Credentials are per request, never inherited from the previous request on a connection.
- Distinguish the human login token, identity hub token, short-lived broker token and local caller token. Only the local token goes to an agent at mint. Hub tokens use `TokenKey::new(actual_hub_origin, identity_id)` and TokenStore (`crates/agmen-client/src/token_store.rs:81` to 86, `crates/agmen-client/src/token_store.rs:156`, `crates/agmen-client/src/token_store.rs:179`). Broker refresh stays in HubClient and Session (`crates/agmen-client/src/hub.rs:142` to 146; `crates/agmen-client/src/token.rs:20` to 31).
- Proposed local-token namespace: use the same TokenStore with `TokenKey::new(&Url::parse("https://agmend.invalid")?, identity_id)` for local credentials. This reserved origin is a storage namespace, never a network destination. The stored local value is JSON `{origin, owner_id, local_token}`; parse it and check all three against the current login and caller. Refuse to overwrite a local entry from another origin or owner. This separates local and hub credentials without changing TokenStore's API or raw hub-token entries. Mint returns `{identity_id, local_token}` once; identity_list returns no credentials. Record this choice for review.
- Generate 32 random bytes and encode 64 lowercase hex characters for a local token. Add the already used getrandom 0.4.3 dependency (`crates/agmen-client/Cargo.toml:16`); its cached source exports `fill(&mut [u8]) -> Result<(), Error>` at `getrandom-0.4.3/src/lib.rs:87`. The stdlib has no cryptographic token API used here. Keep token values out of Debug, errors and database rows. Compare all 64 bytes without early exit after checking the length.
- Bound new work: at most 64 managed identities and 64 managed projects; at most 256 durable stream positions per project; 256 queued events per watcher; 64 concurrent local connections, preserving `crates/agmend/src/lib.rs:44` to 45. Cap every new JSON HTTP body at 16 MiB, each message at the protocol envelope cap, replay and drain pages at 500, identity lists at 64 and pending rows at 10,000 per identity. A full pending queue stops ingestion before advancing its durable position; it never discards undelivered rows.
- New finite operations have 10 s deadlines. Watch writes have a 10 s deadline and close on overflow. Streaming tasks end on daemon shutdown, sign-out, identity removal, quota failure or connection loss; reconnect at most 10 times per run with delays 1, 2, 4, 8, 16, then 30 s, then report offline. A fresh explicit call or daemon restart starts a new run. Stream one event at a time with no unbounded collect. Process at most 500 replay rows per iteration and 100 pages per catch-up run, persisting progress before yielding or reporting incomplete catch-up.
- New SSE ingestion reuses SseParser's 256 KiB line and event caps (`crates/agmend/src/sse.rs:11` to 13). Feed it chunks of at most 16 KiB so the event vector per push is also bounded. Existing HTTP connect, request and silence timeouts are 10 s, 10 s and 45 s (`crates/agmend/src/hub.rs:25` to 33). Existing unrelated console behavior stays compatible.
- Service tests inject unit destinations and a process runner. They never load a real LaunchAgent or systemd unit. Any real LaunchAgent load waits for Andy. No test modifies the user's service files, keychain, clipboard or running services.
- Every task's gate means all six commands at `README.md:336` to 341: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo nextest run --workspace`; `cargo test --doc --workspace`; `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`; `cargo deny check`. Run the narrow test after an edit, then this full gate before declaring the task done. Commit each task with the one-line message in its last step, with no trailers.

## Review Focus

1. **A sibling identity's credential.** `post_requires_identity_token` and `sibling_token_is_unauthorized` require unauthorized and no publish. `agent_cannot_mint` requires forbidden and zero HTTP mint calls.
2. **A restart between arrival and drain.** `pending_survives_restart` requires one delivery after reopening the same database. `pending_replays_after_cursor_crash_window` proves replay repairs a crash after a client cursor moved but before the daemon committed.
3. **No desktop watch is open.** `pending_collects_without_watch_clients` requires mentions, DMs and ASSIGNs to enter the managed identity's queue anyway. `pending_collects_mentions_and_dms` checks the qualifying set and a second empty drain.
4. **Service management fails.** `service_install_uses_injected_paths_and_runner`, `service_failure_keeps_recoverable_unit` and `service_status_reports_three_states` check exact files, command order, failure and status. No real service is touched.
5. **Boundaries and compatibility.** `identity_reads_are_scoped`, `pending_frame_cap_keeps_undelivered_tail`, `watch_slow_reader_closes` and `local_error_codes_round_trip` check isolation, bounds and additive errors. Keep `live_without_agmend_says_so` and both desktop refusal tests passing.

---

## Part A: service (L7)

### Task A T1: Parse foreground and service commands

**Files:**
- Modify: `crates/agmend/src/main.rs:11` to 25
- Test: `crates/agmend/tests/cli.rs:37` to 54 and inline parser tests in `main.rs`

**Interfaces:**
- Consumes: current argument handling, which only examines the first argument (`crates/agmend/src/main.rs:14` to 24).
- Produces: binary-private `enum Command { Foreground, Help, Service(ServiceAction) }`, `enum ServiceAction { Install, Uninstall, Status }`, and `fn parse(args: &[std::ffi::OsString]) -> Result<Command, String>`.
- Produces: CLI forms: `agmend`, `agmend --help`, `agmend -h`, `agmend service install`, `agmend service uninstall`, `agmend service status`. Any other complete argument list exits 2 with usage. Parse at most four arguments, including one extra to reject excess input; never allocate from an unbounded iterator.

- [ ] **Step 1: Write the failing tests:** `service_commands_parse` checks all three actions; `foreground_and_help_parse` checks the empty list and both help flags; `extra_and_non_utf8_arguments_are_refused` checks `--help extra`, `service`, `service restart`, `service install extra`, unknown and non-UTF-8 arguments. Replace the old CLI assertion with a genuinely unknown action such as `service frobnicate`, still requiring exit 2 and usage. The old test explicitly expects install to fail (`crates/agmend/tests/cli.rs:38` to 45).
- [ ] **Step 2:** Run `cargo nextest run -p agmend`. Expected: FAIL on the missing parser. No test invokes a real service action.
- [ ] **Step 3:** Implement parsing before logging, TLS or runtime startup. Keep the current foreground path (`crates/agmend/src/main.rs:61` to 73). Add parser usage for the service forms. Until A T3 wires execution, a parsed service command must return an explicit unavailable error with `// shortcut: service execution waits for A T3 in this lane; remove before handoff.`, never start the daemon.
- [ ] **Step 4:** Run `cargo nextest run -p agmend`, then the gate. Expected: PASS. A T3 replaces the temporary unavailable branch in this same lane before handoff.
- [ ] **Step 5:** Commit: `Parse agmend foreground and service commands`

### Task A T2: Render Unix unit files behind a test seam

**Files:**
- Create: `crates/agmend/src/service/{mod.rs,launchd.rs,systemd.rs}`
- Modify: `crates/agmend/src/main.rs` (module declaration and ServiceAction import)
- Test: inline tests in the three new service modules

**Interfaces:**
- Consumes: A T1's ServiceAction and `std::env::current_exe()` for the absolute installed binary path.
- Produces: `struct ServicePaths { home: PathBuf, executable: PathBuf, uid: u32 }`, `struct Unit { path: PathBuf, contents: String }`, `fn launchd::render(paths: &ServicePaths) -> anyhow::Result<Unit>`, `fn systemd::render(paths: &ServicePaths) -> anyhow::Result<Unit>`.
- Produces: launchd Label `dev.agmen.agmend`, one absolute ProgramArguments entry, RunAtLoad=true and KeepAlive=false. Produces systemd `[Service]` with Type=simple, escaped absolute ExecStart and Restart=no, plus `[Install]` WantedBy=default.target. These units start at login without automatic crash retries; status and an explicit install handle recovery. Both invoke plain agmend with no extra daemon argument. Bound rendered content to 64 KiB; refuse relative executable paths, NUL and line breaks.

- [ ] **Step 1: Write the failing tests:** `launchd_renders_absolute_binary` checks label, executable, RunAtLoad=true, KeepAlive=false and the exact LaunchAgents path; `launchd_escapes_xml` checks ampersand, quotes and angle brackets; `systemd_renders_user_unit` checks the exact user path, ExecStart, restart settings and install target; `systemd_escapes_exec_path` checks spaces, quotes, backslashes, percent and dollar characters; `service_render_refuses_invalid_paths` checks relative paths, NUL, newlines and the cap. Compare parsed or exact output, never run it.
- [ ] **Step 2:** Run `cargo nextest run -p agmend`. Expected: FAIL because service renderers do not exist.
- [ ] **Step 3:** Implement pure renderers with std string escaping. Use the installed launchd.plist manual and the official [systemd command rules](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html) and [quoting rules](https://www.freedesktop.org/software/systemd/man/latest/systemd.syntax.html). Keep both renderers available to unit tests on either Unix host, and select only the native backend in production. Add D36's shortcut comment, and `// shortcut: no automatic restart after a crash; revisit when Andy chooses a bounded restart policy.` beside each restart setting. Avoid a template dependency for these two short files.
- [ ] **Step 4:** Run `cargo nextest run -p agmend`, then the gate. Expected: PASS; rendering tests perform no writes.
- [ ] **Step 5:** Commit: `Render launchd and systemd user units for agmend`

### Task A T3: Install, uninstall and report service status

**Files:**
- Modify: `crates/agmend/src/main.rs`, `crates/agmend/src/service/{mod.rs,launchd.rs,systemd.rs}`
- Test: inline service tests; retain CLI timeout coverage in `crates/agmend/tests/cli.rs:8` to 34

**Interfaces:**
- Consumes: A T1's action and A T2's Unit and ServicePaths. launchctl's installed manual defines bootstrap, bootout and print (`/usr/share/man/man1/launchctl.1:90` to 104, `/usr/share/man/man1/launchctl.1:261`). The official [systemctl manual](https://www.freedesktop.org/software/systemd/man/latest/systemctl.html) documents --user, daemon-reload, enable, start, disable, stop and show.
- Produces: `struct ProcessResult { code: Option<i32>, stdout: Vec<u8>, stderr: Vec<u8> }`; `fn manage(action: ServiceAction, paths: &ServicePaths, run: &mut dyn FnMut(&[OsString]) -> anyhow::Result<ProcessResult>) -> anyhow::Result<String>`; `fn run_process(argv: &[OsString]) -> anyhow::Result<ProcessResult>`.
- Produces: exact manager argv, with `<uid>` from `ServicePaths.uid` and `<unit>` the rendered unit path. launchd: `launchctl print gui/<uid>/dev.agmen.agmend`; `launchctl bootout gui/<uid>/dev.agmen.agmend`; `launchctl bootstrap gui/<uid> <unit>`. systemd, each prefixed `systemctl --user`: `daemon-reload`; `enable agmend.service`; `start agmend.service`; `disable agmend.service`; `stop agmend.service`; `show agmend.service --property=LoadState --property=ActiveState`.
- Produces: status rules from one manager call. launchd: `print` exit 0 with a `state = running` line is `running`; exit 0 without it is `stopped`; exit 113 (launchctl's not-found code, checked on this macOS host) is `stopped` if the unit file exists and `not installed` if it does not; any other exit is a manager error. systemd: `LoadState=not-found` is `not installed`; `ActiveState=active` is `running`; any other `ActiveState` is `stopped`; a nonzero exit is a manager error.
- Produces: status output ending in a newline: `running`, `stopped` or `not installed`. Return exit 0 for these known states. Manager errors return exit 1 with context; bad syntax still returns 2. Do not treat permission or bus failures as stopped.

- [ ] **Step 1: Write the failing tests:** `service_install_uses_injected_paths_and_runner` writes only into a temp home, checks exact unit content and command order; `service_reinstall_is_idempotent` repeats install without duplicate registration; `service_uninstall_is_idempotent` removes only the managed unit and succeeds when absent; `service_status_reports_three_states` distinguishes absent, loaded but stopped and running; `service_failure_keeps_recoverable_unit` injects a load failure and requires a retained unit and contextual error; `service_status_manager_error_is_an_error` injects permission/bus failure; `service_runner_timeout_reaps_child` and `service_runner_caps_output` test a harmless child in a temp directory, with a 10 s deadline and 64 KiB per output stream.
- [ ] **Step 2:** Run `cargo nextest run -p agmend`. Expected: FAIL because manage is absent.
- [ ] **Step 3:** Implement native selection in the binary. Install writes the rendered unit via a private temporary file in its destination directory and renames once complete. Keep a preexisting unit on write failure. For launchd, inspect `gui/<uid>/dev.agmen.agmend`, bootout the existing service if loaded, then bootstrap `gui/<uid>` with the unit path. For systemd, run user daemon-reload, enable and start the unit; uninstall disables and stops it before deleting and reloading. Implement exact argv and manager-result interpretation from the platform manuals. Status is read-only. Do not delete the unit if stopping it fails. Reinstall uses at most four manager calls, uninstall at most three, status one; no retries.
- [ ] **Step 4:** Replace A T1's temporary unavailable branch with manage before foreground runtime startup. The production runner uses no shell, closes stdin, reads both pipes concurrently, caps each at 64 KiB, polls at 20 ms and kills and reaps on timeout or output overflow. Run `cargo nextest run -p agmend`, then the gate. Expected: PASS. Tests inject a fake runner for every manager operation. Do not run a real install without Andy.
- [ ] **Step 5:** Commit: `Manage agmend as a Unix user service`

### Task A T4: Offer service installation in the desktop offline view

**Files:**
- Modify: `crates/agmen-desktop/src/ui/live_root.rs:108` to 119
- Test: `crates/agmen-desktop/src/ui/live_root_tests.rs:54` to 105

**Interfaces:**
- Consumes: A T3's completed `agmend service install` command and the existing Wording view (`crates/agmen-desktop/src/ui/live_root.rs:108` to 119).
- Produces: `Wording.command == Some("agmend service install")`, `Wording.note == None` for AgmendDown. Keep Retry and the existing view state transitions.

- [ ] **Step 1: Write the failing test:** update `live_without_agmend_says_so` to assert the new exact command, no foreground-terminal note, Retry present, no fixture backend and successful fake-daemon recovery. Replace the old assertions at `crates/agmen-desktop/src/ui/live_root_tests.rs:74` to 81; they intentionally lock in the behavior this task replaces.
- [ ] **Step 2:** Run `cargo nextest run -p agmen-desktop live_without_agmend_says_so`. Expected: FAIL on command equality.
- [ ] **Step 3:** Change only the offline wording and remove its now-resolved foreground shortcut. Do not execute installation from the UI. The user runs the offered command.
- [ ] **Step 4:** Run the same narrow test, then the gate. Expected: PASS.
- [ ] **Step 5:** Commit: `Offer agmend service install in the desktop offline view`

## Part B: core (L10)

### Task B T1: Add local Forbidden and StaleRequest codes

**Files:**
- Modify: `crates/agmen-local/src/message.rs:42` to 57
- Test: inline tests in `message.rs`; regressions in `crates/agmen-desktop/src/backend/agmend_tests.rs:201` to 245

**Interfaces:**
- Consumes: `enum ErrorCode` and Response's error encoding (`crates/agmen-local/src/message.rs:42` to 65, `crates/agmen-local/src/message.rs:108` to 117).
- Produces: `ErrorCode::Forbidden` serialized as `"forbidden"`; `ErrorCode::StaleRequest` serialized as `"stale_request"`. Existing constructors and signatures remain unchanged.

- [ ] **Step 1: Write the failing tests:** `local_error_codes_round_trip` serializes and deserializes both variants through Response, preserving id and message; `unknown_local_error_still_parses` deserializes an unrecognized code to Unknown and preserves the response id and message.
- [ ] **Step 2:** Run `cargo nextest run -p agmen-local`. Expected: FAIL because the variants do not exist.
- [ ] **Step 3:** Add both variants before Unknown with short documentation. No NackReason or schema change. The lane's desktop pointers are ApiError construction sites rather than exhaustive matches (`crates/agmen-desktop/src/backend/agmend_tests.rs:208` to 211, `crates/agmen-desktop/src/backend/agmend_tests.rs:230` to 233); inspect actual consumers if the compiler reports any exhaustive match.
- [ ] **Step 4:** Run `cargo nextest run -p agmen-local`, `cargo nextest run -p agmen-desktop hub_refusal_reaches_subscribers` and `cargo nextest run -p agmen-desktop a_refused_watch_says_why`, then the gate. Expected: PASS.
- [ ] **Step 5:** Commit: `Add forbidden and stale_request local API errors`

### Task B T4: Add the durable daemon state store

**Files:**
- Create: `crates/agmend/src/state.rs`
- Modify: `crates/agmend/src/{lib.rs,api.rs}`, `crates/agmend/Cargo.toml`, `Cargo.lock`
- Test: inline state tests and `crates/agmend/tests/support/mod.rs`

**Interfaces:**
- Consumes: the existing `Config::new(socket: impl Into<PathBuf>, login_file: impl Into<PathBuf>, tokens: TokenStore) -> Self` and `Agmend::bind(config: Config) -> Result<Self, AgmendError>` (`crates/agmend/src/lib.rs:99` to 103, `crates/agmend/src/lib.rs:181`). Keep these public signatures unchanged.
- Produces: `struct Store`, `enum StoreError`, `fn Store::open(path: &Path) -> Result<Self, StoreError>`, `fn Store::ingest(&mut self, origin: &str, project: &ProjectId, message: &StampedEnvelope) -> Result<(), StoreError>`, `fn Store::drain(&mut self, origin: &str, identity: &IdentityId, limit: u32, byte_budget: usize) -> Result<Vec<StampedEnvelope>, StoreError>`, `fn Store::positions(&self, origin: &str, project: &ProjectId) -> Result<BTreeMap<Stream, u64>, StoreError>`.
- Produces: managed metadata `struct ManagedIdentity { hub_url: Url, broker_url: Broker, tenant: TenantId, project: ProjectId, identity_id: IdentityId, owner_id: IdentityId, handle: String, kind: IdentityKind }`, plus `fn Store::register(&mut self, identity: &ManagedIdentity) -> Result<(), StoreError>` and `fn Store::identities(&self, origin: &str, owner: &IdentityId) -> Result<Vec<ManagedIdentity>, StoreError>`. None contains a secret.
- Produces: API context `struct Core { console: Arc<Console>, store: Arc<std::sync::Mutex<Store>> }` and changes the private boundary to `async fn api::serve(stream: UnixStream, core: Arc<Core>)`. Existing code passes only Console (`crates/agmend/src/api.rs:25`, `crates/agmend/src/lib.rs:231` to 234). Extend Core with identities in B T2; do not create a second console or socket.

- [ ] **Step 1: Write the failing tests:** `state_opens_private_database` checks parent 0700 and database 0600, including WAL sidecars; `state_reopen_preserves_identities_and_positions`; `pending_store_deduplicates_stream_seq` inserts the same event twice and gets one row; `pending_store_deduplicates_cross_stream_message` delivers an ASSIGN on feed and inbox and gets one notification; `pending_store_transaction_rolls_back_on_full` fills 10,000 rows and requires no cursor advance for the refused event; `pending_store_drain_is_atomic` runs two drains and sees disjoint results; `state_rejects_future_schema` requires a contextual startup error. Test maximum seq conversion and malformed stored JSON as errors.
- [ ] **Step 2:** Run `cargo nextest run -p agmend`. Expected: FAIL because Store is absent.
- [ ] **Step 3:** Add `rusqlite = { version = "0.39.0", features = ["bundled"] }`, using the lockfile version. Open agmend.db only after the socket lock is acquired, so a second daemon cannot migrate it. Add a contextual AgmendError store variant. Use schema version 1, WAL and FULL synchronous mode, bounded 5 s busy wait and IMMEDIATE transactions. Keep 0700 parent directories and 0600 database files before SQLite creates sidecars.
- [ ] **Step 4:** Implement tables for managed identities, stream positions, pending rows and receipt deduplication. Namespace every row by actual hub origin and project or identity. In one transaction, verify contiguous stream seq, insert qualifying pending rows, record receipts and advance the durable position. Duplicate seq is a no-op; a gap returns an explicit error for the collector to replay. Qualifiers are stamped mentioned, DM recipient or ASSIGN assignee, excluding the sender. Do not use the broader `Me::is_for_me` unchanged: it also selects ASK, GRANT, DENY, ANSWER and owner ALERT (`crates/agmen-client/src/event.rs:47` to 65). Persist message ids for cross-stream deduplication until both feed and that identity's inbox positions pass their copies. Bound receipts at 20,000 per identity; if this cap blocks safe pruning, stop ingestion visibly rather than evicting deduplication evidence.
- [ ] **Step 5:** Drain the oldest prefix that fits both 500 rows and `MAX_FRAME - 4096` serialized bytes. Mark that exact selection delivered in one transaction. Keep stream seq order; across streams use durable insertion order, without claiming a global seq. Delete delivered payload rows, retain bounded receipts. Empty drains return `[]`. A disk error returns an error, never an empty success. Run SQLite and token-store calls in spawn_blocking, with at most 64 outstanding jobs under the connection cap; no unbounded job channel. Run `cargo nextest run -p agmend`, then the gate. Expected: PASS.
- [ ] **Step 6:** Commit: `Persist agmend identities and pending delivery state`

### Task B T2: Bind identity requests to their local token

**Files:**
- Create: `crates/agmend/src/identities.rs`
- Modify: `crates/agmend/src/{api.rs,lib.rs,login.rs}`, `crates/agmend/Cargo.toml`, `Cargo.lock`
- Test: `crates/agmend/tests/{api.rs,support/mod.rs}` and inline identity tests

**Interfaces:**
- Consumes: B T1 errors, B T4 metadata and agmen-client TokenStore get/set. Credentials currently rereads the login file per call (`crates/agmend/src/login.rs:42` to 47); retain that property for owner checks.
- Produces: `enum Caller { Human, Identity(ManagedIdentity) }`, `struct Identities`, `async fn Identities::authorize(&self, identity: Option<&IdentityId>, token: Option<&str>) -> Result<Caller, ApiError>`, `async fn Identities::post(&self, caller: &ManagedIdentity, envelope: ClientEnvelope) -> Result<serde_json::Value, ApiError>`. Core owns `Arc<Identities>` in addition to its B T4 fields.
- Produces: transport rule: a valid local token with matching `as` yields Caller::Identity; missing, wrong, foreign-origin, unknown or sibling credentials yield unauthorized. A token presented without `as` also yields unauthorized, so an agent credential cannot be silently treated as human. Identity attempts to call identity_mint or identity_list yield forbidden after authentication, before dispatch.

- [ ] **Step 1: Write the failing tests with a seeded identity, without minting:** replace `a_request_with_as_is_refused_for_now` (`crates/agmend/tests/api.rs:79` to 88) with `post_requires_identity_token`; add `sibling_token_is_unauthorized`, `identity_token_is_not_a_human_credential`, `identity_origin_and_owner_must_match_login`, `identity_authorization_is_per_request`, `seeded_identity_post_uses_its_session` and `human_post_is_unchanged`. Assert exact error codes and zero publish calls on rejection; a good token publishes once as A, never the human or sibling B. Add `agent_cannot_mint` now: valid A credentials get forbidden and zero mint calls even before B T3 exists.
- [ ] **Step 2:** Run `cargo nextest run -p agmend --test api`. Expected: FAIL because `as` still returns bad_request (`crates/agmend/src/api.rs:77` to 81).
- [ ] **Step 3:** Extract and remove params.local_token once at the boundary. Authenticate after hello/version validation and before method selection, including streaming selection. Keep unknown-method handling and human console dispatch. Add the Unix-only shortcut at serve. For identity callers, allow post, inbox, watch, state and pending only; reject human console methods with forbidden. A selected project must match the managed identity, never caller-supplied hub metadata. Seed integration fixtures by first binding and dropping a temporary daemon to create the schema, then writing managed metadata with rusqlite and credentials with TokenStore before rebinding. Do not expose a production seeding API.
- [ ] **Step 4:** Reuse `HubClient::new(base: Url, project: ProjectId, identity: IdentityId, token: impl Into<String>) -> Result<Self, ClientError>` (`crates/agmen-client/src/hub.rs:92` to 97), `SessionConfig::new(kind, topics, hub, broker, state_dir)` (`crates/agmen-client/src/session.rs:246` to 252), `Session::connect(config) -> Result<(Session, Events), ClientError>` (`crates/agmen-client/src/session.rs:381` to 383) and `Session::publish(&self, env: ClientEnvelope) -> Result<(), ClientError>` (`crates/agmen-client/src/session.rs:495`). Run a bounded runtime per seeded identity, with its Session state folder at `<socket parent>/identities/<identity_id>` (mode 0700, holding the client's `agmen.db`), and read Events continuously. Place successful publish tests inline in identities.rs with a private `#[cfg(test)]` publisher closure returning a bounded future of `Result<OutboxState, ApiError>`. Drive api::serve on an in-process Unix socket with that Core. Keep authorization failures in tests/api.rs. Do not expose a production transport option or contact a real broker. Identity post waits at most 5 s for OutboxState::Recorded, returning `{stream,seq}`; if still pending return `{queued:true,id}`; a final rejection is code hub with its reason. The human result does not change. Do not build another outbox or hold a mutex across ACK waiting.
- [ ] **Step 5:** Reject missing identity hub tokens contextually and never send local tokens to HTTP or MQTT. Use the local storage namespace defined above. Add getrandom only when local-token generation is wired in B T3, rather than introducing an unused dependency here. Run `cargo nextest run -p agmend`, then the gate. Expected: PASS. All fixtures use TokenStore::Dir, following `crates/agmend/tests/support/mod.rs:360` to 361.
- [ ] **Step 6:** Commit: `Authenticate local identity calls and post through their sessions`

### Task B T5: Serve inbox, watch, state and pending end to end

**Files:**
- Modify: `crates/agmend/src/{api.rs,identities.rs,state.rs,hub.rs,lib.rs}`
- Test: `crates/agmend/tests/{api.rs,support/mod.rs}` and inline store tests

**Interfaces:**
- Consumes: B T2's Caller, B T4's durable store, `Hub::events(&self, project: &ProjectId, after: &BTreeMap<Stream,u64>, last_id: Option<&str>) -> Result<reqwest::Response, HubError>` (`crates/agmend/src/hub.rs:275` to 280) and ClientEvent's serialized forms (`crates/agmen-client/src/event.rs:73` to 99).
- Produces: new method contracts, with identity selection in top-level `as`:
  - `inbox {project, after?: u64 = 0, limit?: u32 = 500, local_token?} -> {messages: Vec<StampedEnvelope>, next_after: Option<u64>, head: u64}`. This is a read of that caller's inbox, not a pending drain. A human reads their own inbox. A caller cannot choose an arbitrary stream.
  - `watch {project, local_token?}` streams Response results containing ClientEvent with the watch request id. Send Me first, then Message, Delivery or Connection. It takes over the connection. An identity gets only the project feed and its inbox; a human gets their feed and own inbox. Keep console_watch's ConsoleEvent contract unchanged.
  - `state {project, key, local_token?} -> {key, as_of_seq, value}`. Accept roster or head only, as protocol section 6 requires (`docs/protocol.md:189`). Return the hub wrapper, without claiming roster is a consistent snapshot.
  - `pending {local_token?} -> Vec<StampedEnvelope>` requires an authenticated identity caller. It returns and marks delivered one bounded batch. A human call without `as` gets bad_request, not an arbitrary identity's queue.
- Produces: private helpers `async fn Hub::inbox(&self, project: &ProjectId, identity: &IdentityId, after: u64, limit: u32) -> Result<Page, HubError>` and `async fn Hub::state(&self, project: &ProjectId, key: &str) -> Result<Value, HubError>`, preserving existing Hub page and roster signatures. Identities provides `async fn inbox(&self, caller: &Caller, raw: Value) -> Result<Value, ApiError>`, `async fn state(&self, caller: &Caller, raw: Value) -> Result<Value, ApiError>`, `async fn pending(&self, caller: &ManagedIdentity) -> Result<Value, ApiError>` and `async fn watch(&self, caller: &Caller, id: u64, raw: Value, reader: OwnedReadHalf, writer: &mut OwnedWriteHalf) -> Result<(), ApiError>`.

- [ ] **Step 1: Write the failing tests:** `identity_reads_are_scoped` seeds A and B, checks A cannot name B's project or inbox and cannot see B's DM; `inbox_paginates_without_draining_pending` checks after-exclusive ordering, 1 to 500 validation and repeated reads; `state_preserves_hub_wrapper` checks key, as_of_seq and value and rejects unsupported keys; `watch_streams_events` sends two message events and requires seq 1 then 2 and the watch request id; `watch_slow_reader_closes` fills 256 slots and requires a visible terminal error or closure, never silent loss.
- [ ] **Step 2: Write the failing durability tests:** `pending_collects_mentions_and_dms` sends a mention for A, an unmentioned FYI, DM to A, ASSIGN to A, B's DM and A's self-message; only the mention, DM and ASSIGN drain, in seq order within each stream, then `[]`. `pending_collects_without_watch_clients` sends the same input without any watch connection. `pending_survives_restart` stops and awaits the daemon, rebinds the same socket and database, drains the message once, and drains empty again. `pending_replays_after_cursor_crash_window` advances a Session cursor but kills the fixture before Store::ingest and requires recovery from the independent durable position. `pending_cross_stream_copy_is_once` checks feed and inbox copies of ASSIGN. `pending_frame_cap_keeps_undelivered_tail` verifies a full frame contains only the oldest fitting prefix and the next drain gets the rest. `pending_on_disk_failure_is_error` requires no position advance. All fixture waits time out after 5 s, and all spawned tasks are aborted and awaited on teardown.
- [ ] **Step 3:** Run `cargo nextest run -p agmend --test api`. Expected: FAIL with not_found for the new methods.
- [ ] **Step 4:** Implement the methods and strict typed params. Extend the fake hub beyond its current feed-only replay assertion (`crates/agmend/tests/support/mod.rs:192` to 194), with per-identity inboxes, state wrappers, scoped event streams and bounded publish injection. Reuse the existing SSE parser and human credentials; do not alter Console to serve identity calls. The current console event handler drops inbox messages and only broadcasts feed (`crates/agmend/src/feed.rs:318` to 333), so it cannot own this durable collector.
- [ ] **Step 5:** Start one durable human-authenticated SSE collector per managed project during daemon startup, independently of local watchers. Resume from Store::positions, never the Session's delivery cursor. For every stamped message, commit pending rows and positions before notifying local watchers. On a seq gap, replay that stream from the daemon position in pages of at most 500 before accepting the later event. On restart or reconnect, the hub events endpoint resumes from the full position map, as protocol specifies (`docs/protocol.md:186`). Reuse Session for identity posting and Delivery/Connection events; do not persist the same message twice from both transports. Session Message events may prompt catch-up but are not the authoritative durable cursor. This avoids a crash window between the client's committed cursor and the daemon queue.
- [ ] **Step 6:** Start collectors for all registered identities' projects even with no subscribers, stop them on sign-out and clear stale runtime handles before a different login can use them. Retain their namespaced database rows for the original owner. Put all collector and Session handles under the daemon lifetime and await their bounded shutdown, at most 5 s each and 10 s total, then abort and await stragglers. Apply the global quotas, reconnect limits and frame budgets. pending marks delivered when its transaction commits; a client disconnect after commit can lose that response. Do not claim transport-level exactly-once delivery without an acknowledgement protocol.
- [ ] **Step 7:** Run `cargo nextest run -p agmend`, then the gate. Expected: PASS, including pending_survives_restart in the default profile. The existing it profile excludes restart names (`.config/nextest.toml:12` to 14); these fake-hub tests must not have an `it_` prefix.
- [ ] **Step 8:** Commit: `Serve scoped local reads and persist pending messages across restarts`

### Task B T3: Mint and list identities after the roles mint body lands

**Files:**
- Modify: `crates/agmend/src/{api.rs,identities.rs,state.rs,hub.rs,login.rs,lib.rs}`, `crates/agmend/Cargo.toml`, `Cargo.lock`
- Test: `crates/agmend/tests/{api.rs,support/mod.rs}` and inline identity tests

**Interfaces:**
- Consumes: merged M3a's roles mint request and EditScope. Verify their actual signatures after the integration prerequisite. The desired body is fixed by `docs/protocol.md:182`: `{handle, project, kind, role, verbs, edits, role_sha256, agent?, delegated_by?}`. Do not depend on the old three-field request (`crates/agmen-hub/src/identities.rs:144` to 149).
- Produces: local `identity_mint {handle, project, kind?: "agent", role, verbs, edits, role_sha256, agent?, delegated_by?} -> {identity_id, local_token}`. Both role data and the hash are required; default kind is agent, never a default unrestricted role. Reuse M3a's serde body type, importing its shared EditScope. Reject caller metadata such as owner_id, hub_url or broker_url.
- Produces: local `identity_list {} -> Vec<ManagedIdentity>` scoped to the current human and actual hub origin, in handle then identity_id order. It lists identities managed by this daemon, not other machines' identities, and contains no token fields.
- Produces: `async fn Identities::mint(&self, raw: Value) -> Result<Value, ApiError>`, `async fn Identities::list(&self) -> Result<Value, ApiError>` and `async fn Hub::mint(&self, body: &MintRequest) -> Result<Minted, HubError>`, where MintRequest is M3a's body and Minted is the checked response type. Preserve the hub's id, handle, kind, tenant, project and once-returned identity token fields (`crates/agmen-hub/src/identities.rs:151` to 159); inspect any M3a response changes before implementation.

- [ ] **Step 1: Check the prerequisite:** verify M3a's role fields, wire serialization, listener kind, shared EditScope and mint response in the integrated tree. Confirm the broker URL source matches the existing identity metadata contract (`crates/agmen-client/src/identity.rs:59` to 75). If those interfaces are absent, stop B T3 and report the missing prerequisite. Do not add hub, roles or proto implementation to this lane.
- [ ] **Step 2: Write the failing tests:** `identity_mint_posts_complete_roles_body_once` asserts every required role field, optional agent/delegated_by forwarding, one HTTP POST and default kind agent; `identity_mint_returns_local_token_once` checks a 64-character token, no hub token in the response or list, and later authorize succeeds; `identity_list_is_owner_scoped_and_sorted` checks another owner/origin is absent, stable order and empty `[]`; `agent_cannot_mint` and `agent_cannot_list` check forbidden and no hub calls; `identity_mint_hub_refusal_is_not_registered` checks no local row or usable token on a 4xx; `identity_mint_store_failure_is_reported` injects token/database failure and requires an error, no running session and no success response; `minted_identity_post_and_pending` mints then posts as the identity and receives one qualifying event through pending after restart.
- [ ] **Step 3:** Run `cargo nextest run -p agmend --test api`. Expected: FAIL because mint and list are not dispatched yet.
- [ ] **Step 4:** Implement the human-only dispatcher and one mint POST through the existing no-redirect HTTP client (`crates/agmend/src/hub.rs:163` to 169). Capture current login origin and owner once, verify them again before committing local state, and reject a changed login. Validate the complete role body through M3a's types. Take hub URL from the login. Fetch the broker URL from authenticated `GET /v1/me`, whose response includes it (`crates/agmen-hub/src/me.rs:16` to 22, `crates/agmen-hub/src/me.rs:64`). Parse it as Broker before minting, following the existing CLI (`crates/agmen-cli/src/cmd/join.rs:108` to 120). Add `async fn Hub::broker(&self) -> Result<Broker, HubError>` using the same credentials and timeout, and extend the fake hub's me route. Test malformed and unreachable me responses, requiring zero mint POSTs. Never guess the broker host or accept it from an agent parameter.
- [ ] **Step 5:** Add getrandom 0.4.3, generate the local token once and save the hub token under the actual-origin TokenKey and the local token under its reserved-origin TokenKey. Persist managed metadata only after both writes succeed, then start the runtime and durable collector. On a local failure remove any credentials written by this attempt, record a contextual error and do not start it. The remote mint cannot be rolled back by a SQLite transaction: report the minted id for reconciliation and never retry the POST automatically. If the client loses the once-returned local token, require an explicit future rotation path; do not disclose it through identity_list.
- [ ] **Step 6:** Enforce 64 managed identities before a POST and validate delegated_by through the merged role rules. Restore managed identities from metadata and TokenStore on startup, with their separate state folders, and wire them to B T2 and B T5 end to end. Do not connect a second Session for the same identity when a second local caller arrives. Keep the runtime map bounded at 64 and share the existing Session. Empty lists are valid. Missing/revoked tokens produce a visible offline/error state, never human fallback. Run `cargo nextest run -p agmend`, then the gate. Expected: PASS. Real hub/broker integration and real service loading require their own approved environment; do not substitute them for deterministic fake-hub tests.
- [ ] **Step 7:** Commit: `Mint role-scoped identities and restore their local sessions`

---

## Not in this plan

- Devices, registration and device-scoped credential changes (D17).
- login_start and login_status, which belong to P7. Reuse the existing CLI login reader and renewal path (`crates/agmend/src/login.rs:46` to 76).
- Windows service support, Windows named pipes and S6 (D36).
- Launcher, worker stdin/ACP delivery, wake-up scheduling, listener delegation and settings enforcement. Their consumers can call pending later. The design's 30 s wake interval and eight-message thread brake apply to those future consumers (`docs/design.md:461`, `docs/design.md:474`), not to draining queued data.
- Rebuilding desktop-live Tasks 5 or 8, changing the client's database schema, implementing M3a in this lane, or adding hub routes for identity listing.
