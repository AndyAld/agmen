# Self-host Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One command, `agmen host up`, runs a free agmen hub for one group on the host's own machine: it starts the hub, EMQX and Postgres in Docker, makes the hub reachable through Tailscale Funnel, ngrok or the host's own address, makes the person the group's owner with a one-time claim token, creates the first project and prints an invite link; the owner and group admins then manage members with `agmen group`.

**Architecture:** The hub gains a group layer above projects (`tenant_members` with `TenantRole` owner, admin and member), a project creation API, member controls, and a self-host mode in which it adopts one group at first start and logs a claim token. It configures itself from a data directory (a generated ES256 key and EMQX API key) and proxies MQTT over WebSocket at `/mqtt`, so one public HTTPS origin carries both the API and the broker. The CLI writes a compose file and a `0600` env file under `~/.agmen/host/<name>/`, drives `docker compose`, opens the tunnel, and then reuses what the onboarding plan provides, in process: `client::human_at` and `account::device_login` to sign in, `join::join_here` and `setup::run` to join the worktree and set up agents; then it runs `agmen invite`.

**Tech Stack:** Rust 1.99.1 (edition 2024), axum 0.8, hyper 1.12 (HTTP upgrade passthrough), sqlx 0.9 on Postgres 16.15, aws-lc-rs 1.18 (key generation), EMQX 6.3.2, Docker Compose v2, Tailscale CLI, ngrok agent 3.39.13 in Docker.

**Spec:** `docs/specs/2026-10-26-onboarding-and-hosting-design.md` (decision 7, and the group rules and the self-host retention rule in decision 8 that self-host needs). Cross-plan names: `.superpowers/sdd/onboarding/plan-contracts.md`. This is plan 2 of 4; it runs after `docs/plans/onboarding.md` and consumes its Task 1 (`crates/agmen-hub/src/auth/github.rs`), Task 1b (`account::device_login`), Task 2 (`agmen invite` printing the `/i#` link), Task 3 (`agmen_proto::RepoUrl`, migration `0007_project_repo_url`), Task 4 (`Home::fake_program` and the `PATH` rule in the CLI test harness, `setup::run`) and Task 7 (`client::human_at`, `join::join_here`).

## Global Constraints

Copied from the spec and the contracts; every task's requirements include these.

- `agmen host up` "Checks for Docker and stops with the install link if it is missing."
- It "Writes a compose file and a `0600` env file under `~/.agmen/host/<name>/`. It generates every internal secret: the database password, the broker API key, the node cookie and the dashboard password." (This plan has the hub generate the broker API key on first start, per the contracts; see Task 5.)
- It "Starts the hub, the broker and Postgres with pinned tags. On first start, the hub generates and keeps its own JWT signing key, so `init.sh` and hand-set variables go away."
- Reachability: "**Tailscale Funnel**, when the `tailscale` CLI is installed and signed in. It gives a stable `<machine>.<tailnet>.ts.net` name, and visitors need no Tailscale." "**Otherwise ngrok,** with an authtoken the person pastes once; the free plan gives one stable domain." "**Or `--public-url <url>`** for anyone with their own address or reverse proxy."
- "The hub proxies MQTT over WebSocket at `/mqtt` on the same origin. One public HTTPS address then serves both the API and the broker, which is all a tunnel can carry."
- Claim: "The hub prints the token to its local log at first start; it is single use and expires after 1 hour. This replaces `agmen-hub bootstrap` with database access." "The self-host claim token is local, single use and expires after 1 hour. Without it, nobody else who reaches the tunnel can claim the hub."
- "`agmen host status`, `agmen host stop` and `agmen host logs` manage it afterwards."
- "**Free, no limits:** self-hosting has no seat limits, no billing, no trial and no calls to agmen's servers."
- "**Same member controls as hosted:** invite, remove, change roles, revoke an identity."
- History retention: "Self-host keeps everything unless the host sets a limit." The limit is `AGMEN_RETENTION_DAYS` (1 to 36500, from the hosted plan's Task 1); this plan passes it through the compose file, and an empty value means no limit.
- Roles: "a tenant has one **owner** (the creator, who pays) and any number of **group admins**." "What the owner and group admins can do: create projects, see and change seats, remove a member from every project at once, and change roles." "What only the owner can do: open billing, and hand ownership (and billing) to another group admin."
- "**A self-hosted hub has one group.** Claiming it makes you its owner. Only the owner and group admins can create projects, so a stranger who reaches the tunnel cannot create anything."
- "Removing a member revokes their identities through the existing kick-then-ban path (R36, R64)": reuse `identities::ban_and_kick` (`crates/agmen-hub/src/identities.rs:360-383`), which bans first and then kicks.
- Images: "The hub image and, for self-host, the broker image are published with pinned version tags, never `latest`." Hub image `ghcr.io/agmen-dev/agmen-hub:<version>`; broker `emqx/emqx:6.3.2` (single node), EMQ's own image under its pinned tag, which agmen does not republish while the EMQ question is open (the onboarding plan records this reading and lists Andy's ruling on it); Postgres `postgres:16.15` (as `deploy/dev/compose.yaml:57`); ngrok `ngrok/ngrok:3.39.13-alpine` (newest on Docker Hub, checked 2026-10-26).
- Anti-goals: no self-hoster creates a GitHub OAuth app or holds a client secret; "A free self-hosted hub phones home, needs agmen's servers to run, or limits seats" must never be true.
- Humans are keyed on GitHub's numeric account id, never the login (R39).
- Contract names: env `AGMEN_HUB_MODE`, values `self-host` (default) and `hosted`; `TenantRole { Owner, Admin, Member }`, one owner per tenant; CLI `agmen host up|status|stop|logs` and `agmen group members|remove|role|transfer`. This plan adds `agmen group revoke <identity id>` beside them for the spec's "revoke an identity".
- Rulings R1 to R69 in `.superpowers/sdd/mvp/progress.md` bind unless named here. This plan extends R37 (bootstrap keeps working and now also writes a tenant role) and R56 (`GET /v1/me` gains `groups`).
- Rust per `~/.claude/rules/rust/rust.md`. **The gate** is one command: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo nextest run --workspace --locked && cargo test --doc --workspace --locked && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked && cargo deny check`; it passes when it exits 0. **The it run** is `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run --workspace --locked --profile it`; it passes when nextest's summary line shows `0 failed`. No test name may contain `restart` unless it restarts EMQX (`.config/nextest.toml` routes those to `it-restart`). The default profile skips `it_*` tests and the `it` profile runs only them, so each task runs its unit tests and its `it_*` tests with separate commands.
- Migrations: Task 1's file is `<N>_tenant_roles`, where `<N>` is one above the highest number in `crates/agmen-hub/migrations/` when Task 1 starts: `0008` after the onboarding plan's `0006_github_device` and `0007_project_repo_url`. Task 4's is `<N+1>_hub_group`.
- Writing: one-line commit messages with no trailers; no em or en dashes anywhere (ranges use "to").

## Review Focus

1. **Running `agmen host up` again on an existing host.** People rerun it after a reboot, an upgrade or a failed tunnel. Expected: every secret in `.env` is kept (a new Postgres password would lock the hub out of its own volume), the compose file is rewritten with the current pins, and steps already done are skipped. Tests: Task 7 `up_twice_keeps_env_secrets`; Task 9 `it_host_up_claims_creates_project_and_invites` (second run).
2. **A stale claim token.** The person walks away for more than an hour, or the hub restarted and minted a new token. Expected: the hub answers 410 `claim_expired` or 403 `bad_claim_token`; every start of an unclaimed hub replaces the token; `host up` restarts the hub once and claims with the new token. Tests: Task 4 `it_claim_expired_is_refused`, `it_claim_new_start_replaces_token`; Task 9 `it_host_up_mints_new_claim_when_stale`.
3. **A stranger on the public URL.** Anyone can sign in with GitHub through the tunnel. Expected: without the token they cannot claim, they cannot create projects, and they cannot list or change the group's members. Tests: Task 2 `it_stranger_cannot_create_project`; Task 3 `it_non_member_cannot_list_members`; Task 4 `it_claim_wrong_token_refused`, `it_claim_after_owner_refused`.
4. **The public URL does not answer yet.** Tailscale Funnel's public DNS can stay empty for over 15 minutes on a new machine (selfhost-research.json cites open Tailscale issues from October 2026), and a reverse proxy can be misconfigured. Expected: `host up` waits a bounded time, then exits 1 naming the URL and what to check, leaves the stack running, and a later rerun continues. Test: Task 8 `up_stops_when_public_url_unreachable`.
5. **Removing a member whose agents are connected in several projects.** Expected: every identity they own in the group is revoked, banned and kicked, their unused invites stop working, every affected roster is republished, the owner cannot be removed, and repeating the call is safe. Tests: Task 3 `it_remove_member_revokes_everywhere`, `it_remove_member_is_repeatable`.

---

## Phase A: the group layer in the hub

### Task 1: Tenant roles

**Files:**
- Create: `crates/agmen-hub/migrations/<N>_tenant_roles.up.sql`, `crates/agmen-hub/migrations/<N>_tenant_roles.down.sql`, `crates/agmen-hub/src/tenants.rs`, `crates/agmen-hub/tests/it_tenants.rs`
- Modify: `crates/agmen-hub/src/db.rs:70-120` (add `TenantRole` beside `Role`), `crates/agmen-hub/src/lib.rs:21-61` (module, `pub use db::TenantRole`), `crates/agmen-hub/src/invites.rs:167-187` (accept joins the group), `crates/agmen-hub/src/auth/github.rs` (`claim_bootstrap_admin`, moved there unchanged from `auth/oauth.rs:361-385` by onboarding Task 1, writes a tenant role), `crates/agmen-hub/src/me.rs:14-67` (`groups`), `crates/agmen-hub/src/test_hooks.rs:107-140` (`seed_human` writes a tenant role), `crates/agmen-hub/tests/support/mod.rs:359` (`seed_member` writes a tenant role)

**Interfaces:**
- Consumes: `db::{Role, UnknownRole, is_unique_violation}`, `identities::HumanId`, `http::{ApiError, Hub}`; onboarding Task 1 (`claim_bootstrap_admin` in `auth/github.rs`).
- Produces:
  - Table `tenant_members (tenant_id text REFERENCES tenants, human_id text REFERENCES humans, role text CHECK (role IN ('owner','admin','member')), created_at timestamptz, PRIMARY KEY (tenant_id, human_id))`, unique partial index `tenant_members_one_owner ON tenant_members (tenant_id) WHERE role = 'owner'`, index on `human_id`. The migration touches no other table; `projects.repo_url` comes from onboarding's `0007_project_repo_url`.
  - `pub enum TenantRole { Member, Admin, Owner }` in `db.rs`, declared lowest rights first so the derived `Ord` ranks them; same derives and serde (`lowercase`) as `Role`; `pub const fn as_str(self) -> &'static str`; `FromStr<Err = UnknownRole>`; `Display`.
  - `pub(crate) async fn tenant_role(pool: &PgPool, tenant: &str, human: &HumanId) -> Result<Option<TenantRole>, ApiError>` in `tenants.rs`.
  - `pub(crate) async fn join_tenant(conn: &mut PgConnection, tenant: &str, human: &str, role: TenantRole) -> Result<(), sqlx::Error>`: inserts the row; an existing row is never lowered and never raised to owner, only raised from member to admin.
  - `pub(crate) fn require_group_admin(role: Option<TenantRole>) -> Result<TenantRole, ApiError>`: owner or admin, else 403 `group_admin_required`.
  - `GET /v1/me` gains `groups: [{tenant, name, role}]` ordered by name, then tenant id.

