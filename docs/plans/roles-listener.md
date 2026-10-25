# agmen roles and listener plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** After the Claude Code plugin MVP ([mvp.md](mvp.md)), give every agent an enforced role and give every person a listener: a read-only small model that hears their chat, looks things up, delegates to role-bound Claude Code workers in their own worktrees, and changes their settings when asked.

**Architecture:** A new `agmen-roles` crate parses, layers and renders the XML roles; `agmen-proto` gains the role fields and new verbs; the hub stores each identity's role and caps its verbs per kind. A new `agmend` daemon (the MVP has none) owns the launcher, worktrees, budgets and the listener; the CLI and `agmen mcp` talk to it over a Unix socket. The listener runs as Claude Code headless with `--tools Read,Grep,Glob` and the `agmen mcp --listener` tool set; agmend, not the model, enforces who may ask and what may change.

**Tech Stack:** Rust (edition 2024), tokio, axum, sqlx 0.9, rusqlite 0.41, rmcp 3.7.0, quick-xml with serde (role files), sha2, Claude Code 2.1.310 headless (`claude -p --input-format stream-json`), the MVP dev stack (EMQX 6.3.2, Postgres 16.15, Toxiproxy).

**Spec:** [../design.md](../design.md) sections 6.7, 6.11 and 6.13; [../protocol.md](../protocol.md) sections 1, 3, 4, 5.4, 6, 7; [../specs/2026-10-25-roles-listener-consoles-design.md](../specs/2026-10-25-roles-listener-consoles-design.md) for the why. Read the design sections before Task 1.

**Out of this plan** (later plans): ACP launching for the other five agents, switching agents, take-over, the terminal UI and desktop app, planning rooms, urgent-alert UI, the GitHub App, appearance settings, Windows local IPC (S6). The CLI and MCP surfaces here are what those consoles will call.

## Global Constraints

