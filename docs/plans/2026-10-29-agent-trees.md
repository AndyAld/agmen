# Agent trees and settings (Phase 5, current runner): implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an agent agmen launched launch child agents through agmen, under role launch lists and four safeguard settings that each turn off in one change, and let parents and the person stop a subtree, all on today's per-batch Claude Code runner.

**Architecture:** agmend's `Managed` (`crates/agmend/src/managed.rs`) gains a parent link per worker record and two identity-scoped local API methods, `child_launch` and `child_stop`, which reuse the existing launch path under the existing `launching` lock. The runner hands each batch `AGMEN_IDENTITY` and `AGMEN_LOCAL_TOKEN`, so `agmen launch` (CLI) and the MCP `launch` tool call those methods as the parent. Safeguards live in the `[agents]` table of `~/.config/agmen/settings.toml`: one serde type in `agmen-local`, read by agmend at launch and batch time and written by the desktop's Settings, Agents page. The hub's `delegated_by` check widens from "a live listener" to "any live identity of this owner and project".

**Tech Stack:** Rust 2024 workspace, tokio, serde, toml and toml_edit (both already in agmend and agmen-desktop), rmcp (agmen-cli), gpui (agmen-desktop), sqlx and Postgres (agmen-hub `it` tests). No new dependencies.

**Spec:** `docs/specs/2026-10-29-solo-mode-and-agent-trees-design.md` on `origin/design/solo-mode` (sections Agent trees, Safeguard settings, Changes to existing documents). Roadmap: `docs/plans/2026-10-29-solo-mode.md` on the same branch, row 5.

## Global Constraints

- Defaults: tree depth 4, agents running per person 16, budgets on, child budgets shared with the parent.
- Every safeguard can be turned off in one change, with no confirmation dialog, and shows its warning directly under the setting while off.
- Depth warning, verbatim: "Agents can launch agents without limit. A looping agent can start processes until the machine or your API rate limit runs out."
- Budgets warning, verbatim: "No spending caps. Agents spend until you stop them."
- When a limit is reached, agmend refuses the launch and the launching agent gets an error it can report. Nothing already running is stopped.
- Shared budgets: a child's cap comes out of its parent's remaining budget at launch, and the parent's spent figure includes its whole subtree.
- A role lists the roles it may launch; a role with no list may launch nothing.
- A parent may stop any agent below it; the person may stop any of their agents; siblings cannot stop each other; stopping defaults to the whole subtree.
- The CLI ships `launch`, not `delegate`. It reads `AGMEN_IDENTITY` and `AGMEN_LOCAL_TOKEN` and has no `--coordinator` flag (Andy, 2026-10-29).
- The listener stays headless; there is no coordinator listener for now (Andy, 2026-10-29).
- The desktop's SpawnWorker launches only `claude` for now (Andy, 2026-10-29).
- Every loop, wait, buffer and retry has an explicit bound.
- Workspace lints forbid `unsafe_code` and deny `unwrap_used`, `expect_used`, `panic` (root `Cargo.toml:28` to `33`).
- No new dependencies unless named and justified; each needs owner approval.
- Changing an existing test's assertion needs owner approval. This plan marks each one with "OWNER APPROVAL".

## What builds now and what waits

Builds now, on the per-batch runner (`crates/agmend/src/launch.rs`, `ClaudeRunner::run` at `:484`):

- Safeguard settings type, agmend reader, desktop Settings, Agents page with warnings (Tasks 1, 10).
- Role launch lists in role files and built-in roles (Task 2).
- The hub's widened `delegated_by` check (Task 3).
- Parent links, depth and agents-per-person limits, role list checks, `child_launch` (Task 4).
- Budgets on and off, shared or separate child budgets (Task 5).
- Stop subtree for the person and for a parent (Task 6).
- `AGMEN_IDENTITY` and `AGMEN_LOCAL_TOKEN` in each batch's environment (Task 7).
- `agmen launch` and `agmen workers stop` as a parent (Task 8), MCP `launch` and `stop_agent` tools (Task 9).
- SpawnWorker for `claude` (Task 11).

Waits for Phase 4 (terminal delivery) or later:

- Children as terminal agents in agmend's PTYs, and any agent kind other than Claude Code.
- "Waiting on me" for a child's permission prompt, relayed to its parent; a parent reading a child's terminal output.
- In-process subagents listed under their parent, and `@parent/child` addressing.
- Removing the owner and others batch tiers (`launch.rs:61` to `79`). Until then a child briefed by its parent agent runs in the others tier, without Bash, Edit, Write or NotebookEdit (see open question 1).
- Cost from interactive sessions. Budgets here use each batch's `total_cost_usd` (`managed.rs:897` to `903`).
- The listener's `delegate` rebriefed as `launch`, and the listener's `max_workers` counting toward agents per person. The listener runtime is not wired yet (`crates/agmend/src/listener/*.rs` all carry `expect(dead_code, reason = "wired by L21")`); L21's delegate part (`docs/plans/completion.md:340`, roles-listener.md Task 6 `delegate {reply_to, brief, role, ...}` at `docs/plans/roles-listener.md:211`) should call `child_launch`'s launch path once the listener is a managed identity.
- Recents and Quit Everything stopping children first (Phase 6).
- The agent tree view in the desktop, terminal UI and web console, and the desktop's "stop the subtree too?" question (not in roadmap row 5; see open question 12).
- The design.md edits for 6.3, 6.11 and 6.13 belong to Phase 1 Task 6 of the roadmap file.

## Review Focus

1. **A launch chain that loops** (an agent whose role may launch its own role keeps launching): with both limits on, the depth or count refusal stops it; with both off, the registry bound `MAX_AGENTS = 64` (`managed.rs:27`) stops it with an error, never a hang. A record on disk with a parent cycle must not make any walk loop. Pinned by Task 4's `launch_chain_stops_at_the_registry_bound_with_limits_off` and Task 6's `parent_cycle_on_disk_stops_without_looping`.
2. **Two children launched at the same moment when one slot is left**: exactly one succeeds. Pinned by Task 4's `concurrent_child_launches_respect_the_count`.
3. **A parent stopped while its child is being launched**: no child may end up running under a stopped parent after a subtree stop. Pinned by Task 6's `stop_and_launch_do_not_interleave`.
4. **Budget sharing when a parent stops or a child's launch fails**: a stopped parent leaves running children their carved caps; a failed child launch returns its reservation. Pinned by Task 5's `failed_child_launch_returns_the_reservation` and Task 6's `parent_stop_without_subtree_keeps_child_caps`.
5. **A forged parent**: an agent cannot name its own parent (agmend takes it from the authenticated caller), cannot launch through the human `agent_launch`, and the hub refuses a `delegated_by` of another owner, another project, or a revoked identity. Pinned by Task 4's `child_launch_refuses_a_supplied_parent` and `worker_peer_may_call_child_methods_only_with_identity`, and Task 3's `it_delegated_by_any_live_parent_of_owner`.

---

### Task 1: Safeguard settings type and agmend reader