The backfill in the up migration (no data changes outside `tenant_members`): every human with a membership in a project of a tenant gets one row in that tenant, `admin` when any of those memberships is project `admin`, else `member`; then, per tenant, one row becomes `owner`: the human who claimed that tenant's earliest claimed `bootstrap_admins` row, or, when none was claimed, the project admin with the earliest `memberships.created_at` (ties by human id).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/agmen-hub/src/db.rs, tests module
#[test] fn tenant_roles_are_ordered_by_rights()  // Owner > Admin > Member; each as_str() parses back; "coordinator".parse::<TenantRole>() is Err

// crates/agmen-hub/tests/it_tenants.rs
#[tokio::test] async fn it_tenant_roles_backfill()        // apply the up migrations below <N> by their SQL from agmen_hub::MIGRATOR.iter(); seed tenant T with P1 and P2:
                                                          // A admin of P1 and claimer of P1's bootstrap_admins row, B admin of P2 (membership 1 s older than A's),
                                                          // C member of P1 and P2; apply <N>: rows are A owner, B admin, C member; exactly one owner in T
#[tokio::test] async fn it_one_owner_per_tenant()          // a second 'owner' row for T fails with a unique violation
#[tokio::test] async fn it_invite_accept_joins_group()     // accepting a P1 invite adds (T, human, 'member'); accepting a P2 invite keeps one row; an existing admin row stays admin
#[tokio::test] async fn it_bootstrap_claim_makes_owner()   // bootstrap row for GitHub id 4242 in a tenant without an owner: first sign-in as 4242 gives role owner;
                                                          // a bootstrap row for id 2 in another project of the same tenant gives admin at 2's sign-in
#[tokio::test] async fn it_me_lists_groups()               // GET /v1/me after the claim above: groups == [{"tenant":T,"name":<T's name>,"role":"owner"}]
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p agmen-hub --locked tenant_roles` then `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-hub --locked --profile it it_tenant it_one_owner it_invite_accept_joins it_bootstrap_claim it_me_lists`
Expected: compile errors for `TenantRole`, then failures on the missing table.

- [ ] **Step 3: Write the migration, `TenantRole`, `tenants.rs`, and the four call sites**

`invites::accept` calls `join_tenant(&mut tx, &tenant, human, TenantRole::Member)` inside its transaction. `claim_bootstrap_admin` writes `owner` when the tenant has no owner and `admin` otherwise, in the same transaction as the memberships it creates. `seed_human` and the test-support `seed_member` write `admin` for a project admin and `member` otherwise.

- [ ] **Step 4: Run the tests to verify they pass, then the gate and the it run**

Run: `cargo nextest run -p agmen-hub --locked -E 'test(=tenant_roles_are_ordered_by_rights)'`, then `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-hub --locked --profile it -E 'test(=it_tenant_roles_backfill) | test(=it_one_owner_per_tenant) | test(=it_invite_accept_joins_group) | test(=it_bootstrap_claim_makes_owner) | test(=it_me_lists_groups)'`, then the gate, then the it run.
Expected: `1 test run: 1 passed`, then `5 tests run: 5 passed`; the gate exits 0; the it run shows `0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-hub
git commit -m "Add tenant roles: owner, admin and member per group"
```

### Task 2: Project creation API

**Files:**
- Create: `crates/agmen-hub/src/projects.rs`, `crates/agmen-hub/tests/it_projects.rs`
- Modify: `crates/agmen-hub/src/bootstrap.rs:87-91` (move `is_name` to `db.rs` as `is_display_name`), `crates/agmen-hub/src/db.rs`, `crates/agmen-hub/src/http.rs:117-140` (route), `crates/agmen-hub/src/lib.rs:21-42`

**Interfaces:**
- Consumes: Task 1 `tenant_role`, `require_group_admin`; onboarding Task 3 `agmen_proto::RepoUrl` (`FromStr<Err = BadRepoUrl>`, `Serialize`; refuses user info, passwords, a leading `-`, whitespace and more than 512 bytes) and the `projects.repo_url` column with its `length <= 512` CHECK; `db::new_id`.
- Produces:
  - `pub(crate) fn is_display_name(name: &str) -> bool` in `db.rs` (1 to 64 characters, no control characters, no space at either end), used by `bootstrap.rs` and Task 4. This is the one project-name rule; the hosted plan's group creation calls `create_project` below rather than adding its own.
  - `pub(crate) struct NewProject { pub(crate) tenant: String, pub(crate) project: String, pub(crate) name: String, pub(crate) repo_url: Option<RepoUrl> }` (`Serialize`; the 201 body).
  - `pub(crate) async fn create_project(conn: &mut PgConnection, tenant: &str, name: &str, repo_url: Option<RepoUrl>, creator: &HumanId) -> Result<NewProject, ApiError>` in `projects.rs`: checks `is_display_name`, inserts the project and the creator's `admin` membership on the caller's connection, so a caller holding a transaction (the hosted plan's `POST /v1/groups`) gets both rows or neither. It does not check the caller's tenant role; the handler does.
  - `POST /v1/projects` (human token), handled by `projects::create`, body `{"tenant": "<id>", "name": "<name>", "repo_url": "<url>"?}` (unknown fields refused). The handler reads `repo_url` as a `String` and parses it with `RepoUrl::from_str`, so a bad URL is 422 `bad_repo_url` (the `BadRepoUrl` message as the error text) rather than a generic body error. 201 `NewProject`; the caller becomes the project's `admin`. Refusals: 403 `group_admin_required` (caller not owner or admin of the tenant, including non-members), 422 `bad_name`, 422 `bad_repo_url`, 409 `project_exists` (same name in the tenant).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/agmen-hub/tests/it_projects.rs
#[tokio::test] async fn it_group_admin_creates_project()      // owner posts {tenant, name:"widgets", repo_url:"https://github.com/acme/widgets.git"}: 201;
                                                              // project id matches ^[a-z2-9]{16}$; GET /v1/me lists it with role "admin"; an admin (not owner) also gets 201
#[tokio::test] async fn it_stranger_cannot_create_project()   // signed-in human with no row in the tenant: 403 group_admin_required; a group member: 403 group_admin_required; no row written
#[tokio::test] async fn it_project_name_rules()               // "", " x", 65 chars: 422 bad_name; "widgets" twice: second is 409 project_exists
#[tokio::test] async fn it_project_repo_url_is_optional()     // no repo_url: 201 with repo_url null
#[tokio::test] async fn it_project_refuses_credentials_in_repo_url() // "https://user:ghp_x@github.com/acme/w.git", "https://user@github.com/acme/w.git", "-oProxyCommand=x":
                                                              // each 422 bad_repo_url; no projects row has a repo_url containing "@github.com"
```

- [ ] **Step 2: Run them to verify they fail**

Run: `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-hub --locked --profile it -E 'binary(it_projects)'`
Expected: FAIL (no route, no function).

- [ ] **Step 3: Implement `create_project`, the handler `projects::create` and the route `POST /v1/projects`**

The handler opens a transaction, checks `require_group_admin(tenant_role(..))`, calls `create_project`, and commits; a unique violation on `(tenant_id, name)` becomes 409.

- [ ] **Step 4: Run the tests to verify they pass, then the gate**

Run: the Step 2 command, then the gate.
Expected: `5 tests run: 5 passed`; the gate exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-hub
git commit -m "Add POST /v1/projects for group owners and admins"
```

### Task 3: Group member controls

**Files:**
- Modify: `crates/agmen-hub/src/tenants.rs` (handlers), `crates/agmen-hub/src/identities.rs:302-383` (`revoke` permission, `ban_and_kick` and `ban_until` become `pub(crate)`), `crates/agmen-hub/src/http.rs:117-140` (routes)
- Test: `crates/agmen-hub/tests/it_tenants.rs`

**Interfaces:**
- Consumes: Task 1; `identities::{ban_and_kick, ban_until, BROKER_TOKEN_TTL}`; `PresenceState::roster_changed`.
- Produces (all human token; members are named by `humans.handle`):
  - `GET /v1/tenants/{tenant}/members`: any group member. 200 `{"members": [{"human_id", "handle", "role", "projects": [{"project", "name", "role"}], "identities": [{"id", "project", "handle"}]}]}` ordered by handle; `identities` lists the member's identities in the tenant with `revoked_at` null, ordered by project then handle, so an owner or admin can name one to revoke. Non-member: 403 `not_a_group_member`.
  - `DELETE /v1/tenants/{tenant}/members/{handle}`: owner, or an admin removing a member or themself. 204. In one transaction: set `revoked_at` on the target's live identities in the tenant, delete their memberships in the tenant's projects, delete unused invites they created in the tenant, delete their `tenant_members` row. After the commit: `ban_and_kick` each identity the target owns in the tenant whose `revoked_at` is null or newer than `BROKER_TOKEN_TTL` plus 5 minutes, and `roster_changed` for each affected project. 404 `no_such_member` only when the target has no row and no such identity, so a repeated call bans and kicks again and answers 204. Refusals: 409 `owner_cannot_be_removed`, 403 `owner_required` (admin removing another admin), 403 `group_admin_required`. A failed ban or kick answers 502 after every step was tried, as `revoke` does.
  - `PUT /v1/tenants/{tenant}/members/{handle}/role`, body `{"role": "admin" | "member"}`: owner or admin. 200 `{handle, role}`. Refusals: 409 `owner_role_fixed` (target is the owner), 422 `use_transfer` (body role `owner`), 404 `no_such_member`.
  - `POST /v1/tenants/{tenant}/transfer`, body `{"to": "<handle>"}`: owner only (403 `owner_required`). The target must be an admin (409 `not_a_group_admin`, also for the owner themself). One transaction demotes the owner to admin, then promotes the target, so the one-owner index holds. 200 `{owner: "<handle>"}`.
  - `DELETE /v1/identities/{id}` also succeeds for an owner or admin of the identity's tenant (today only its owner, `identities.rs:316-327`); anyone else still gets 404.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn it_members_lists_handles_roles_projects()  // owner O, admin A, member M in P1 and P2: three entries by handle, M's projects list both with "member";
                                                                  // M's live identities in P1 and P2 appear in M's identities with their ids; a revoked one does not
#[tokio::test] async fn it_non_member_cannot_list_members()        // signed-in stranger: 403 not_a_group_member
#[tokio::test] async fn it_remove_member_revokes_everywhere()      // M has identities in P1 and P2, both connected as <id>-mcp, and an unused invite to P1;
                                                                  // O deletes M: 204; both revoked_at set; both clients gone from the broker within 2 s;
                                                                  // reconnect with the old broker token: CONNACK 0x8A; GET broker-token with M's identity token: 403 identity_revoked;
                                                                  // accepting M's invite: 404; M has no memberships and no tenant_members row
#[tokio::test] async fn it_remove_member_is_repeatable()           // after a first DELETE of M: lift M's bans with unban_username; the same DELETE again: 204;
                                                                  // reconnect as <id>-mcp with M's old broker token: ConnectReturnCode::Banned (CONNACK 0x8A), so the ban was applied again
#[tokio::test] async fn it_remove_rules()                          // delete O: 409 owner_cannot_be_removed; A deletes another admin: 403 owner_required;
                                                                  // M deletes anyone: 403 group_admin_required; A deletes A: 204
#[tokio::test] async fn it_role_changes()                          // O sets M admin: 200 and M's row is admin; set O's role: 409 owner_role_fixed; body role "owner": 422 use_transfer
#[tokio::test] async fn it_transfer_to_admin_only()                // O to M (member): 409 not_a_group_admin; A to O: 403 owner_required; O to A: 200, A owner, O admin
#[tokio::test] async fn it_group_admin_revokes_member_identity()   // A deletes M's identity: 204; another member deletes it: 404
```