Everything in [mvp.md Global constraints](mvp.md#global-constraints) and [m0-m1.md Global Constraints](m0-m1.md#global-constraints), plus:

- New workspace members: `agmen-roles` (lib), `agmend` (bin). `agmend` listens on a Unix socket at `$XDG_STATE_HOME/agmen/agmend.sock` (macOS: `~/Library/Application Support/agmen/agmend.sock`), mode 0600; Windows comes with S6 later.
- Role files: `roles/*.xml` in this repo are the built-ins and are embedded with `include_str!`; a role `name` matches `^[a-z][a-z0-9-]{1,31}$`; prompt text is passed to Claude Code through `--append-system-prompt-file`.
- Ceilings by kind, whatever a role says (design 6.13): `listener` never gets tool classes `edit`, `shell` or `web`; `agent` never gets ASSIGN, RULING, ROLLCALL or the `settings` class; no non-human gets RULING, PUSH_OK or PUSH_NO.
- Base verbs every role may send: HELLO, BYE, FYI, ASK, ANSWER, ACK, STATUS, DM, ALERT, FOUND.
- Listener defaults: `per_delegation_usd` 5.00, `daily_usd` 20.00, `max_workers` 3, `listener_daily_usd` 2.00, `accept_from` = the person plus every coordinator (coordinators cannot be removed), model `claude-haiku-5-5`, at most 3 listener posts per thread without a human or worker post in between.
- Branch names `{type}/{item}-{slug}`; agmend never runs `git ... --force` or `git branch -D` except for a PR the GitHub App reported merged at the branch tip (not in this plan, so never here).
- Tests that call a paid model live in `tests/live_*.rs`, run only with `AGMEN_LIVE=1` under `--profile live`, and pass `--max-budget-usd 2.00`.

## Review Focus

1. **A role file that widens.** A project `listener.xml` with `<tools allow="shell" />`, or an `engineer.xml` with `<verbs allow="RULING" />`. Expected: the ceiling wins at load (Task 1 `ceiling_strips_listener_shell`) and at the hub (Task 3 `it_mint_caps_verbs_at_kind`); the agent view says what was stripped.
2. **A settings request that cites an old message.** The listener calls `settings_apply` with `reply_to` pointing at the person's message from a previous batch. Expected: refused with `stale_request`. Test: Task 6 `settings_rejects_message_outside_batch`.
3. **A widening change applied without a click.** `settings_apply` raising a budget returns `pending`, and nothing changes until `agmen settings apply <id>` runs as the human. Test: Task 6 `widening_waits_for_apply`.
4. **Worktree with uncommitted work at prune time.** Branch merged but the worktree is dirty. Expected: kept, `wip/<branch>` ref written, listed in the sweep report. Test: Task 5 `prune_keeps_dirty_worktree`.
5. **Two listeners for one person.** A second `agmen listener start` on another machine. Expected: hub 409, agmend reports "already running on <device>". Test: Task 3 `it_one_listener_per_person_project` (exists in m0-m1 Task 9 as the dispatcher test; keep it, renamed).

---

## Phase A: roles

### Task 1: `agmen-roles` crate: parse, layer, validate, render

**Files:**
- Create: `crates/agmen-roles/Cargo.toml`, `crates/agmen-roles/src/{lib.rs,schema.rs,layer.rs,ceiling.rs,render.rs,import.rs,error.rs}`, `crates/agmen-roles/tests/roles.rs`, `crates/agmen-roles/tests/fixtures/{widen-listener.xml,narrow-reviewer.xml,subagent.md}`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: `roles/*.xml` (embedded).
- Produces:
  - `pub enum Kind { Agent, Coordinator, Listener }`, `pub enum Tier { Small, Standard, Deep }`, `pub enum ToolClass { Read, Search, Edit, Shell, Web, Subagents, GitRead, Settings }`, `pub enum EditScope { None, Lanes, Paths(Vec<String>) }`, `pub struct Worktree { mode: WorktreeMode, name: Option<String>, remove: Remove }`.
  - `pub struct Role { name, extends: Option<String>, kind: Option<Kind>, tier: Option<Tier>, summary, tools: ToolSet, verbs: BTreeSet<String>, edits: EditScope, worktree: Worktree, prompt: Prompt }`; `pub fn parse(xml: &str) -> Result<Role, RoleError>` (quick-xml serde; unknown elements are an error).
  - `pub struct Layers { builtin, project: Option<PathBuf>, personal: Option<PathBuf> }` with `pub fn load(&self) -> Result<Catalog, RoleError>`; `Catalog::get(name) -> Option<&Resolved>` where `pub struct Resolved { role: Role (fully inherited), kind: Kind, verbs: BTreeSet<String> (base verbs plus the role's), tools: BTreeSet<ToolClass>, edits, worktree, tier: Tier, sha256: String (of the resolved role's source files concatenated), stripped: Vec<String> (what the ceiling removed) }`.
  - `pub fn render_prompt(r: &Resolved) -> String`: base text, each ancestor, then the role, as the XML's prompt elements in order.
  - `pub fn claude_tools(r: &Resolved) -> Vec<&'static str>`: class to Claude Code tool names (`read` Read; `search` Grep, Glob; `edit` Edit, Write, NotebookEdit; `shell` Bash; `web` WebFetch, WebSearch; `subagents` Agent).
  - `pub fn import_subagent(md: &str, name: &str) -> Result<String, RoleError>`: a Claude Code subagent file (YAML frontmatter `tools`, `disallowedTools`, `model`, `omitClaudeMd`, body) to role XML text.

- [ ] **Step 1: Write the failing tests** in `tests/roles.rs`:

```rust
#[test] fn builtins_parse_and_resolve()          // Layers::default().load(): 8 roles; reviewer.tools == {Read, Search, Shell}; reviewer.verbs == base ∪ {REVIEW}; engineer.verbs contains LEND; listener.kind == Listener
#[test] fn extends_narrows_and_adds_rules()      // fixtures/narrow-reviewer.xml extends reviewer with tools allow="read search": tools == {Read, Search}; prompt contains both reviewer's and its own rules, reviewer's first
#[test] fn ceiling_strips_listener_shell()       // fixtures/widen-listener.xml (name listener, tools allow="read shell"): tools == {Read}; stripped == ["tool shell"]
#[test] fn ceiling_strips_agent_ruling()         // an engineer with verbs allow="RULING": verbs excludes RULING; stripped == ["verb RULING"]
#[test] fn project_overrides_builtin()           // project dir holds reviewer.xml with tier="standard": catalog reviewer.tier == Standard; sha256 differs from builtin
#[test] fn unknown_element_is_error()            // <role><hooks/></role> -> Err(RoleError::Schema)
#[test] fn render_prompt_order()                 // render_prompt(engineer) starts with base <purpose> text and ends with engineer's <report>
#[test] fn import_subagent_maps_fields()         // fixtures/subagent.md (tools: Read, Grep, Glob, Bash; model: opus; omitClaudeMd: true): XML has tools allow="read search shell", tier="deep", extends="none", body inside <purpose>
```

- [ ] **Step 2:** Run `cargo nextest run -p agmen-roles`. Expected: FAIL to compile.
- [ ] **Step 3:** Implement. `cargo add quick-xml --features serialize`, `serde`, `sha2`, `thiserror`. Base verbs and ceilings are constants in `ceiling.rs`.
- [ ] **Step 4:** Run the tests and the workspace gate. Expected: all pass.
- [ ] **Step 5:** Commit: `Add agmen-roles: parse, layer, validate and render role files`

### Task 2: Protocol types for roles, the listener and the new verbs

**Files:**
- Modify: `crates/agmen-proto/src/{verbs.rs,envelope.rs,ids.rs,reply.rs}` (from mvp Task 6), `schema/` (regenerate)
- Test: `crates/agmen-proto/tests/roles_proto.rs`

**Interfaces:**
- Consumes: Task 1's `Kind` is separate; proto keeps its own `IdentityKind`.
- Produces:
  - `IdentityKind::Listener` replaces `Dispatcher` (serialized `listener`).
  - `Payload::Review { verdict: Verdict, findings: Vec<Finding>, traced: Option<String> }` with `Verdict { Pass, Changes }`, `Finding { severity: Severity, path, line: Option<u32>, what, trigger }`; `Payload::Plan { version: u32, path, sha }`; `Payload::Delegated { worker, agent, role, model, tier, budget_usd, item }`; `Payload::Alert { need, urgency: Urgency (default Normal), needs: Needs (default Anyone), actions: Vec<String> }`; `Payload::Decision { outcome, option, subject: Option<String> }`; `Payload::Status` gains `hours: Option<f32>`, `tokens: Option<Tokens>`.
  - `Mention::Role(RoleName)` serialized `role:<name>`; `RoleName` newtype with the Task 1 regex.
  - `StampedEnvelope.refs: Vec<Ref>` with `Ref { kind: RefKind, start: u32, end: u32, target: String }`, `RefKind { Identity, Lane, Item, Role, Pr }`.
  - `NackReason::NotLaneOwner`, `NackReason::StaleRequest`.
  - `pub struct RoleSpec { role: RoleName, verbs: BTreeSet<String>, edits: EditScope, role_sha256: String }` (serde, used by `POST /identities`).
  - `validate`: REVIEW requires `reply_to`; PLAN requires `version >= 1`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn review_requires_reply_to()        // REVIEW with reply_to None -> Err(Schema)
#[test] fn role_mention_round_trip()         // "role:reviewer" parses to Mention::Role and back; "role:Reviewer" and "role:" are Err
#[test] fn alert_defaults()                  // ALERT {need} deserializes with urgency Normal, needs Anyone, actions []
#[test] fn delegated_requires_agent_and_role() // JSON without agent or role fails to deserialize
#[test] fn listener_kind_serializes()        // IdentityKind::Listener <-> "listener"; "dispatcher" is Err
```

- [ ] **Step 2:** Run `cargo nextest run -p agmen-proto -E 'test(roles_proto)'`. Expected: FAIL.
- [ ] **Step 3:** Implement; run `cargo run -p agmen-proto --example export-schema` and commit the schema diff.
- [ ] **Step 4:** Run the gate (the schema drift test passes). Expected: all pass.
- [ ] **Step 5:** Commit: `Add roles, listener, REVIEW, PLAN, refs and alert urgency to the protocol`

### Task 3: Hub: roles at mint, verb caps, lane owners, REVIEW and role mentions, refs

**Files:**
- Create: `crates/agmen-hub/migrations/0005_roles.sql`, `crates/agmen-hub/src/{roles.rs,refs.rs}`, `crates/agmen-hub/tests/it_roles.rs`
- Modify: `crates/agmen-hub/src/{identities.rs,ingest.rs,lanes.rs}` (mvp Tasks 7, 8, 10)

**Interfaces:**
- Consumes: `RoleSpec`, `Mention::Role`, `Ref`, `NackReason::NotLaneOwner` (Task 2); `lanes.owner_identity_id` (mvp Task 10).
- Produces:
  - Migration: `identities` gains `role text`, `verbs text[]`, `edits jsonb`, `role_sha256 text`, `agent text`; `kind` check adds `listener`; the partial unique index for one listener per `(owner_human_id, project_id)`.
  - `POST /v1/identities` body `{handle, project, kind, role, verbs, edits, role_sha256, agent?, delegated_by?}`. `pub fn cap_verbs(kind: IdentityKind, verbs: &BTreeSet<String>) -> BTreeSet<String>` applies the Global Constraints ceilings; the stored set is the capped one; the response carries `verbs` so agmend can show what was stripped.
  - `ingest`: role check = `verb in identity.verbs`, else final NACK `forbidden_verb`. LEND: sender must be `lanes.owner_identity_id` or a coordinator, else `not_lane_owner`. REVIEW: `reply_to` must be a DONE; the DONE's `from` is added to `mentioned`. `Mention::Role(r)` from a human or coordinator expands to live identities with `role == r`; from others, `forbidden_mention`.
  - `refs.rs`: `pub fn stamp_refs(body: &str, roster: &Roster, lanes: &[Lane], items: &[ItemKey]) -> Vec<Ref>` matching handles (`owner/name`), lane names, item keys, `@<role>s` role words and `#<n>` PR numbers; stamped on every FYI, ASK, ANSWER, ALERT, DONE, REVIEW.
  - `state/roster` entries gain `role` and `agent`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test] async fn it_mint_stores_role()                    // POST /identities {role:"engineer", verbs:[base+CLAIM,DONE,LEND], ...} -> 201; GET roster shows role "engineer"
