# Agent trees and settings (Phase 5, current runner): implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an agent agmen launched launch child agents through agmen, under role launch lists and safeguard settings that each turn off in one change, and let parents and the person stop a subtree, all on today's per-batch Claude Code runner.

**Architecture:** agmend's `Managed` (`crates/agmend/src/managed.rs`) gains a parent link per worker record, a child to parent index, and two identity-scoped local API methods, `child_launch` and `child_stop`, which reuse the existing launch path under the existing `launching` lock. A child's ancestors' messages run in the others tier like anyone else's (ruling 1, reversed until Phase 4), and the tree methods are refused from an others-tier batch. The runner hands each batch `AGMEN_IDENTITY` and `AGMEN_LOCAL_TOKEN`, in its environment and in its private `mcp.json`, so `agmen launch` (CLI) and the MCP `launch` tool call those methods as the parent. Safeguards live in the `[agents]` table of `~/.config/agmen/settings.toml`: one serde type in `agmen-local`, read by agmend and written by the desktop's Settings, Agents page. The hub's `delegated_by` check widens from "a live listener" to "any live identity of this owner and project".

**Tech Stack:** Rust 2024 workspace, tokio, serde, toml and toml_edit (both already in agmend and agmen-desktop), rmcp (agmen-cli), gpui (agmen-desktop), sqlx and Postgres (agmen-hub `it` tests). No new dependencies.

**Spec:** `docs/specs/2026-10-29-solo-mode-and-agent-trees-design.md` on `origin/design/solo-mode` (sections Agent trees, Safeguard settings, Changes to existing documents). Roadmap: `docs/plans/2026-10-29-solo-mode.md` on the same branch, row 5. Owner rulings: the last section of this file.

## Global Constraints

- Defaults: tree depth 4, agents running per person 16, budgets on, child budgets shared with the parent, separate child budget 5.00 USD.
- The person's own launch is depth level 1 (ruling 2).
- Every safeguard can be turned off in one change, with no confirmation dialog, and shows its warning directly under the setting while off.
- Depth warning: the spec's text, verbatim, plus ruling 3's line: "Agents can launch agents without limit. A looping agent can start processes until the machine or your API rate limit runs out. agmend still stops at 256 agents."
- Agents warning (rulings 3 and 8): "Agents can launch any number of agents across all trees. A looping agent can start processes until the machine or your API rate limit runs out. agmend still stops at 256 agents."
- Budgets warning, verbatim: "No spending caps. Agents spend until you stop them."
- agmend registers at most 256 managed agents (`MAX_MANAGED`, ruling 3). It is not a setting. One shared constant drives the registry bound, the store's identity bound and the identity session bound, so they cannot drift apart.
- Until Phase 4, a child's ancestors' messages run in the others tier (ruling 1). A child briefed only by its parent can read and reply but cannot run commands or edit files; real work needs a brief or an approval from the person.
- When a limit is reached, agmend refuses the launch and the launching agent gets an error it can report. Nothing already running is stopped.
- Shared budgets: a child's cap comes out of its parent's remaining budget at launch, and the parent's spent figure includes its whole subtree. A child amount that would leave the parent under 0.50 USD is refused (ruling 6c).
- A role lists the roles it may launch; a role with no list may launch nothing. Launched children are kind `agent` only (ruling 5).
- A parent may stop any agent below it; the person may stop any of their agents; siblings cannot stop each other; stopping stops the whole subtree, without asking, until Phase 6 adds the question (ruling 12).
- Only agents agmen launched can launch (ruling 13).
- An invalid `[agents]` table refuses launches with the parse error; agmend never falls back to the defaults silently (ruling 14).
- The CLI ships `launch`, not `delegate`. It reads `AGMEN_IDENTITY` and `AGMEN_LOCAL_TOKEN` and has no `--coordinator` flag (Andy, 2026-10-29).
- The local token is never written to a log, a worktree, an answer or an error. Its only file is the agent's 0600 `mcp.json` under agmend's state folder (ruling 9).
- The listener stays headless; there is no coordinator listener for now (Andy, 2026-10-29).
- Every loop, wait, buffer and retry has an explicit bound.
- Workspace lints forbid `unsafe_code` and deny `unwrap_used`, `expect_used`, `panic` (root `Cargo.toml:28` to `33`).
- No new dependencies unless named and justified; each needs owner approval.
- Changing an existing test's assertion needs owner approval. This plan marks each one with "OWNER APPROVAL".

## What builds now and what waits

Builds now, on the per-batch runner (`crates/agmend/src/launch.rs`, `ClaudeRunner::run` at `:484`):

- Safeguard settings type and agmend reader (Task 1), the desktop's Settings, Agents page (Task 10).
- Role launch lists in role files and built-in roles (Task 2).
- The hub's widened `delegated_by` check (Task 3).
- Parent links, the tier guard, depth and count limits, the 256 bound, `child_launch` with a brief (Task 4).

Consequence of ruling 1 until Phase 4: the batch tiers stay as they are (`managed.rs:831` to `833`). A child's batch is in the owner tier only when every message is from the human owner. A brief or message from its parent puts it in the others tier, where Claude runs without Bash, Edit, Write or NotebookEdit (`launch.rs:73` to `79`). So a child briefed only by its parent can read and reply but cannot edit; real work needs a brief or approval from the person.
- Budgets on and off, shared or separate child budgets (Task 5).
- Stop subtree for the person and for a parent (Task 6).
- After L21-W merges: the credential in each batch's environment and `mcp.json` (Task 7), `agmen launch` and `agmen workers stop` as a parent (Task 8), MCP `launch` and `stop_agent` (Task 9).

Not in this plan:

- SpawnWorker already launches `claude` on `release/next` (3ec0926, L20r), with L20r's `worker-N` names, which stay. Later desktop follow-up (ruling 15): SpawnWorker uses the desktop's "Default agent model" when it is set, and agmend's default otherwise.
- Phase 4 (terminal delivery): children as terminal agents in agmend's PTYs and any agent kind other than Claude Code; "waiting on me" for a child's prompt, relayed to its parent; a parent reading a child's terminal; in-process subagents and `@parent/child`; cost from interactive sessions; removing the owner and others batch tiers (`launch.rs:61` to `79`).
- Phase 6: the agent tree view in the desktop, terminal UI and web console, the "stop the subtree too?" question (ruling 12), Recents and Quit Everything stopping children first.
- The listener's `delegate` rebriefed as `launch`, and the listener's `max_workers` counting toward agents per person. The listener runtime is not wired yet (`crates/agmend/src/listener/*.rs` carry `expect(dead_code, reason = "wired by L21")`), and ruling 13 limits launching to agents agmen launched.
- The design.md edits for 6.3, 6.11 and 6.13 belong to Phase 1 Task 6 of the roadmap file.

## Review Focus