The tests lift each EMQX ban they caused with the existing `unban_username` helper (`crates/agmen-hub/tests/support/mod.rs:567`), even on failure. No test lists bans through the EMQX API: a ban is proven by a reconnect answered `Banned`, as `it_identities.rs:577-605` does, so the tests need nothing a broker other than EMQX would lack.

- [ ] **Step 2: Run them to verify they fail**

Run: `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-hub --locked --profile it it_members it_non_member it_remove it_role_changes it_transfer it_group_admin_revokes`
Expected: FAIL (404 on the new routes).

- [ ] **Step 3: Implement the four handlers in `tenants.rs`, register the routes, widen `revoke`**

- [ ] **Step 4: Run the tests to verify they pass, then the gate**

Run: the Step 2 command, then `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-hub --locked --profile it -E 'binary(it_identities)'` (the widened `revoke`), then the gate.
Expected: `8 tests run: 8 passed`, then `it_identities` shows `0 failed`; the gate exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-hub
git commit -m "Add group member controls: list, remove, change role, transfer"
```

### Task 4: Hub mode, the self-host group and the claim token

**Files:**
- Create: `crates/agmen-hub/migrations/<N+1>_hub_group.up.sql`, `crates/agmen-hub/migrations/<N+1>_hub_group.down.sql`, `crates/agmen-hub/src/group.rs`, `crates/agmen-hub/tests/it_claim.rs`
- Modify: `crates/agmen-hub/src/config.rs:186-358` (`mode`, `group_name`), `crates/agmen-hub/src/http.rs:27-46` (`HubError::Group`), `crates/agmen-hub/src/http.rs:85-140` (startup and route), `crates/agmen-hub/src/lib.rs`, `crates/agmen-hub/src/test_hooks.rs` (hook), `crates/agmen-hub/src/main.rs:42-47` (ANSI only on a terminal), `crates/agmen-hub/tests/support/mod.rs:259-290` (new `Config` fields)

**Interfaces:**
- Consumes: Task 1 `TenantRole`, `join_tenant`; Task 2 `is_display_name`; `db::{new_secret, hash_secret}`.
- Produces:
  - `pub enum HubMode { SelfHost, Hosted }` in `config.rs` (`Default` is `SelfHost`), from `AGMEN_HUB_MODE` (`self-host` or `hosted`; anything else is `ConfigError::Invalid { name: "AGMEN_HUB_MODE", what: "self-host or hosted" }`). `Config.mode: HubMode`.
  - `Config.group_name: String` from `AGMEN_GROUP_NAME`, default `agmen`, checked with `is_display_name`.
  - Table `hub_group (singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton), tenant_id text NOT NULL UNIQUE REFERENCES tenants (id), claim_hash bytea, claim_expires_at timestamptz, CHECK ((claim_hash IS NULL) = (claim_expires_at IS NULL)))`.
  - `pub(crate) async fn ensure_group(pool: &PgPool, name: &str) -> Result<TenantId, GroupError>`: the `hub_group` tenant; with no row, adopts the only tenant, or creates tenant `name` when there are none; with two or more tenants and no row, `GroupError::ManyTenants(n)` ("a self-host hub has one group, but the database has {n} tenants; set AGMEN_HUB_MODE=hosted").
  - `pub(crate) struct ClaimToken { pub(crate) token: String, pub(crate) expires_at: Timestamp }` (token is `new_secret()`, 64 hex).
  - `pub(crate) async fn mint_claim_token(pool: &PgPool) -> Result<Option<ClaimToken>, GroupError>`: `None` when the group has an owner; otherwise stores the hash with `expires_at = now() + 1 hour`, replacing any earlier token.
  - `pub(crate) fn log_claim_token(token: &ClaimToken)`: one `tracing::warn!` with the fields `claim_token` and `expires_at` and the message "this hub has no owner yet; agmen host up claims it with this token". Task 9 parses `claim_token=<64 hex>` from this line.
  - `pub enum GroupError` (thiserror, `#[non_exhaustive]`): `ManyTenants(i64)`, `Database(#[from] sqlx::Error)`, `Random(#[from] getrandom::Error)`. `HubError::Group(#[source] GroupError)`.
  - `Hub::connect` in `SelfHost` mode runs `ensure_group`, then `mint_claim_token`, then `log_claim_token`; in `Hosted` mode it does neither.
  - `POST /v1/claim` (human token; registered only in `SelfHost` mode), body `{"token": "<hex>"}`: 200 `{tenant, name, role: "owner"}` and the caller's row becomes `owner` (`join_tenant` then an update to owner, in one transaction holding the `hub_group` row `FOR UPDATE`; the hash and expiry are cleared). Checked in this order: the group has an owner, 409 `already_claimed`; no stored hash or a different hash, 403 `bad_claim_token`; expired, 410 `claim_expired`.
  - Test hook `pub async fn mint_claim_token(&self) -> Result<Option<String>, HookError>` on `Hub`.
  - `main.rs` sets `.with_ansi(std::io::stderr().is_terminal())`, so `docker compose logs` never carries color codes.

- [ ] **Step 1: Write the failing tests**

```rust
// config.rs tests
#[test] fn hub_mode_parses()     // unset and "" give SelfHost; "self-host" SelfHost; "hosted" Hosted; "selfhost": Invalid { name: "AGMEN_HUB_MODE", .. }
#[test] fn group_name_rules()    // unset gives "agmen"; " x" and 65 chars: Invalid { name: "AGMEN_GROUP_NAME", .. }

// group.rs tests
#[test] fn claim_log_line_has_the_token_field()  // fmt subscriber with ansi off writing to a buffer: the line contains "claim_token=" followed by the 64 hex token and "expires_at="

// tests/it_claim.rs
#[tokio::test] async fn it_fresh_hub_creates_one_group()        // fresh database: one tenant named "agmen" and one hub_group row; a second TestHub on the same database: still one tenant
#[tokio::test] async fn it_claim_makes_owner()                  // token from the hook; POST /v1/claim: 200 role "owner"; GET /v1/me groups shows owner; same token again: 409 already_claimed
#[tokio::test] async fn it_claim_wrong_token_refused()          // 64 zeros: 403 bad_claim_token; the real token still works afterwards
#[tokio::test] async fn it_claim_expired_is_refused()           // UPDATE hub_group SET claim_expires_at = now() - interval '1 second': 410 claim_expired
#[tokio::test] async fn it_claim_new_start_replaces_token()     // t1 from the hook; start a second TestHub on the same database; claim with t1: 403 bad_claim_token; a fresh hook token works
#[tokio::test] async fn it_claim_after_owner_refused()          // after a claim: the hook returns None and POST /v1/claim with any token is 409 already_claimed
#[tokio::test] async fn it_hosted_mode_has_no_claim_route()     // AGMEN_HUB_MODE hosted: POST /v1/claim is 404 and no hub_group row exists
#[tokio::test] async fn it_many_tenants_refuse_self_host()      // two tenants and no hub_group row: Hub::connect fails with HubError::Group(GroupError::ManyTenants(2))
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p agmen-hub --locked hub_mode group_name claim_log`, then the `it` profile with `it_fresh_hub it_claim it_hosted_mode it_many_tenants`.
Expected: FAIL (types and route missing).

- [ ] **Step 3: Implement the config fields, the migration, `group.rs`, the route and the hook; set `mode: HubMode::SelfHost, group_name: "agmen".to_owned()` in the test harness**

- [ ] **Step 4: Run the tests to verify they pass, then the gate and the it run**