**Files:**
- Create: `crates/agmen-local/src/safeguards.rs`, `crates/agmen-local/src/safeguards_tests.rs`, `crates/agmend/src/safeguards.rs`, `crates/agmend/src/safeguards_tests.rs`
- Modify: `crates/agmen-local/src/lib.rs` (add `pub mod safeguards;`), `crates/agmend/src/lib.rs` (`Config` at `:129` to `140` gains `settings_file: Option<PathBuf>`; `Config::from_env` at `:172` to `188` sets it to `~/.config/agmen/settings.toml` beside `personal_roles`; add `mod safeguards;`)

**Interfaces:**
- Produces, in `agmen_local::safeguards`:
  - `pub const TABLE: &str = "agents";`
  - `pub const MAX_LIMIT: u32 = 64;` (equals `MAX_AGENTS` at `crates/agmend/src/managed.rs:27`)
  - `pub const DEPTH_OFF_WARNING: &str`, `pub const AGENTS_OFF_WARNING: &str`, `pub const BUDGETS_OFF_WARNING: &str` (depth and budgets copy from Global Constraints; agents copy is open question 8, provisional text: "Agents can launch any number of agents across all trees. A looping agent can start processes until the machine or your API rate limit runs out.")
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "lowercase")] pub enum ChildBudgets { Shared, Separate }`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(default)] pub struct Safeguards { pub limit_tree_depth: bool, pub max_tree_depth: u32, pub limit_agents: bool, pub max_agents: u32, pub budgets: bool, pub child_budgets: ChildBudgets }` with `Default` = `true, 4, true, 16, true, Shared`. No `deny_unknown_fields`, so a newer desktop's keys do not break an older agmend.
  - `pub fn validate(&self) -> Result<(), SafeguardsError>`: each max in `1..=MAX_LIMIT`.
  - `#[derive(Debug, thiserror::Error)] pub enum SafeguardsError { #[error("{key} must be 1 to 64, not {value}")] OutOfRange { key: &'static str, value: u32 } }`
- Produces, in `crates/agmend/src/safeguards.rs`: `pub(crate) fn load(path: Option<&Path>) -> Result<Safeguards, ApiError>`. A missing path, file or `[agents]` table gives the defaults. It reads at most 1 MiB (as `listener/settings.rs:18`), parses with `toml`, takes the `agents` table, deserializes and validates. An unreadable or invalid file is `ApiError { code: BadRequest, message: "settings.toml [agents]: <reason>" }`.