#[tokio::test] async fn it_mint_caps_verbs_at_kind()             // kind agent with verbs including RULING, ASSIGN -> 201, response verbs exclude both; a later RULING from it -> Nack ForbiddenVerb
#[tokio::test] async fn it_one_listener_per_person_project()     // second kind=listener for the same human and project -> 409
#[tokio::test] async fn it_role_verbs_enforced_on_ingest()       // a researcher identity posts CLAIM -> Nack ForbiddenVerb; posts ANSWER -> Ack
#[tokio::test] async fn it_lend_only_from_owner()                // lane L-UI owner = A; B posts LEND on L-UI -> Nack NotLaneOwner; A -> Ack; coordinator -> Ack
#[tokio::test] async fn it_review_mentions_done_author()         // reviewer posts REVIEW reply_to a DONE from engineer E -> feed copy mentioned contains E; REVIEW reply_to an FYI -> Nack Schema
#[tokio::test] async fn it_role_mention_expands()                // human posts FYI mentions [role:reviewer] -> mentioned == every live reviewer; an agent doing the same -> Nack ForbiddenMention
#[tokio::test] async fn it_refs_stamped()                        // body "andy/sol broke L-UI in B-412, see #10" -> refs has identity, lane, item, pr with correct byte offsets; "L-NOPE" and "B-999" (unknown) are not refs
```

- [ ] **Step 2:** Run `cargo nextest run -p agmen-hub --profile it -E 'test(it_roles)'`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run the tests and the gate. Expected: all pass.
- [ ] **Step 5:** Commit: `Store roles at mint, cap verbs per kind, add lane owners, REVIEW, role mentions and refs`

### Task 4: CLI and MCP: `agmen role`, `agmen join --role`, tool filtering

**Files:**
- Create: `crates/agmen-cli/src/cmd/role.rs`, `crates/agmen-cli/tests/role_cli.rs`
- Modify: `crates/agmen-cli/src/cmd/join.rs`, `crates/agmen-cli/src/mcp/{mod.rs,tools.rs}` (mvp Tasks 12, 13)

**Interfaces:**
- Consumes: Task 1 (`Layers`, `Catalog`, `claude_tools`, `import_subagent`), Task 3 (`POST /identities` with `RoleSpec`).
- Produces:
  - `agmen role list` (name, layer, kind, tier, tools, verbs; `--json`), `agmen role show <name>` (the resolved XML and what the ceiling stripped), `agmen role import <subagent.md> [--to project|personal]` (writes `.agmen/roles/<name>.xml` or `~/.config/agmen/roles/<name>.xml`).
  - `agmen join <invite> --role <name>` (default `engineer`): resolves the role from `Layers { project: <repo>/.agmen/roles, personal: ~/.config/agmen/roles }`, mints with `RoleSpec`, and writes `role` and `role_sha256` into `.agmen/identity.json`.
  - `agmen mcp`: lists only the tools the identity's verbs and tool classes allow (`claim` needs CLAIM, `done` needs DONE, `lend` needs LEND, `review` needs REVIEW; `check_path` always). New tool `review {reply_to, verdict, findings, traced?}`. The `pending` block on every tool result (mvp) is unchanged.

- [ ] **Step 1: Write the failing tests** (`assert_cmd` with a fake hub, as mvp Task 12): `role_list_shows_builtins` (8 rows, `reviewer deep`), `role_show_reports_stripped` (project `listener.xml` with shell: output has `stripped: tool shell`), `role_import_writes_xml` (file exists, parses with Task 1), `join_with_role_mints_spec` (fake hub received `role: "reviewer"` and 3 tool classes), `mcp_lists_only_role_tools` (reviewer: `review`, `ask`, `answer`, `dm`, `inbox`, `state`, `check_path`, no `claim` or `done`; engineer: no `review`).
- [ ] **Step 2:** Run `cargo nextest run -p agmen-cli -E 'test(role_cli)'`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run the tests and the gate. Expected: all pass.
- [ ] **Step 5:** Commit: `Add agmen role commands, join --role, and role-filtered MCP tools`

---

## Phase B: agmend and the launcher

### Task 5: `agmend`: local API, launcher for Claude Code, worktrees, budgets, prune

**Files:**
- Create: `crates/agmend/Cargo.toml`, `crates/agmend/src/{main.rs,api.rs,state.rs,launch.rs,worktree.rs,budget.rs,service.rs}`, `crates/agmend/tests/{api.rs,worktree.rs,it_launch.rs,live_launch.rs}`
- Modify: `crates/agmen-cli/src/cmd/{launch.rs,worktree.rs,workers.rs}` (new), `Cargo.toml`

**Interfaces:**
- Consumes: Task 1 (`render_prompt`, `claude_tools`), Task 4 (mint with role), m0-m1 Task 13 local API frame format (`{v, id, method, params}`), design 6.7.
- Produces:
  - Local API methods: `hello`, `launch {agent:"claude", role, item?, lane?, budget_usd, base, branch?, brief}` returning `{identity_id, worktree, branch}`; `workers {project}`; `worker_stop {identity_id}`; `worker_budget {identity_id, add_usd}`; `activity {identity_id, after?, follow}`; `prune {confirm?: branch}` returning `{removed: [branch], kept: [{branch, reason}]}`.
  - `worktree.rs`: `pub fn create(repo, base, branch) -> Result<PathBuf>` (`git worktree add -b <branch> <path> <base>`), `pub fn merged_state(repo, branch, base) -> Merged { Ancestor, Cherry, No }` (`git merge-base --is-ancestor`, then `git cherry`), `pub fn remove(repo, path, branch) -> Result<()>` (`git worktree remove` then `git branch -d`, never `--force`), `pub fn save_wip(repo, path, branch) -> Result<Oid>` (temporary `GIT_INDEX_FILE`, `commit-tree`, `update-ref refs/wip/<branch>`). Branch name from `{type}/{item}-{slug}` with `slug` = brief words, lowercase, `-`, max 40 chars.
  - `launch.rs`: argv `claude -p --verbose --input-format stream-json --output-format stream-json --max-budget-usd <remaining> --session-id <uuid> --tools <claude_tools(role)> --mcp-config <file> --strict-mcp-config --append-system-prompt-file <rendered role>`; stdin kept open; stream-json to a rolling log under the state dir; `result.total_cost_usd` into `budget.rs`; thresholds 80% warn (ALERT normal), 95% wrap-up message on stdin, 100% stop the process group then `save_wip`. Workers stay up after DONE (design 6.7); `worker_stop` ends them.
  - `service install|uninstall|status` for launchd and systemd user units (m0-m1 Task 13).
  - CLI: `agmen launch --agent claude --role <r> --item --lane --budget --base --branch --brief`, `agmen workers`, `agmen worktree prune [--confirm <branch>]`, `agmen budget <id> --add`.

- [ ] **Step 1: Write the failing unit tests** (`worktree.rs`, against a temp git repo):

```rust
#[test] fn create_makes_branch_from_base()       // worktree exists; `git branch --list` has fix/b-1-slug; HEAD == base
#[test] fn merged_state_ancestor()               // merge the branch into base -> Ancestor
#[test] fn merged_state_cherry_after_rebase()    // rebase-merge the commit onto base -> Cherry
#[test] fn prune_removes_merged_clean()          // Ancestor + clean -> removed; worktree dir gone; branch gone
#[test] fn prune_keeps_dirty_worktree()          // Ancestor + an uncommitted file -> kept {reason:"uncommitted"}; refs/wip/<branch> exists and contains the file
#[test] fn prune_keeps_unmerged()                // No -> kept {reason:"not merged"}; `--confirm <branch>` removes it
#[test] fn never_forces()                        // the git argv log from the whole module contains no "--force" and no "-D"
```

- [ ] **Step 2: Write the failing API tests** (`api.rs`, fake `claude` script on PATH that echoes stream-json): `launch_renders_role_and_tools` (argv has `--tools Read,Grep,Glob,Edit,Write,NotebookEdit,Bash` for engineer and the prompt file starts with base's `<purpose>`), `launch_mints_with_role_spec` (fake hub got `role: "engineer"`), `budget_stops_at_cap` (fake claude emits cost 1.05 of 1.00: process group killed, `refs/wip/<branch>` written), `worker_survives_done` (fake claude posts DONE; process still running; `workers` shows `state: "idle"`).
- [ ] **Step 3:** Write `live_launch.rs`: `live_engineer_claims_and_dones` (real `claude`, real compose hub: a role-bound Engineer with brief "add tests/smoke.rs that asserts 1+1" posts CLAIM, DONE with `changes` and pushes to a local bare remote; budget 2.00).
- [ ] **Step 4:** Run `cargo nextest run -p agmend` and `--profile it`. Expected: FAIL.
- [ ] **Step 5:** Implement.
- [ ] **Step 6:** Run unit, it and (with `AGMEN_LIVE=1`) live tests, then the gate. Expected: all pass; record the live cost in the commit body.
- [ ] **Step 7:** Commit: `Add agmend with the Claude Code launcher, worktrees, budgets and prune`

---

## Phase C: the listener

### Task 6: Listener runtime, git-read and settings tools, delegation policy, reviews

**Files:**
- Create: `crates/agmend/src/listener/{mod.rs,policy.rs,settings.rs,queue.rs}`, `crates/agmen-cli/src/mcp/listener_tools.rs`, `crates/agmen-cli/src/cmd/{listener.rs,delegate.rs,settings.rs}`, `crates/agmend/tests/{listener.rs,live_listener.rs}`, `spikes/s7-dispatcher/fixtures/triage.jsonl` (reuse or create per m0-m1 Task 7)
- Modify: `crates/agmend/src/api.rs`, `crates/agmen-cli/src/mcp/mod.rs`

**Interfaces:**
- Consumes: Task 5 (`launch`, `workers`, budgets), Task 3 (listener kind, DELEGATED, REVIEW), m0-m1 Task 13 `pending {as}`.
- Produces:
  - `agmen listener start --project <p> [--agent claude] [--model claude-haiku-5-5] [--coordinator]`, `stop`, `status`, `set --model|--effort|--per-delegation|--daily|--max-workers|--accept-from +<handle>|-<handle>` (removing a coordinator is refused). Settings in `~/.config/agmen/settings.toml` `[listener.<project>]`.
  - Runtime: `claude -p --model <m> --input-format stream-json --output-format stream-json --verbose --tools Read,Grep,Glob --strict-mcp-config --mcp-config <listener.json> --append-system-prompt-file <rendered listener role>`; each batch is one stream-json user message holding the pending events as `{seq, from_handle, from_kind, verb, body, reply_to}` lines; at most one batch per 30 s.
  - `queue.rs`: what reaches it (design 6.11): its own mentions, DMs and assignments; its person's plain chat messages (no other mention), mentions, DMs and assignments; DONE, STATUS, REVIEW and ALERT from workers it started. Each batch gets a `batch_id`; the settings tools check it.
  - `agmen mcp --listener` tools: `git_log {rev_range?, n?}`, `git_show {rev, stat?}`, `git_diff {base, head}`, `git_status` (argv fixed by agmend, run in the project's main checkout, output capped at 32 KiB); `delegate {reply_to, brief, role, agent?, tier, budget_usd, item?, lane?, base?, branch?}`; `workers`; `stop_worker {worker}`; `settings_get {scope}`; `settings_apply {reply_to, changes: [{target, key, value}]}`; `roster`.
  - `policy.rs`: `pub fn check_delegate(req, msg_from, settings, today_spend, running) -> Decision { Allow, Pending(reason), Refuse(reason) }`; `accept_from` always includes the person and coordinators.
  - `settings.rs`: `pub fn classify(change) -> Direction { Narrow, Widen }` (widen: higher cap or budget, added tool class or lane or LEND, push approval `off`, added `accept_from`); `settings_apply` refuses unless `reply_to`'s hub-stamped `from` is the listener's person and the message is in the current `batch_id` (`stale_request`); `Narrow` applies now and posts an FYI change card (before, after, `undo:<id>`); `Widen` stores a pending change and posts the card with `apply:<id>`; `agmen settings apply|undo <id>` and the local API `settings_decide {id, apply: bool}` run as the human.
  - Reviews: on DONE from a worker the person owns and `reviews != "off"`, agmend launches a one-shot `reviewer` (tier deep, budget `per_delegation_usd`) with a detached worktree at the DONE's commit (`git worktree add --detach`), which is removed after its REVIEW. `agmen listener set --reviews off` turns it off.
  - Loop rule: at most 3 listener posts per thread without a human or worker post; agmend drops and logs the rest.

- [ ] **Step 1: Write the failing tests** (`listener.rs`, fake `claude` and fake hub):

```rust
#[tokio::test] async fn queue_includes_persons_plain_chat()   // human posts FYI with no mentions -> in the listener's next batch; an agent's unmentioned FYI is not
#[tokio::test] async fn delegate_from_stranger_refused()       // delegate reply_to a message from another person -> Refuse; from a coordinator -> Allow
#[tokio::test] async fn coordinator_cannot_be_removed()        // set --accept-from -adam/coord -> error "coordinators always reach your listener"
#[tokio::test] async fn delegate_over_cap_is_pending()         // budget_usd 6.00 with per_delegation 5.00 -> Pending; ALERT posted; `agmen delegate approve` launches
#[tokio::test] async fn delegated_posts_with_agent_and_role()  // after Allow: DELEGATED {agent:"claude", role:"engineer", tier, budget_usd} with reply_to = request
#[tokio::test] async fn settings_rejects_message_outside_batch() // settings_apply reply_to from the previous batch -> "stale_request"; nothing changed
#[tokio::test] async fn settings_rejects_other_person()         // reply_to a coordinator's message asking to raise a cap -> refused; listener's answer says only the owner can
#[tokio::test] async fn narrowing_applies_with_undo()           // lower fx1 budget 5 -> 3: applied; card posted with undo id; `agmen settings undo <id>` restores 5
#[tokio::test] async fn widening_waits_for_apply()              // raise budget 5 -> 8: not applied; card with apply id; `agmen settings apply <id>` applies; audit log has both rows
#[tokio::test] async fn done_starts_one_shot_reviewer()         // worker DONE -> a reviewer launch with --tools Read,Grep,Glob,Bash and a detached worktree at the commit; after its REVIEW the worktree is gone
#[tokio::test] async fn listener_thread_cap()                   // 4 listener posts in one thread with no reply -> the 4th is dropped and logged
#[tokio::test] async fn git_read_tools_fixed_args()             // git_log {rev_range:"main..fix/x; rm -rf /"} -> argv is ["git","log","--oneline","-n","50","main..fix/x; rm -rf /"] passed as one argument, no shell
```

- [ ] **Step 2:** Write `live_listener.rs` (real `claude-haiku-5-5`, `AGMEN_LIVE=1`): `live_lookup_answers_from_git` (batch asks "did commit <sha> land on main?"; the listener calls `git_log` and ANSWERs with the sha, no `delegate` call), `live_assign_delegates_engineer` (ASSIGN batch -> exactly one `delegate` with role `engineer`, reply_to = the ASSIGN id), `live_injection_never_delegates_or_widens` (every injection fixture: no `delegate`, no `settings_apply`, no RULING).
- [ ] **Step 3:** Run `cargo nextest run -p agmend -E 'test(listener)'`. Expected: FAIL.
- [ ] **Step 4:** Implement.
- [ ] **Step 5:** Run unit tests, live tests with `AGMEN_LIVE=1`, and the gate. Expected: all pass; record per-turn cost and the triage table in `docs/adr/0007-listener-runtime.md` (decision rule from m0-m1 Task 7: keep haiku at 90% or better with zero injection delegations, else `sonnet`).
- [ ] **Step 6:** Commit: `Add the listener: runtime, git-read and settings tools, delegation policy, reviews`

### Task 7: End-to-end exit tests

**Files:**
- Modify: `tests/e2e/tests/it_m1.rs` (mvp Task 15 crate), add `tests/e2e/tests/it_roles_listener.rs`

- [ ] **Step 1: Write the exit tests** with two agmend instances in two temp homes and the compose hub, using a fake `claude` scripted per test (no model cost):

```rust
#[tokio::test] async fn it_assign_away_person_delegates()     // coordinator ASSIGNs B-7 to andy while andy's listener runs: DELEGATED then DONE appear in the thread from a worker whose roster entry has role "engineer" and delegated_by = the listener
#[tokio::test] async fn it_role_verb_refused_end_to_end()     // the worker posts RULING -> watch sees nothing; its ack stream has Nack forbidden_verb
#[tokio::test] async fn it_review_reaches_engineer()           // after DONE, a REVIEW from the one-shot reviewer lands in the engineer's pending queue and on its stdin
#[tokio::test] async fn it_settings_widen_needs_click()        // andy's chat "raise fx1's budget to $8": card posted, budget still 5; `agmen settings apply <id>` on andy's machine -> 8; the same text from adam changes nothing
#[tokio::test] async fn it_merged_branch_pruned()              // merge the worker's branch in the bare remote; `agmen worktree prune` removes the worktree; a second worker with an uncommitted file is kept with a wip ref
```

- [ ] **Step 2:** Run `cargo nextest run -p agmen-e2e --profile it`. Expected: PASS (all five, plus the MVP's).
- [ ] **Step 3:** Commit: `Add roles and listener end-to-end exit tests`

## Spec coverage

| Spec item | Task |
|---|---|
| Role format, layers, ceilings, render, import (design 6.13) | 1, 4 |
| Protocol: listener kind, REVIEW, PLAN, DELEGATED, ALERT fields, role mentions, refs, NACKs (protocol 1, 3, 4, 5.4) | 2 |
| Roles at mint, verb caps, lane owners and LEND, REVIEW mentions, role mention expansion, refs stamping (protocol 1, 3, 4, 6) | 3 |
| `join --role`, MCP tool filtering, `review` tool (design 6.8, 6.13) | 4 |
| Launcher with roles, worktree lifecycle, workers after DONE, budgets (design 6.7) | 5 |
| Listener runtime, tools, policy, settings from chat, reviews, loop cap (design 6.11) | 6 |
| Exit checks for M3's listener items (design 7) | 7 |

Not in this plan: PLAN verb handling beyond types (planning rooms), ALERT urgency UI, `refs` rendering, hour and token caps, ACP agents, switching, take-over, consoles, GitHub App.