Run: `cargo nextest run -p agmen-hub --locked -E 'test(=hub_mode_parses) | test(=group_name_rules) | test(=claim_log_line_has_the_token_field)'`, then `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-hub --locked --profile it -E 'binary(it_claim)'`, then the gate, then the it run.
Expected: `3 tests run: 3 passed`, then `8 tests run: 8 passed`; the gate exits 0; the it run shows `0 failed`, so the existing hub, client and CLI integration tests still pass (a fresh test database gets its own group at connect and their seeded tenants are separate).

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-hub
git commit -m "Add self-host mode with one group and a one-time claim token"
```

### Task 5: Hub self-configuration

**Files:**
- Create: `crates/agmen-hub/src/self_config.rs`
- Modify: `crates/agmen-hub/src/config.rs:96-105,186-218,281-358` (`HubSecrets`, `AGMEN_DATA_DIR`, `AGMEN_EMQX_API_KEY` default), `crates/agmen-hub/src/emqx.rs:62-79` (`EmqxAdmin::new` takes the secret), `crates/agmen-hub/src/http.rs:27-46,85-114` (`HubError::SelfConfig`, load at connect), `crates/agmen-hub/src/main.rs:24-123` (`health` command), `crates/agmen-hub/Dockerfile:11-24` (data directories), `crates/agmen-hub/Cargo.toml` (deps), `crates/agmen-hub/tests/support/mod.rs:259-290`, `spikes/s11-rmqtt/tests/it_s11_broker.rs` (`it_hub_emqx_admin_against_shim` switches to the new `EmqxAdmin::new(&config, api_secret, http)`, building `EmqxConfig { api_url, api_key }` and passing `Secret::new(api_secret)`: the broker spike, which runs first, is a workspace member that the gate compiles, and it calls today's two-argument `new`, `docs/plans/broker-spike.md` Task 4), `THIRD_PARTY_NOTICES.md` (regenerated: `aws-lc-rs` and `base64` join `agmen-hub`'s graph)

**Interfaces:**
- Consumes: `SigningKey::from_pem`, `db::new_secret`; the broker spike's `it_hub_emqx_admin_against_shim` in `spikes/s11-rmqtt/tests/it_s11_broker.rs`.
- Produces:
  - `pub enum HubSecrets { Given { jwt_signing_key: PathBuf, emqx_api_secret: Secret }, Generated { data_dir: PathBuf } }`, as `Config.secrets`, replacing `Config.jwt_signing_key` and `EmqxConfig.api_secret`. Both `AGMEN_JWT_SIGNING_KEY` and `AGMEN_EMQX_API_SECRET` set: `Given` (the cluster and the dev stack, unchanged). Neither set: `Generated` from `AGMEN_DATA_DIR`, else `ConfigError::Missing("AGMEN_DATA_DIR")`. Only one set: `ConfigError::Missing(<the other>)`. `AGMEN_JWT_EXTRA_KEYS` works with both.
  - `AGMEN_EMQX_API_KEY` defaults to `agmen-hub`.
  - `pub fn EmqxAdmin::new(config: &EmqxConfig, api_secret: Secret, http: reqwest::Client) -> Result<Self, EmqxError>`.
  - `pub(crate) struct Loaded { pub(crate) signing: SigningKey, pub(crate) emqx_api_secret: Secret }` and `pub(crate) fn load(secrets: &HubSecrets, api_key: &str) -> Result<Loaded, SelfConfigError>`. `Generated` keeps `<data_dir>/jwt/hub-1.pem` (key id `hub-1`; ES256 P-256 PKCS#8 from `aws_lc_rs::signature::EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())`, PEM `PRIVATE KEY` with base64 lines of 64; directory `0700`, file `0600`) and `<data_dir>/broker/api-keys.txt` (EMQX bootstrap format, one line `<api_key>:<new_secret()>:administrator`; file `0644` because EMQX runs as uid 1000 and reads it from a shared volume). Each file is written to a temporary name in its directory, then renamed; an existing file is read, never replaced.
  - `pub enum SelfConfigError` (thiserror, `#[non_exhaustive]`): `Io { path: PathBuf, #[source] source: std::io::Error }` (Display names the path), `Malformed { path: PathBuf }`, `KeyGen`, `Key(#[from] TokenError)`. `HubError::SelfConfig(#[source] SelfConfigError)`.
  - `agmen-hub health`: GET `http://127.0.0.1:<port of AGMEN_LISTEN, default 8080>/healthz` with a 3 s timeout; exit 0 on 200, else the status or error on stderr and exit 1. It reads no other variable. Compose's healthcheck uses it, since the distroless image has no shell or wget.
  - Dockerfile: the build stage makes `/out/var/lib/agmen/broker`; the runtime stage adds `COPY --from=build --chown=65532:65532 /out/var/lib/agmen /var/lib/agmen`, so a named volume mounted there starts owned by the hub's user. `AGMEN_DATA_DIR` (`/var/lib/agmen` in the image and the compose file) is the hub's one data root: later features that keep files put them in a subdirectory of it, because the distroless nonroot user cannot create a directory elsewhere.
  - Dependencies (both already in `Cargo.lock`): `aws-lc-rs = "1.18.2"`, `base64 = "0.23.1"`; dev-dependency `tempfile = "3.27.0"`.

- [ ] **Step 1: Write the failing tests**

```rust
// self_config.rs tests (tempfile directories)
#[test] fn generated_secrets_are_created_once()          // first load creates jwt/hub-1.pem and broker/api-keys.txt; a second load returns kid "hub-1", the same JWK x and y, and the same API secret
#[test] fn generated_files_have_their_modes()            // unix: jwt dir 0o700, hub-1.pem 0o600, api-keys.txt 0o644
#[test] fn api_keys_file_format()                        // content matches ^agmen-hub:[0-9a-f]{64}:administrator\n$
#[test] fn generated_key_signs_tokens_the_jwks_verifies() // BrokerTokens::new(loaded.signing, vec![]).mint_hub(60 s) verifies with the JWKS key (jsonwebtoken, ES256)
#[test] fn unwritable_data_dir_names_the_path()          // unix: data dir chmod 0o500: Err(Io) whose Display contains the data dir path
#[test] fn malformed_api_keys_file_is_refused()          // "agmen-hub:short" in the file: Err(Malformed) naming the file

// config.rs tests
#[test] fn secrets_come_from_env_or_data_dir()           // both given: Given; neither plus AGMEN_DATA_DIR: Generated; only AGMEN_JWT_SIGNING_KEY: Missing("AGMEN_EMQX_API_SECRET");
                                                         // neither and no AGMEN_DATA_DIR: Missing("AGMEN_DATA_DIR"); AGMEN_EMQX_API_KEY unset gives "agmen-hub"

// main.rs tests
#[test] fn commands()                                    // extends the existing test: ["health"] parses to Command::Health; ["health", "x"] is an error
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p agmen-hub --locked -E 'test(/self_config::/) | test(=secrets_come_from_env_or_data_dir) | test(/main::tests::commands$/)'`
Expected: FAIL (module missing).

- [ ] **Step 3: Implement `self_config.rs`, the config change, `EmqxAdmin::new`, `Hub::connect` loading through `load`, `agmen-hub health`, and the Dockerfile change; switch the test harness to `HubSecrets::Given`**

Update the doc example on `Config::from_lookup` (`config.rs:256-279`) and `USAGE` (`main.rs:24-25`).

- [ ] **Step 4: Run the tests, the gate and the it run**