- [ ] **Step 1: Write the failing tests.**
  - `agmen-local`: `defaults_match_the_spec` (`Safeguards::default()` equals `{true, 4, true, 16, true, Shared}`); `limits_are_bounded` (`max_tree_depth` 0 and 65 fail `validate`, 1 and 64 pass); `child_budgets_spell_lowercase` (`serde_json::to_value(ChildBudgets::Separate) == json!("separate")`); `warnings_are_the_spec_copy` (`DEPTH_OFF_WARNING` and `BUDGETS_OFF_WARNING` equal the Global Constraints text byte for byte).
  - `agmend`: `missing_file_or_table_gives_defaults`; `agents_table_is_read` (file `[agents]\nlimit_tree_depth = false\nmax_agents = 3\n` gives depth off, 3 agents, rest default); `other_tables_are_ignored` (a `[desktop]` and `[listener.p1]` table beside it parse); `invalid_value_is_refused` (`max_agents = 0` and `budgets = "yes"` give `ErrorCode::BadRequest` whose message starts with `settings.toml [agents]`); `oversized_file_is_refused` (1 MiB plus one byte).
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmen-local -E 'test(/safeguards/)'` and `cargo nextest run -p agmend -E 'test(/safeguards/)'`. Expected: compile errors for the missing items.
- [ ] **Step 3: Implement** the items above. `Config::settings_file` is set only by `from_env`; `Config::new` leaves it `None`, so existing tests read the defaults.
- [ ] **Step 4: Run the tests.** Expected: all pass. Then `cargo clippy -p agmen-local -p agmend --all-targets -- -D warnings`: clean.
- [ ] **Step 5: Commit.** `git commit -m "Add agent safeguard settings and agmend's reader"`

### Task 2: Role launch lists

**Files:**
- Modify: `crates/agmen-roles/src/schema.rs` (`Role` at `:139` to `164` gains a field; `validate` at `:198` to `213` allows element `launches` with attribute `roles`; `parse_inner`'s `Ok(Role {` at `:320`), `crates/agmen-roles/src/layer.rs` (`Resolved` at `:66` to `87`; the widening check at `:296` to `305`; `Ok(Resolved {` at `:341`), `crates/agmen-roles/src/compat.rs:280` (the `Role` literal gets `launches: None`), `roles/coordinator.xml`, `roles/engineer.xml`, `roles/planner.xml` (provisional lists, open question 4)
- Test: `crates/agmen-roles/tests/roles.rs`

**Interfaces:**
- Produces:
  - `Role.launches: Option<BTreeSet<String>>`: `None` when the file has no `<launches>`, else the names in `<launches roles="a b"/>`, each checked with `valid_name` (`schema.rs:177`).
  - `Resolved.launches: BTreeSet<String>`: the nearest declared list in the inheritance chain, else empty. A role that extends a non-base role may only narrow it: a declared list that is not a subset of the inherited one is `RoleError::Widening` (the check at `layer.rs:296`).
- Provisional built-in lists: coordinator `engineer reviewer tester researcher planner`; engineer `reviewer tester`; planner `researcher`; all others none.

- [ ] **Step 1: Write the failing tests** in `tests/roles.rs`:
  - `launches_parses_and_resolves`: a personal role `<role name="lead" extends="base"><launches roles="engineer reviewer"/></role>` resolves with `launches == {"engineer","reviewer"}`.
  - `no_launches_means_none`: built-in `reviewer` resolves with empty `launches`.
  - `launches_inherit_and_may_only_narrow`: `extends="engineer"` with no `<launches>` inherits `{reviewer, tester}`; with `<launches roles="reviewer"/>` gets `{reviewer}`; with `<launches roles="engineer"/>` is `RoleError::Widening`.
  - `launches_rejects_bad_names_and_attributes`: `roles="Engineer"`, `roles="../x"` and `<launches roles="a" extra="b"/>` are `RoleError::Schema`.
  - `builtin_launch_lists`: coordinator, engineer and planner have the provisional lists above.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmen-roles --test roles`. Expected: FAIL.
- [ ] **Step 3: Implement** the field, the element, inheritance and the widening rule, and add `<launches .../>` to the three built-in files after `<worktree>`. Editing built-in XML changes those roles' `sha256`; `batch_role` (`managed.rs:579` to `593`) then refuses the next batch of any running agent on them with "role ... changed since launch; relaunch the agent to use it". Say so in the commit body.
- [ ] **Step 4: Run** `cargo nextest run -p agmen-roles -p agmen-local -p agmend` (agmen-local's `roles_tests.rs` and agmend's role tests read the built-in files). Expected: all pass; clippy clean.
- [ ] **Step 5: Commit.** `git commit -m "Let a role list the roles it may launch"`

### Task 3: Hub accepts any live parent in delegated_by

**Files:**
- Modify: `crates/agmen-hub/src/identities.rs:310` to `326`
- Test: `crates/agmen-hub/tests/it_roles.rs`

**Interfaces:**
- Produces: `POST /v1/identities` with `delegated_by` succeeds when that id is a live identity (any kind: agent, coordinator, listener; the kind CHECK at `migrations/0006_roles.up.sql:3` to `4` allows no other) of the same owner and project, and the new identity's kind is `agent`. The query drops `AND kind='listener'` and keeps `FOR UPDATE`. The refusal stays 422 `bad_role` with the message "delegated_by must name a live identity of this owner and project for an agent child".
- `ingest.rs:292` to `305` (DELEGATED's worker check) is unchanged.

- [ ] **Step 1: Write the failing test** `it_delegated_by_any_live_parent_of_owner`, using the file's `room`, `mint`, `mint_ok`, `role`, `member_with_token` and `roster` helpers:
  - parent = `mint_ok(.., "andy/parent", ..)` (kind agent): child with `delegated_by` parent is 201, and the roster row for the child has `delegated_by == parent`;
  - a grandchild with `delegated_by` the child is 201;
  - a coordinator parent (`role(.., "coordinator", ..)`) gives 201;
  - adam's agent as parent of andy's mint: 422; an agent in another project: 422; a revoked parent (`UPDATE identities SET revoked_at=now()`): 422;
  - `kind: "coordinator"` or `kind: "listener"` with a valid `delegated_by`: 422.
- [ ] **Step 2: OWNER APPROVAL: change an existing assertion.** `crates/agmen-hub/tests/it_roles.rs:832` to `838`, in `it_delegated_by_validated_and_rostered`, mints `andy/bad` with `delegated_by` = an agent worker and asserts `assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);` (`:838`). Under the widened check that mint is 201. Proposed change: replace that block with a mint whose `delegated_by` is adam's identity, still asserting 422. Do not edit it before Andy approves.
- [ ] **Step 3: Run the tests to see the new one fail.** Run: `cargo nextest run -p agmen-hub --profile it -E 'test(/it_delegated_by|it_fix1_delegation|it_coordinator_listener_assign/)'` (needs the dev stack in `deploy/dev`). Expected: the new test FAILs at the agent parent.
- [ ] **Step 4: Implement** the query change.
- [ ] **Step 5: Run the same command.** Expected: all pass, including `it_fix1_delegation_waits_for_listener_revocation` (`it_roles.rs:186`) and `it_coordinator_listener_assign_cap_and_delegated_worker_check` (`:861`, whose `:871` outsider case stays 422).
- [ ] **Step 6: Commit.** `git commit -m "Hub: delegated_by may name any live identity of the owner and project"`

### Task 4: Parent links, limits and child_launch

**Files:**
- Create: `crates/agmend/src/managed_tree.rs` (included from `managed.rs` with `#[path = "managed_tree.rs"] mod tree;`, as `managed_actions.rs` is at `managed.rs:1111` to `1114`), `crates/agmend/src/managed_tree_tests.rs` (included from `managed_tests.rs` as `#[path = "managed_tree_tests.rs"] mod tree;` so it can use `Fixture`, `worker_call` and `restarted`)
- Modify: `crates/agmend/src/managed.rs` (`Record` `:47` to `68`; `Managed` `:126` to `138` and `Managed::new` `:144` to `163` gain `settings_file: Option<PathBuf>`; `launch` `:231` to `311`; `complete_launch` `:313` to `398`; `mint` body `:408`; `list` entry `:615`), `crates/agmend/src/managed_actions.rs` (`Stored` `:86` to `108`, `into_record` `:112` to `158`), `crates/agmend/src/lib.rs:280` to `286` (pass `config.settings_file`), `crates/agmend/src/api.rs` (`owner_peer_check` `:150` to `151`; `identity_call` `:271` to `296`)
- Test fixture: `managed_tests.rs` gains `tree_fixture()`, built like `role_fixture()` (`:22` to `47`): its fake hub records each mint body and answers id `agent001` for `owner/worker`, `kid00001`, `kid00002` and so on for other handles in mint order, and verbs `["FYI"]`.

**Interfaces:**
- Consumes: `agmen_local::safeguards::Safeguards`, `crate::safeguards::load` (Task 1); `Resolved.launches` (Task 2).
- Produces:
  - `Record` gains `#[serde(default, skip_serializing_if = "Option::is_none")] parent: Option<IdentityId>`, `#[serde(default = "root_depth")] depth: u32` (`fn root_depth() -> u32 { 1 }`), `#[serde(default)] shared_budget: bool`. `Stored` gains the same three with the same defaults, so older `agent.json` files load as roots.
  - `child_launch {name, role, model?, budget_usd?}`, an identity-scoped local API method: `pub(crate) async fn launch_child(self: &Arc<Self>, caller: &ManagedIdentity, raw: Value) -> Result<Value, ApiError>`. Params struct `ChildLaunch { name: String, role: String, model: Option<String>, budget_usd: Option<serde_json::Number> }` with `deny_unknown_fields`; name, model and budget checks are `validate_launch`'s (`managed.rs:193` to `229`). The answer is `launch`'s plus `"parent"` and `"depth"`.
  - `async fn admit(&self, parent: Option<&Record>, owner: &IdentityId, safeguards: &Safeguards) -> Result<u32, ApiError>`, called with `launching` held; returns the new depth (`1` for the person's launch, `parent.depth + 1` for a child).
  - `fn running_for(&self, workers: &BTreeMap<IdentityId, Arc<Mutex<Worker>>>, owner: &IdentityId)` counts workers whose `state != "offline"` and whose owner is `owner`; it locks each worker once.
  - `launch` and `launch_child` share one path. Extract `async fn launch_at(self: &Arc<Self>, launch: Launch, root: PathBuf, parent: Option<Record>) -> Result<Value, ApiError>` from `launch` `:241` to `310`. For a child, `root` is the parent record's `worktree`, so `find_identity` returns the parent's identity file and `worktree::create` branches from the parent's `HEAD` (open question 10). `launch_at` takes `launching` before `admit` and holds it through the insert, as `launch` does at `:234`.
  - `mint` adds `"delegated_by": parent.identity.identity_id` to the body only when there is a parent.
  - `list` entries gain `"parent"` (id or null) and `"depth"`.
  - API: `identity_call` routes `"child_launch" => core.managed.launch_child(caller, raw).await`. `owner_peer_check` lets `Caller::Identity` call `child_launch` and `child_stop` from a worker peer, as it lets `post` (`api.rs:150`). `Caller::Human` gets NotFound for both (they are not in `call` at `:333` to `341`).
- Refusals, exact text:
  - caller not a managed worker of this login: `bad_request("only agents agmen launched can launch agents")`
  - parent role list: `bad_request("role {parent} may not launch {child}; it may launch: {names, comma separated, or nothing}")`; a parent with no role uses `{parent}` = `"(none)"` and launches nothing.
  - a child role of kind coordinator or listener: `bad_request("a launched agent's role must be an agent role")` (open question 5).
  - depth: `bad_request("tree depth limit {max} reached: {parent handle} is at depth {depth}. Raise or turn off Tree depth in Settings, Agents")`
  - count: `bad_request("{max} agents are already running for you. Raise or turn off Agents running per person in Settings, Agents")`
  - registry: the existing `bad_request("64 managed agents are already registered")` (`managed.rs:239`).
  - settings: `crate::safeguards::load`'s error (open question 14).

- [ ] **Step 1: Write the failing tests** in `managed_tree_tests.rs`. Each writes a project role `chain.xml` (`<role name="chain" extends="base" version="1"><summary>c</summary><launches roles="chain"/></role>`) where `role_layers_use_main_checkout_and_personal_precedence` (`managed_tests.rs:282`) writes its project role, writes `[agents]` settings to a file it sets as the daemon's `settings_file`, launches `owner/worker` with role `chain`, and calls children with `worker_call`-style framed requests that carry `as` and the identity's `local_token` from `identities.local_token(id)`, from peer 4242 (a worker peer, `managed_tests.rs:514`).
  - `child_launch_records_parent_and_depth`: `child_launch {name:"k1", role:"chain"}` as `agent001` answers `parent:"agent001"`, `depth:2`; `agent_list` shows the same; the second mint body has `"delegated_by":"agent001"`; the first has no `delegated_by` key.
  - `child_launch_refuses_a_role_not_in_the_list`: role `engineer` is refused with "role chain may not launch engineer; it may launch: chain", and nothing is minted.
  - `role_without_a_list_launches_nothing`: a worker launched with no role is refused with "role (none) may not launch chain; it may launch: nothing".
  - `depth_limit_counts_levels`: `max_tree_depth = 2`: worker to `k1` is Ok; `k1` to `k2` is refused with the depth text naming `owner/k1` and depth 2; after writing `limit_tree_depth = false`, `k1` to `k2` is Ok at depth 3.
  - `agent_count_counts_running_agents`: `max_agents = 2`: worker and `k1` running, `k2` refused with the count text; after `managed.stop({"identity":"kid00001"})`, `k2` is Ok.
  - `concurrent_child_launches_respect_the_count`: `max_agents = 2`, `tokio::join!` two `child_launch` calls from `agent001`; exactly one is Ok, the other has the count text.
  - `human_launch_applies_the_count`: `max_agents = 1`: a second `managed.launch` is refused with the count text.
  - `launch_chain_stops_at_the_registry_bound_with_limits_off`: both limits off; each new child launches one more child in a loop of at most 70 calls; the 64th registered agent's next call returns "64 managed agents are already registered" and the loop ends.
  - `child_launch_refuses_a_supplied_parent`: params with `"parent":"x"` or `"delegated_by":"x"` are BadRequest and nothing is minted.
  - `worker_peer_may_call_child_methods_only_with_identity`: `owner_peer_check(.., &Caller::Identity(worker), "child_launch", || Ok(4242))` is Ok; with `Caller::Human` it is Forbidden "agents cannot call owner-only methods"; a framed `child_launch` with no `as` answers NotFound.
  - `stored_record_without_tree_fields_loads_as_root`: an `agent.json` written without the three fields loads with `parent: None`, `depth: 1`, `shared_budget: false`.
  - `child_worktree_branches_from_the_parent_head`: after a commit in the parent's worktree, `k1`'s branch contains that commit (`git merge-base --is-ancestor`), and `k1`'s worktree is a sibling of the parent's. Whether `worktree::create` places a worktree made from a linked worktree beside the parent is unverified; this test settles it.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmend -E 'test(/tree::/)'`. Expected: compile errors.
- [ ] **Step 3: Implement** the interfaces above. Ancestor walks follow `parent` through `workers` with a visited set and stop after `MAX_AGENTS` steps.
- [ ] **Step 4: Run the tree tests, then all of agmend.** Run: `cargo nextest run -p agmend`. Expected: all pass, including `worker_peer_cannot_call_owner_methods_even_without_identity_credentials` (`managed_tests.rs:941`), `framed_owner_request_from_worker_is_forbidden` (`:1005`) and `managed_agent_methods_require_a_human_caller` (`identities.rs:1565`), unchanged. Clippy clean.
- [ ] **Step 5: Commit.** `git commit -m "agmend: child_launch with parent links, depth and agent count limits"`

### Task 5: Budgets on or off, shared or separate

**Files:**
- Modify: `crates/agmend/src/budget.rs` (`Budget` `:100` to `165`), `crates/agmend/src/budget_tests.rs`, `crates/agmend/src/launch.rs` (`RunSpec` `:33` to `46`, `args` `:81` to `86`), `crates/agmend/src/launch_tests.rs:107` to `121` (the `spec()` literal gets `cap_budget: true`; no assertion changes), `crates/agmend/src/managed.rs` (`run_spec` `:544` to `574`, `agent_budget` rebuild `:687` to `695`, `tick` `:792` to `797`, `list` entry `:615`), `crates/agmend/src/managed_actions.rs` (`load` `:168` to `201`), `crates/agmend/src/managed_tree.rs`
- Test: `managed_tree_tests.rs`, `budget_tests.rs`, `launch_tests.rs`

**Interfaces:**
- Consumes: Task 4's `launch_at`, `Record.shared_budget`, `Safeguards.budgets`, `Safeguards.child_budgets`.
- Produces:
  - `Budget` gains `reserved: Money`. `pub(crate) fn reserve(&mut self, amount: Money) -> Result<(), Overdrawn>` refuses and changes nothing when `amount` is more than `remaining`; `pub(crate) fn release(&mut self, amount: Money)` subtracts, saturating at zero; `remaining_usd` is `cap - spent - reserved`, saturating; `exhausted` is `spent + reserved >= cap`; `pub(crate) fn reserved(&self) -> Money`. `#[derive(Debug, thiserror::Error)] #[error("the parent has {remaining} USD left")] pub(crate) struct Overdrawn { pub(crate) remaining: String }`.
  - `RunSpec.cap_budget: bool`. When false, `args` leaves out `--max-budget-usd` and its value.
  - `Managed::safeguards(&self) -> Result<Safeguards, ApiError>` caches the last read for 1 s in a `Mutex<Option<(tokio::time::Instant, Safeguards)>>`, so `tick` (every 100 ms, `managed.rs:760`) reads the file at most once a second. In `tick`, a read error keeps budgets on and logs once per read.
  - `tick` skips a worker on `held.budget.exhausted()` only while `budgets` is on; `run_spec` sets `cap_budget` from `budgets`.
  - Child budget at `child_launch`, while budgets is on:
    - Shared: amount = `budget_usd` if given, else the smaller of 5 USD and the parent's remaining. `parent.budget.reserve(amount)` under the parent's lock, inside `launching`. Over the remaining is `bad_request("{Overdrawn}; ask for at most that, or ask your person to raise the parent's budget")`. If the launch then fails, `release(amount)`. The child record has `shared_budget: true`.
    - Separate: amount = `budget_usd` if given, else 5 USD, as `validate_launch` (`managed.rs:221` to `227`) defaults (open question 7). No reservation.
  - While budgets is off, a child gets the same amount and no reservation; its `shared_budget` is false.
  - `load` re-reserves, after all records load: for each record with `shared_budget` and a loaded parent, `parent.budget.reserve(child cap)`, logging and skipping an overdraw.
  - `agent_budget` keeps `reserved` when it rebuilds the budget.
  - `list` entries gain `"reserved_usd"` and `"tree_spent_usd"` (own spend plus every descendant's, walked with the Task 4 bound). `"spent_usd"` keeps its meaning (own spend).

- [ ] **Step 1: Write the failing tests.**
  - `budget_tests.rs`: `reserve_reduces_remaining` (cap 2, reserve 0.5: `remaining_usd() == "1.500000"`); `overdraw_is_refused_and_changes_nothing`; `reserved_counts_toward_exhausted` (cap 1, reserve 1: `exhausted()`); `release_saturates_at_zero`.
  - `launch_tests.rs`: `uncapped_run_has_no_max_budget_flag` (`cap_budget: false`: no arg equals `--max-budget-usd`).
  - `managed_tree_tests.rs`:
    - `shared_child_budget_comes_out_of_the_parent`: parent cap 2, child asks 0.5: the parent's `reserved_usd` is `"0.500000"` and its next batch's `budget_remaining` is `"1.500000"`; after one child batch (FakeRunner reports cost 1), the parent's `tree_spent_usd` is its own spend plus 1.
    - `shared_child_over_the_parent_remaining_is_refused`: parent cap 2, child asks 3: refused with "the parent has 2.000000 USD left"; nothing minted.
    - `failed_child_launch_returns_the_reservation`: child named `reject` (the fake hub refuses `owner/reject`, `managed_tests.rs:34`): the launch fails and the parent's `reserved_usd` is back to `"0.000000"`.
    - `separate_child_budget_leaves_the_parent_alone`: `child_budgets = "separate"`: parent `reserved_usd` stays 0; child cap is the asked amount.
    - `budgets_off_runs_past_the_cap_and_still_counts`: `budgets = false`, worker cap 2, three batches of cost 1: all three run, the third with `cap_budget == false`, and `spent_usd` is `"3.000000"`.
    - `reservations_survive_restart`: after `restarted(&fixture)`, the parent's `reserved_usd` equals the child's cap.
    - `raising_the_parent_budget_keeps_reservations`: `agent_budget {add_usd: 1}` on the parent keeps `reserved_usd`.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmend -E 'test(/budget|tree::|uncapped/)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** `cargo nextest run -p agmend`. Expected: all pass, including `mention_runs_one_batch_then_resume_and_stops_at_budget` (`managed_tests.rs:885`) and the assertion `runs[3].budget_remaining == "7.000000"` (`managed_tests.rs:1341`), unchanged. Clippy clean.
- [ ] **Step 5: Commit.** `git commit -m "agmend: budgets on or off, shared or separate child budgets"`

### Task 6: Stop a subtree

**Files:**
- Modify: `crates/agmend/src/managed.rs` (`stop` `:624` to `649`), `crates/agmend/src/managed_tree.rs`, `crates/agmend/src/api.rs` (`identity_call`: `"child_stop"`)
- Test: `managed_tree_tests.rs`

**Interfaces:**
- Consumes: Task 4's parent links and walk bound; `stop_worker` (`managed.rs:702` to `732`).
- Produces:
  - `agent_stop {identity, subtree?}`: `subtree` defaults to true. The `Stop` struct at `:627` gains `#[serde(default)] subtree: Option<bool>`; callers that send only `identity` (`crates/agmen-cli/src/cmd/workers.rs:121`, the desktop's `agmend.rs:844`) keep working. The answer keeps `identity` and `state` and adds `"stopped": [ids, deepest first]`.
  - `child_stop {identity, subtree?}`, identity-scoped: `pub(crate) async fn stop_child(&self, caller: &ManagedIdentity, raw: Value) -> Result<Value, ApiError>`. The target must be a strict descendant of the caller; otherwise `crate::identities::forbidden()`. A caller cannot stop itself, an ancestor or a sibling.
  - `async fn stop_tree(&self, root: IdentityId, subtree: bool) -> Result<Vec<IdentityId>, ApiError>`: holds `launching` for the whole stop, so no launch interleaves. It collects descendants breadth first with a visited set, at most `MAX_AGENTS` nodes, groups them by distance, and stops the deepest group first; each group's `stop_worker` calls run in a `JoinSet`; the whole stop has a 10 s deadline, after which the rest of the set is aborted, as `shutdown` does at `:746` to `756`.
  - Children of a stopped parent that are not stopped (`subtree: false`) keep their records, caps and reservations.

- [ ] **Step 1: Write the failing tests:**
  - `stop_stops_the_subtree_children_first`: worker, `k1` under it, `k2` under `k1`; `agent_stop {identity:"agent001"}` answers `stopped == ["kid00002","kid00001","agent001"]`; the captured BYE posts arrive in that order; all three list as `offline`.
  - `stop_without_subtree_keeps_children_running`: `subtree:false` stops only the worker; `k1` stays `idle`.
  - `child_may_stop_only_its_descendants`: `k1` `child_stop`s `k2`: Ok. `k1` on `agent001` (its parent): Forbidden. Sibling `k3` on `k1`: Forbidden. `k1` on itself: Forbidden.
  - `stop_and_launch_do_not_interleave`: `tokio::join!` `k1`'s `child_launch` of `k2` and `agent_stop {identity:"agent001"}`; afterwards no worker that is not `offline` has a parent that is `offline`.
  - `parent_cycle_on_disk_stops_without_looping`: write two `agent.json` records naming each other as parent, `load`, then `agent_stop` one; it returns within 10 s.
  - `parent_stop_without_subtree_keeps_child_caps`: shared child with cap 0.5; `agent_stop {identity:"agent001", subtree:false}`; the child's next batch has `budget_remaining == "0.500000"` and the parent's `reserved_usd` is still `"0.500000"`.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmend -E 'test(/tree::/)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the interfaces above. `stop` (human) calls `stop_tree`.
- [ ] **Step 4: Run** `cargo nextest run -p agmend`. Expected: all pass, including `stopping_active_agent_cancels_batch_and_shutdown_stops_every_agent` (`managed_tests.rs:758`) and `stop_and_shutdown_post_goodbye_and_mark_offline` (`:856`), unchanged.
- [ ] **Step 5: Commit.** `git commit -m "agmend: stop an agent's subtree, deepest first"`

### Task 7: Each batch knows its identity and local token

**Files:**
- Modify: `crates/agmend/src/identities.rs` (remove the `expect(dead_code, reason = "wired by L21")` attributes at `:106` to `107`, `:116` to `117` and `:167` to `168`, which this task fulfills; add `#[derive(Clone)]` to `LocalToken` at `:108`), `crates/agmend/src/launch.rs` (`RunSpec`; `run`'s command at `:489` to `494`), `crates/agmend/src/launch_tests.rs` (`spec()` literal gains the new fields), `crates/agmend/src/managed.rs` (`run_spec`, `tick` `:842`)
- Test: `launch_tests.rs`, `managed_tests.rs`

**Interfaces:**
- Produces:
  - `RunSpec.identity: IdentityId` and `RunSpec.local_token: Option<crate::identities::LocalToken>`. `RunSpec`'s derived `Debug` prints `LocalToken(<redacted>)` through `LocalToken`'s own `Debug` (`identities.rs:110` to `114`).
  - `ClaudeRunner::run` sets `AGMEN_IDENTITY` to the identity id and, when present, `AGMEN_LOCAL_TOKEN` to `token.expose()`, beside `AGMEN_MANAGED` at `:492`. The attach script (`launch.rs:383` to `408`) is unchanged, so no token is written to disk (an attached session is the person's own and uses the human path).
  - `tick` fetches `identities.local_token(&id)` before spawning the batch. `Ok(None)` or an error runs the batch with `None` and logs at warn: the agent can still work; it cannot launch.
- Claude Code passing its environment to a stdio MCP server it spawns is unverified. The Bash tool inheriting it is also unverified. Task 9's step 6 checks both by hand.

- [ ] **Step 1: Write the failing tests:**
  - `launch_tests.rs`: `batch_environment_names_identity_and_token`: a fake `claude` script (as `launch_tests.rs:607` writes one) writes `$AGMEN_IDENTITY` and `${#AGMEN_LOCAL_TOKEN}` (the length, so the test never prints the token) to files; after `run`, they read `agent001` and `64`. With `local_token: None`, the length file reads `0`.
  - `launch_tests.rs`: `run_spec_debug_redacts_the_token`: `format!("{spec:?}")` contains `LocalToken(<redacted>)` and not the token text.
  - `managed_tests.rs`: `batch_carries_a_token_that_authorizes_as_the_agent`: after one batch, `runs[0].identity == "agent001"` and `identities.authorize(Some(&runs[0].identity), Some(runs[0].local_token.as_ref().unwrap().expose()))` is `Caller::Identity` for `agent001`.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmend -E 'test(/environment_names|redacts_the_token|carries_a_token/)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** `cargo nextest run -p agmend` and `cargo clippy -p agmend --all-targets -- -D warnings`. Expected: pass; no `unfulfilled_lint_expectations`.
- [ ] **Step 5: Commit.** `git commit -m "agmend: give each batch AGMEN_IDENTITY and AGMEN_LOCAL_TOKEN"`

### Task 8: `agmen launch` and `agmen workers stop` as a parent

**Files:**
- Modify: `crates/agmen-cli/src/agmend.rs` (remove `expect(dead_code, reason = "used by L21-CLI")` from `call_as` at `:124`; add `LaunchCredential`), `crates/agmen-cli/src/cmd/workers.rs` (`launch` `:93` to `116`, `workers` stop `:118` to `128`, `WorkersCommand::Stop` `:39`), `crates/agmen-cli/tests/support/mod.rs:472` (`Home::run` adds `.env_remove("AGMEN_IDENTITY").env_remove("AGMEN_LOCAL_TOKEN")`, so existing CLI tests pass even when run inside a managed agent; add `pub async fn run_env(&self, cwd, args, env: &[(&str, &str)])`)
- Test: `crates/agmen-cli/tests/workers.rs`

**Interfaces:**
- Consumes: `child_launch` (Task 4), `child_stop` (Task 6), `Agmend::call_as` (`agmend.rs:125` to `140`), `LocalToken: FromStr` (`agmend.rs:162` to `177`).
- Produces:
  - `pub(crate) struct LaunchCredential { pub(crate) identity: IdentityId, pub(crate) token: LocalToken }` with `pub(crate) fn from_env() -> Result<Option<Self>, Fail>`: neither variable set is `Ok(None)`; both set and valid is `Ok(Some)`; one set, or an invalid value, is `Fail::usage("AGMEN_IDENTITY and AGMEN_LOCAL_TOKEN must be set together")` or the parse error. The token's text never appears in an error.
  - `agmen launch` with a credential sends `call_as(identity, token, "child_launch", {name, role?, model?, budget_usd?})`. `--project` with a credential is `Fail::usage("--project does not apply to an agent's launch: the child joins its parent's project")`. `--role` stays optional in clap; agmend refuses a child without one in its list. The output line is unchanged (`workers.rs:109` to `114`).
  - `agmen workers stop <id> [--keep-children]`: `--keep-children` adds `"subtree": false`. With a credential the method is `child_stop`, else `agent_stop`. Without the flag the params stay `{"identity": id}`.

- [ ] **Step 1: Write the failing tests** in `tests/workers.rs`:
  - `launch_as_an_agent_sends_child_launch`: `run_env` with both variables (token `"a".repeat(64)`), `launch --name k1 --role reviewer`: the request method is `child_launch`, `as_ == "parent01"`, params minus `local_token` equal `{"name":"k1","role":"reviewer"}`, `params["local_token"]` equals the token, and stdout has the answer's identity, worktree and branch.
  - `launch_with_one_variable_is_a_usage_error`: only `AGMEN_IDENTITY` set: exit 2, and the fake saw no request after `hello`.
  - `launch_as_an_agent_refuses_project`: exit 2.
  - `stop_as_an_agent_sends_child_stop`: method `child_stop`, `as_ == "parent01"`.
  - `stop_keep_children_sends_subtree_false`: no variables: `agent_stop` with `{"identity":"agent001","subtree":false}`.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmen-cli --test workers`. Expected: the new tests FAIL; existing ones pass.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** `cargo nextest run -p agmen-cli`. Expected: all pass, including `launch_sends_agent_launch_with_this_worktree` (`workers.rs:87`) and `workers_stop_sends_agent_stop` (`:151`), unchanged. Clippy clean.
- [ ] **Step 5: Commit.** `git commit -m "agmen launch and workers stop act as the parent agent from its environment"`

### Task 9: MCP `launch` and `stop_agent` tools

**Files:**
- Modify: `crates/agmen-cli/src/mcp/mod.rs` (`run` `:102` to `115`; `McpOptions` `:204` onward gains `launch: Option<LaunchCredential>` and `with_launch`), `crates/agmen-cli/src/mcp/tools.rs` (two tools; the startup filter beside `hidden_tools` at `:47` to `65`)
- Test: `crates/agmen-cli/src/mcp/tools.rs` unit tests, `crates/agmen-cli/tests/mcp.rs`

**Interfaces:**
- Consumes: `LaunchCredential` (Task 8), `Agmend::call_as`.
- Produces:
  - `run` reads `LaunchCredential::from_env()`; on `Ok(Some)` it calls `with_launch`. On an error it logs to stderr and serves without the tools (an MCP server that exits leaves the agent with no agmen tools at all).
  - Tool `launch {name, role, model?, budget_usd?}` (`budget_usd: Option<f64>`, sent as `serde_json::Number::from_f64`, refused when that is `None`) calls `child_launch` and answers `launched @<handle> (<identity>) at depth <n> on <branch>. Send it its brief with dm or post.`
  - Tool `stop_agent {identity, subtree?}` calls `child_stop` and answers `stopped <ids, comma separated>`.
  - Both tools are hidden when `launch` is `None`. `TOOLS` in `tests/support/mcp.rs:24` (14 tools) and the reviewer list at `tests/mcp.rs:86` to `101` stay as they are, because test options have no credential.
  - The MCP server connects to agmend at `options.state_base.join(agmen_client::AGMEND_SOCKET)` with `Agmend::connect`, so tests reach a fake at the home's state folder (as `tests/workers.rs:32` binds one).

- [ ] **Step 1: Write the failing tests:**
  - unit `launch_tools_hidden_without_a_credential`: the startup tool list without a credential has neither name; with one it has both.
  - `tests/mcp.rs` `launch_tool_calls_child_launch_as_the_agent`: a fake agmend at the home's socket answers `child_launch`; the tool's text starts with `launched @owner/k1`; the request's `as_` is the credential's identity.
  - `tests/mcp.rs` `stop_agent_tool_calls_child_stop`.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmen-cli -E 'test(/launch_tool|stop_agent_tool|launch_tools_hidden/)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** `cargo nextest run -p agmen-cli`. Expected: all pass, including `lists_tools` and `mcp_lists_only_role_tools` (`tests/mcp.rs:74`, `:82`), unchanged.
- [ ] **Step 5: Commit.** `git commit -m "MCP: launch and stop_agent tools for agents agmen launched"`
- [ ] **Step 6: Andy checks by hand (not automated; it runs real Claude Code).** Launch a worker with role `engineer` from the desktop, DM it "launch a reviewer named k1, then list your tools". Expected: the batch's MCP tool list includes `launch`, and `k1` appears in `agmen workers`. If `launch` is missing, Claude Code does not pass the environment to MCP servers (open question 9). Also ask it to run `agmen launch --name k2 --role reviewer` with Bash in an owner batch, which checks the Bash path.

### Task 10: Settings, Agents page

**Files:**
- Create: `crates/agmen-desktop/src/appearance/agents.rs` (`load_safeguards`, `save_safeguards`), `crates/agmen-desktop/src/ui/settings_agents.rs` (the page, included from `settings_view.rs` with `#[path]` as `settings_rows.rs` is at `:1390`)
- Modify: `crates/agmen-desktop/src/appearance/mod.rs` (export), `crates/agmen-desktop/src/ui/settings_view.rs` (render match `:1328` to `1335`: `Page::Agents => self.agents_page(cx)`), `crates/agmen-desktop/src/ui/components/stepper.rs` (`MIN`/`MAX` at `:16` to `17` become per-instance fields defaulting to 6 and 72; add `pub(crate) fn range(self, min: u32, max: u32) -> Self`; font steppers keep 6 to 72)
- Test: `crates/agmen-desktop/src/ui/settings_view_tests.rs`, unit tests in `appearance/agents.rs`

**Interfaces:**
- Consumes: `agmen_local::safeguards::{Safeguards, ChildBudgets, TABLE, MAX_LIMIT, *_WARNING}` (Task 1); `read_settings_doc` and `write_settings_text` (`appearance/settings.rs:487`, `:432`); `setting` (`settings_rows.rs:174`), `Toggle`, `Stepper`, `Segmented`, `Label`, `Color::Warning`.
- Produces:
  - `pub(crate) fn load_safeguards(path: &Path) -> Result<Safeguards, AppearanceError>`: missing file or table gives defaults; an invalid table is an error.
  - `pub(crate) fn save_safeguards(path: &Path, value: &Safeguards) -> Result<(), AppearanceError>`: writes only the six `[agents]` keys with `toml_edit`, keeping every other key, table and comment, as `DesktopSettings::save` (`settings.rs:340`) does for `[desktop]`.
  - Rows, in order, with `debug_selector`s:
    - "Tree depth": `Toggle` `agents:depth:limit`, `Stepper` `agents:depth:max` with `range(1, 64)`, shown while on.
    - "Agents running per person": `Toggle` `agents:count:limit`, `Stepper` `agents:count:max` with `range(1, 64)`.
    - "Budgets": `Toggle` `agents:budgets`.
    - "Child budgets": `Segmented` `agents:child-budgets` with "Shared with parent" and "Separate", shown only while Budgets is on.
    - Under each off safeguard, a `Label` in `Color::Warning` with selector `agents:depth:warning`, `agents:count:warning` or `agents:budgets:warning`, carrying the matching constant.
  - Each control saves at once. No dialog. A save error shows in the page's existing `self.error` line.

- [ ] **Step 1: Write the failing tests:**
  - `appearance/agents.rs`: `save_keeps_other_tables_and_comments` (a file with a comment, `[desktop]` and `[listener.p1]` keeps them byte for byte outside `[agents]`); `invalid_agents_table_is_an_error_and_the_file_is_untouched`.
  - `settings_view_tests.rs`, with the file's `Home`, `open` and `click` helpers:
    - `agents_page_shows_the_defaults`: after `click(cx, "settings-page:agents")`, `settings-placeholder` is absent, the four controls are present, and no `agents:*:warning` is present.
    - `turning_depth_off_is_one_click_and_warns`: one click on `agents:depth:limit` writes `limit_tree_depth = false` and keeps `max_tree_depth`; `agents:depth:warning` is present; a second click removes it.
    - `turning_count_off_is_one_click_and_warns` and `turning_budgets_off_is_one_click_and_warns`: the same for the other two.
    - `child_budgets_hidden_while_budgets_are_off`.
    - `depth_stepper_stays_within_1_to_64`: at 1, `agents:depth:max:decrement` does nothing; at 64, increment does nothing.
- [ ] **Step 2: Run them to see them fail.** Run: `cargo nextest run -p agmen-desktop -E 'test(/agents_page|one_click_and_warns|child_budgets_hidden|stepper_stays|save_keeps_other|invalid_agents_table/)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** `cargo nextest run -p agmen-desktop`. Expected: all pass, including `unbuilt_pages_say_they_come_with_agmend` (`settings_view_tests.rs:259`, Listener page) and `page_rows_are_28_px_list_items` (`:509`, Keys page), unchanged.
- [ ] **Step 5: Shots.** Andy compares the page with the mockups (the `mockup-checker` agent can do the comparison on this branch's commit).
- [ ] **Step 6: Commit.** `git commit -m "Desktop: Settings, Agents page with the four safeguards and their warnings"`

### Task 11: SpawnWorker launches Claude Code

**Files:**
- Modify: `crates/agmen-desktop/src/backend/agmend.rs:889`
- Test: `crates/agmen-desktop/src/backend/agmend_tests.rs`

**Interfaces:**
- Consumes: `queue_agent` (`agmend.rs:532` to `539`), `AgentRow.name` (`backend/mod.rs:460`).
- Produces: `Action::SpawnWorker { agent }` with `agent == "claude"` queues `agent_launch {project, name, root?}`, as `LaunchAgent` does at `:833` to `843`, with no model or budget (agmend's defaults, `sonnet` and 5 USD, apply; open question 15). `name` is `fn free_claude_name(snapshot: &Snapshot) -> Option<String>`: the first `claude-<n>` for `n` in `1..=64` that no roster row's `name` uses; `None` is `BackendError::Unsupported("64 claude agents already exist")`. Any other `agent` is `BackendError::Unsupported("only Claude Code can start a worker for now")`.

- [ ] **Step 1: OWNER APPROVAL: change an existing assertion.** `unsupported_actions_say_so` (`agmend_tests.rs:917`) lists `Action::SpawnWorker { agent: "claude".into() }` (`:938` to `940`) among actions that must return `Err(BackendError::Unsupported(_))` (`:942` to `948`). After this task that action succeeds. Proposed change: replace it in that list with `Action::SpawnWorker { agent: "codex".into() }`. Do not edit it before Andy approves.
- [ ] **Step 2: Write the failing tests:** `spawn_worker_launches_claude_through_agent_launch` (the fake agmend's `agent_requests[0]` is `agent_launch` with `{"project": PROJECT, "name": "claude-1"}`, plus `root` when the backend has one, as at `agmend_tests.rs:1153` to `1159`); `spawn_worker_skips_names_in_use` (a roster row named `claude-1` gives `claude-2`).
- [ ] **Step 3: Run them to see them fail.** Run: `cargo nextest run -p agmen-desktop -E 'test(/spawn_worker/)'`. Expected: FAIL.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run** `cargo nextest run -p agmen-desktop`. Expected: all pass; the fixture's `spawn_worker_*` tests (`fixture_tests.rs:618` to `718`) are unchanged.
- [ ] **Step 6: Commit.** `git commit -m "Desktop: SpawnWorker launches a Claude Code agent"`

### Finish

- [ ] Run the gate from `Jenkinsfile:49` to `54`: `cargo fmt --all --check && cargo clippy --locked --workspace --all-targets --all-features -- -D warnings && cargo nextest run --locked --workspace && cargo test --locked --doc --workspace && RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps && cargo deny --locked check`. Then the hub's `it` profile for Task 3: `cargo nextest run --locked -p agmen-hub --profile it`. Expected: all pass.
- [ ] `rg -n '[\x{2013}\x{2014}]'` over the changed files: no matches.

## Existing test assertions this plan would change (owner approval needed)

1. `crates/agmen-hub/tests/it_roles.rs:838`, `assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);` for a mint whose `delegated_by` is an agent worker (`:832` to `837`). The widened check makes it 201 (Task 3, step 2).
2. `crates/agmen-desktop/src/backend/agmend_tests.rs:938` to `948`: `Action::SpawnWorker { agent: "claude" }` must return `BackendError::Unsupported`. It now launches (Task 11, step 1).

Edits to test code that change no assertion: the `spec()` literal at `crates/agmend/src/launch_tests.rs:107` to `121` gains fields (Tasks 5, 7); `crates/agmen-cli/tests/support/mod.rs:472` removes two environment variables (Task 8); `managed_tests.rs` gains `tree_fixture()` and a `mod tree` include (Task 4).

## Open questions for the owner

1. **Children briefed by their parent run in the restricted tier.** On the per-batch runner, a batch counts as the owner's only when every message is from the human owner (`managed.rs:831` to `833`); otherwise Claude runs without Bash, Edit, Write or NotebookEdit (`launch.rs:73` to `79`). So a child that gets its brief from its parent agent can read and reply but not edit until Phase 4 removes the tiers. Should messages from a child's ancestors count as the owner's on this runner? This plan does not change it.
2. **Depth counting.** This plan counts the person's own launch as level 1, so the default 4 allows a person's agent, its child, grandchild and great-grandchild. Is that the intended meaning of "4 levels"?
3. **"Off" and the registry bound.** With depth or count off, agmend still stops at `MAX_AGENTS = 64` registered agents (`managed.rs:27`), which the bound rule needs. Keep 64, raise it, or make it a setting?
4. **Built-in launch lists.** Proposed: coordinator launches engineer, reviewer, tester, researcher, planner; engineer launches reviewer, tester; planner launches researcher; the rest launch nothing. Editing the built-in files changes their hashes, so running agents on those roles are refused at their next batch until relaunched (`managed.rs:584` to `588`). Accept both?
5. **Child kinds.** This plan lets a launched child only be kind `agent`, at agmend and at the hub. Should a coordinator be able to launch a coordinator?
6. **Shared budget details.** (a) A child with no amount gets the smaller of 5 USD and the parent's remaining. (b) A reservation is not returned when the child stops or is removed. (c) A parent that reserves its whole budget can no longer run a batch, so it cannot read its children's reports. (d) `agent_budget` on a shared child adds to the child only, not taken from the parent. Which of these should change?
7. **Separate budgets "from your per-person caps".** No per-person cap exists outside the listener's settings (`per_delegation` 5 USD, `daily` 20 USD, `listener/policy.rs:73` to `74`). This plan gives a separate child its asked amount or 5 USD. Should it use the listener's caps, or a new per-person setting?
8. **Copy for the agents-per-person warning.** The spec says "Same risk, counted across all trees". Provisional text is in Task 1.
9. **How the MCP server gets the credential.** If Claude Code does not pass its environment to stdio MCP servers (unverified; Task 9 step 6 checks), the fallback is an `env` block in the agent's `mcp.json`, which puts the local token in a 0600 file under agmend's state folder. Approve that fallback?
10. **Child branch base.** This plan branches a child from its parent's current `HEAD`. Should it be the project's base branch instead?
11. **Brief.** `launch` takes no brief; the parent sends one with `dm` or `post`, as the tool's answer says. The old `delegate` took `brief`. Add it?
12. **Tree view and the stop question in the clients.** Spec: agents shown as a tree, and stopping asks whether to stop the subtree. Neither is in roadmap row 5. Which plan should own them? Until then the desktop's Stop stops the subtree without asking.
13. **Only launched agents can launch.** `child_launch` requires the caller to be an agent agmen launched. A joined interactive session, or the listener once wired, cannot launch through it. Keep that for now?
14. **An invalid `[agents]` table.** This plan refuses launches with the parse error and keeps budgets on for batches. Or should agmend fall back to the defaults silently?
15. **SpawnWorker defaults.** It names agents `claude-1`, `claude-2` and so on, and uses agmend's default model and budget rather than the desktop's "Default agent model" setting. Change either?