1. **A stranger's request laundered through a parent (SECURITY-RELEVANT).** A stranger who reaches a parent puts the parent in an others-tier batch. From there `child_launch`, `child_stop` and a brief are refused, so the stranger cannot start or stop agents through it. A plain `dm` or `post` from that parent still reaches the child, and the child's next batch must run in the others tier, because ancestors' messages get no owner authority (ruling 1). Pinned by Task 4's `tree_methods_are_refused_from_an_others_tier_batch`, `ancestor_messages_run_in_the_others_tier` and `parent_dm_from_an_others_tier_batch_keeps_the_child_in_the_others_tier`.
2. **A launch chain that loops** (an agent whose role may launch its own role keeps launching): with both limits on, the depth or count refusal stops it; with both off, `MAX_MANAGED = 256` stops it with an error, never a hang. A record on disk with a parent cycle must not make any walk loop. Pinned by Task 4's `launch_chain_stops_at_the_registry_bound_with_limits_off` and Task 6's `parent_cycle_on_disk_stops_without_looping`.
3. **Two children launched at the same moment when one slot is left**: exactly one succeeds. Pinned by Task 4's `concurrent_child_launches_respect_the_count`.
4. **Budget sharing when a child stops, is removed, or fails to launch, or the parent stops**: the child's unspent reservation returns to the parent, its spend stays counted, a failed launch returns the whole reservation, and a stopped parent leaves running children their caps. Pinned by Task 5's `stopped_child_returns_its_unspent_reservation`, `failed_child_launch_returns_the_reservation` and Task 6's `parent_stop_without_subtree_keeps_child_caps`.
5. **A forged parent or a leaked token**: an agent cannot name its own parent (agmend takes it from the authenticated caller), cannot launch through the human `agent_launch`, and the hub refuses a `delegated_by` of another owner, another project or a revoked identity. The token appears in no log, worktree file or answer. Pinned by Task 4's `child_launch_refuses_a_supplied_parent` and `worker_peer_may_call_child_methods_only_with_identity`, Task 3's `it_delegated_by_any_live_parent_of_owner`, and Task 7's `token_stays_out_of_worktrees_and_answers`.

---

### Task 1: Safeguard settings type and agmend reader

**Files:**
- Create: `crates/agmen-local/src/safeguards.rs`, `crates/agmen-local/src/safeguards_tests.rs`, `crates/agmend/src/safeguards.rs`, `crates/agmend/src/safeguards_tests.rs`
- Modify: `crates/agmen-local/src/lib.rs` (add `pub mod safeguards;`), `crates/agmend/src/lib.rs` (`Config` at `:129` to `140` gains `settings_file: Option<PathBuf>`; `Config::from_env` at `:172` to `188` sets it to `~/.config/agmen/settings.toml` beside `personal_roles`; add `mod safeguards;`)

**Interfaces:**
- Produces, in `agmen_local::safeguards`:
  - `pub const TABLE: &str = "agents";`
  - `pub const MAX_MANAGED: usize = 256;` (ruling 3): the one constant behind agmend's registry, store identity and identity session bounds (Task 4). `pub const MAX_LIMIT: u32 = 256;` with `const _: () = assert!(MAX_LIMIT as usize == MAX_MANAGED);`, so the settings range cannot drift from it.
  - `pub const DEPTH_OFF_WARNING: &str`, `pub const AGENTS_OFF_WARNING: &str`, `pub const BUDGETS_OFF_WARNING: &str`, with the Global Constraints text.
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "lowercase")] pub enum ChildBudgets { Shared, Separate }`
  - `#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)] #[serde(default)] pub struct Safeguards { pub limit_tree_depth: bool, pub max_tree_depth: u32, pub limit_agents: bool, pub max_agents: u32, pub budgets: bool, pub child_budgets: ChildBudgets, pub separate_child_usd: f64 }` with `Default` = `true, 4, true, 16, true, Shared, 5.0` (ruling 7). No `deny_unknown_fields`, so a newer desktop's keys do not break an older agmend.
  - `pub fn validate(&self) -> Result<(), SafeguardsError>`: each max in `1..=MAX_LIMIT`; `separate_child_usd` finite, at least 0.01 and at most 1000.
  - `#[derive(Debug, thiserror::Error)] pub enum SafeguardsError { #[error("{key} must be 1 to 256, not {value}")] OutOfRange { key: &'static str, value: u32 }, #[error("separate_child_usd must be 0.01 to 1000")] SeparateChildUsd }`
- Produces, in `crates/agmend/src/safeguards.rs`: `pub(crate) fn load(path: Option<&Path>) -> Result<Safeguards, ApiError>`. A missing path, file or `[agents]` table gives the defaults. It reads at most 1 MiB (as `listener/settings.rs:18`), parses with `toml`, takes the `agents` table, deserializes and validates. An unreadable or invalid file is `ApiError { code: BadRequest, message: "settings.toml [agents]: <reason>" }` (ruling 14: no fallback).