Run: the Step 2 command, then `set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it -E 'test(=it_hub_emqx_admin_against_shim)'`, then `scripts/third-party-notices.sh` (onboarding Task 11) to regenerate the notice for the new dependencies and `scripts/third-party-notices.sh --check`, then the gate, then the it run.
Expected: `8 tests run: 8 passed` (six in `self_config`, `secrets_come_from_env_or_data_dir`, `commands`); `1 test run: 1 passed` for the spike test, which starts its own spike broker and leaves the dev stack alone; the check exits 0; the gate exits 0; the it run shows `0 failed`, which proves the dev stack's `Given` path is unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-hub Cargo.lock spikes/s11-rmqtt/tests/it_s11_broker.rs THIRD_PARTY_NOTICES.md
git commit -m "Generate the hub's signing key and EMQX API key on first start"
```

### Task 6: The `/mqtt` same-origin proxy

**Files:**
- Create: `crates/agmen-hub/src/mqtt_proxy.rs`, `crates/agmen-hub/tests/it_mqtt_proxy.rs`
- Modify: `crates/agmen-hub/src/config.rs:162-178,330-334` (`AGMEN_MQTT_WS_UPSTREAM`, the derived public broker URL), `crates/agmen-hub/src/http.rs:117-140` (merge the proxy router), `crates/agmen-hub/Cargo.toml`, `crates/agmen-hub/tests/support/mod.rs:259-290`, `THIRD_PARTY_NOTICES.md` (regenerated: `hyper` and `hyper-util` join `agmen-hub`'s graph)

**Interfaces:**
- Consumes: `http::public_link`.
- Produces:
  - `Config.mqtt_ws_upstream: Option<Url>` from `AGMEN_MQTT_WS_UPSTREAM` (`ws://host:port/path`; else `Invalid { name: "AGMEN_MQTT_WS_UPSTREAM", .. }`).
  - When `AGMEN_PUBLIC_MQTT_URL` is unset and the upstream is set, the public broker URL is `AGMEN_PUBLIC_URL` with scheme `wss` and `mqtt` appended through `public_link` (an `http` public URL is `Invalid { name: "AGMEN_PUBLIC_MQTT_URL", what: "wss URL; set an https AGMEN_PUBLIC_URL" }`). Otherwise the default stays `AGMEN_MQTT_URL` (R57).
  - `pub(crate) fn router<S: Clone + Send + Sync + 'static>(upstream: Url, max_connections: usize) -> Router<S>` serving `GET /mqtt`; `Hub::router` merges it when the upstream is set, with `MAX_PROXIED_CONNECTIONS: usize = 1024`. `UPSTREAM_HANDSHAKE_TIMEOUT: Duration = 10 s`.
  - Behavior: no `Upgrade: websocket` header, 426 `upgrade_required`; no free slot, 503 `too_many_connections`; upstream connect or handshake fails, times out or answers anything but 101, 502 `upstream_unavailable`. It forwards only `Upgrade`, `Connection`, `Sec-WebSocket-Key`, `Sec-WebSocket-Version`, `Sec-WebSocket-Protocol` and `Sec-WebSocket-Extensions` (Host becomes the upstream's), answers 101 with `Upgrade`, `Connection`, `Sec-WebSocket-Accept`, `Sec-WebSocket-Protocol` and `Sec-WebSocket-Extensions` from the upstream, then copies bytes both ways (`hyper::upgrade::on` on both sides, `hyper_util::rt::TokioIo`, `tokio::io::copy_bidirectional`) in a spawned task that holds the connection's semaphore permit. Mark the detached task `// shortcut: proxied connections are not drained on shutdown; revisit if shutdown must close them cleanly`.
  - Dependencies (in `Cargo.lock` already): `hyper = { version = "1.12.0", features = ["client", "http1"] }`, `hyper-util = { version = "0.1.21", features = ["tokio"] }`; dev-dependency `rumqttc_next = { version = "0.34.0", package = "rumqttc-next", default-features = false, features = ["websocket"] }`.

- [ ] **Step 1: Write the failing tests**

```rust
// mqtt_proxy.rs tests: the router on 127.0.0.1:0 in front of a fake upstream TCP server that records the request head,
// answers "HTTP/1.1 101 Switching Protocols" with Sec-WebSocket-Accept and "Sec-WebSocket-Protocol: mqtt", then echoes bytes
#[tokio::test] async fn proxy_passes_bytes_both_ways()          // client upgrade with protocol mqtt: 101 with sec-websocket-protocol "mqtt"; a 70 KiB write comes back byte for byte
#[tokio::test] async fn proxy_forwards_only_websocket_headers() // client sends Authorization and Cookie: the upstream head has neither; Host is the upstream's host:port
#[tokio::test] async fn proxy_needs_upgrade()                   // plain GET /mqtt: 426, body error "upgrade_required"
#[tokio::test] async fn proxy_upstream_down_is_502()            // upstream port closed: 502 within 11 s
#[tokio::test] async fn proxy_caps_connections()                // max_connections 1: first connection held open; second: 503 too_many_connections; after the first closes, a third gets 101

// config.rs tests
#[test] fn public_mqtt_url_follows_the_proxy()  // upstream set, public "https://box.tail1234.ts.net": "wss://box.tail1234.ts.net/mqtt";
                                               // public "https://example.com/hub/": "wss://example.com/hub/mqtt"; public "http://x": Invalid AGMEN_PUBLIC_MQTT_URL;
                                               // upstream "http://emqx:8083/mqtt": Invalid AGMEN_MQTT_WS_UPSTREAM; explicit AGMEN_PUBLIC_MQTT_URL still wins

// tests/it_mqtt_proxy.rs (TestHub with the upstream at AGMEN_IT_MQTT_WS)
#[tokio::test] async fn it_mqtt_over_hub_proxy()  // identity broker token; rumqttc_next over ws://<hub>/mqtt as <id>-mcp: CONNACK success; publish to its own up/: PUBACK success;
                                                 // publish to another identity's up/: PUBACK 0x87
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p agmen-hub --locked proxy_ public_mqtt_url_follows`, then the `it` profile with `it_mqtt_over_hub_proxy`.
Expected: FAIL.

- [ ] **Step 3: Implement `mqtt_proxy.rs`, the config change and the merge**

`it_mqtt_over_hub_proxy` reads the upstream only from `AGMEN_IT_MQTT_WS` (`ws://localhost:8083/mqtt` in `deploy/dev/init.sh:27`), so a run against another broker points it there by setting that one variable.

- [ ] **Step 4: Run the tests to verify they pass, then the gate**

Run: `cargo nextest run -p agmen-hub --locked -E 'test(/mqtt_proxy::/) | test(=public_mqtt_url_follows_the_proxy)'`, then `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-hub --locked --profile it -E 'test(=it_mqtt_over_hub_proxy)'`, then `scripts/third-party-notices.sh` and `scripts/third-party-notices.sh --check`, then the gate.
Expected: `6 tests run: 6 passed`, then `1 test run: 1 passed`; the check exits 0; the gate exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-hub Cargo.lock THIRD_PARTY_NOTICES.md
git commit -m "Proxy MQTT over WebSocket at /mqtt on the hub's own origin"
```

---

## Phase B: `agmen host` and `agmen group` in the CLI

### Task 7: `agmen host up|status|stop|logs` with `--public-url`

**Files:**
- Create: `crates/agmen-cli/src/cmd/host/mod.rs` (args and the four commands), `crates/agmen-cli/src/cmd/host/files.rs` (host name, env file, rendering), `crates/agmen-cli/src/cmd/host/docker.rs` (Docker check and `Compose`), `crates/agmen-cli/src/cmd/host/compose.yaml`, `crates/agmen-cli/src/cmd/host/emqx.conf`, `crates/agmen-cli/tests/host.rs`
- Modify: `crates/agmen-cli/src/cmd/mod.rs:5-11,26-73,97-122` (`Host(host::HostArgs)`), `crates/agmen-cli/src/paths.rs:17-30` (`host_dir`)

**Interfaces:**
- Consumes: Task 5 (`AGMEN_DATA_DIR`, `agmen-hub health`), Task 6 (`AGMEN_MQTT_WS_UPSTREAM`), Task 4 (`AGMEN_HUB_MODE`, `AGMEN_GROUP_NAME`); the hub image `ghcr.io/agmen-dev/agmen-hub:<version>` that the onboarding plan's release pipeline publishes; the onboarding plan's compiled `DEFAULT_GITHUB_CLIENT_ID`, so the compose file sets no GitHub variable; onboarding Task 4's CLI test harness: `Home::new`, `apply_env` (its one `PATH` rule, `<home>/bin:/usr/bin:/bin`) and `Home::fake_program(&self, name: &str, script: &str)`, which logs each call's `$*` to `$HOME/calls/<name>.log`. This plan adds no second fake-program helper and never widens `PATH`.
- Produces:
  - `pub(crate) struct HostName(String)`, `FromStr` with the rule `[a-z0-9][a-z0-9-]{0,31}`, default `agmen`.
  - `pub(crate) fn host_dir(name: &str) -> Result<PathBuf>` in `paths.rs`: `$HOME/.agmen/host/<name>` (`%USERPROFILE%` on Windows), created `0700`.
  - `pub(crate) struct HostEnv` in `files.rs`: `fn load(path: &Path) -> Result<Option<HostEnv>>`, `fn get(&self, key: &str) -> Option<&str>`, `fn set(&mut self, key: &str, value: &str)`, `fn ensure_secret(&mut self, key: &str)` (32 random bytes as hex, only when absent), `fn write(&self, path: &Path) -> Result<()>` (sorted `KEY=value` lines, temp file then rename, mode `0600`). Keys: `AGMEN_POSTGRES_PASSWORD`, `AGMEN_EMQX_NODE_COOKIE`, `AGMEN_EMQX_DASHBOARD_PASSWORD`, `AGMEN_PUBLIC_URL`, `AGMEN_HOST_PORT`, `AGMEN_GROUP_NAME`, `AGMEN_TUNNEL` (`url`, `tailscale` or `ngrok`), `AGMEN_RETENTION_DAYS` (written only when `--retention-days` is given; absent or empty means the hub keeps everything; deleting the line and rerunning `host up` removes a limit); Task 8 adds `AGMEN_NGROK_AUTHTOKEN` and `COMPOSE_PROFILES`.
  - `pub(crate) fn render_compose(name: &HostName) -> String` (replaces `{{name}}` and `{{version}}` with the host name and `env!("CARGO_PKG_VERSION")`) and `pub(crate) const EMQX_CONF: &str = include_str!("emqx.conf")`.
  - `pub(crate) async fn check_docker() -> Result<()>` in `docker.rs`: `docker` missing, exit 1 with "Docker is not installed. Install it from https://docs.docker.com/get-docker/ and run agmen host up again."; `docker version --format {{.Server.Version}}` fails, exit 1 with "Docker is installed but its engine is not running: start Docker Desktop or the docker service, then run agmen host up again."; `docker compose version` fails, exit 1 naming https://docs.docker.com/compose/install/.
  - `pub(crate) struct Compose { dir: PathBuf }` running `docker compose --project-directory <dir> -f <dir>/compose.yaml <args>`: `async fn up(&self, services: &[&str]) -> Result<()>` (`up -d --wait --wait-timeout 180 [services]`), `async fn stop(&self) -> Result<()>`, `async fn restart(&self, service: &str) -> Result<()>`, `async fn ps(&self) -> Result<Vec<ServiceState>>` (`ps --all --format json`, one JSON object per line; `ServiceState { service, state, health }`), `async fn port(&self, service: &str, port: u16) -> Result<SocketAddr>`, `async fn logs_text(&self, service: &str) -> Result<String>` (`logs --no-color <service>`), `async fn logs(&self, service: Option<&str>, follow: bool, tail: u32) -> Result<()>` (inherits stdio). Every call except `up` and `logs` has a 30 s timeout.
  - CLI: `agmen host up [--name <name>] [--public-url <url>] [--port <port, default 7080>] [--wait <secs, default 90>] [--retention-days <1 to 36500>]` (`--retention-days` is a clap `u32` range, so 0 and 36501 are usage errors), `agmen host status [--name]`, `agmen host stop [--name]`, `agmen host logs [--name] [--follow] [--tail <n, default 200>] [hub|emqx|postgres|ngrok]`.
  - `up` in this task: check Docker; load or create `.env`, keeping every existing value and adding missing secrets, and setting `AGMEN_RETENTION_DAYS` when `--retention-days` is given; validate `--public-url` (https, a host, no query or fragment; `AGMEN_HOST_ALLOW_HTTP=1` also lets `http` through, for the CLI's integration tests, which drive an in-process hub over http); when `ps` shows no running hub, refuse a busy port with "port <p> is in use: pass --port <free port>"; write `compose.yaml`, `emqx.conf` and `.env`; `up`; poll `http://127.0.0.1:<port>/healthz` every 1 s until 200, at most 60 times. When `up` fails, print the last 20 lines of `logs_text("hub")`. Without `--public-url` (until Task 8), exit 2 naming `--public-url`.
  - `status`: no host directory, exit 3 with "no host named <name>: run agmen host up". Otherwise one line per service with state and health, then the public URL and tunnel, then `<public url>/livez` as "reachable" or "not reachable: <error>".
  - `stop`: `compose stop`, then "stopped; the data stays in Docker volumes; agmen host up starts it again".

The compose template (`crates/agmen-cli/src/cmd/host/compose.yaml`):

```yaml
# Written by agmen host up on every run. Secrets are in .env (mode 0600).
name: agmen-{{name}}

services:
  postgres:
    image: postgres:16.15
    environment:
      POSTGRES_USER: agmen
      POSTGRES_DB: agmen
      POSTGRES_PASSWORD: ${AGMEN_POSTGRES_PASSWORD:?run agmen host up}
    volumes: ["pg-data:/var/lib/postgresql/data"]
    healthcheck: { test: ["CMD-SHELL", "pg_isready -U agmen -d agmen"], interval: 2s, timeout: 5s, retries: 30 }
    restart: unless-stopped

  hub:
    image: ghcr.io/agmen-dev/agmen-hub:{{version}}
    environment:
      AGMEN_HUB_MODE: self-host
      AGMEN_GROUP_NAME: ${AGMEN_GROUP_NAME:?run agmen host up}
      AGMEN_DATABASE_URL: postgres://agmen:${AGMEN_POSTGRES_PASSWORD}@postgres:5432/agmen
      AGMEN_PUBLIC_URL: ${AGMEN_PUBLIC_URL:?run agmen host up}
      AGMEN_DATA_DIR: /var/lib/agmen
      AGMEN_EMQX_API_URL: http://emqx:18083/api/v5
      AGMEN_MQTT_URL: mqtt://emqx:1883
      AGMEN_MQTT_WS_UPSTREAM: ws://emqx:8083/mqtt
      AGMEN_RETENTION_DAYS: ${AGMEN_RETENTION_DAYS:-}
    ports: ["127.0.0.1:${AGMEN_HOST_PORT:?run agmen host up}:8080"]
    volumes: ["hub-data:/var/lib/agmen", "broker-keys:/var/lib/agmen/broker"]
    depends_on: { postgres: { condition: service_healthy } }
    healthcheck: { test: ["CMD", "/usr/local/bin/agmen-hub", "health"], interval: 2s, timeout: 5s, retries: 30 }
    restart: unless-stopped

  emqx:
    image: emqx/emqx:6.3.2
    environment:
      EMQX_NODE__COOKIE: ${AGMEN_EMQX_NODE_COOKIE:?run agmen host up}
      EMQX_DASHBOARD__DEFAULT_PASSWORD: ${AGMEN_EMQX_DASHBOARD_PASSWORD:?run agmen host up}
    volumes: ["./emqx.conf:/opt/emqx/etc/emqx.conf:ro", "broker-keys:/opt/emqx/etc/agmen:ro", "emqx-data:/opt/emqx/data"]
    # After the hub: EMQX reads the hub's JWKS and the API key file the hub writes.
    depends_on: { hub: { condition: service_healthy } }
    healthcheck: { test: ["CMD", "/opt/emqx/bin/emqx", "ctl", "status"], interval: 5s, timeout: 5s, retries: 20 }
    restart: unless-stopped

  ngrok:
    image: ngrok/ngrok:3.39.13-alpine
    profiles: ["ngrok"]
    command: ["http", "http://hub:8080"]
    environment:
      NGROK_AUTHTOKEN: ${AGMEN_NGROK_AUTHTOKEN:-}
    ports: ["127.0.0.1::4040"]
    restart: unless-stopped

volumes:
  pg-data:
  hub-data:
  broker-keys:
  emqx-data:
```

`emqx.conf` is `deploy/dev/emqx/emqx.conf` with three changes: no `cookie` line (the env sets it), `cluster.name = agmen-host`, and the JWT `endpoint = "http://hub:8080/v1/jwks"`. `deploy/dev/` itself does not change: the dev stack and `init.sh` stay for the tests.

- [ ] **Step 1: Write the failing tests**

The fake `docker` is `home.fake_program("docker", script)`, so its calls land in `$HOME/calls/docker.log`. The script answers `version`, `compose version`, `compose ... ps` (from `$HOME/fake/ps.json`), `compose ... port` (from `$HOME/fake/port`), `compose ... logs` (from `$HOME/fake/hub.log`, moving `$HOME/fake/hub2.log` over it on `restart`), exits 1 for a subcommand `<s>` when `$HOME/fake/fail-<s>` exists, and exits 0 for the rest. It calls `/bin/cat` and `/bin/mv` by absolute path. Task 8 adds a fake `tailscale` the same way. A fake in `<home>/bin` shadows a real `docker` or `tailscale` in `/usr/bin` or `/bin`, since `<home>/bin` comes first, so no test ever drives the machine's real Docker or Tailscale.

`up_without_docker_names_the_install_link` needs `docker` to be absent, which onboarding's rule cannot promise (Linux installs it in `/usr/bin`). It sets `PATH` to `<home>/bin` alone after `apply_env`, through the helper `fn narrow_path(cmd: &mut Command)` in `tests/host.rs`; no fake program runs in that test, and the CLI binary is started by its absolute path. This narrows the rule for one test and never widens it.

The hub stub is a small axum server on 127.0.0.1 that answers `GET /healthz` and `GET /livez` with 200; each test passes its port as `--port`. Every test that expects exit 0 also sets `AGMEN_HOST_ALLOW_HTTP=1`, passes `--public-url http://127.0.0.1:<stub port>` (or `http://localhost:<stub port>`), and writes a `ps.json` that shows the hub running, so the port check passes. These runs stay at exit 0 after Task 8 (the `<public url>/livez` wait answers at once) and after Task 9 (no login file and no terminal: `up` stops after the stack, see Task 9).

```rust
// crates/agmen-cli/tests/host.rs (#![cfg(unix)])
#[test] fn up_without_docker_names_the_install_link()  // narrow_path, no fake docker: exit 1; stderr contains "https://docs.docker.com/get-docker/"; no host directory written
#[test] fn up_with_docker_stopped_says_so()            // fake/fail-version exists: exit 1; stderr contains "not running"
#[test] fn up_writes_private_env_and_pinned_compose()  // --public-url http://127.0.0.1:<stub port>: exit 0; host dir 0o700; .env 0o600 with AGMEN_POSTGRES_PASSWORD,
                                                       // AGMEN_EMQX_NODE_COOKIE and AGMEN_EMQX_DASHBOARD_PASSWORD each 64 hex, AGMEN_TUNNEL=url, and no AGMEN_RETENTION_DAYS line;
                                                       // compose.yaml contains "ghcr.io/agmen-dev/agmen-hub:<CARGO_PKG_VERSION>", "emqx/emqx:6.3.2", "postgres:16.15" and no ":latest";
                                                       // calls/docker.log has "compose --project-directory <dir> -f <dir>/compose.yaml up -d --wait --wait-timeout 180"
#[test] fn up_twice_keeps_env_secrets()                // second run with --public-url http://localhost:<stub port>: the three secrets are byte-identical, AGMEN_PUBLIC_URL changed
#[test] fn up_stores_retention_limit()                 // --retention-days 30: exit 0; .env has AGMEN_RETENTION_DAYS=30; compose.yaml contains "AGMEN_RETENTION_DAYS: ${AGMEN_RETENTION_DAYS:-}";
                                                       // a second run without the flag keeps AGMEN_RETENTION_DAYS=30; --retention-days 0 and 36501: exit 2
#[test] fn up_refuses_bad_public_urls()                // without AGMEN_HOST_ALLOW_HTTP: "http://hub.example.com", "https://hub.example.com/?a=1", "ftp://x": exit 2, stderr names --public-url
#[test] fn up_reports_busy_port()                      // a listener on the --port and ps shows no hub: exit 1; stderr contains "is in use"
#[test] fn up_shows_hub_logs_when_start_fails()        // fake/fail-up exists, hub.log has "AGMEN_PUBLIC_URL is not a valid": exit 1; stderr contains that line
#[test] fn host_name_rule()                            // --name Bad_Name and a 33-character name: exit 2
#[test] fn status_without_host_exits_3()               // stderr contains "agmen host up"
#[test] fn stop_runs_compose_stop()                    // after an exit-0 up: `host stop` exit 0; the last line of calls/docker.log ends with "compose.yaml stop"
#[test] fn logs_passes_service_and_tail()              // `host logs --tail 50 hub`: calls/docker.log has "logs --tail 50 hub"

// files.rs tests
#[test] fn env_file_round_trips()           // write then load gives the same pairs; ensure_secret leaves a present value alone
#[test] fn emqx_conf_matches_dev_rules()    // the mqtt, authentication, authorization and durable_sessions blocks equal include_str!("../../../../../deploy/dev/emqx/emqx.conf")'s,
                                            // except the JWT endpoint line, which is "http://hub:8080/v1/jwks"
#[test] fn compose_publishes_only_the_hub() // rendered compose: the only fixed host port is "127.0.0.1:${AGMEN_HOST_PORT", and no "1883:", "8083:" or "18083:" mapping
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'binary(host) | test(/env_file|emqx_conf|compose_publishes/)'`
Expected: FAIL (no `host` command).

- [ ] **Step 3: Implement `host/` and register `agmen host`**

- [ ] **Step 4: Run the tests to verify they pass, then shellcheck the fake script, then the gate**

Run: the Step 2 command; then write the fake `docker` script's text to a temporary file and run `shellcheck -s sh <that file>`; then the gate.
Expected: `15 tests run: 15 passed` (twelve in `host.rs`, three in `files.rs`); shellcheck prints nothing and exits 0; the gate exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-cli
git commit -m "Add agmen host up, status, stop and logs with --public-url"
```

### Task 8: Tunnels: Tailscale Funnel and ngrok

**Files:**
- Create: `crates/agmen-cli/src/cmd/host/tunnel.rs`
- Modify: `crates/agmen-cli/src/cmd/host/mod.rs` (`--tunnel`, the choice, the reachability wait, `stop` turns Funnel off), `crates/agmen-cli/tests/host.rs` (a fake `tailscale` through `home.fake_program("tailscale", script)` that exits 127 when `$HOME/fake/tailscale-missing` exists, else prints `$HOME/fake/tailscale-status.json` for `status --json` and `$HOME/fake/funnel-status.json` for `funnel status --json`, and exits 0 for the rest; calls land in `$HOME/calls/tailscale.log`. Every Task 8 test that runs `host up` without `--public-url` installs it, so a real `tailscale` on the test machine is never run)

**Interfaces:**
- Consumes: Task 7 `HostEnv`, `Compose`.
- Produces:
  - `pub(crate) enum Tunnel { Url(Url), Tailscale, Ngrok }` and `pub(crate) async fn choose(public_url: Option<Url>, flag: Option<TunnelKind>, env: &HostEnv) -> Result<Tunnel>`, where `TunnelKind { Tailscale, Ngrok }` is the clap value enum behind the new `host up --tunnel tailscale|ngrok`: `--public-url` wins; then `--tunnel`; then the stored `AGMEN_TUNNEL`; then Tailscale when `tailscale status --json` has `BackendState == "Running"` and a non-empty `Self.DNSName` (no `tailscale` program, a non-zero exit and any other state all fall through alike); then ngrok when a token is known (stored `AGMEN_NGROK_AUTHTOKEN`, env `NGROK_AUTHTOKEN`, or a line read from stdin when it is a terminal, after "Paste your ngrok authtoken from https://dashboard.ngrok.com/get-started/your-authtoken:"; a token is `[A-Za-z0-9_]{20,100}`). Otherwise exit 2 with one message naming all three: sign in to Tailscale, an ngrok authtoken, or `--public-url <https url>`.
  - `pub(crate) fn tailscale_url(status_json: &str) -> Result<Url>`: `https://` plus `Self.DNSName` without its trailing dot.
  - `pub(crate) fn funnel_target(funnel_status_json: &str, dns_name: &str) -> Option<String>`: `Web["<dns_name>:443"].Handlers["/"].Proxy` (Tailscale's `ipn.ServeConfig` shape; locally only the empty `{}` was checked, on Tailscale 1.104.1).
  - Tailscale path: when `funnel_target` names a target other than `http://127.0.0.1:<port>`, exit 1 with "Tailscale Funnel on this machine already serves <target> on 443; free it with tailscale funnel reset, or use --public-url". Otherwise, after the stack is up, run `tailscale funnel --bg --yes <port>` (flags from `tailscale funnel --help` on 1.104.1).
  - ngrok path: store the token and `COMPOSE_PROFILES=ngrok` in `.env`; `compose.up(&["ngrok"])`; `compose.port("ngrok", 4040)`; GET `http://<addr>/api/tunnels` every 1 s, at most 30 times, until a tunnel's `public_url` starts with `https://`; store it as `AGMEN_PUBLIC_URL`; then the full `up`.
  - Reachability: after the stack and tunnel are up, GET `<public url>/livez` every 2 s until 200 or `--wait` seconds pass. On timeout, exit 1, leave the stack running, and say "the hub runs, but <url> does not answer yet." plus, for Tailscale, "Tailscale Funnel can take up to 15 minutes to reach a new machine's public DNS."; for a URL, "check that your proxy sends <url> to 127.0.0.1:<port>, including WebSocket upgrades on /mqtt."; and always "Run agmen host up again; nothing is lost."
  - `stop` on a Tailscale host also runs `tailscale funnel --https=443 off` (Tailscale's documented off form; it is not in the local help text, so a failure only prints a warning naming `tailscale funnel reset`).

- [ ] **Step 1: Write the failing tests**

```rust
// tunnel.rs tests
#[test] fn tailscale_url_trims_dot()      // {"BackendState":"Running","Self":{"DNSName":"box.tail1234.ts.net."}}: "https://box.tail1234.ts.net/"
#[test] fn funnel_target_parsing()        // "{}": None; Web["box.tail1234.ts.net:443"].Handlers["/"].Proxy "http://127.0.0.1:3000": Some("http://127.0.0.1:3000")
#[test] fn ngrok_url_from_tunnels()       // {"tunnels":[{"public_url":"http://a"},{"public_url":"https://able-otter-123.ngrok-free.app"}]}: the https one

// tests/host.rs
#[test] fn tailscale_preferred_when_signed_in()     // fake tailscale Running with box.tail1234.ts.net., funnel status {}; `host up --wait 1`: exit 1 (nothing answers);
                                                    // .env AGMEN_TUNNEL=tailscale, AGMEN_PUBLIC_URL=https://box.tail1234.ts.net; tailscale log has "funnel --bg --yes 7080"
#[test] fn funnel_busy_is_refused()                 // funnel status proxies to http://127.0.0.1:3000: exit 1; stderr contains "3000" and "tailscale funnel reset"; no "funnel --bg" call
#[test] fn tailscale_stopped_is_not_used()          // BackendState "Stopped", no ngrok token, stdin not a terminal: exit 2
#[test] fn ngrok_when_no_tailscale()                // fake/tailscale-missing (exit 127, the shell's "not found"), NGROK_AUTHTOKEN=2abcDEFghiJKLmnoPQRstu_v: fake port answers a local server with the tunnels JSON;
                                                    // exit 1 after --wait 1; .env has AGMEN_TUNNEL=ngrok, the token, COMPOSE_PROFILES=ngrok,
                                                    // AGMEN_PUBLIC_URL=https://able-otter-123.ngrok-free.app; docker log has "up -d --wait --wait-timeout 180 ngrok" before the full up
#[test] fn no_tunnel_lists_three_options()          // fake/tailscale-missing, no token, stdin not a terminal: exit 2; stderr names "Tailscale", "ngrok" and "--public-url"
#[test] fn up_stops_when_public_url_unreachable()   // --public-url https://127.0.0.1:9 --wait 2: exit 1 within 10 s; stderr names the URL and "agmen host up again"; docker log has no "stop" or "down"
#[test] fn stop_turns_funnel_off()                  // after the Tailscale up above: `host stop`: tailscale log has "funnel --https=443 off"
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'binary(host) | test(/tunnel::/)'`
Expected: the new tests FAIL.

- [ ] **Step 3: Implement `tunnel.rs` and wire it into `up` and `stop`**

- [ ] **Step 4: Run the tests to verify they pass, then the gate**

Run: the Step 2 command; then write the fake `tailscale` script's text to a temporary file and run `shellcheck -s sh <that file>`; then the gate.
Expected: `22 tests run: 22 passed` (nineteen in `host.rs`: Task 7's twelve, whose exit-0 runs still pass, and this task's seven; three in `tunnel.rs`); shellcheck prints nothing and exits 0; the gate exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-cli
git commit -m "Open the hub through Tailscale Funnel or ngrok in agmen host up"
```

### Task 9: `agmen host up` claims the hub and starts the group

**Files:**
- Create: `crates/agmen-cli/src/cmd/host/onboard.rs`
- Modify: `crates/agmen-cli/src/cmd/host/mod.rs` (`--project`, `--handle`, call `onboard` after reachability), `crates/agmen-cli/src/cmd/join.rs` (`JoinArgs::with_handle`), `crates/agmen-cli/src/client.rs:329-352` (`Me.groups`), `crates/agmen-cli/tests/host.rs`, `crates/agmen-cli/tests/it_cli.rs`

**Interfaces:**
- Consumes: Task 4 `POST /v1/claim` and the `claim_token=` log field; Task 2 `POST /v1/projects` and its use of `RepoUrl`; Task 1 `GET /v1/me` `groups`; onboarding Task 3 `agmen_proto::RepoUrl` (`FromStr<Err = BadRepoUrl>`; `agmen-cli` already depends on `agmen-proto`); onboarding Task 1b `account::device_login(hub: &Url) -> Result<Human>`; onboarding Task 7 `client::human_at(hub: &Url) -> Result<Option<Human>>` and `join::join_here(human: &Human, root: &Path, project: &str, args: &JoinArgs) -> Result<IdentityFile>`; onboarding Task 4 `setup::run(root: &Path, identity: &IdentityFile, only: Option<AgentKind>, plugin: Plugin) -> Result<Vec<Change>>` and `setup::plugin_choice(no_plugin: bool) -> Result<Plugin>`; onboarding Task 2 `agmen invite --project <id>` (prints `https://<hub>/i#<code>`); `agmen_client::find_identity`; `client::{Api, refusal}`, `git::toplevel`.
- Produces:
  - `agmen host up` gains `--project <project name>` and `--handle <name>` (the name after the person's handle, as `agmen join --handle` takes it).
  - `pub(crate) fn JoinArgs::with_handle(handle: Option<String>) -> JoinArgs` in `join.rs`: no invite or code, no `--hub`, no project, kind `agent`. It is how a caller in process builds the `args` that `join_here` takes; the hosted plan's `agmen init` uses it too.
  - `pub(crate) fn claim_token_from_logs(logs: &str) -> Option<String>`: the last `claim_token=` followed by exactly 64 lowercase hex digits.
  - `pub(crate) async fn onboard(hub: &Url, compose: &Compose, project: Option<&str>, handle: Option<&str>) -> Result<()>`, run by `up` after the reachability wait, each step skipped when already done, so a rerun makes no second claim, project or identity (it does print a fresh invite):
    1. Sign in: `client::human_at(hub)`. On `None` with stdin a terminal, `account::device_login(hub)` in process. On `None` without a terminal, print "the hub runs at <hub>; sign in from a terminal with agmen login --hub <hub>, then run agmen host up again to claim it" and exit 0: the device flow waits up to 15 minutes for a person, and a script or service must not hang on it.
    2. Claim unless `/v1/me` `groups` shows the person as owner or admin of a group: read `claim_token_from_logs(compose.logs_text("hub"))` and POST `/v1/claim`. On 410 `claim_expired`, 403 `bad_claim_token`, or no token in the logs: `compose.restart("hub")`, wait for `/healthz` as Task 7 does, read the logs again and retry once. On 409 `already_claimed`: exit 1 with "this hub already has an owner; ask them for an invite".
    3. Outside a git worktree: print "run agmen host up again inside a clone of your repository to create the first project" and exit 0.
    4. Create the first project unless the person already belongs to a project in the group: POST `/v1/projects` with `--project <name>` or the worktree folder's name, and `repo_url` set to `git remote get-url origin` only when `RepoUrl::from_str` accepts it. When it refuses, send no `repo_url` and print "the project has no repository URL: <BadRepoUrl message>", never the origin URL itself, which can hold a token.
    5. Join unless `agmen_client::find_identity(root)` finds this worktree's identity: `join_here(&human, root, &project, &JoinArgs::with_handle(handle.map(str::to_owned)))`, then print `joined project <name> (<id>) as <identity handle> (<identity id>)`.
    6. Set up agents for the identity from step 5 or the one found: `setup::run(root, &identity, None, setup::plugin_choice(false)?)`, printing each `Change` on its own line. A setup error prints `agent setup failed: <why>; run agmen setup` and `up` goes on, as `agmen join` does. Setup leaves an agent already set up `Unchanged`, so a rerun writes nothing.
    7. Run `<current exe> invite --project <id>` with inherited stdio, so the person gets `agmen invite`'s link and clipboard lines.
  - `client::Me` gains `#[serde(default)] groups: Vec<Group>` with `Group { tenant, name, role }`.

- [ ] **Step 1: Write the failing tests**

```rust
// onboard.rs tests
#[test] fn claim_token_from_logs_takes_the_last()  // two compose lines "hub-1  | ... claim_token=<a> ..." and "... claim_token=<b> ...": Some(b);
                                                   // 63 hex: None; uppercase hex: None; no field: None

// tests/host.rs (Task 7's stub and fake docker; no login file; stdin not a terminal)
#[test] fn up_off_terminal_without_login_says_how_to_claim()  // exit 0; stdout contains "agmen login --hub http://127.0.0.1:<stub port>" and "agmen host up again";
                                                              // the stub received no request to /v1/auth/device

// tests/it_cli.rs (support Ctx, Task 7's fake docker through home.fake_program, AGMEN_HOST_ALLOW_HTTP=1, --public-url = ctx.base, --port = its port,
// a seeded human signed in at ctx.base with no group role; fake ps.json shows the hub running; fake hub.log holds a token from ctx.hub.mint_claim_token())
#[tokio::test] async fn it_host_up_claims_creates_project_and_invites()  // worktree "widgets" with origin https://github.com/acme/widgets.git, `host up --handle sol`: exit 0;
                                                                       // /v1/me groups shows role "owner"; a project "widgets" with that repo_url; .agmen/identity.json names it
                                                                       // with a handle ending "/sol"; stdout contains "joined project widgets" and "/i#"; a second `host up`: exit 0, still one project, the identity file unchanged
#[tokio::test] async fn it_host_up_keeps_tokens_out_of_repo_url()      // origin https://andy:ghp_s3cret@github.com/acme/widgets.git: exit 0; the project's repo_url is null;
                                                                       // neither stdout nor stderr contains "ghp_s3cret"
#[tokio::test] async fn it_host_up_mints_new_claim_when_stale()        // hub.log holds 64 zeros, hub2.log a fresh hook token: exit 0; docker log has "restart hub"; role "owner"
#[tokio::test] async fn it_host_up_outside_git_stops_after_claim()     // run in a plain temp folder: exit 0; role "owner"; no project created; stdout says to run it inside a clone
#[tokio::test] async fn it_host_up_refuses_a_claimed_hub()             // another human claimed first: exit 1; stderr contains "already has an owner"
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'test(=claim_token_from_logs_takes_the_last) | test(=up_off_terminal_without_login_says_how_to_claim)'`, then `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-cli --locked --profile it -E 'test(/it_host_up/)'`.
Expected: FAIL.

- [ ] **Step 3: Implement `onboard.rs`, `JoinArgs::with_handle` and `Me.groups`, add `--project` and `--handle`; call `onboard` at the end of `up`**

- [ ] **Step 4: Run the tests to verify they pass, then the gate and the it run**

Run: the two Step 2 commands, then `cargo nextest run -p agmen-cli --locked`, then the gate, then the it run.
Expected: `2 tests run: 2 passed`, then `5 tests run: 5 passed`; the third run shows `0 failed`, including Task 7's and Task 8's `host.rs` tests; the gate exits 0; the it run shows `0 failed`, including onboarding's `it_join` tests, which still go through `join_here`.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-cli
git commit -m "Have agmen host up claim the hub, create the first project and print an invite"
```

### Task 10: `agmen group members|remove|role|transfer|revoke`

**Files:**
- Create: `crates/agmen-cli/src/cmd/group.rs`
- Modify: `crates/agmen-cli/src/cmd/mod.rs:26-73,97-122` (`Group(group::GroupArgs)`), `crates/agmen-cli/src/cmd/account.rs:123-161` (`whoami` lists groups), `crates/agmen-cli/tests/cli.rs`, `crates/agmen-cli/tests/it_cli.rs`

**Interfaces:**
- Consumes: Task 3 routes, the `identities` field of the member list, the widened `DELETE /v1/identities/{id}`, and the refusal codes; Task 9 `Me.groups`; `client::{human, Api, send, refusal}`; `agmen_client::find_identity`.
- Produces:
  - `--group <name or id>` on every `agmen group` command, resolved by `resolve_group`.
  - `agmen group members [--group]`: one line per member, `<role> <handle> (<project names, comma separated>)`, in the hub's order, then one indented line per live identity, `  <identity id> <identity handle> in <project name>`.
  - `agmen group remove <handle> [--group]`: "removed <handle> from <group name>".
  - `agmen group role <handle> <admin|member> [--group]` (clap value enum, so `owner` is a usage error).
  - `agmen group transfer <handle> [--group] --yes`: without `--yes`, exit 2 with "this makes <handle> the owner of <group name>; only they can hand it back. Rerun with --yes".
  - `agmen group revoke <identity id>`: `DELETE /v1/identities/{id}`; prints "revoked <identity id>; its agent is disconnected and cannot reconnect". A 404 prints "no identity <identity id> that you can revoke (agmen group members lists them)" and exits 1. It takes no `--group`, since the identity id names its tenant. `agmen leave` stays the way to revoke this worktree's own identity.
  - `pub(crate) async fn resolve_group(human: &Human, flag: Option<&str>) -> Result<Group>` (Task 9's `client::Group`): with `--group`, the `/v1/me` `groups` entry whose tenant id equals it, else the entries whose name equals it exactly (a hosted name is the full stored `<owner handle>/<name>`); one match is the answer, none exits 2 with "no group <value>: agmen whoami lists yours", several exit 2 listing each `<name> (<id>)`. Without `--group`: this worktree's tenant, else the only entry in `groups`, else exit 2 with "name the group: --group <name or id> (agmen whoami lists them)", the wording the hosted plan's Task 12 expects. This is the one group resolver; the hosted plan's `--group` flags call it.
  - Refusals map to plain messages: `group_admin_required` "only the group's owner or an admin can do that"; `owner_required` "only the group's owner can do that"; `owner_cannot_be_removed` "the owner cannot be removed; transfer ownership first"; `not_a_group_admin` "make <handle> an admin first: agmen group role <handle> admin"; `no_such_member` "<handle> is not in this group"; `not_a_group_member` exits 3.
  - `whoami` prints `  group <name> (<id>): <role>` per group.
  - The hosted plan adds `seats` and `billing` beside these.

- [ ] **Step 1: Write the failing tests**

```rust
// tests/cli.rs
#[test] fn group_usage_errors_exit_2()  // "group role x owner", "group transfer x" (no --yes), "group remove", "group revoke": exit 2

// tests/it_cli.rs
#[tokio::test] async fn it_group_controls()  // owner O and member M in the group: `group members` lists both with roles; `group role M admin` then members shows admin;
                                            // `group transfer M --yes` makes M owner (O now admin); O's `group remove M`: exit 1, stderr "the owner cannot be removed";
                                            // M's `group remove O`: exit 0 and O has no tenant row
#[tokio::test] async fn it_group_member_cannot_change_roles()  // member runs `group role X admin`: exit 1; stderr "only the group's owner or an admin"
#[tokio::test] async fn it_group_revoke_identity()  // M joined a worktree: `group members` as O shows M's identity id; O's `group revoke <id>`: exit 0, stdout "revoked <id>";
                                                   // the identity's revoked_at is set; M's `group revoke` of O's identity: exit 1, stderr "no identity"
#[tokio::test] async fn it_group_flag_takes_id_or_name()  // O seeded into tenants "alpha" and "beta": `group members` with no flag outside a worktree: exit 2, stderr "name the group";
                                                         // `--group <beta id>` and `--group beta` list beta's members; `--group gamma`: exit 2, stderr "no group gamma";
                                                         // a third tenant also named "beta": `--group beta` exits 2 and stderr names both beta ids
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'test(=group_usage_errors_exit_2)'`, then `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-cli --locked --profile it -E 'test(/it_group_/)'`.
Expected: FAIL.

- [ ] **Step 3: Implement `group.rs`, register it, extend `whoami`**

- [ ] **Step 4: Run the tests to verify they pass, then the gate**

Run: the two Step 2 commands, then the gate.
Expected: `1 test run: 1 passed`, then `4 tests run: 4 passed`; the gate exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-cli
git commit -m "Add agmen group members, remove, role, transfer and revoke"
```

---

## Phase C: proof on real Docker

### Task 11: Self-host end to end, README and ADR

**Files:**
- Create: `docs/adr/<next free number>-self-host.md` (`0008` after the broker spike's `0007-broker-rmqtt.md`, since `docs/adr/` ends at `0006-claude-code-plugin.md` on `mvp/impl`, the spike runs first and takes `0007`, and the onboarding plan adds none; take the next free number at the time rather than a fixed one)
- Modify: `README.md` (a "Host it yourself" section)

**Interfaces:**
- Consumes: Tasks 1 to 10; a hub image built locally and tagged `ghcr.io/agmen-dev/agmen-hub:<workspace version>` until the onboarding plan's release pipeline publishes it.
- Produces: the ADR (compose layout and pins, the hub generating the broker API key on first start instead of `host up`, why `/mqtt` is a byte passthrough, the Funnel and ngrok findings below, and the check results) and the README section (requirements: Docker, plus Tailscale signed in with HTTPS and Funnel allowed, or an ngrok account, or an https address; the one command; `status`, `stop`, `logs`; where data lives and that `stop` keeps it; that the hub keeps all history unless `--retention-days <n>` sets a limit, and, when the hosted plan's pruning (its Task 9) has not shipped yet as this task runs, that the limit is stored but not yet applied; `agmen group members|remove|role|transfer|revoke`).

- [ ] **Step 1: Build the hub image**

Run: `docker build -f crates/agmen-hub/Dockerfile -t ghcr.io/agmen-dev/agmen-hub:$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name=="agmen-hub") | .version') .`
Expected: the build succeeds.

- [ ] **Step 2: Run the Tailscale path on a real machine and record each result in the ADR**

In a clone of a test repository on a machine with Tailscale signed in: `agmen host up`. Expected: the stack is healthy (`agmen host status` shows hub, emqx and postgres running and healthy); the browser approval signs in; the person is owner; the project exists; `.agmen/identity.json` is written; an invite link prints. From a second machine or home: open the link, `agmen join <link>`, then `agmen post FYI hi` from one side and `agmen watch --json` on the other receives it over `wss://<machine>.<tailnet>.ts.net/mqtt`. `docker compose logs hub` shows one `claim_token=` line and no claim token after the claim when the hub is restarted. `agmen host stop`, then `agmen host up` again: same secrets in `.env`, no second project, and the friend's watch reconnects. Record whether Funnel keeps an idle MQTT connection open for 10 minutes (the 30 s idle claim in selfhost-research.json is unverified; the client's keep-alive is 30 s).

- [ ] **Step 3: Run the ngrok path the same way and record it**

With Tailscale signed out and an ngrok authtoken: the tunnel URL is found through the agent API on the ephemeral port, the hub answers there, MQTT works over `/mqtt`, and the dev domain is the same after `agmen host stop` and `up`. Record whether the free plan's browser interstitial page shows on `/i` (selfhost-research.json lists it) and confirm `ngrok/ngrok:3.39.13-alpine` serves its agent API on port 4040 inside the container.

- [ ] **Step 4: Write the ADR and the README section**

- [ ] **Step 5: Check the text, then run the gate**

Run: `rg -n '[\x{2013}\x{2014}]' README.md docs/adr/*-self-host.md`, then the gate.
Expected: rg prints nothing and exits 1 (no en or em dashes); the gate exits 0.

- [ ] **Step 6: Commit**

```bash
git add README.md docs/adr
git commit -m "Record the self-host design and check, and document agmen host"
```

## Spec coverage

| Spec item | Task |
|---|---|
| Decision 7 step 1: Docker check with the install link | 7 |
| Decision 7 step 2: compose and `0600` env under `~/.agmen/host/<name>/`, generated secrets | 7 (database password, node cookie, dashboard password), 5 (broker API key) |
| Decision 7 step 3: pinned images; the hub keeps its own JWT key | 5, 7 |
| Decision 7 step 4: Tailscale Funnel, ngrok, `--public-url`; `/mqtt` on the same origin | 6, 8 |
| Decision 7 step 5: sign-in and the claim token (local log, single use, 1 hour) | 4, 9 |
| Decision 7 step 6: first project, agents set up, invite link | 2, 9 |
| `agmen host status`, `stop`, `logs` | 7, 8 |
| Free, no limits, no calls to agmen's servers | 4 (no seat checks in self-host mode), 7 (no external endpoints but the image registries and the tunnel) |
| Same member controls: invite, remove, change roles, revoke an identity | 3 (API), 10 (`agmen group remove`, `role`, `transfer`, `revoke`; invites as today, R55 and R58) |
| Decision 8 retention: "Self-host keeps everything unless the host sets a limit" | 7 (`--retention-days`, `AGMEN_RETENTION_DAYS` in `.env` and the compose file); the pruning itself is the hosted plan's Task 9 |
| Security notes: repository URLs carry no credentials | 2 (`RepoUrl` on `POST /v1/projects`), 9 (origin URL sent only when `RepoUrl` accepts it) |
| Decision 8 roles: owner, admins, one group on self-host, only owner and admins create projects | 1, 2, 3, 4 |
| Security notes: claim token, kick and ban on removal, pinned images | 3, 4, 7 |
| Flows: "Starter self-hosting" | 9, 11 |