- [ ] **Step 1: Write the failing tests.**
  - `agmen-local`: `defaults_match_the_rulings` (`Safeguards::default()` equals `{true, 4, true, 16, true, Shared, 5.0}`); `limits_are_bounded` (`max_tree_depth` 0 and 257 fail, 1 and 256 pass; `separate_child_usd` 0.0, -1.0, NaN and 1000.01 fail); `child_budgets_spell_lowercase`; `warnings_end_with_the_registry_line` (depth and agents warnings end with "agmend still stops at 256 agents."; the depth warning starts with the spec's sentence byte for byte; the budgets warning equals the spec's text).
  - `agmend`: `missing_file_or_table_gives_defaults`; `agents_table_is_read` (`[agents]\nlimit_tree_depth = false\nmax_agents = 3\nseparate_child_usd = 2.5\n`); `other_tables_are_ignored`; `invalid_value_is_refused_not_defaulted` (`max_agents = 0`, `budgets = "yes"` and an unparsable file each give `ErrorCode::BadRequest` starting with `settings.toml [agents]`); `oversized_file_is_refused`.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmen-local -E 'test(/safeguards/)'` and `cargo nextest run -p agmend -E 'test(/safeguards/)'`. Expected: compile errors.
- [ ] **Step 3: Implement** the items above. `Config::new` leaves `settings_file` `None`, so existing tests read the defaults.
- [ ] **Step 4: Run the tests.** Expected: pass. `cargo clippy -p agmen-local -p agmend --all-targets -- -D warnings`: clean.
- [ ] **Step 5: Commit.** `git commit -m "Add agent safeguard settings and agmend's reader"`

### Task 2: Role launch lists

**Files:**
- Modify: `crates/agmen-roles/src/schema.rs` (`Role` at `:139` to `164`; `validate` at `:198` to `213` allows element `launches` with attribute `roles`; `parse_inner`'s `Ok(Role {` at `:320`), `crates/agmen-roles/src/layer.rs` (`Resolved` at `:66` to `87`; the widening check at `:296` to `305`; `Ok(Resolved {` at `:341`), `crates/agmen-roles/src/compat.rs:280` (`launches: None`), `roles/coordinator.xml`, `roles/engineer.xml`, `roles/planner.xml`
- Test: `crates/agmen-roles/tests/roles.rs`

**Interfaces:**
- Produces:
  - `Role.launches: Option<BTreeSet<String>>`: `None` without `<launches>`, else the names in `<launches roles="a b"/>`, each checked with `valid_name` (`schema.rs:177`).
  - `Resolved.launches: BTreeSet<String>`: the nearest declared list in the inheritance chain, else empty. A role extending a non-base role may only narrow it; a wider list is `RoleError::Widening` (the check at `layer.rs:296`).
- Built-in lists (ruling 4): coordinator `engineer reviewer tester researcher planner`; engineer `reviewer tester`; planner `researcher`; all others none.

- [ ] **Step 1: Write the failing tests** in `tests/roles.rs`: `launches_parses_and_resolves`; `no_launches_means_none` (reviewer); `launches_inherit_and_may_only_narrow` (`extends="engineer"`: none inherits `{reviewer, tester}`, `reviewer` narrows, `engineer` is `RoleError::Widening`); `launches_rejects_bad_names_and_attributes` (`roles="Engineer"`, `roles="../x"`, an extra attribute); `builtin_launch_lists` (the three lists above).
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmen-roles --test roles`. Expected: FAIL.
- [ ] **Step 3: Implement** the field, the element, inheritance and the widening rule, and add `<launches .../>` after `<worktree>` in the three built-in files. This changes those roles' `sha256`, so `batch_role` (`managed.rs:579` to `593`) refuses the next batch of any running agent on coordinator, engineer or planner with "role ... changed since launch; relaunch the agent to use it" (ruling 4 accepts this).
- [ ] **Step 4: Run** `cargo nextest run -p agmen-roles -p agmen-local -p agmend`. Expected: pass; clippy clean.
- [ ] **Step 5: Commit** with a body for the release notes:
  ```
  git commit -m "Let a role list the roles it may launch" -m "Built-in coordinator, engineer and planner roles change hash: agents running on them need a relaunch."
  ```

### Task 3: Hub accepts any live parent in delegated_by

**Files:**
- Modify: `crates/agmen-hub/src/identities.rs:310` to `326`
- Test: `crates/agmen-hub/tests/it_roles.rs`

**Interfaces:**
- Produces: `POST /v1/identities` with `delegated_by` succeeds when that id is a live identity (any kind the CHECK at `migrations/0006_roles.up.sql:3` to `4` allows: agent, coordinator, listener) of the same owner and project, and the new identity's kind is `agent` (ruling 5). The query drops `AND kind='listener'` and keeps `FOR UPDATE`. The refusal stays 422 `bad_role` with "delegated_by must name a live identity of this owner and project for an agent child".
- `ingest.rs:292` to `305` (DELEGATED's worker check) is unchanged.

- [ ] **Step 1: Change the existing assertion (approved by the owner review).** In `it_delegated_by_validated_and_rostered`, the block at `it_roles.rs:832` to `838` mints `andy/bad` with `delegated_by` = an agent worker and asserts 422. Rewrite it to keep the 422 with another person's identity as the parent: mint `adam`'s agent with `member_with_token` and `mint`, then mint `andy/bad` with `delegated_by` = adam's agent, and assert `StatusCode::UNPROCESSABLE_ENTITY`. Then add a separate assertion: `andy/child` with `delegated_by` = `worker["id"]` (andy's own live agent) is `StatusCode::CREATED`.
- [ ] **Step 2: Write the failing test** `it_delegated_by_any_live_parent_of_owner`, with the file's `room`, `mint`, `mint_ok`, `role`, `member_with_token` and `roster` helpers: an agent parent gives 201 and the roster row has `delegated_by == parent`; a grandchild under that child gives 201; a coordinator parent gives 201; an agent in another project gives 422; a revoked parent gives 422; `kind: "coordinator"` or `"listener"` with a valid `delegated_by` gives 422.
- [ ] **Step 3: Run to see them fail.** Run: `cargo nextest run -p agmen-hub --profile it -E 'test(/it_delegated_by|it_fix1_delegation|it_coordinator_listener_assign/)'` (needs the dev stack in `deploy/dev`). Expected: the 201 cases FAIL.
- [ ] **Step 4: Implement** the query change.
- [ ] **Step 5: Run the same command.** Expected: pass, including `it_fix1_delegation_waits_for_listener_revocation` (`it_roles.rs:186`) and `it_coordinator_listener_assign_cap_and_delegated_worker_check` (`:861`; its `:871` outsider case stays 422).
- [ ] **Step 6: Commit.** `git commit -m "Hub: delegated_by may name any live identity of the owner and project"`

### Task 4: Parent links, the tier guard, limits and child_launch

**SECURITY-RELEVANT: the tier guard.** `child_launch`, `child_stop` and the brief are refused from an others-tier batch, so a stranger who reaches a parent cannot launch, stop or brief agents through it. This task does not widen the owner tier: a child's ancestors' messages stay in the others tier (ruling 1, reversed until Phase 4).

**Files:**
- Create: `crates/agmend/src/managed_tree.rs` (included from `managed.rs` with `#[path = "managed_tree.rs"] mod tree;`, as `managed_actions.rs` is at `managed.rs:1111` to `1114`), `crates/agmend/src/managed_tree_tests.rs` (included from `managed_tests.rs` as `#[path = "managed_tree_tests.rs"] mod tree;`, so it can use `Fixture`, `worker_call`, `enqueue_from`, `batch` and `restarted`)
- Modify:
  - `crates/agmend/src/managed.rs`: `MAX_AGENTS` at `:27` is replaced by `agmen_local::safeguards::MAX_MANAGED` (256) at every use (`:187`, `:238`, `managed_actions.rs:195`, `:505`, `:1201`, `:1217`); `Record` `:47` to `68`; `Worker` `:81` to `94`; `Managed` `:126` to `138` and `new` `:144` to `163` gain `settings_file: Option<PathBuf>` and `tree: Mutex<BTreeMap<IdentityId, Option<IdentityId>>>`; `launch` `:231` to `311`; `complete_launch` `:313` to `398`; `mint` body `:408`; `list` entry `:615`; `tick` batch spawn `:842` to `853`; `run_worker` `:858` to `870`. `tick`'s owner tier at `:831` to `833` is unchanged.
  - `crates/agmend/src/managed_actions.rs`: `Stored` `:86` to `108`, `into_record` `:112` to `158`, `load` `:168` to `201`, `adopt`'s bound at `:505` to `506`.
  - `crates/agmend/src/state.rs:337` to `338` (the literal 64 identity bound and its "64 managed identities" text use `MAX_MANAGED`; the 64-project bound at `:343` to `344` stays).
  - `crates/agmend/src/identities.rs:774` to `775` (the literal 64 session bound and its text use `MAX_MANAGED`; the 64-project collectors bound at `:507` stays).
  - `crates/agmend/src/lib.rs:280` to `286` (pass `config.settings_file`), `crates/agmend/src/api.rs` (`owner_peer_check` `:150` to `151`; `identity_call` `:271` to `296`).
- Test fixture: `managed_tests.rs` gains `tree_fixture()`, built like `role_fixture()` (`:22` to `47`, including its `owner/reject` refusal at `:34`); its fake hub records each mint body and answers id `agent001` for `owner/worker` and `kid00001`, `kid00002` and so on for other handles in mint order, with verbs `["FYI"]`. Add `enqueue_from_agent(fixture, seq, sender, recipient)`, which is `enqueue_from` (`:1257`) with `"from_kind":"agent"`.

**Interfaces:**
- Consumes: `Safeguards`, `MAX_MANAGED`, `crate::safeguards::load` (Task 1); `Resolved.launches` (Task 2).
- Produces:
  - `Record` and `Stored` gain `#[serde(default, skip_serializing_if = "Option::is_none")] parent: Option<IdentityId>`, `#[serde(default = "root_depth")] depth: u32` (`fn root_depth() -> u32 { 1 }`, ruling 2) and `#[serde(default)] shared_budget: bool`. Older `agent.json` files load as roots.
  - `Managed.tree` maps each worker to its parent. `launch_at`, `load` and `remove` keep it current. Every walk over it (ancestors, depth, descendants) keeps a visited set and stops after `MAX_MANAGED` steps. Lock order is `launching`, then `tree`, then `workers`, then one worker; no code holds one worker's lock while taking another's.
  - `Worker` gains `batch_tier: Option<Tier>` with `enum Tier { Owner, Others }`, set from `owner_only` when `tick` spawns a batch (`:842` to `853`) and cleared by `run_worker` when the batch ends (`:868` to `869`). The tier rule itself is unchanged: owner only when every row is from the human owner (`:831` to `833`); an ancestor's message is others tier (ruling 1).
  - Tier guard (SECURITY-RELEVANT): `child_launch` and `child_stop` (Task 6) first check the caller worker's `batch_tier`. Anything but `Some(Tier::Owner)` is `forbidden_with("launching, stopping or briefing agents needs a batch with only your owner's messages; this batch includes other senders")`, with `ErrorCode::Forbidden`.
  - `child_launch {name, role, model?, budget_usd?, brief?}`, identity-scoped: `pub(crate) async fn launch_child(self: &Arc<Self>, caller: &ManagedIdentity, raw: Value) -> Result<Value, ApiError>`. Params `ChildLaunch { name: String, role: String, model: Option<String>, budget_usd: Option<serde_json::Number>, brief: Option<String> }` with `deny_unknown_fields`; name, model and budget checks are `validate_launch`'s (`managed.rs:193` to `229`). The answer is `launch`'s plus `"parent"` and `"depth"`.
  - Brief (ruling 11): after the child is registered and its HELLO is queued (`managed.rs:388` to `392`), agmend posts `envelope(Payload::Dm {}, brief)` with `to: vec![child id]` as the parent identity through `identities.post` (the durable outbox). The hub stamps it from the parent; it reaches the child's pending queue and is its first batch. That batch runs in the others tier (ruling 1), so the child can read and reply but not edit until the person messages it. A brief that the envelope's body limit refuses fails the launch before minting.
  - `async fn admit(&self, parent: Option<&Record>, owner: &IdentityId, safeguards: &Safeguards) -> Result<u32, ApiError>`, called with `launching` held; returns `1` for the person's launch, `parent.depth + 1` for a child.
  - `launch` and `launch_child` share `async fn launch_at(self: &Arc<Self>, launch: Launch, root: PathBuf, parent: Option<Record>, brief: Option<String>) -> Result<Value, ApiError>`, extracted from `launch` `:241` to `310`. For a child, `root` is the parent record's `worktree`, so `worktree::create` branches from the parent's current `HEAD` (ruling 10). `launch_at` takes `launching` before reading safeguards and holds it through the insert, as `launch` does at `:234`.
  - `mint` adds `"delegated_by"` only when there is a parent. `list` entries gain `"parent"` and `"depth"`.
  - API: `identity_call` routes `"child_launch"`. `owner_peer_check` lets `Caller::Identity` call `child_launch` and `child_stop` from a worker peer, as it lets `post` (`api.rs:150`). `Caller::Human` gets NotFound for both.
- Refusals, exact text:
  - caller not a managed worker of this login: `bad_request("only agents agmen launched can launch agents")` (ruling 13)
  - role list: `bad_request("role {parent} may not launch {child}; it may launch: {names, comma separated, or nothing}")`; a parent with no role is `(none)` and launches nothing.
  - child role of kind coordinator or listener: `bad_request("a launched agent's role must be an agent role")` (ruling 5)
  - depth: `bad_request("tree depth limit {max} reached: {parent handle} is at depth {depth}. Raise or turn off Tree depth in Settings, Agents")`
  - count: `bad_request("{max} agents are already running for you. Raise or turn off Agents running per person in Settings, Agents")`
  - registry: `bad_request("256 managed agents are already registered. agmend stops at 256 agents")` at `managed.rs:239` and `managed_actions.rs:506`.
  - settings: `crate::safeguards::load`'s error (ruling 14).

- [ ] **Step 1: Change an existing assertion (approved by integrally-dc, 2026-10-29).** `registration_and_stream_counts_are_bounded` (`crates/agmend/src/state_tests.rs:816` to `826`) registers 64 identities (`for n in 1..64` plus `who`) and asserts the next is `Err(StoreError::Full(_))` (`:826`). Rewrite it to assert that the 256th identity is accepted and the 257th is refused with `StoreError::Full`: loop `1..MAX_MANAGED` (255 registrations plus `who`), assert the last of them is Ok, then assert `extra001` is `Full`. Keep the rest of the test as it is.
- [ ] **Step 2: Write the failing tests** in `managed_tree_tests.rs`. Each writes a project role `chain.xml` (`<role name="chain" extends="base" version="1"><summary>c</summary><launches roles="chain"/></role>`) where `role_layers_use_main_checkout_and_personal_precedence` (`managed_tests.rs:282`) writes its project role, writes `[agents]` settings to a file set as the daemon's `settings_file`, launches `owner/worker` with role `chain`, sets the caller worker's `batch_tier` to `Some(Tier::Owner)` unless a test says otherwise, and calls children with framed requests carrying `as` and `identities.local_token(id)` from peer 4242 (a worker peer, `managed_tests.rs:514`).
  - `child_launch_records_parent_and_depth`: answers `parent:"agent001"`, `depth:2`; `agent_list` agrees; the second mint body has `"delegated_by":"agent001"`; the first has none.
  - `tree_methods_are_refused_from_an_others_tier_batch`: with `batch_tier` `Some(Tier::Others)`, and again with `None`, `child_launch` (with and without `brief`) is Forbidden with the tier text and nothing is minted or posted. After `enqueue_from(.., "sender01", "agent001")` and a real `batch`, the run is others tier and a `child_launch` made while it runs is refused.
  - `ancestor_messages_run_in_the_others_tier`: `k1` under the worker; `enqueue_from_agent(.., "agent001", "kid00001")` then `batch`: the run has `owner_only == false` and the others-tier `--disallowedTools` value `Bash,Edit,Write,NotebookEdit`.
  - `parent_dm_from_an_others_tier_batch_keeps_the_child_in_the_others_tier`: `enqueue_from(.., "sender01", "agent001")` and `batch` put the worker in an others-tier batch; while it runs, a DM from `agent001` to `kid00001` is enqueued with `enqueue_from_agent`; the child's next batch has `owner_only == false`. (A managed agent's `dm` and `post` go to the hub through the MCP helper's held session, `crates/agmen-cli/src/mcp/tools.rs:435` then `Agent::send` at `crates/agmen-cli/src/joined.rs:271` and `publish_held` at `:359`, so agmend never sees them; the test feeds the child's pending queue directly, as the hub would.)
  - `brief_reaches_the_child_as_a_dm_from_the_parent`: `child_launch` with `brief:"review src/a.rs"`: the captured posts include a `Payload::Dm` posted as `agent001` with `to == ["kid00001"]` and that body; the child's first batch has `owner_only == false`.
  - `child_launch_refuses_a_role_not_in_the_list`, `role_without_a_list_launches_nothing`, `child_role_must_be_an_agent_role` (a personal role `boss` of kind coordinator in `chain`'s list is refused).
  - `depth_limit_counts_levels`: `max_tree_depth = 2`: the person's worker is depth 1; worker to `k1` Ok (depth 2); `k1` to `k2` refused naming `owner/k1` at depth 2; with `limit_tree_depth = false`, Ok at depth 3.
  - `agent_count_counts_running_agents`, `concurrent_child_launches_respect_the_count` (`tokio::join!` two launches with one slot left: exactly one Ok), `human_launch_applies_the_count`.
  - `launch_chain_stops_at_the_registry_bound_with_limits_off`: both limits off; a loop of at most 260 `child_launch` calls, each from the newest child; the call after the 256th registered agent returns the registry text and the loop ends.
  - `invalid_agents_table_refuses_launch`: `max_agents = "many"`: both `launch` and `child_launch` fail with the `settings.toml [agents]` message.
  - `child_launch_refuses_a_supplied_parent`: `"parent"` or `"delegated_by"` in params is BadRequest; nothing minted.
  - `worker_peer_may_call_child_methods_only_with_identity`: `owner_peer_check` with `Caller::Identity` and `child_launch` from 4242 is Ok; with `Caller::Human` it is Forbidden "agents cannot call owner-only methods"; a framed `child_launch` with no `as` answers NotFound.
  - `stored_record_without_tree_fields_loads_as_root`.
  - `child_worktree_branches_from_the_parent_head`: after a commit in the parent's worktree, `k1`'s branch contains it and `k1`'s worktree is a sibling of the parent's. Whether `worktree::create` places a worktree made from a linked worktree beside the parent is unverified; this test settles it.
- [ ] **Step 3: Run them to see them fail.** Run: `cargo nextest run -p agmend -E 'test(/tree::/)'`. Expected: compile errors.
- [ ] **Step 4: Implement** the interfaces above.
- [ ] **Step 5: Run** `cargo nextest run -p agmend`. Expected: pass, including `launch_without_role_keeps_todays_args` (`launch_tests.rs:60`), `worker_peer_cannot_call_owner_methods_even_without_identity_credentials` (`managed_tests.rs:941`), `framed_owner_request_from_worker_is_forbidden` (`:1005`), `mixed_queue_runs_owner_batch_before_default_batch` (`:1053`) and `managed_agent_methods_require_a_human_caller` (`identities.rs:1565`), unchanged. Clippy clean.
- [ ] **Step 6: Commit.** `git commit -m "agmend: child_launch with briefs, parent links, the tier guard and tree limits"`

### Task 5: Budgets on or off, shared or separate

**Files:**
- Modify: `crates/agmend/src/budget.rs` (`Budget` `:100` to `165`), `crates/agmend/src/budget_tests.rs`, `crates/agmend/src/launch.rs` (`RunSpec` `:33` to `46`, `args` `:81` to `86`), `crates/agmend/src/launch_tests.rs:107` to `121` (the `spec()` literal gains `cap_budget: true`; no assertion changes), `crates/agmend/src/managed.rs` (`run_spec` `:544` to `574`, `agent_budget` `:651` to `700`, `tick` `:792` to `797`, `run_worker` cost handling `:891` to `960`, `list` entry `:615`), `crates/agmend/src/managed_actions.rs` (`SavedStatus` `:153`, `load` `:168` to `201`, `start` `:203`, `remove` `:357`, `save_spent` `:936` to `946`), `crates/agmend/src/managed_tree.rs`
- Test: `managed_tree_tests.rs`, `budget_tests.rs`, `launch_tests.rs`

**Interfaces:**
- Consumes: Task 4's `launch_at`, `tree`, `Record.shared_budget`; `Safeguards.budgets`, `child_budgets`, `separate_child_usd`.
- Produces:
  - `Budget` gains `reserved: Money` (unspent caps of running shared children) and `children_spent: Money` (spend of shared children, kept after they stop or are removed). `remaining_usd` is `cap - spent - children_spent - reserved`, saturating; `exhausted` is `spent + children_spent + reserved >= cap`.
    - `pub(crate) fn reserve(&mut self, amount: Money, floor: Money) -> Result<(), Overdrawn>` refuses, changing nothing, when `remaining - amount < floor`.
    - `pub(crate) fn release(&mut self, amount: Money)` subtracts from `reserved`, saturating.
    - `pub(crate) fn charge_child(&mut self, cost: Money)` moves `cost` from `reserved` to `children_spent` (saturating on `reserved`).
    - `#[derive(Debug, thiserror::Error)] #[error("launching this child would leave the parent under 0.50 USD; the parent has {remaining} USD left")] pub(crate) struct Overdrawn { pub(crate) remaining: String }`
    - `pub(crate) const PARENT_FLOOR_USD: &str = "0.5";` (ruling 6c)
  - `status.json` gains `"children_spent_usd"`; `SavedStatus` reads it with a default of 0, and `save_spent` writes both.
  - `RunSpec.cap_budget: bool`. When false, `args` leaves out `--max-budget-usd` and its value.
  - `Managed::safeguards(&self) -> Result<Safeguards, ApiError>` caches a good read for 1 s. `tick` uses the last good read; while the file is invalid, `tick` keeps that last good value, logs the parse error at warn once per read, and `agent_list` carries `"settings_error": "<message>"`, so the error is shown, not silently replaced (ruling 14). With no good read since start, `tick` enforces budgets.
  - `tick` skips an exhausted worker only while `budgets` is on; `run_spec` sets `cap_budget` from `budgets`.
  - Child budget at `child_launch` while budgets is on:
    - Shared (rulings 6a, 6c): amount = `budget_usd` if given, else the smaller of 5 USD and the parent's remaining minus 0.50 USD; zero or less is refused with `Overdrawn`. `parent.budget.reserve(amount, 0.50)` inside `launching`; refusal is `bad_request("{Overdrawn}")`. A launch that then fails calls `release(amount)`. The record has `shared_budget: true`.
    - Separate (ruling 7): amount = `budget_usd` if given, else `separate_child_usd`. No reservation.
  - While budgets is off, a child gets `budget_usd` or `separate_child_usd`, no reservation, `shared_budget: false`.
  - A shared child's batch cost is also passed to `parent.budget.charge_child(cost)` in `run_worker`, after the child's own `add_cost_usd`, without holding both locks at once.
  - Stop or remove of a shared child (ruling 6b): `parent.budget.release(child cap - child spent)`; its spend stays in the parent's `children_spent`. `agent_start` of a shared child reserves its unspent cap again with the 0.50 floor and is refused with `Overdrawn` if it does not fit.
  - `load`: reservations start at 0 (every loaded agent is offline); `children_spent` comes from `status.json`.
  - `agent_budget` (ruling 6d): on a shared child, `add_usd` is reserved from the parent (`reserve(add, 0)`) and refused with "the parent has {remaining} USD left; add to the parent first" if it does not fit. On any agent, the rebuild at `:687` to `695` keeps `reserved` and `children_spent`.
  - `list` entries gain `"reserved_usd"`, `"children_spent_usd"` and `"tree_spent_usd"` (own spend plus every descendant's, walked over `tree` with the Task 4 bound). `"spent_usd"` keeps its meaning.

- [ ] **Step 1: Write the failing tests.**
  - `budget_tests.rs`: `reserve_reduces_remaining`; `reserve_keeps_the_floor` (cap 2: `reserve(1.5, 0.5)` Ok, `reserve(1.6, 0.5)` is `Overdrawn` and changes nothing); `charge_child_moves_reserved_to_children_spent`; `release_saturates_at_zero`.
  - `launch_tests.rs`: `uncapped_run_has_no_max_budget_flag`.
  - `managed_tree_tests.rs`:
    - `shared_child_budget_comes_out_of_the_parent`: parent cap 2, child asks 0.5: parent `reserved_usd` `"0.500000"`, next parent batch `budget_remaining` `"1.500000"`; after one child batch of cost 0.25 (set FakeRunner's cost), parent `children_spent_usd` `"0.250000"`, `reserved_usd` `"0.250000"`, `tree_spent_usd` includes it.
    - `default_shared_amount_keeps_the_floor`: parent cap 2, no amount: child cap `"1.500000"`; a second child with no amount is refused with the 0.50 USD text.
    - `shared_child_that_breaks_the_floor_is_refused`: parent cap 2, child asks 1.6: refused with "would leave the parent under 0.50 USD"; nothing minted.
    - `failed_child_launch_returns_the_reservation`: child `reject`: parent `reserved_usd` back to `"0.000000"`.
    - `stopped_child_returns_its_unspent_reservation`: child cap 0.5, spent 0.25, then `agent_stop` on the child: parent `reserved_usd` `"0.000000"`, `children_spent_usd` `"0.250000"`. The same after `agent_remove`.
    - `starting_a_shared_child_reserves_again`: after the stop above, `agent_start` on the child: parent `reserved_usd` `"0.250000"`; with the parent's remaining too low it is refused.
    - `raising_a_shared_child_draws_from_the_parent`: `agent_budget {add_usd: 0.5}` on the child: parent `reserved_usd` up by 0.5; `add_usd: 100` refused with "add to the parent first".
    - `separate_child_budget_uses_the_setting`: `child_budgets = "separate"`, `separate_child_usd = 2.5`, no amount: child cap `"2.500000"`; parent `reserved_usd` stays 0.
    - `budgets_off_runs_past_the_cap_and_still_counts`.
    - `children_spend_survives_restart`: after `restarted(&fixture)`, the parent's `children_spent_usd` is kept and `reserved_usd` is 0.
    - `invalid_table_is_reported_in_the_list`: an invalid table gives `agent_list` a `settings_error` and budgets stay enforced.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmend -E 'test(/budget|tree::|uncapped/)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** `cargo nextest run -p agmend`. Expected: pass, including `mention_runs_one_batch_then_resume_and_stops_at_budget` (`managed_tests.rs:885`) and `runs[3].budget_remaining == "7.000000"` (`:1341`), unchanged.
- [ ] **Step 5: Commit.** `git commit -m "agmend: budgets on or off, shared or separate child budgets"`

### Task 6: Stop a subtree

**Files:**
- Modify: `crates/agmend/src/managed.rs` (`stop` `:624` to `649`), `crates/agmend/src/managed_tree.rs`, `crates/agmend/src/api.rs` (`identity_call`: `"child_stop"`)
- Test: `managed_tree_tests.rs`

**Interfaces:**
- Consumes: Task 4's `tree`, walk bound and tier guard; Task 5's release on stop; `stop_worker` (`managed.rs:702` to `732`).
- Produces:
  - `agent_stop {identity, subtree?}`: `subtree` defaults to true (ruling 12: no question until Phase 6). The `Stop` struct at `:627` gains `#[serde(default)] subtree: Option<bool>`; callers sending only `identity` (`crates/agmen-cli/src/cmd/workers.rs:121`, the desktop's `agmend.rs:844`) keep working. The answer keeps `identity` and `state` and adds `"stopped": [ids, deepest first]`.
  - `child_stop {identity, subtree?}`, identity-scoped, behind the Task 4 tier guard: `pub(crate) async fn stop_child(&self, caller: &ManagedIdentity, raw: Value) -> Result<Value, ApiError>`. The target must be a strict descendant of the caller in `tree`; otherwise `crate::identities::forbidden()`.
  - `async fn stop_tree(&self, root: IdentityId, subtree: bool) -> Result<Vec<IdentityId>, ApiError>`: holds `launching` for the whole stop, collects descendants breadth first over `tree` (visited set, at most `MAX_MANAGED` nodes), stops the deepest group first, each group in a `JoinSet`, with a 10 s overall deadline after which the rest is aborted, as `shutdown` does at `:746` to `756`.
  - Children left running (`subtree: false`) keep their records, caps and reservations.

- [ ] **Step 1: Write the failing tests:** `stop_stops_the_subtree_children_first` (`stopped == ["kid00002","kid00001","agent001"]`, BYE posts in that order); `stop_without_subtree_keeps_children_running`; `child_may_stop_only_its_descendants` (descendant Ok; parent, sibling and self Forbidden); `child_stop_is_refused_from_an_others_tier_batch`; `stop_and_launch_do_not_interleave` (`tokio::join!` a `child_launch` under `k1` and `agent_stop` of the worker: afterwards no running worker has an offline parent); `parent_cycle_on_disk_stops_without_looping` (two records naming each other as parent: `agent_stop` returns within 10 s); `parent_stop_without_subtree_keeps_child_caps`.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmend -E 'test(/tree::/)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** `stop` (human) calls `stop_tree`.
- [ ] **Step 4: Run** `cargo nextest run -p agmend`. Expected: pass, including `stopping_active_agent_cancels_batch_and_shutdown_stops_every_agent` (`managed_tests.rs:758`) and `stop_and_shutdown_post_goodbye_and_mark_offline` (`:856`), unchanged.
- [ ] **Step 5: Commit.** `git commit -m "agmend: stop an agent's subtree, deepest first"`

### Task 7: Each batch carries its identity and local token

**Depends on:** L21-W merged (it owns the local token's wiring into managed runs), and Task 4.

**Files:**
- Modify: `crates/agmend/src/identities.rs` (remove the `expect(dead_code, reason = "wired by L21")` attributes at `:106` to `107`, `:116` to `117` and `:167` to `168` if L21-W has not already; add `#[derive(Clone)]` to `LocalToken` at `:108`), `crates/agmend/src/launch.rs` (`RunSpec`; `run`'s command at `:489` to `494`), `crates/agmend/src/launch_tests.rs` (`spec()` literal gains the fields), `crates/agmend/src/managed.rs` (`complete_launch`'s `mcp.json` at `:370` to `373`; `run_spec`; `tick` `:842`), `crates/agmend/src/managed_actions.rs` (`start` and `attach` rewrite `mcp.json` with `replace_private_json`, `:1174`)
- Test: `launch_tests.rs`, `managed_tests.rs`

**Interfaces:**
- Produces:
  - `RunSpec.identity: IdentityId` and `RunSpec.local_token: Option<crate::identities::LocalToken>`; `RunSpec`'s `Debug` prints `LocalToken(<redacted>)` (`identities.rs:110` to `114`).
  - `ClaudeRunner::run` sets `AGMEN_IDENTITY` and, when present, `AGMEN_LOCAL_TOKEN` beside `AGMEN_MANAGED` at `:492`, for the Bash path.
  - Ruling 9: the agent's `mcp.json` (under `agent_dir`, `state_dir/managed/<id>/`, `managed_actions.rs:162` to `164`, never in a worktree) carries `"env": {"AGMEN_IDENTITY": .., "AGMEN_LOCAL_TOKEN": ..}` in the `agmen` server entry, at mode 0600, so the MCP server gets the credential whether or not Claude Code passes its own environment (unverified). `start` rewrites it, so agents launched before this task get the block.
  - `tick` fetches `identities.local_token(&id)` before spawning. `Ok(None)` or an error runs the batch with `None` and logs at warn without the token: the agent works but cannot launch.
  - The attach script (`launch.rs:383` to `408`) is unchanged, so no token reaches Terminal's script file.

- [ ] **Step 1: Write the failing tests:**
  - `launch_tests.rs`: `batch_environment_names_identity_and_token` (a fake `claude`, as `launch_tests.rs:607` writes one, writes `$AGMEN_IDENTITY` and `${#AGMEN_LOCAL_TOKEN}` to files: `agent001` and `64`; `0` with `None`); `run_spec_debug_redacts_the_token`.
  - `managed_tests.rs`: `batch_carries_a_token_that_authorizes_as_the_agent`; `mcp_config_carries_the_credential_privately` (`mcp.json` mode is 0600, its `env` authorizes as the agent); `token_stays_out_of_worktrees_and_answers` (after launch, start and one batch, no file under the worktree and no `agent_list`, `agent_launch` or error answer contains the token text, checked with a substring search).
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmend -E 'test(/environment_names|redacts_the_token|carries_a_token|carries_the_credential|token_stays_out/)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** `cargo nextest run -p agmend` and clippy with `-D warnings`. Expected: pass; no `unfulfilled_lint_expectations`.
- [ ] **Step 5: Commit.** `git commit -m "agmend: give each batch its identity and local token"`

### Task 8: `agmen launch` and `agmen workers stop` as a parent

**Depends on:** L21-W merged (it owns `call_as` at `crates/agmen-cli/src/agmend.rs:124`, marked "used by L21-CLI"), and Tasks 4, 6 and 7.

**Files:**
- Modify: `crates/agmen-cli/src/agmend.rs` (remove `expect(dead_code)` from `call_as` at `:124` if L21-W has not; add `LaunchCredential`), `crates/agmen-cli/src/cmd/workers.rs` (`LaunchArgs` `:11` to `23`, `launch` `:93` to `116`, `WorkersCommand::Stop` `:39`, stop `:118` to `128`), `crates/agmen-cli/tests/support/mod.rs:472` (`Home::run` adds `.env_remove("AGMEN_IDENTITY").env_remove("AGMEN_LOCAL_TOKEN")`; add `run_env`)
- Test: `crates/agmen-cli/tests/workers.rs`

**Interfaces:**
- Produces:
  - `pub(crate) struct LaunchCredential { pub(crate) identity: IdentityId, pub(crate) token: LocalToken }`, `pub(crate) fn from_env() -> Result<Option<Self>, Fail>`: neither set is `Ok(None)`; one set or an invalid value is `Fail::usage("AGMEN_IDENTITY and AGMEN_LOCAL_TOKEN must be set together")` or the parse error, never echoing the token.
  - `agmen launch --name <n> [--role <r>] [--model <m>] [--budget <usd>] [--brief <text>]`. With a credential: `call_as(identity, token, "child_launch", {name, role?, model?, budget_usd?, brief?})`; `--project` is `Fail::usage("--project does not apply to an agent's launch: the child joins its parent's project")`. Without one, `--brief` is `Fail::usage("--brief is for an agent's launch; message your agent after launching it")`. The output line is unchanged (`workers.rs:109` to `114`).
  - `agmen workers stop <id> [--keep-children]`: the flag adds `"subtree": false`; with a credential the method is `child_stop`. Without the flag the params stay `{"identity": id}`.

- [ ] **Step 1: Write the failing tests:** `launch_as_an_agent_sends_child_launch` (method `child_launch`, `as_ == "parent01"`, params minus `local_token` equal `{"name":"k1","role":"reviewer","brief":"look at a.rs"}`); `launch_with_one_variable_is_a_usage_error` (exit 2, no request after `hello`); `launch_as_an_agent_refuses_project`; `brief_without_a_credential_is_a_usage_error`; `stop_as_an_agent_sends_child_stop`; `stop_keep_children_sends_subtree_false`.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmen-cli --test workers`. Expected: new tests FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo nextest run -p agmen-cli`. Expected: pass, including `launch_sends_agent_launch_with_this_worktree` (`workers.rs:87`) and `workers_stop_sends_agent_stop` (`:151`), unchanged.
- [ ] **Step 5: Commit.** `git commit -m "agmen launch and workers stop act as the parent agent"`

### Task 9: MCP `launch` and `stop_agent` tools

**Depends on:** Task 8 (and so L21-W).

**Files:**
- Modify: `crates/agmen-cli/src/mcp/mod.rs` (`run` `:102` to `115`; `McpOptions` gains `launch: Option<LaunchCredential>` and `with_launch`), `crates/agmen-cli/src/mcp/tools.rs` (two tools; the startup filter beside `hidden_tools` at `:47` to `65`)
- Test: `tools.rs` unit tests, `crates/agmen-cli/tests/mcp.rs`

**Interfaces:**
- Produces:
  - `run` reads `LaunchCredential::from_env()` (from Claude Code's environment or the `mcp.json` `env` block, Task 7). On an error it logs the error (never the token) to stderr and serves without the two tools.
  - Tool `launch {name, role, model?, budget_usd?, brief?}` (`budget_usd: Option<f64>`, sent with `serde_json::Number::from_f64`, refused when that is `None`) calls `child_launch` and answers `launched @<handle> (<identity>) at depth <n> on <branch>`, plus `; brief sent` when there was one.
  - Tool `stop_agent {identity, subtree?}` calls `child_stop` and answers `stopped <ids, comma separated>`.
  - Both tools are hidden when `launch` is `None`, so `TOOLS` (`tests/support/mcp.rs:24`) and the reviewer list (`tests/mcp.rs:86` to `101`) stay as they are.
  - The server connects with `Agmend::connect(&options.state_base.join(agmen_client::AGMEND_SOCKET))`.

- [ ] **Step 1: Write the failing tests:** `launch_tools_hidden_without_a_credential`; `launch_tool_calls_child_launch_as_the_agent` (a fake agmend at the home's socket, as `tests/workers.rs:32` binds one); `stop_agent_tool_calls_child_stop`; `refusal_text_reaches_the_agent` (the tier refusal from the fake comes back as the tool's error text).
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmen-cli -E 'test(/launch_tool|stop_agent_tool|launch_tools_hidden|refusal_text/)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo nextest run -p agmen-cli`. Expected: pass, including `lists_tools` and `mcp_lists_only_role_tools` (`tests/mcp.rs:74`, `:82`), unchanged.
- [ ] **Step 5: Commit.** `git commit -m "MCP: launch and stop_agent tools for agents agmen launched"`
- [ ] **Step 6: Andy checks by hand (real Claude Code, not automated).** Launch a worker with role `engineer`; as its owner, DM it "launch a reviewer named k1 with the brief 'say hello', then list your tools". Expected: its tools include `launch`, `k1` appears in `agmen workers` with parent the worker, and `k1`'s first batch answers the brief. Then have a second person (or a non-owner identity) ask the worker to launch: the tool answers with the tier refusal.

### Task 10: Settings, Agents page

**Files:**
- Create: `crates/agmen-desktop/src/appearance/agents.rs` (`load_safeguards`, `save_safeguards`), `crates/agmen-desktop/src/ui/settings_agents.rs` (included from `settings_view.rs` with `#[path]`, as `settings_rows.rs` is at `:1390`)
- Modify: `crates/agmen-desktop/src/appearance/mod.rs` (export), `crates/agmen-desktop/src/ui/settings_view.rs` (render match `:1328` to `1335`: `Page::Agents => self.agents_page(cx)`; a `TextInput` entity for the separate child amount, as `agent_model` at `:196` and `:256` to `264`), `crates/agmen-desktop/src/ui/components/stepper.rs` (`MIN`/`MAX` at `:16` to `17` become per-instance fields defaulting to 6 and 72; add `pub(crate) fn range(self, min: u32, max: u32) -> Self`)
- Test: `crates/agmen-desktop/src/ui/settings_view_tests.rs`, unit tests in `appearance/agents.rs`

**Interfaces:**
- Consumes: `agmen_local::safeguards` (Task 1); `read_settings_doc` and `write_settings_text` (`appearance/settings.rs:487`, `:432`); `setting` (`settings_rows.rs:174`), `Toggle`, `Stepper`, `Segmented`, `Field`, `InputFrame`, `Button`, `Label`, `Color::Warning`.
- Produces:
  - `pub(crate) fn load_safeguards(path: &Path) -> Result<Safeguards, AppearanceError>` and `pub(crate) fn save_safeguards(path: &Path, value: &Safeguards) -> Result<(), AppearanceError>` (writes only the seven `[agents]` keys with `toml_edit`, keeping every other key, table and comment, as `DesktopSettings::save` at `settings.rs:340` does).
  - Rows with `debug_selector`s: "Tree depth" (`Toggle` `agents:depth:limit`, `Stepper` `agents:depth:max` with `range(1, 256)` while on); "Agents running per person" (`agents:count:limit`, `agents:count:max` with `range(1, 256)`); "Budgets" (`agents:budgets`); "Child budgets" (`Segmented` `agents:child-budgets`, "Shared with parent" and "Separate", only while Budgets is on); "Separate child budget (USD)" (`agents:separate-usd` input and `agents:separate-usd:save`, only while Budgets is on and Child budgets is Separate; an invalid amount shows the `SafeguardsError` text).
  - Under each off safeguard, a `Label` in `Color::Warning`: `agents:depth:warning`, `agents:count:warning`, `agents:budgets:warning`, with the matching constant.
  - Each control saves at once. No dialog.

- [ ] **Step 1: Write the failing tests:** `save_keeps_other_tables_and_comments`; `invalid_agents_table_is_an_error_and_the_file_is_untouched`; `agents_page_shows_the_defaults`; `turning_depth_off_is_one_click_and_warns` (one click writes `limit_tree_depth = false`, keeps `max_tree_depth`, shows `agents:depth:warning`; a second click removes it); `turning_count_off_is_one_click_and_warns`; `turning_budgets_off_is_one_click_and_warns`; `child_budgets_hidden_while_budgets_are_off`; `separate_amount_shows_only_for_separate_and_saves` (enter `2.5`, save: file has `separate_child_usd = 2.5`; `0` shows the error and writes nothing); `depth_stepper_stays_within_1_to_256`.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmen-desktop -E 'test(/agents_page|one_click_and_warns|child_budgets_hidden|separate_amount|stepper_stays|save_keeps_other|invalid_agents_table/)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo nextest run -p agmen-desktop`. Expected: pass, including `unbuilt_pages_say_they_come_with_agmend` (`settings_view_tests.rs:259`) and `page_rows_are_28_px_list_items` (`:509`), unchanged.
- [ ] **Step 5: Shots.** Andy compares the page with the mockups (the `mockup-checker` agent can run the comparison).
- [ ] **Step 6: Commit.** `git commit -m "Desktop: Settings, Agents page with the safeguards and their warnings"`

### Finish

- [ ] Run the gate from `Jenkinsfile:49` to `54`: `cargo fmt --all --check && cargo clippy --locked --workspace --all-targets --all-features -- -D warnings && cargo nextest run --locked --workspace && cargo test --locked --doc --workspace && RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps && cargo deny --locked check`, then `cargo nextest run --locked -p agmen-hub --profile it`. Expected: all pass.
- [ ] Release notes: agents running on the built-in coordinator, engineer or planner roles need a relaunch (ruling 4).
- [ ] `rg -n '[\x{2013}\x{2014}]'` over the changed files: no matches.

## Existing test assertions this plan changes

1. `crates/agmen-hub/tests/it_roles.rs:832` to `838`: the 422 stays, with another person's identity as the parent; the 201 for the owner's own live agent is a separate new assertion (Task 3, step 1). Approved in the owner review.
2. `crates/agmend/src/state_tests.rs:816` to `826`, `registration_and_stream_counts_are_bounded`: asserts the 65th registered identity is `StoreError::Full`. Rewritten to assert the 256th is accepted and the 257th refused (Task 4, step 1). Approved by integrally-dc, 2026-10-29.

Edits to test code that change no assertion: the `spec()` literal at `crates/agmend/src/launch_tests.rs:107` to `121` gains fields (Tasks 5, 7); `crates/agmen-cli/tests/support/mod.rs:472` removes two environment variables (Task 8); `managed_tests.rs` gains `tree_fixture()`, `enqueue_from_agent` and a `mod tree` include (Task 4).

## Owner rulings (2026-10-29, via integrally-dc)

Structure: SpawnWorker work moved out of this plan (L20r on `release/next`, 3ec0926, already launches `claude`). Tasks 7 and 8 come after L21-W merges. `child_launch` and `child_stop` are agreed.

1. (Reversed until Phase 4, on a second review.) A child's ancestors' messages run in the others tier, like anyone else's. `child_launch`, `child_stop` and the brief stay refused from others-tier batches, and agmend tracks each batch's tier; the refusals are security-relevant. Built in: Task 4 (tier guard, `tree_methods_are_refused_from_an_others_tier_batch`, `ancestor_messages_run_in_the_others_tier`, `parent_dm_from_an_others_tier_batch_keeps_the_child_in_the_others_tier`), Task 6 (`child_stop_is_refused_from_an_others_tier_batch`), Review Focus 1, Global Constraints.
   - Why reversed: a managed agent's `dm` and `post` go straight to the hub through the MCP helper's held session (`crates/agmen-cli/src/mcp/tools.rs:435`, `Agent::send` at `crates/agmen-cli/src/joined.rs:271`, then `publish_held` at `:359`). agmend never sees them, so it cannot record the sender's tier at send time, and an owner tier for ancestors would let a stranger's request reach an owner-tier child through an ordinary DM from its parent.
   - Rejected option: agmend writes the batch tier into the helper's config, the helper stamps it on outgoing messages, and the child's agmend trusts the stamp. Rejected because the stamp is client data the hub cannot verify, and it would build on batch tiers that Phase 4 removes.
   - Consequence: until Phase 4, a child briefed only by its parent can read and reply but cannot run commands or edit files. Real work needs a brief or an approval from the person.
2. The person's launch is depth level 1. Task 4 `admit`, `depth_limit_counts_levels`.
3. A hard registry bound of 256 (`MAX_MANAGED`), not a setting. Both "off" warnings add "agmend still stops at 256 agents." Tasks 1 and 4; Review Focus 2. Approved on the second review: one shared constant, `agmen_local::safeguards::MAX_MANAGED`, drives the registry bound, the store identity bound (`state.rs:337` to `338`) and the identity session bound (`identities.rs:774`), and `registration_and_stream_counts_are_bounded` is rewritten for 256 and 257 (Task 4, step 1).
4. Built-in launch lists as proposed; the hash change is accepted; the commit message and release notes say agents on those roles need a relaunch. Task 2, Finish.
5. Children are kind `agent` only, for now. Tasks 3 and 4.
6. Shared budgets: (a) default amount stands: the smaller of 5 USD and the parent's remaining, held under the floor in (c); (b) a child's stop or removal returns its unspent reservation, and its spend stays counted; (c) a child amount that would leave the parent under 0.50 USD is refused with an error that says so; (d) `agent_budget` on a shared child takes the amount from the parent's remaining and is refused if it does not fit. Task 5; Review Focus 4.
7. New per-person setting `separate_child_usd` in `[agents]`, default 5.00; the listener's caps are not reused. Tasks 1, 5 and 10.
8. The provisional agents warning, plus the 256 line. Task 1.
9. Credential fallback approved: a 0600 `mcp.json` under agmend's state folder, like the listener's, kept out of logs and worktrees. Task 7 (`mcp_config_carries_the_credential_privately`, `token_stays_out_of_worktrees_and_answers`); Review Focus 5.
10. A child branches from the parent's current `HEAD`. Task 4.
11. `launch` takes an optional `brief`, delivered as the child's first message from the parent through the pending queue. Tasks 4, 8 and 9.
12. Tree views and the stop question go to the Phase 6 plan. Until then Stop stops the subtree without asking. Task 6.
13. Only launched agents can launch, for now. Task 4.
14. An invalid `[agents]` table refuses launches with the parse error, with no silent fallback to the defaults. Tasks 1 and 4; Task 5 shows the error in `agent_list` while batches keep the last good settings.
15. (Corrected) Keep L20r's `worker-N` names. Later desktop follow-up, not in this plan: SpawnWorker uses the desktop's "Default agent model" when it is set, and agmend's default otherwise.

## Raised after the rulings (closed)

- **Chat messages from a parent in an others-tier batch.** Closed by reversing ruling 1 until Phase 4 (see ruling 1).
- **The 64 bounds elsewhere.** Closed by ruling 3's approval: one shared constant for the three bounds and the rewritten test. The 64-project bounds (`state.rs:343` to `344`, `identities.rs:507`) stay.
