# agmen hosted Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A paid, multi-tenant agmen hub at `https://api.getagmen.ai` that anyone can start from `agmen init` with a 7-day trial, with seats, Paddle billing, quotas, retention and the launch safeguards of spec decision 10.

**Architecture:** One hub binary serves both modes; `AGMEN_HUB_MODE=hosted` turns on groups with plans, seats, the write gate (read-only and daily quotas), Paddle checkout, portal and signed webhooks, a plan clock that runs the trial and dunning timeline, and retention pruning. Plan rules live in one pure module (`billing/plan.rs`) that every other part asks. The roster fix (full roster over HTTP, small retained head), the isolation tests and the self-host retention limit ship in both modes. The hosted deployment is a kustomize overlay in `groupdrive/agmen-kubernetes`, the public site is static HTML on Cloudflare Pages.

**Tech Stack:** Rust 1.99.1 (edition 2024), axum 0.8, sqlx 0.9 on Postgres, reqwest 0.13, `hmac` 0.13 (already in `Cargo.lock`), clap 4; Paddle Billing API and webhooks; kustomize; Cloudflare Pages and Workers with wrangler 4.45.0 (on npm since 2025-10-23, engines `node >=18`; checked 2026-10-26); plain HTML and Node's built-in test runner for the site and the uptime probe.

**Spec:** `docs/specs/2026-10-26-onboarding-and-hosting-design.md` (decisions 8 and 10, with 9's results, and decision 6 under "Decisions Andy made" for the site). Cross-plan names: `.superpowers/sdd/onboarding/plan-contracts.md`.

**Consumes from earlier plans** (checked against each plan's Produces lists on 2026-10-26; each task's Interfaces block repeats what it uses):

- Onboarding plan (`docs/plans/onboarding.md`):
  - Task 1b: `pub(crate) async fn device_login(hub: &Url) -> Result<Human>` in `crates/agmen-cli/src/cmd/account.rs`.
  - Task 2: `POST /v1/invites` answers `{code, link, project, role, expires_at}`, `link` = `https://<hub origin>/i#<code>`.
  - Task 3: column `projects.repo_url`; `agmen_proto::RepoUrl` (`FromStr<Err = BadRepoUrl>`), refused with 422 `bad_repo_url`.
  - Task 4: `pub(crate) async fn run(root: &Path, identity: &IdentityFile, only: Option<AgentKind>, plugin: Plugin) -> Result<Vec<Change>>` in `crates/agmen-cli/src/setup/mod.rs`, called as `setup::run(root, identity, None, setup::plugin_choice(false)?)`.
  - Task 7: `pub(crate) async fn human_at(hub: &Url) -> Result<Option<Human>>` in `crates/agmen-cli/src/client.rs`; `pub(crate) async fn join_here(human: &Human, root: &Path, project: &str, args: &JoinArgs) -> Result<IdentityFile>` in `crates/agmen-cli/src/cmd/join.rs`; `agmen join [<link|code>] [--hub <url>] [--project <id>] [--handle <name>] [--kind agent|coordinator]`.
  - Tasks 13 and 14: hub image `ghcr.io/agmen-dev/agmen-hub:<version>`; Homebrew `agmen-dev/tap/agmen`; `install/install.sh` and `install/install.ps1`, uploaded to every release of `github.com/agmen-dev/agmen` as assets `install.sh` and `install.ps1` (`[[dist.extra-artifacts]]`). Task 17 uses only their URLs and does not wait for these tasks.
- Self-host plan (`docs/plans/self-host.md`):
  - Task 1: `TenantRole { Member, Admin, Owner }`, table `tenant_members (tenant_id, human_id, role)`, `tenant_role(pool: &PgPool, tenant: &str, human: &HumanId) -> Result<Option<TenantRole>, ApiError>`, `require_group_admin(role: Option<TenantRole>) -> Result<TenantRole, ApiError>` (403 `group_admin_required`), `join_tenant(conn: &mut PgConnection, tenant: &str, human: &str, role: TenantRole) -> Result<(), sqlx::Error>`, all in `crates/agmen-hub/src/tenants.rs`; `GET /v1/me` `groups: [{tenant, name, role}]` ordered by name, then tenant id. This plan's own functions take `&TenantId` and pass `tenant.as_str()` to these.
  - Task 2: `pub(crate) async fn create_project(conn: &mut PgConnection, tenant: &str, name: &str, repo_url: Option<RepoUrl>, creator: &HumanId) -> Result<NewProject, ApiError>` and `POST /v1/projects` (`projects::create`) in `crates/agmen-hub/src/projects.rs`; `is_display_name` (422 `bad_name`).
  - Task 3: `GET /v1/tenants/{tenant}/members` (each member with `projects` and live `identities`), `DELETE /v1/tenants/{tenant}/members/{handle}` (kick then ban, R36, R64), `PUT /v1/tenants/{tenant}/members/{handle}/role`, `POST /v1/tenants/{tenant}/transfer`, handlers in `tenants.rs`; `DELETE /v1/identities/{id}` widened so a group owner or admin can revoke a member's identity; `ban_and_kick` and `ban_until` made `pub(crate)` in `crates/agmen-hub/src/identities.rs`; refusal codes `owner_required`, `group_admin_required`, `not_a_group_member`.
  - Task 4: `Config.mode: HubMode { SelfHost, Hosted }` from `AGMEN_HUB_MODE`; `POST /v1/claim` (self-host only).
  - Task 5: `HubSecrets { Given { .. }, Generated { data_dir } }` from `AGMEN_DATA_DIR`; the image creates `/var/lib/agmen` owned by 65532.
  - Task 6: `GET /mqtt` (the same-origin MQTT proxy, `crates/agmen-hub/src/mqtt_proxy.rs`).
  - Tasks 7 to 9: `agmen host up [--name <host name>] [--public-url <url>] [--tunnel tailscale|ngrok] [--port <port>] [--wait <secs>] [--retention-days <1 to 36500>] [--project <project name>] [--handle <name>]` (`--name` is the host directory, `[a-z0-9][a-z0-9-]{0,31}`; `--handle` goes to `join_here` through `JoinArgs::with_handle`, in process); the compose file passes `AGMEN_RETENTION_DAYS: ${AGMEN_RETENTION_DAYS:-}`; `client::Me.groups: Vec<Group>` with `Group { tenant, name, role }` (Task 9).
  - Task 10: `crates/agmen-cli/src/cmd/group.rs` with `agmen group members|remove|role|transfer|revoke`, `--group <name or id>` on each (but `revoke`, whose identity id names its tenant), and `pub(crate) async fn resolve_group(human: &Human, flag: Option<&str>) -> Result<Group>`: the `/v1/me` `groups` entry whose tenant id equals the flag, else whose name equals it exactly (hosted names are the full `<owner handle>/<name>`); several matches exit 2 listing `<name> (<id>)`; no flag means this worktree's tenant, else the only group, else exit 2 "name the group: --group <name or id> (agmen whoami lists them)". It is the one group resolver; this plan calls it and does not change it.
- Broker spike plan (`docs/plans/broker-spike.md`): the ADR found by name, `docs/adr/*-broker-rmqtt.md` (number `NNNN`, the next free one, expected `0007`), with one `Verdict: pass` or `Verdict: fail` line under Decision and a Consequences list that names every `deny.toml` exception scoped "only through rmqtt in spike s11"; in `spikes/s11-rmqtt`: `Acl`, `TokenVerifier`, `BanList`, `client_id_allowed`, `Broker` (`kick`, `ban_username`, `unban_username`, `client`, `delete_retained`), `admin::router` (the EMQX-shaped admin shim) and the spike's `it_*` tests.

**Order:** Task 17 (the `getagmen.ai` site and legal pages) starts first, before Task 1 and before the onboarding plan's release pipeline exists (controller ruling, 2026-10-26), because Paddle reviews the site before it approves the account (spec, "Actions for Andy"). It needs no hub code and no other task of any plan; it names the installers only by their release URLs. Tasks 1 to 16 then run in order, after the onboarding, self-host and broker spike plans.

## Global Constraints

Copied from the spec. Every task's requirements include these.

- Hub mode: env `AGMEN_HUB_MODE`, values `self-host` (default) and `hosted`. "Self-host: one group, claim token, no seats, no billing, no quotas beyond today's rate limit. Hosted: many groups, trials, seats, Paddle, quotas, retention." (contracts)
- Anti-goals: "A free self-hosted hub phones home, needs agmen's servers to run, or limits seats." "A trial on agmen hosted can fill the database without bound." "agmen hosted runs EMQX for customers without a license that allows it."
- Onboarding choice: "**Self host (free, requires setup):** runs decision 7." "**agmen hosted (7-day free trial):** signs in with GitHub and creates a group on agmen's hub, with a first project and the person as creator. It then sets up their agents and prints an invite link."
- "**Group:** a group is a tenant. Its creator pays and controls it."
- "**Seats:** a seat is a human member of the tenant, counted once across all its projects. Agents are identities owned by members and do not take seats. They count toward the group's usage quota."
- Plans: Self host, Free, "Your own hardware, no limits". Team, "$25/month after a 7-day trial", "Creator plus 3 invited seats; $10/month per extra seat". Business, "$100/month for 10 seats; $200 for 25; $350 for 50; custom above 50", "Next-business-day email support, an onboarding call, longer history and higher quotas".
- Roles: "a tenant has one **owner** (the creator, who pays) and any number of **group admins**." Owner and group admins "create projects, see and change seats, remove a member from every project at once, and change roles." "What only the owner can do: open billing, and hand ownership (and billing) to another group admin."
- "An accepted invite takes a seat only if the person is not already a member of the group."
- "`agmen init` when you already own or administer a group: it adds a project to that group, and asks which group if there are several. `agmen init --new-group` starts another group. A second group gets no trial (one trial per GitHub account), so it goes straight to checkout."
- Trial: "the trial starts with no card." "one trial per GitHub account id." "a trial has Team's 4 seats. Adding a seat starts checkout." "a trial group picks Team or Business at checkout. Business has no separate trial." "in-app reminders in the CLI and a hub ALERT on the feed on days 5 and 7. No email is needed, so the GitHub app keeps no scopes." "the group becomes read-only for 14 days, so no new posts, claims or invites. Then it is deleted. Paying at any point restores it."
- Payment fails: "**Days 0 to 14:** full access, while the provider retries the card. The owner sees reminders." "**After 14 days:** read-only for 90 days, with the same restrictions as an expired trial." "**After that:** deleted. Paying at any point restores it."
- Seat changes: "**Team:** seats are a per-seat quantity at $10 each." "**Business:** sold in steps of 10, 25 and 50 seats. A group that needs an 11th seat moves to the 25 step, and the provider prorates the change." Upgrades and step changes happen "through the provider's checkout and portal".
- Quotas per group per day: "**Trial:** 5,000 messages." "**Team:** 10,000 messages, plus 1,000 for each extra seat." "**Business:** 50,000 for 10 seats, scaled by the step." "**At 80%:** the hub sends an ALERT to the owner." "**At 100%:** posts are refused with a new NACK reason, `quota_exceeded`, until 00:00 UTC." "**Stored bytes:** bounded by retention and the existing 96 KB message cap, so there is no separate byte quota."
- Billing: Paddle, merchant of record, "5% plus $0.50 per charge". "Team is one subscription with two items: a $25 base price, and a $10 seat price whose quantity is the number of extra seats." "Business is three prices: $100, $200 and $350, for 10, 25 and 50 seats. A step change swaps the item, with Paddle's proration." "The trial lives in the hub, not in Paddle." "the hub takes signed webhooks from the provider and keeps each tenant's plan state: trial, active, past due, read-only, deleted. Checkout and the billing portal are the provider's hosted pages. The hub stores no card data."
- Controls, in the CLI and the hub API: "see seats used and paid for", "add or remove paid seats", "remove a member, which revokes their identities through the existing kick-and-ban path", "change roles", "open the billing portal". "The desktop app's first-run choice and its settings screens use the same API. They become tasks in the desktop plan (`desktop/gpui`), built once the desktop app has a live backend instead of its fixture."
- Retention: "Team keeps 90 days of message history." "Business keeps 1 year." "Self-host keeps everything unless the host sets a limit."
- Decision 10: today's limit is "10 messages a second per identity with a burst of 30" (`crates/agmen-hub/src/ingest.rs:40-42`). The roster fix ships "before launch: no interim cap". Abuse limits: "one trial per GitHub account id, at most 3 groups owned per account, and the tenant quota above." Isolation: "one group can never read, post to or administer another's projects." Ops: "the nightly Postgres backup (already in agmen-kubernetes), restore tested once, and alerts when the hub or broker is down." Site: "pricing, terms of service, a privacy policy and a refund policy".
- Deployment: "one deployment in agmen's own cluster (`groupdrive/agmen-kubernetes`), sized as the cost model does: k3s on Hetzner". "Andy's own group is a tenant on it. Until the broker spike passes, Andy's dogfood hub keeps running EMQX for his own group". Spike fallback: "EMQX's managed cloud (Dedicated Flex: $234 a month at 1,000 sessions, about $642 at 5,000)".
- Domain: "`getagmen.ai` is the one canonical domain"; hosted hub `https://api.getagmen.ai`; install script `https://getagmen.ai/install.sh` and `install.ps1` (contracts). Images `ghcr.io/agmen-dev/agmen-hub:<version>`, pinned semver tags, never `latest`.
- Site: "hosted on Cloudflare Pages, which is free." Legal pages "adapted from Basecamp's open policies (CC BY 4.0, credit required)". They say "what agmen stores: the GitHub numeric id and login, and chat between people and their agents; retention periods; deletion after non-payment; Paddle as a subprocessor; a privacy contact; GDPR access and deletion rights." "Refund policy: written to match Paddle's 14-day refund window." "The adapted text is not legal advice."
- Gate (contracts): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`, `cargo nextest run --workspace --locked`, `cargo test --doc --workspace --locked`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`, `cargo deny check`. Integration tests (called "the `it` command" below): `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run --workspace --locked --profile it`, narrowed with `-p` and `-E` as each step says. No test name contains `restart` (`.config/nextest.toml` sends those to `it-restart`, which restarts the dev EMQX).
- Writing: no em or en dashes in code, docs, site text or commit messages; one-line commit messages with no trailers.

## Review Focus

1. **A fresh worktree on a pruned project.** A new identity (cursor 0) joins a project whose oldest messages retention removed. Today the client stops replay at the first gap (`crates/agmen-client/src/replay.rs:330-335`), so it would never catch up. Expected: replay skips the pruned seqs and delivers from the oldest kept message. Tests: Task 9 `it_replay_after_prune_catches_up` and `replay_skips_pruned_seqs`.
2. **Webhooks out of order or delivered twice.** Paddle redelivers `subscription.past_due` after a later `subscription.updated` (active), or sends one event id twice. Expected: the plan reflects the newest `occurred_at`; a repeat changes nothing. Tests: Task 6 `it_webhook_out_of_order_keeps_newest` and `it_webhook_repeat_is_idempotent`.
3. **Two people accept invites for the last seat at once.** Expected: exactly one joins; the other gets 403 `no_free_seat`. Test: Task 3 `it_last_seat_race_one_wins`.
4. **A group hits its quota while agents hold claims.** Refused HEARTBEATs would let leases lapse and lose work. Expected: HEARTBEAT, RELEASE and BYE are neither counted nor refused by the quota. Test: Task 4 `it_quota_exempts_heartbeat_release_bye`.
5. **A deleted group leaves data or live connections behind.** Expected: after deletion no row in any table names the tenant or its projects, and every identity's client ids are kicked and banned. Test: Task 8 `it_delete_group_leaves_no_rows`.

---

## Phase A: hub

### Task 1: Plan model and hosted settings

**Files:**
- Create: `crates/agmen-hub/src/billing/mod.rs`, `crates/agmen-hub/src/billing/plan.rs`, `crates/agmen-hub/src/billing/plan_tests.rs`, `crates/agmen-hub/src/billing/catalog.rs`
- Modify: `crates/agmen-hub/src/config.rs:186-218` (struct `Config`), `:281-358` (`from_lookup`), tests at `:361-513`; `crates/agmen-hub/src/lib.rs:21-61` (module and re-exports)

**Interfaces:**
- Consumes: `Config.mode: HubMode` (self-host Task 4).
- Produces (all `pub`, re-exported from `agmen_hub`):
  - `enum PlanState { Trial, Active, PastDue, ReadOnly, Deleted }`, `as_str()` giving `trial`, `active`, `past_due`, `read_only`, `deleted`, plus `FromStr`.
  - `enum BusinessStep { Seats10, Seats25, Seats50 }` with `fn seats(self) -> u32` (10, 25, 50) and `fn for_seats(n: u32) -> Option<Self>` (smallest step holding `n`; `None` above 50).
  - `enum Plan { Team { extra_seats: u32 }, Business(BusinessStep) }`.
  - `enum ReadOnlyCause { TrialEnded, AwaitingPayment, PaymentFailed, Canceled }`.
  - `enum Retention { Days(u32), Forever }`.
  - `enum BillingEvent { Active(Plan), PastDue, Canceled, Paid }`.
  - `enum PlanEffect { TrialReminder { day: u8, ends_at: Timestamp }, BecameReadOnly(ReadOnlyCause), Delete }` (only `tick` makes effects; `apply` returns the new record and its caller posts any ALERT).
  - `struct PlanRecord { pub state: PlanState, pub plan: Option<Plan>, pub trial_started_at: Option<Timestamp>, pub past_due_since: Option<Timestamp>, pub read_only_cause: Option<ReadOnlyCause>, pub read_only_since: Option<Timestamp>, pub reminders_sent: u8, pub last_event_at: Option<Timestamp> }` (passive data) with `fn trial(now: Timestamp) -> Self`, `fn awaiting_payment(now: Timestamp) -> Self`, `fn seats_paid(&self) -> u32`, `fn daily_quota(&self) -> u32`, `fn retention(&self) -> Retention`, `fn writable(&self) -> bool`, `fn trial_ends_at(&self) -> Option<Timestamp>`, `fn read_only_at(&self) -> Option<Timestamp>`, `fn delete_at(&self) -> Option<Timestamp>`, `fn tick(&self, now: Timestamp) -> (PlanRecord, Vec<PlanEffect>)`, `fn apply(&self, event: BillingEvent, occurred_at: Timestamp) -> PlanRecord`.
  - `fn next_utc_midnight(now: Timestamp) -> Timestamp`.
  - Constants: `TRIAL_DAYS = 7`, `TRIAL_REMINDER_OFFSETS_DAYS = [4, 6]` (day 5 and day 7 begin 4 and 6 days after the start), `TRIAL_READ_ONLY_DAYS = 14`, `PAST_DUE_GRACE_DAYS = 14`, `PAID_READ_ONLY_DAYS = 90`, `TEAM_INCLUDED_SEATS = 4`, `MAX_OWNED_GROUPS = 3`.
  - `struct Catalog { pub team_base: String, pub team_seat: String, pub business_10: String, pub business_25: String, pub business_50: String }` with `fn items_for(&self, plan: Plan) -> Vec<LineItem>`, `fn plan_of(&self, items: &[LineItem]) -> Option<Plan>`, `fn env_lines(&self) -> String`; `struct LineItem { pub price_id: String, pub quantity: u32 }`.
  - `struct PaddleConfig { pub api_url: Url, pub api_key: Secret, pub webhook_secret: Secret }`, `struct HostedConfig { pub paddle: PaddleConfig, pub catalog: Catalog }`.
  - `Config.hosted: Option<HostedConfig>` (`Some` exactly when `mode` is `Hosted`), `Config.retention_days: Option<u32>`.
  - Env: `AGMEN_PADDLE_API_URL` (default `https://api.paddle.com`; sandbox `https://sandbox-api.paddle.com`), `AGMEN_PADDLE_API_KEY`, `AGMEN_PADDLE_WEBHOOK_SECRET`, `AGMEN_PADDLE_PRICE_TEAM_BASE`, `AGMEN_PADDLE_PRICE_TEAM_SEAT`, `AGMEN_PADDLE_PRICE_BUSINESS_10`, `AGMEN_PADDLE_PRICE_BUSINESS_25`, `AGMEN_PADDLE_PRICE_BUSINESS_50`, `AGMEN_RETENTION_DAYS` (self-host only, 1 to 36500; unset or empty means no limit, since self-host's compose passes `${AGMEN_RETENTION_DAYS:-}`; ignored in hosted mode, where the plan decides).

Rules the tests pin (spec values): seats paid are 4 for a trial, `4 + extra_seats` for Team, the step for Business; quota is 5,000 for a trial, `10_000 + 1_000 * extra_seats` for Team, `50_000 * step / 10` for Business (50,000, 125,000, 250,000), and 0 when not writable; retention is 90 days for trial and Team, 365 for Business. Read-only lasts 14 days after `TrialEnded` and `AwaitingPayment`, 90 after `PaymentFailed` and `Canceled`. `apply` ignores an event whose `occurred_at` is before `last_event_at`, and anything on `Deleted`. `Active(plan)` and `Paid` restore any state but `Deleted` to `Active`; `PastDue` keeps the first `past_due_since`; `Canceled` on `Active` or `PastDue` goes read-only (`Canceled`); `PastDue` and `Canceled` on a trial change nothing.

- [ ] **Step 1: Write the failing tests** in `billing/plan_tests.rs` (wired with `#[cfg(test)] #[path = "plan_tests.rs"] mod tests;`) and `config.rs`:

```rust
#[test] fn trial_has_four_seats_and_5000_messages()      // PlanRecord::trial(t0): seats_paid 4, daily_quota 5_000, retention Days(90), writable, trial_ends_at t0+7d
#[test] fn team_quota_and_seats()                        // Team{extra_seats:0}: 4 seats, 10_000; Team{extra_seats:3}: 7 seats, 13_000
#[test] fn business_steps()                              // Seats10: 10 seats, 50_000, Days(365); Seats25: 25, 125_000; Seats50: 50, 250_000
#[test] fn step_for_seats()                              // for_seats(1)=Seats10, (10)=Seats10, (11)=Seats25, (25)=Seats25, (26)=Seats50, (50)=Seats50, (51)=None
#[test] fn trial_reminders_on_days_5_and_7()             // tick(t0+4d) -> [TrialReminder{day:5, ends_at:t0+7d}], reminders_sent 1; tick again at t0+4d -> no effect; tick(t0+6d) -> day 7
#[test] fn trial_end_is_read_only_for_14_days()          // tick(t0+7d): ReadOnly, cause TrialEnded, writable false, quota 0, delete_at t0+21d; tick(t0+21d) -> Deleted, [Delete]
#[test] fn awaiting_payment_deletes_after_14_days()      // awaiting_payment(t0): ReadOnly, delete_at t0+14d
#[test] fn payment_failure_timeline()                    // Active(Team) + PastDue at t1: PastDue, writable; tick(t1+14d) -> ReadOnly PaymentFailed, delete_at t1+104d; tick(t1+104d) -> Deleted
#[test] fn paying_restores()                             // from ReadOnly(TrialEnded), Active(Business Seats10) -> Active, read_only_* None; from ReadOnly(PaymentFailed), Paid -> Active with the old plan
#[test] fn cancel_goes_read_only_for_90_days()           // Active + Canceled at t1: ReadOnly Canceled, delete_at t1+90d
#[test] fn older_events_are_ignored()                    // last_event_at t2; apply(PastDue, t1 < t2) returns the record unchanged
#[test] fn deleted_is_final()                            // Deleted + Active(..) stays Deleted
#[test] fn past_due_keeps_first_since()                  // PastDue at t1 then PastDue at t1+2d: past_due_since stays t1
#[test] fn midnight_utc()                                // next_utc_midnight(2026-11-02T23:59:59Z) = 2026-11-03T00:00:00Z; of 00:00:00Z is the next day
#[test] fn catalog_round_trips()                         // items_for(Team{extra_seats:2}) = [base x1, seat x2]; Team{0} = [base x1]; plan_of inverts both; plan_of([business_25 x1]) = Business(Seats25); unknown price -> None
// config.rs
#[test] fn hosted_needs_paddle_settings()                // mode hosted without AGMEN_PADDLE_API_KEY -> ConfigError::Missing("AGMEN_PADDLE_API_KEY")
#[test] fn self_host_ignores_paddle_settings()           // mode self-host with every AGMEN_PADDLE_* set -> config.hosted is None
#[test] fn retention_days_bounded()                      // "30" -> Some(30); unset -> None; "" -> None; "0", "36501", "x" -> Invalid{name:"AGMEN_RETENTION_DAYS"}
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cargo nextest run -p agmen-hub --locked -E 'test(/plan_tests|config::tests/)'`. Expected: FAIL to compile (`billing` module missing).
- [ ] **Step 3: Implement** the types and functions above. `plan.rs` is pure (no I/O, time passed in); timestamps are `agmen_proto::Timestamp` (jiff). Config reads the Paddle variables only when `mode` is `Hosted`, so a self-hosted hub never builds a Paddle client.
- [ ] **Step 4: Run the tests to verify they pass.** Run: `cargo nextest run -p agmen-hub --locked -E 'test(/plan_tests|config::tests/)'`, then the gate. Expected: PASS, gate clean.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-hub/src/billing crates/agmen-hub/src/config.rs crates/agmen-hub/src/lib.rs
git commit -m "Add the hosted plan model and Paddle settings"
```

### Task 2: Hosted groups, trials and abuse limits

**Files:**
- Create: `crates/agmen-hub/migrations/0020_hosted_groups.up.sql`, `.down.sql`; `crates/agmen-hub/src/groups.rs`; `crates/agmen-hub/tests/it_groups.rs`; `crates/agmen-hub/tests/support/fake_paddle.rs`
- Modify: `crates/agmen-hub/src/http.rs:117-140` (route), `crates/agmen-hub/src/me.rs:14-67` (`groups` fields, `notices`), `crates/agmen-hub/src/tenants.rs` (self-host Task 3's `POST /v1/tenants/{tenant}/transfer` handler calls `check_owner_limit`), `crates/agmen-hub/src/main.rs:71-123` (`set-plan` subcommand), `crates/agmen-hub/src/test_hooks.rs`, `crates/agmen-hub/tests/support/mod.rs:231-320` (`TestHub::start_configured`, `TestHub::start_hosted`), `crates/agmen-cli/src/client.rs` (`Group` gains the two fields)

If the migration numbers 0020 to 0023 are taken when this plan starts, use the next free numbers, in the same order.

**Interfaces:**
- Consumes: Task 1 `PlanRecord`, `MAX_OWNED_GROUPS`; self-host Task 1 `tenant_members`, `TenantRole`, `join_tenant`, `GET /v1/me` `groups`; self-host Task 2 `create_project(conn, tenant, name, repo_url, creator) -> Result<NewProject, ApiError>`; onboarding Task 3 `agmen_proto::RepoUrl`; `humans.provider_sub` (the GitHub numeric id, `migrations/0002_core.up.sql:28-35`).
- Produces:
  - Tables: `tenant_plans (tenant_id PK REFERENCES tenants, state, plan text NULL ('team'|'business'), extra_seats int, business_step int NULL, trial_started_at, past_due_since, read_only_cause, read_only_since, reminders_sent smallint, last_event_at, paddle_customer_id text NULL, paddle_subscription_id text NULL UNIQUE, no_seat_alerted_at timestamptz NULL)`; `trial_grants (github_id text PRIMARY KEY, granted_at timestamptz)` with no foreign key, so it outlives deleted groups.
  - `pub(crate) async fn load_plan(db: impl PgExecutor<'_>, tenant: &TenantId) -> Result<Option<PlanRecord>, sqlx::Error>` and `pub(crate) async fn store_plan(tx: &mut Transaction<'_, Postgres>, tenant: &TenantId, record: &PlanRecord) -> Result<(), sqlx::Error>` in `groups.rs`.
  - `pub(crate) async fn seats_used(db: impl PgExecutor<'_>, tenant: &TenantId) -> Result<u32, sqlx::Error>`: rows of `tenant_members` for the tenant.
  - `pub(crate) async fn check_owner_limit(tx: &mut Transaction<'_, Postgres>, human: &HumanId) -> Result<(), ApiError>` (403 `group_limit`, "you already own 3 groups, the most one account can own").
  - `POST /v1/groups` (human token; hosted only, else 404 `not_hosted`), body `{name, project, repo_url?}` (unknown fields refused) answers 201 `{tenant, group, project, state, trial_ends_at, checkout_url}`. `name` matches `^[a-z0-9][a-z0-9-]{0,38}$` (else 422 `bad_group_name`); the stored tenant name is `<owner handle>/<name>`, so two customers can pick the same name. `project` and `repo_url` go through self-host's `create_project` (422 `bad_name`) and `RepoUrl::from_str` (422 `bad_repo_url`, the `BadRepoUrl` message as the text), the same rules as `POST /v1/projects`. One transaction: `check_owner_limit`, insert the tenant, its plan, the trial grant, `join_tenant(owner)`, then `create_project` on the same connection. The first group of a GitHub id gets `PlanRecord::trial(now)` and a `trial_grants` row; any later group gets `PlanRecord::awaiting_payment(now)` and a Team checkout link (Task 7's `PaddleClient::create_checkout`; until Task 7 lands, `checkout_url` is `null`).
  - `GET /v1/me` `groups` entries gain `state` (`"self_host"` on self-host, else the plan state) and `trial_ends_at` (null unless the state is `trial`). This is the one group list; there is no `GET /v1/groups`. CLI `client::Group` gains `#[serde(default)] state: Option<String>` and `#[serde(default)] trial_ends_at: Option<Timestamp>`.
  - `GET /v1/me` gains `notices: Vec<String>` (owner only, hosted only): trial from day 5, past due, read-only, each naming the group, the date and `agmen group billing`.
  - `agmen-hub set-plan --tenant <id> --plan team|business-10|business-25|business-50` sets `Active` with no Paddle ids (Andy's own group).
  - Test support: `TestHub::start_configured(db_url, github, signing, extra, edit: impl FnOnce(&mut Config)) -> TestHub` (the existing starts call it with a no-op), `TestHub::start_hosted(db_url, github, paddle: &FakePaddle, signing, extra) -> TestHub`; `FakePaddle::start() -> FakePaddle` with `requests() -> Vec<(Method, String, Value)>` and `sign(body: &[u8], ts: i64) -> String`.
  - Test hook: `Hub::set_plan(&self, tenant: &TenantId, record: &PlanRecord) -> Result<(), HookError>`.

- [ ] **Step 1: Write the failing tests** in `tests/it_groups.rs`:

```rust
#[tokio::test] async fn it_first_group_starts_trial()             // POST /v1/groups {name:"acme", project:"web"}: 201, state "trial", trial_ends_at = now+7d (within 5 s); tenant_members has caller as owner; memberships has caller as project admin; trial_grants has the GitHub id
#[tokio::test] async fn it_second_group_gets_no_trial()           // same caller, name "beta": 201, state "read_only"; tenant_plans.read_only_cause "awaiting_payment", delete_at now+14d
#[tokio::test] async fn it_fourth_owned_group_refused()           // caller owns 3: POST /v1/groups -> 403 group_limit; tenants count unchanged
#[tokio::test] async fn it_transfer_refused_when_receiver_owns_three() // admin B owns 3 groups; owner A transfers group 4 to B (POST /v1/tenants/{t}/transfer) -> 403 group_limit; A still owner
#[tokio::test] async fn it_same_group_name_two_owners()           // andy and adam both create "acme": both 201, tenant names "andy/acme" and "adam/acme"
#[tokio::test] async fn it_group_input_rules()                    // name "Acme" -> 422 bad_group_name; project " x" -> 422 bad_name; repo_url "https://u:p@github.com/a/b" -> 422 bad_repo_url; repo_url "https://github.com/acme/web" -> 201 and projects.repo_url stored; after the refusals, tenants and trial_grants counts are unchanged
#[tokio::test] async fn it_groups_not_on_self_host()              // self-host hub: POST /v1/groups -> 404 not_hosted; GET /v1/me groups lists the one group with state "self_host", trial_ends_at null
#[tokio::test] async fn it_me_groups_show_state()                 // after it_first_group_starts_trial's call: GET /v1/me groups[0] has state "trial" and trial_ends_at now+7d
#[tokio::test] async fn it_me_notices_on_day_5()                  // set_plan trial started now-4d: GET /v1/me as owner has 1 notice naming "andy/acme" and "agmen group billing"; as an admin: 0 notices
```

- [ ] **Step 2: Run them to verify they fail.** Run: `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-hub --locked --profile it -E 'binary(it_groups)'`. Expected: FAIL (route 404 or compile error).
- [ ] **Step 3: Implement** the migration, `groups.rs`, `check_owner_limit` (also called from the transfer handler in `tenants.rs`), the `/v1/me` fields and notices, `set-plan`, the CLI `Group` fields and the test support.
- [ ] **Step 4: Run the tests to verify they pass.** Run: the `it` command with `-p agmen-hub -E 'binary(it_groups) | binary(it_tenants) | binary(it_me)'`, then `cargo nextest run -p agmen-hub -p agmen-cli --locked` and the gate. Expected: PASS.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-hub/migrations crates/agmen-hub/src crates/agmen-hub/tests crates/agmen-cli/src/client.rs
git commit -m "Add hosted groups with trials and the three-group limit"
```

### Task 3: Seats and the seat check at invite acceptance

**Files:**
- Modify: `crates/agmen-hub/src/invites.rs:130-196` (`accept`), `crates/agmen-hub/src/groups.rs`, `crates/agmen-hub/src/http.rs:117-140`
- Test: `crates/agmen-hub/tests/it_groups.rs`

**Interfaces:**
- Consumes: Task 1 `PlanRecord::seats_paid`; Task 2 `load_plan`, `seats_used`, `tenant_plans.no_seat_alerted_at`; self-host Task 1 `tenant_role`, `require_group_admin`; `post_as_hub` (`crates/agmen-hub/src/ingest.rs:425`), `HubPost` (`:100-117`).
- Produces:
  - `GET /v1/tenants/{tenant}/seats` (owner or group admin; others 403 `group_admin_required`, the self-host refusal) answers `{used, paid, plan, state, quota, used_today}` (`used_today` from Task 4; 0 until then). On self-host it answers 404 `not_hosted`.
  - In `accept`, hosted only: lock the tenant's `tenant_plans` row `FOR UPDATE` first; if the human has no `tenant_members` row and `seats_used >= seats_paid`, answer 403 `no_free_seat` with "the group has no free seat; its owner or a group admin can add one with: agmen group seats add 1". After the refusal, at most once an hour per group (`no_seat_alerted_at`), post an ALERT to the owner's inbox in the invite's project naming the person and the same command.

- [ ] **Step 1: Write the failing tests** in `tests/it_groups.rs`:

```rust
#[tokio::test] async fn it_trial_fills_four_seats()          // owner + 3 accepted invites: GET seats {used:4, paid:4}; a 5th person's accept -> 403 no_free_seat, description contains "agmen group seats add 1"; invite still unused
#[tokio::test] async fn it_member_rejoins_without_a_seat()   // a group member accepts an invite to a second project of the group while used == paid: 200
#[tokio::test] async fn it_agents_take_no_seat()             // a member mints 3 agent identities: used stays the same
#[tokio::test] async fn it_last_seat_race_one_wins()         // used 3 of 4; two strangers accept two invites concurrently: exactly one 200 and one 403 no_free_seat; used 4
#[tokio::test] async fn it_no_seat_alerts_owner_once_an_hour() // two refused accepts within a minute: exactly 1 ALERT in the owner's inbox stream
#[tokio::test] async fn it_removed_member_frees_seat()       // used 4 of 4; owner DELETE /v1/tenants/{t}/members/{handle}; a new accept -> 200
#[tokio::test] async fn it_seats_admin_only()                // a plain member: GET seats -> 403 group_admin_required
#[tokio::test] async fn it_self_host_has_no_seat_limit()     // self-host hub: 10 accepts all 200; GET seats -> 404 not_hosted
```

- [ ] **Step 2: Run them to verify they fail.** Run: the `it` command with `-p agmen-hub -E 'binary(it_groups)'`. Expected: FAIL (`it_trial_fills_four_seats` gets 200 for the 5th person).
- [ ] **Step 3: Implement** the seats route and the check in `accept`.
- [ ] **Step 4: Run the tests to verify they pass.** Run: the `it` command with `-p agmen-hub -E 'binary(it_groups) | binary(it_auth)'` (the existing invite tests live in `it_auth.rs`), then the gate. Expected: PASS.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-hub/src crates/agmen-hub/tests/it_groups.rs
git commit -m "Check seats when an invite is accepted on agmen hosted"
```

### Task 4: Write gate: read-only and daily quotas

**Files:**
- Create: `crates/agmen-hub/migrations/0021_usage.up.sql`, `.down.sql`; `crates/agmen-hub/src/usage.rs`; `crates/agmen-hub/tests/it_quota.rs`
- Modify: `crates/agmen-proto/src/reply.rs:41-123` (`NackReason`, `as_str`, tests `:158-178`); `schema/` (regenerated); `crates/agmen-hub/src/ingest.rs:233-330` (`accept`); `crates/agmen-hub/src/invites.rs:43-109` and `:130-196`; `crates/agmen-hub/src/lanes.rs` (`replace`); `crates/agmen-hub/src/projects.rs` (self-host Task 2's `projects::create` handler); `crates/agmen-cli/src/output.rs:76` and `crates/agmen-cli/src/joined.rs:333-345` (hints); `crates/agmen-hub/src/test_hooks.rs`

**Interfaces:**
- Consumes: Task 1 `PlanRecord::{writable, daily_quota}`, `next_utc_midnight`; Task 2 `load_plan`.
- Produces:
  - `NackReason::QuotaExceeded` (`quota_exceeded`), `NackReason::ReadOnly` (`read_only`), both final, and `NackReason::Unknown` (`unknown`) with `#[serde(other)]`, so a client reads a reason newer than itself as a final rejection.
  - Table `tenant_usage (tenant_id, day date, messages integer, warned boolean, PRIMARY KEY (tenant_id, day))`; `day` is `(now() AT TIME ZONE 'UTC')::date`.
  - `pub(crate) enum Gate { Open, Quota { limit: u32 }, ReadOnly }` and `pub(crate) async fn write_gate(hub: &Hub, tenant: &TenantId) -> Result<Gate, sqlx::Error>` (`Open` on self-host).
  - `pub(crate) async fn take_quota(tx: &mut Transaction<'_, Postgres>, tenant: &TenantId, limit: u32) -> Result<Taken, sqlx::Error>` with `pub(crate) enum Taken { Counted { messages: u32, warn: bool }, Exhausted }`: one upsert that increments only while `messages < limit`, and sets `warned` the first time `messages * 5 >= limit * 4`.
  - `pub(crate) async fn post_owner_alert(hub: &Hub, tenant: &TenantId, body: &str) -> Result<(), PostError>`: an ALERT on the feed of every project of the tenant, mentioning the owner. Tasks 6 and 8 reuse it.
  - `pub(crate) fn refuse_read_only() -> ApiError` (403 `group_read_only`, "the group is read-only until its owner pays: agmen group billing").
  - Ingest order in `accept`: after `recorded_reply` (a resend of a recorded message still gets its ACK), `ReadOnly` refuses every verb but RELEASE and BYE with NACK `read_only`; `Quota` calls `take_quota` first in the message transaction (before `lock_head`, so every transaction takes the usage row before stream heads) for every verb but HEARTBEAT, RELEASE and BYE; `Exhausted` answers NACK `quota_exceeded` with `retry_after_ms` = milliseconds to `next_utc_midnight`. A `warn` posts `post_owner_alert` after commit: "andy/acme used 80% of today's 5,000 messages; posts stop at 5,000 until 00:00 UTC."
  - HTTP gate: `invites::create`, `invites::accept`, `lanes::replace` and `projects::create` answer `refuse_read_only()` when the gate is `ReadOnly`.
  - CLI: `pub(crate) fn rejection_hint(reason: &str) -> Option<&'static str>` in `output.rs`; `rejected: quota_exceeded` adds "the group used today's message quota; it resets at 00:00 UTC", `rejected: read_only` adds "the group is read-only until its owner pays".
  - Test hook: `Hub::set_usage(&self, tenant: &TenantId, day_offset: i32, messages: u32) -> Result<(), HookError>`.

- [ ] **Step 1: Write the failing tests:**

```rust
// crates/agmen-proto/src/reply.rs
#[test] fn new_reasons_are_final()                 // QuotaExceeded, ReadOnly: is_retryable false; as_str "quota_exceeded", "read_only"
#[test] fn unknown_nack_reason_parses()            // from_value(json!("some_future_reason")) = NackReason::Unknown; not retryable
// crates/agmen-hub/tests/it_quota.rs (hub.ingest in process)
#[tokio::test] async fn it_trial_quota_5000()                       // set_usage(today, 4_999): next FYI ACK; the one after NACK quota_exceeded, retryable false, retry_after_ms = ms to next 00:00 UTC (within 2 s)
#[tokio::test] async fn it_80_percent_alerts_owner_once()           // set_usage(today, 3_999): FYI -> 1 ALERT on the feed mentioning the owner, body contains "80%" and "5,000"; 2 more FYIs -> still 1 ALERT
#[tokio::test] async fn it_team_quota_counts_extra_seats()          // set_plan Active Team{extra_seats:2}: limit 12_000 (set_usage 11_999: ACK, then NACK)
#[tokio::test] async fn it_quota_resets_at_utc_midnight()           // set_usage(yesterday, 5_000): FYI today -> ACK
#[tokio::test] async fn it_quota_exempts_heartbeat_release_bye()    // at limit: HEARTBEAT, RELEASE, BYE ACKed; tenant_usage.messages unchanged
#[tokio::test] async fn it_resend_after_quota_gets_ack()            // FYI id X ACKed at 4_999; resend X at 5_000 -> the same ACK, not quota_exceeded
#[tokio::test] async fn it_read_only_refuses_posts_claims_invites() // set_plan ReadOnly: FYI and CLAIM -> NACK read_only; RELEASE ACKed; POST /v1/invites, PUT lanes and POST /v1/projects -> 403 group_read_only; GET streams 200
#[tokio::test] async fn it_self_host_has_no_quota()                 // self-host: no tenant_usage rows after 40 messages from 2 identities
// crates/agmen-cli/src/output.rs
#[test] fn quota_hint()                            // rejection_hint("quota_exceeded") contains "00:00 UTC"; ("schema") None
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cargo nextest run -p agmen-proto -p agmen-cli --locked -E 'test(new_reasons_are_final) | test(unknown_nack_reason_parses) | test(quota_hint)'` and the `it` command with `-p agmen-hub -E 'binary(it_quota)'`. Expected: FAIL.
- [ ] **Step 3: Implement**, then regenerate the schema: `cargo run -p agmen-proto --example export-schema`.
- [ ] **Step 4: Run the tests to verify they pass.** Run: `cargo nextest run -p agmen-proto -p agmen-cli --locked` (includes `committed_schema_matches_types`) and the `it` command with `-p agmen-hub -E 'binary(it_quota) | binary(it_ingest)'`, then the gate. Expected: PASS.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-proto schema crates/agmen-hub/migrations crates/agmen-hub/src crates/agmen-hub/tests/it_quota.rs crates/agmen-cli/src
git commit -m "Refuse writes on read-only groups and enforce daily quotas"
```

### Task 5: Paddle client and catalog

**Files:**
- Create: `crates/agmen-hub/src/billing/paddle.rs`, `crates/agmen-hub/src/billing/paddle_tests.rs`
- Modify: `crates/agmen-hub/src/http.rs:54-114` (`Inner.paddle: Option<PaddleClient>`), `crates/agmen-hub/src/main.rs:71-123` (`paddle-catalog` subcommand), `crates/agmen-hub/tests/support/fake_paddle.rs`

**Interfaces:**
- Consumes: Task 1 `PaddleConfig`, `Catalog`, `LineItem`, `Plan`; `upstream_client()` (`crates/agmen-hub/src/http.rs:186`).
- Produces:
  - `pub struct PaddleClient` with `fn new(config: &PaddleConfig, http: reqwest::Client) -> Result<Self, PaddleError>`;
    `async fn create_checkout(&self, tenant: &TenantId, items: &[LineItem]) -> Result<Url, PaddleError>` (`POST /transactions` with `items` and `custom_data: {"agmen_tenant": "<id>"}`, returns `data.checkout.url`);
    `async fn update_items(&self, subscription_id: &str, items: &[LineItem]) -> Result<Vec<LineItem>, PaddleError>` (`PATCH /subscriptions/{id}`, `proration_billing_mode: "prorated_immediately"`; an item with quantity 0 is left out);
    `async fn portal_url(&self, customer_id: &str, subscription_id: &str) -> Result<Url, PaddleError>` (`POST /customers/{id}/portal-sessions`, returns `data.urls.general.overview`);
    `async fn ensure_catalog(&self) -> Result<Catalog, PaddleError>` (finds or creates products "agmen Team" and "agmen Business" and the five monthly USD prices, matched by `custom_data.agmen_key` `team_base`, `team_seat`, `business_10`, `business_25`, `business_50`: amounts `"2500"`, `"1000"` (quantity 1 to 1000), `"10000"`, `"20000"`, `"35000"`, `billing_cycle {interval:"month", frequency:1}`).
  - `pub enum PaddleError { Http(reqwest::Error), Refused { status: u16, code: String }, Shape(String) }`.
  - `agmen-hub paddle-catalog` (reads `AGMEN_PADDLE_API_URL`, `AGMEN_PADDLE_API_KEY`) prints `Catalog::env_lines()`: the five `AGMEN_PADDLE_PRICE_*=pri_...` lines.

- [ ] **Step 1: Check the request shapes** against developer.paddle.com (transactions create, subscriptions update, customer portal sessions, prices create, and whether transaction `custom_data` is copied to the subscription). Write any difference into the doc comments of `paddle.rs` and adjust the shapes above before writing tests. Expected: each shape confirmed or corrected, with the doc URL in the comment.
- [ ] **Step 2: Write the failing tests** against `FakePaddle` (no network):

```rust
#[tokio::test] async fn checkout_sends_items_and_tenant()      // create_checkout(t, [base x1, seat x2]): fake saw POST /transactions with those items and custom_data.agmen_tenant = t; returns the fake's checkout url
#[tokio::test] async fn update_drops_zero_quantity()           // update_items(sub, [base x1, seat x0]): PATCH body items has 1 entry; proration_billing_mode "prorated_immediately"
#[tokio::test] async fn portal_url_from_session()               // returns data.urls.general.overview
#[tokio::test] async fn refusal_carries_status_and_code()      // fake answers 400 {"error":{"code":"bad_request"}} -> PaddleError::Refused{status:400, code:"bad_request"}
#[tokio::test] async fn ensure_catalog_is_idempotent()         // first run creates 2 products and 5 prices; second run creates none and returns the same ids
#[tokio::test] async fn api_key_sent_as_bearer()               // every request has Authorization: Bearer <key>
```

- [ ] **Step 3: Run them to verify they fail.** Run: `cargo nextest run -p agmen-hub --locked -E 'test(/paddle_tests/)'`. Expected: FAIL to compile.
- [ ] **Step 4: Implement** `paddle.rs` and the subcommand.
- [ ] **Step 5: Run the tests to verify they pass.** Run: `cargo nextest run -p agmen-hub --locked -E 'test(/paddle_tests/)'`, then the gate. Expected: PASS.
- [ ] **Step 6: Commit:**

```bash
git add crates/agmen-hub/src crates/agmen-hub/tests/support/fake_paddle.rs
git commit -m "Add the Paddle client and the paddle-catalog command"
```

### Task 6: Signed webhooks and the plan state machine

**Files:**
- Create: `crates/agmen-hub/src/billing/webhook.rs`, `crates/agmen-hub/src/billing/webhook_tests.rs`, `crates/agmen-hub/migrations/0022_paddle_events.up.sql`, `.down.sql`, `crates/agmen-hub/tests/it_billing.rs`
- Modify: `crates/agmen-hub/Cargo.toml` (add `hmac = "0.13.0"`, already locked), `crates/agmen-hub/src/http.rs:117-140`, `THIRD_PARTY_NOTICES.md` (regenerated: `hmac` joins `agmen-hub`'s graph)

**Interfaces:**
- Consumes: Task 1 `PlanRecord::apply`, `BillingEvent`, `Catalog::plan_of`, `LineItem`; Task 2 `load_plan`, `store_plan`; Task 4 `post_owner_alert`.
- Produces:
  - `pub(crate) fn verify_signature(header: &str, body: &[u8], secret: &Secret, now: Timestamp) -> Result<(), SignatureError>`: header `Paddle-Signature: ts=<unix>;h1=<hex>`, HMAC-SHA256 over `<ts>:<raw body>` compared in constant time (`Mac::verify_slice`), any of several `h1` values may match, `|now - ts| <= 300 s`. `pub(crate) enum SignatureError { Malformed, Stale, Mismatch }`.
  - `#[derive(Deserialize)] pub(crate) struct PaddleEvent { pub(crate) event_id: String, pub(crate) event_type: String, pub(crate) occurred_at: Timestamp, pub(crate) data: serde_json::Value }` (Paddle's notification envelope; `data` is the entity).
  - `pub(crate) struct PaddleIds { pub(crate) customer_id: String, pub(crate) subscription_id: String }`.
  - `pub(crate) enum WebhookError { Body(serde_json::Error), NoTenant, BadTenant(String), UnknownPrice(String), Shape(&'static str) }` (`thiserror`; `Shape` names the missing field).
  - `pub(crate) fn billing_event(event: &PaddleEvent, catalog: &Catalog) -> Result<Option<(TenantId, BillingEvent, PaddleIds)>, WebhookError>`: tenant from `data.custom_data.agmen_tenant`; a `subscription.*` event with status `active` gives `Active(plan_of(items))` (`UnknownPrice` when `plan_of` is `None`), `past_due` gives `PastDue`, `canceled` gives `Canceled`; `transaction.completed` with a `subscription_id` gives `Paid`; anything else `None`.
  - Table `paddle_events (event_id text PRIMARY KEY, received_at timestamptz)`.
  - `POST /v1/paddle/webhook` (hosted only, else 404 `not_hosted`; no bearer token): 401 `bad_signature` before parsing anything; a known `event_id` answers 200 and changes nothing; otherwise in one transaction it inserts the event id, applies the event with `occurred_at`, stores the Paddle ids and the plan, and answers 200. A `WebhookError` is logged at error level with the event id and answers 200, since a retry cannot fix it. Entering `PastDue` posts `post_owner_alert` with the date the group turns read-only.

- [ ] **Step 1: Write the failing tests:**

```rust
// billing/webhook_tests.rs
#[test] fn signature_accepts_valid()            // secret "pdl_ntfset_test", ts now, h1 = HMAC("ts:body"): Ok
#[test] fn signature_rejects_tampered_body()    // one byte changed: Mismatch
#[test] fn signature_rejects_stale()            // ts = now - 301 s: Stale; now - 299 s: Ok
#[test] fn signature_rejects_malformed()        // "", "ts=1", "h1=ab", "ts=x;h1=zz": Malformed
#[test] fn signature_accepts_any_h1()           // "ts=..;h1=<wrong>;h1=<right>": Ok
#[test] fn events_map_to_billing()              // subscription.activated active with [base x1, seat x1] -> Active(Team{extra_seats:1}); subscription.updated past_due -> PastDue; subscription.canceled -> Canceled; transaction.completed -> Paid; customer.updated -> None
#[test] fn event_errors()                       // no custom_data -> NoTenant; unknown price id "pri_x" -> UnknownPrice("pri_x"); body "{" -> Body
// tests/it_billing.rs (FakePaddle signs bodies)
#[tokio::test] async fn it_unsigned_webhook_refused()            // no header -> 401 bad_signature; tenant_plans unchanged; paddle_events empty
#[tokio::test] async fn it_activation_ends_trial()               // trial group + subscription.activated (Business 25) -> state "active", GET /v1/tenants/{t}/seats paid 25; paddle ids stored
#[tokio::test] async fn it_past_due_keeps_access_and_alerts()     // active + subscription.past_due -> state "past_due"; FYI ACKed; 1 ALERT names the read-only date (+14d)
#[tokio::test] async fn it_payment_restores_read_only()          // ReadOnly(PaymentFailed) + transaction.completed -> "active"; FYI ACKed
#[tokio::test] async fn it_webhook_repeat_is_idempotent()        // the same past_due event twice: past_due_since equals the first occurred_at; 1 ALERT
#[tokio::test] async fn it_webhook_out_of_order_keeps_newest()   // updated(active, t2) then past_due(t1 < t2): state "active"
#[tokio::test] async fn it_webhook_for_other_tenant_only_changes_it() // event naming tenant B: tenant A's row unchanged
#[tokio::test] async fn it_webhook_not_on_self_host()            // self-host: POST /v1/paddle/webhook -> 404 not_hosted
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cargo nextest run -p agmen-hub --locked -E 'test(/webhook_tests/)'` and the `it` command with `-p agmen-hub -E 'binary(it_billing)'`. Expected: FAIL.
- [ ] **Step 3: Implement** `webhook.rs`, the migration and the route; read the raw body as `axum::body::Bytes` so the HMAC sees the exact bytes.
- [ ] **Step 4: Run the tests to verify they pass.** Run: the two commands of Step 2, then `scripts/third-party-notices.sh` (onboarding Task 11) to regenerate the notice for the new dependency, then `scripts/third-party-notices.sh --check` and the gate. Expected: PASS; the check exits 0.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-hub/Cargo.toml Cargo.lock crates/agmen-hub/migrations crates/agmen-hub/src crates/agmen-hub/tests/it_billing.rs THIRD_PARTY_NOTICES.md
git commit -m "Take signed Paddle webhooks and keep each group's plan state"
```

### Task 7: Checkout, seat changes and the portal

**Files:**
- Modify: `crates/agmen-hub/src/groups.rs`, `crates/agmen-hub/src/http.rs:117-140`
- Test: `crates/agmen-hub/tests/it_billing.rs`

**Interfaces:**
- Consumes: Task 1 `BusinessStep::for_seats`, `Catalog::items_for`; Task 2 `seats_used`; Task 5 `PaddleClient`.
- Produces:
  - `PUT /v1/tenants/{tenant}/seats` body `{paid: u32}` (owner or group admin). Trial: `paid <= 4` answers 200 unchanged, more answers 200 `{checkout_url}` for Team with `paid - 4` extra seats. Team: `paid >= 4` updates the seat item to `paid - 4`. Business: moves to `BusinessStep::for_seats(paid)`. `paid < used` answers 409 `seats_in_use` ("remove members first"); `paid > 50` answers 422 `seats_over_50` ("more than 50 seats is custom: write to sales@getagmen.ai"). After Paddle accepts, the hub stores the plan from the returned items; the webhook confirms it.
  - `POST /v1/tenants/{tenant}/plan` body `{plan: "team"|"business", seats?: u32}` (owner only, else 403 `owner_required`): without a subscription it answers `{checkout_url}`; with one it swaps the items and answers `{plan, seats_paid}`.
  - `POST /v1/tenants/{tenant}/portal` (owner only) answers `{portal_url}`; without a subscription 409 `no_subscription`.
  - All three answer 404 `not_hosted` on self-host. Task 2's `POST /v1/groups` now fills `checkout_url` for a group with no trial.

- [ ] **Step 1: Write the failing tests** in `tests/it_billing.rs`:

```rust
#[tokio::test] async fn it_trial_seat_add_starts_checkout()   // trial, PUT seats {paid:5} -> {checkout_url}; fake saw items [team_base x1, team_seat x1]; plan unchanged
#[tokio::test] async fn it_team_seat_add_updates_quantity()   // Active Team{0} with sub: PUT {paid:6} -> fake PATCH [base x1, seat x2]; GET seats paid 6
#[tokio::test] async fn it_business_eleventh_seat_moves_to_25() // Active Business Seats10: PUT {paid:11} -> PATCH [business_25 x1]; paid 25
#[tokio::test] async fn it_seats_below_used_refused()         // used 5, PUT {paid:4} -> 409 seats_in_use; no PATCH sent
#[tokio::test] async fn it_seats_over_50_refused()            // PUT {paid:51} -> 422 seats_over_50
#[tokio::test] async fn it_team_to_business()                 // Active Team: POST plan {plan:"business"} -> PATCH [business_10 x1]
#[tokio::test] async fn it_billing_owner_only()               // group admin: POST plan and POST portal -> 403 owner_required; PUT seats -> 200
#[tokio::test] async fn it_portal_needs_subscription()        // trial: POST portal -> 409 no_subscription; active: 200 with the fake's url
#[tokio::test] async fn it_second_group_gets_checkout_url()    // trial used: POST /v1/groups -> checkout_url equals the fake's url; fake saw [team_base x1] with custom_data.agmen_tenant = the new tenant
```

- [ ] **Step 2: Run them to verify they fail.** Run: the `it` command with `-p agmen-hub -E 'binary(it_billing)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the three routes.
- [ ] **Step 4: Run the tests to verify they pass.** Run: the `it` command with `-p agmen-hub -E 'binary(it_billing) | binary(it_groups)'`, then the gate. Expected: PASS.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-hub/src crates/agmen-hub/tests/it_billing.rs
git commit -m "Add checkout, seat changes and the billing portal"
```

### Task 8: Plan clock: reminders, read-only and deletion

**Files:**
- Create: `crates/agmen-hub/src/billing/clock.rs`, `crates/agmen-hub/tests/it_plan_clock.rs`
- Modify: `crates/agmen-hub/src/mqtt.rs:149-193` (spawn the clock with the other tasks), `crates/agmen-hub/src/lib.rs` (`pub use billing::clock::PlanClockError`), `crates/agmen-hub/src/test_hooks.rs`

**Interfaces:**
- Consumes: Task 1 `PlanRecord::tick`, `PlanEffect`; Task 2 `load_plan`, `store_plan`; Task 4 `post_owner_alert`; `ban_and_kick` and `ban_until` (`crates/agmen-hub/src/identities.rs:351-381`, `pub(crate)` since self-host Task 3).
- Produces:
  - `#[non_exhaustive] pub enum PlanClockError { Database(sqlx::Error), Post(PostError), Broker { tenant: String, failed: usize }, GaveUp { failures: u32, source: Box<PlanClockError> } }` (`thiserror`).
  - `pub(crate) async fn tick_plans(hub: &Hub, now: Timestamp) -> Result<usize, PlanClockError>`: up to 100 tenants per call whose next deadline has passed, plus any tenant whose state is `deleted` but whose row still exists (hosted only), each in its own transaction; effects post after commit: `TrialReminder` posts "Your agmen hosted trial of <group> ends on <date>. Run agmen group billing to keep going."; `Delete` runs `delete_group`.
  - `pub(crate) async fn delete_group(hub: &Hub, tenant: &TenantId) -> Result<(), PlanClockError>`: sets `revoked_at` on every identity of the tenant, bans and kicks each; if any ban or kick fails it returns `Broker { failed }` before deleting rows, so the next tick retries. Then it deletes every row that names the tenant or its projects (outbox, messages, `stream_heads` keyed `<project>/...`, ledger tables, invites, identity tokens, identities, memberships, `tenant_members`, projects, `tenant_usage`, `tenant_plans`, the tenant), keeping `trial_grants` and `paddle_events`.
  - `Hub::run_plan_clock(&self) -> Result<(), PlanClockError>`: `tick_plans` every 60 s, gives up with `GaveUp` after 10 failures in a row like `run_lease_sweeper` (`crates/agmen-hub/src/claims.rs:132-158`).
  - Test hook: `Hub::tick_plans_at(&self, now: Timestamp) -> Result<usize, HookError>`.

- [ ] **Step 1: Write the failing tests** in `tests/it_plan_clock.rs`:

```rust
#[tokio::test] async fn it_day_5_and_7_reminders()          // trial started t0; tick at t0+4d: 1 ALERT per project feed mentioning the owner, containing the end date; tick again: no more; tick t0+6d: 1 more
#[tokio::test] async fn it_trial_end_goes_read_only()       // tick t0+7d: state "read_only"; FYI -> NACK read_only
#[tokio::test] async fn it_unpaid_trial_deleted_after_14_days() // tick t0+21d: tenants row gone; GET /v1/me groups is empty; trial_grants row kept
#[tokio::test] async fn it_past_due_read_only_after_14_days()   // past_due_since t1; tick t1+14d -> "read_only"; tick t1+104d -> deleted
#[tokio::test] async fn it_delete_group_leaves_no_rows()     // a group with 2 projects, messages, claims, invites, identities (one connected over MQTT): after delete_group, for every table in information_schema with a tenant_id or project_id column, 0 rows name the tenant or its projects; the connected client is disconnected within 2 s; its broker-token request -> 403
#[tokio::test] async fn it_other_groups_untouched()          // deleting group A leaves group B's row counts unchanged
#[tokio::test] async fn it_trial_grant_survives_deletion()   // GitHub id 42's trial group is deleted; id 42 creates a group: state "read_only", no second trial
```

- [ ] **Step 2: Run them to verify they fail.** Run: the `it` command with `-p agmen-hub -E 'binary(it_plan_clock)'`. Expected: FAIL.
- [ ] **Step 3: Implement** `clock.rs` and spawn `run_plan_clock` from `start_messaging` only in hosted mode.
- [ ] **Step 4: Run the tests to verify they pass.** Run: the `it` command with `-p agmen-hub -E 'binary(it_plan_clock) | binary(it_messaging_start)'`, then the gate. Expected: PASS.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-hub/src crates/agmen-hub/tests/it_plan_clock.rs
git commit -m "Run the trial and payment timeline and delete unpaid groups"
```

### Task 9: Retention pruning

**Files:**
- Create: `crates/agmen-hub/src/retention.rs`, `crates/agmen-hub/migrations/0023_retention.up.sql`, `.down.sql`, `crates/agmen-hub/tests/it_retention.rs`
- Modify: `crates/agmen-hub/src/replay.rs:32-40` (`Page`) and its query; `crates/agmen-client/src/hub.rs:19-31` (`Page`); `crates/agmen-client/src/cursor.rs:81` (add `skip_to`); `crates/agmen-client/src/replay.rs:322-341` (`replay`); `crates/agmen-client/src/replay_tests.rs`; `crates/agmen-hub/src/mqtt.rs:149-193`; `crates/agmen-hub/src/lib.rs` (`pub use retention::RetentionError`)

**Interfaces:**
- Consumes: Task 1 `PlanRecord::retention`, `Retention`, `Config.retention_days`; Task 2 `TestHub::start_configured`; self-host Task 7's compose line `AGMEN_RETENTION_DAYS: ${AGMEN_RETENTION_DAYS:-}` and `host up --retention-days`.
- Produces:
  - Column `stream_heads.pruned_through bigint NOT NULL DEFAULT 0`.
  - `#[non_exhaustive] pub enum RetentionError { Database(sqlx::Error), GaveUp { failures: u32, source: Box<RetentionError> } }` (`thiserror`).
  - `pub(crate) async fn prune_once(hub: &Hub, now: Timestamp) -> Result<u64, RetentionError>`: per tenant, deletes `messages` with `hub_at < now - retention` in batches of 5,000 rows, and in the same transaction raises each touched stream's `pruned_through` to the highest deleted seq. Hosted uses each plan's retention; self-host uses `AGMEN_RETENTION_DAYS`.
  - `Hub::run_retention(&self) -> Result<(), RetentionError>`: `prune_once` at start, then every hour; gives up with `GaveUp` after 10 failures in a row, as in Task 8.
  - `start_messaging` spawns `run_retention` in hosted mode, and in self-host mode only when `retention_days` is `Some`, so a self-hosted hub with no limit runs no pruning task.
  - Replay `Page` gains `pruned_through: u64` (hub) and `#[serde(default)] pub pruned_through: u64` (client).
  - `pub fn skip_to(&self, stream: &Stream, seq: u64) -> Result<(), ClientError>` on the cursor store: raises the delivery cursor to `seq`, never lowers it.
  - Client `replay` calls `skip_to(stream, page.pruned_through)` when `page.pruned_through > after`, before delivering the page.
  - Test hook: `Hub::prune_at(&self, now: Timestamp) -> Result<u64, HookError>`.

- [ ] **Step 1: Write the failing tests:**

```rust
// tests/it_retention.rs
#[tokio::test] async fn it_team_keeps_90_days()            // messages at now-91d and now-89d: prune_at(now) deletes only the first; pruned_through = its seq
#[tokio::test] async fn it_business_keeps_1_year()         // now-364d kept, now-366d deleted
#[tokio::test] async fn it_self_host_keeps_everything()    // no AGMEN_RETENTION_DAYS: prune_at(now) keeps a 10-year-old message
#[tokio::test] async fn it_self_host_limit_applies()       // AGMEN_RETENTION_DAYS=30: prune_at(now) deletes now-31d
#[tokio::test] async fn it_self_host_limit_runs_on_start() // start_configured with retention_days Some(30), a message at now-31d, then start_messaging (no hook): the message is gone within 5 s
#[tokio::test] async fn it_self_host_no_limit_prunes_nothing() // retention_days None, a message at now-3650d, start_messaging, wait 2 s: the message is still there
#[tokio::test] async fn it_prune_is_batched()              // 12,000 old messages: deleted in 3 statements; all gone
#[tokio::test] async fn it_replay_after_prune_catches_up() // feed seqs 1..=10, 1..=6 pruned; a new identity's session with cursor 0 delivers seqs 7..=10 in order and its cursor ends at 10
// crates/agmen-client/src/replay_tests.rs
#[tokio::test] async fn replay_skips_pruned_seqs()        // fake hub page {pruned_through:6, messages 7..=8, head 8}, cursor 0: delivers 7 and 8, no "does not follow the cursor" stop
#[test] fn skip_to_never_lowers()                          // cursor 9, skip_to(5): still 9
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cargo nextest run -p agmen-client --locked -E 'test(/replay_skips_pruned_seqs|skip_to_never_lowers/)'` and the `it` command with `-p agmen-hub -E 'binary(it_retention)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the migration, `retention.rs`, the page field, `skip_to`, the replay change, and the spawn rule in `start_messaging`.
- [ ] **Step 4: Run the tests to verify they pass.** Run: `cargo nextest run -p agmen-client --locked` and the `it` command with `-p agmen-hub -p agmen-client -E 'binary(it_retention) | binary(it_replay) | binary(it_client)'`, then the gate. Expected: PASS.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-hub/migrations crates/agmen-hub/src crates/agmen-hub/tests/it_retention.rs crates/agmen-client/src
git commit -m "Prune message history to each plan's retention"
```

### Task 10: The roster fix

This amends ruling R51 (`state/roster` was the whole array) and R60 (handles resolved through the retained roster). The spec replaces both, so the retained-roster assertions in `crates/agmen-client/tests/it_state.rs:39-66` change with it.

**Files:**
- Create: `crates/agmen-hub/src/roster.rs`, `crates/agmen-hub/tests/it_roster.rs`
- Modify: `crates/agmen-hub/src/presence.rs:270-317` (move `Hub::roster` to `roster.rs`), `:425-469` (`flush`: publish the head, drop the shortcut at `:455-461`); `crates/agmen-hub/src/http.rs:117-140`; `crates/agmen-client/src/hub.rs` (add `roster`); `crates/agmen-client/src/state.rs:1-7` (docs); `crates/agmen-cli/src/joined.rs:158-187` (`resolve`) and `:189-210` (`mentions`); their callers `crates/agmen-cli/src/cmd/post.rs:75`, `:79`, `:161`, `:301` and `crates/agmen-cli/src/mcp/tools.rs:183`; `crates/agmen-cli/src/mcp/tools.rs:95-100`, `:241-256` (`state` tool); `crates/agmen-client/tests/it_state.rs:39-66`; `docs/protocol.md:34-39`; `mods/agmen-claude/skills/agmen/SKILL.md:8`

**Interfaces:**
- Consumes: `RosterEntry` (`crates/agmen-hub/src/presence.rs:70-87`), `Caller` (`crates/agmen-hub/src/auth/mod.rs:50`), the replay access rule (`crates/agmen-hub/src/replay.rs:49-95`: identity or human member of the project).
- Produces:
  - `GET /v1/projects/{p}/roster` (identity token of the project or human member, else 403): the full array of R51 entries, with `ETag: "<etag>"`.
  - `pub(crate) fn roster_head(roster: &[RosterEntry]) -> RosterHead` with `struct RosterHead { count: usize, online: usize, etag: String }`, `etag` the first 16 hex characters of SHA-256 of the array's JSON. The retained `state/roster` is `{"count":N,"online":M,"etag":"..."}`, under 4 KiB at any size.
  - `pub async fn roster(&self) -> Result<Value, ClientError>` on `HubClient`.
  - CLI: `pub(crate) async fn resolve(&self, names: &[String]) -> Result<Vec<IdentityId>>` and `pub(crate) async fn mentions(&self, mentions: &[String]) -> Result<Vec<Mention>>` read the roster over HTTP, so both drop today's `via: Via<'_>` parameter (added in f9ede79); the five call sites above drop their `via` argument. `read_state(via, key)` stays for the `state` tool's other keys. The `state` tool with key `roster` answers `state/roster: [<full array>]` from HTTP, so its output keeps today's form.
  - The plugin has no roster reader (`rg roster mods/agmen-claude/hooks` finds none); only its skill text changes, to say `state` with key `roster` lists everyone.

- [ ] **Step 1: Write the failing tests:**

```rust
// crates/agmen-hub/src/roster.rs
#[test] fn head_counts_and_hashes()                    // 3 entries, 1 online: count 3, online 1, etag 16 hex chars; reordering statuses changes the etag
#[test] fn head_stays_small()                          // 500 entries with 39-char handles: serialized head < 4_096 bytes
// tests/it_roster.rs
#[tokio::test] async fn it_full_roster_over_http()     // 5 humans x 4 agents + 5 humans = 25 entries: GET roster returns 25; retained state/roster {count:25, ...} under 4 KiB
#[tokio::test] async fn it_head_changes_with_roster()  // mint an identity: retained etag differs from before; GET roster ETag equals the new head etag
#[tokio::test] async fn it_roster_members_only()       // a human of another project: 403; an identity of this project: 200
// crates/agmen-cli/tests/it_cli.rs (keeps it_handles_resolve_through_the_roster, :320) and it_mcp.rs:277 (output still starts "state/roster: [")
#[tokio::test] async fn it_handles_resolve_past_25()   // 30 identities: agmen dm <the 30th handle> "hi" exits 0
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cargo nextest run -p agmen-hub --locked -E 'test(/roster::tests/)'` and the `it` command with `-p agmen-hub -p agmen-cli -p agmen-client -E 'binary(it_roster) | binary(it_cli) | binary(it_mcp) | binary(it_state)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the route, the head, `HubClient::roster`, the CLI and tool changes, and update `docs/protocol.md` (the `state/roster` row and line 39) and `it_state.rs` (`roster_status` reads `HubClient::roster`).
- [ ] **Step 4: Run the tests to verify they pass.** Run: the two commands of Step 2, then the gate. Expected: PASS.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-hub/src crates/agmen-hub/tests/it_roster.rs crates/agmen-client crates/agmen-cli docs/protocol.md mods/agmen-claude/skills/agmen/SKILL.md
git commit -m "Serve the full roster over HTTP and retain a small roster head"
```

### Task 11: Tenant isolation tests

**Files:**
- Create: `crates/agmen-hub/tests/it_tenancy.rs`

**Interfaces:**
- Consumes: every route registered in `crates/agmen-hub/src` (today `http.rs:117-140`, `health.rs:19-23` and `auth/mod.rs:41-45`, plus Tasks 2 to 10 and the onboarding and self-host plans' routes); `tests/support/mqtt.rs` for broker checks.
- Produces: the CI guard that one group can never read, post to or administer another's projects.

The two lists the guard checks, as the routes stand after this plan and its two predecessors:

- `ROUTES` (each tried with B's ids): `DELETE /v1/identities/{id}`, `GET /v1/identities/{id}/broker-token`, `POST /v1/invites` (body naming B's project), `POST /v1/invites/{code}/accept` (B's code is unknown to A, so 404), `GET /v1/projects/{p}/streams/{s}`, `GET` and `PUT /v1/projects/{project}/lanes`, `POST /v1/projects/{project}/check-path`, `GET /v1/projects/{project}/items`, `GET /v1/projects/{p}/roster`, `POST /v1/projects` (body naming B's tenant), `GET /v1/tenants/{tenant}/members`, `DELETE /v1/tenants/{tenant}/members/{handle}`, `PUT /v1/tenants/{tenant}/members/{handle}/role`, `POST /v1/tenants/{tenant}/transfer`, `GET` and `PUT /v1/tenants/{tenant}/seats`, `POST /v1/tenants/{tenant}/plan`, `POST /v1/tenants/{tenant}/portal`; `POST /v1/identities` with B's project in the body.
- `NOT_SCOPED` (each with its reason): `/livez`, `/healthz`, `/statusz` (no tenant data); `/v1/jwks` (public keys); `/i` (static page); `/v1/auth/device` and `/v1/auth/token` (sign-in, no tenant; onboarding Task 1 removes `/v1/auth/device/approve` and both `/v1/auth/oauth/github` routes, so they are not listed); `/v1/me` (the caller's own rows only); `/v1/claim` (self-host only, one group); `/mqtt` (byte proxy; the broker ACL is tested in `it_no_cross_tenant_broker`); `/v1/paddle/webhook` (signed by Paddle, no bearer token); `POST /v1/groups` (creates a new tenant).
- Skipped files: `crates/agmen-hub/src/test_github.rs` (onboarding Task 1's `FakeGithub`: GitHub's device code, access token, `/user` and `/users/{login}` routes) and `crates/agmen-hub/src/broker/shim.rs` (Task 14's EMQX-shaped admin shim). Both compile only with `test-hooks` and serve test servers of their own, never a route of the hub, so the guard does not read them.

- [ ] **Step 1: Write the tests.** Two hosted groups A and B, each with an owner, an admin, a member, an identity and a project. For every `ROUTES` row the table names the method, the path with B's ids, and the token from A that tries it.

```rust
#[test] fn isolation_table_covers_every_route()          // reads every .rs under src/ (CARGO_MANIFEST_DIR) but the two skipped files, collects the path literal of each .route( call, allowing whitespace and newlines before it (http.rs:126 and :133 wrap the call); each path is in ROUTES or NOT_SCOPED; fails naming the missing path
#[tokio::test] async fn it_no_cross_tenant_http()        // each ROUTES row with A's owner, admin, member and identity tokens against B's ids: 403 or 404, never 2xx; B's row counts unchanged afterwards
#[tokio::test] async fn it_no_cross_tenant_broker()      // A's identity: publish to B's up/ -> PUBACK 0x87; subscribe to B's feed, state/# and presence/+ -> SUBACK 0x87
#[tokio::test] async fn it_invite_cannot_cross_tenants() // A's admin creates an invite naming B's project -> 403
#[tokio::test] async fn it_hub_posts_stay_in_tenant()    // A's quota ALERT and trial reminder appear only in A's streams
```

- [ ] **Step 2: Run them.** Run: `cargo nextest run -p agmen-hub --locked -E 'test(isolation_table_covers_every_route)'` and the `it` command with `-p agmen-hub -E 'binary(it_tenancy)'`. Expected: PASS. A path the guard names that is in neither list is a route this plan did not know of: add it to `ROUTES` unless it carries no tenant data. Any 2xx is a bug in the route that answered it: fix it there with a regression test in that route's own test file, then rerun.
- [ ] **Step 3: Run the gate.** Expected: clean.
- [ ] **Step 4: Commit:**

```bash
git add crates/agmen-hub/tests/it_tenancy.rs
git commit -m "Add tenant isolation tests over every hub route"
```

---

## Phase B: CLI

### Task 12: `agmen init`

**Files:**
- Create: `crates/agmen-cli/src/cmd/init.rs`, `crates/agmen-cli/tests/it_init.rs`
- Modify: `crates/agmen-cli/src/cmd/mod.rs:5-11`, `:26-73`, `:95-122`; `crates/agmen-cli/tests/cli.rs` (`join.rs` is not modified: `join_here` is `pub(crate)` from onboarding Task 7 and `JoinArgs::with_handle` comes from self-host Task 9)

**Interfaces:**
- Consumes:
  - Onboarding: `client::human_at(hub: &Url) -> Result<Option<Human>>` (Task 7); `account::device_login(hub: &Url) -> Result<Human>` (Task 1b); `join::join_here(human: &Human, root: &Path, project: &str, args: &JoinArgs) -> Result<IdentityFile>`; `setup::run(root: &Path, identity: &IdentityFile, only: Option<AgentKind>, plugin: Plugin) -> Result<Vec<Change>>` with `setup::plugin_choice(false)?`; `POST /v1/invites` `link`; `agmen_proto::RepoUrl`.
  - Self-host: `agmen host up [--project <project name>] [--handle <name>]` (Task 9); `client::Me.groups` (Task 9); `pub(crate) fn JoinArgs::with_handle(handle: Option<String>) -> JoinArgs` in `join.rs` (Task 9: no invite or code, no `--hub`, no project, kind `agent`); `resolve_group(human: &Human, flag: Option<&str>) -> Result<Group>` (Task 10), which already matches a tenant id or an exact group name.
  - This plan: Task 2 `POST /v1/groups`, `GET /v1/me` `groups` with `state` and `trial_ends_at`; self-host `POST /v1/projects`; `crates/agmen-cli/src/git.rs` worktree helpers; `Fail` (`crates/agmen-cli/src/output.rs:54-80`).
- Produces:
  - `agmen init [--self-host | --hosted] [--hub <url>] [--group <name or id>] [--new-group] [--name <project name>] [--handle <name>]` and `pub(crate) const HOSTED_HUB: &str = "https://api.getagmen.ai"` (overridden by `--hub` or env `AGMEN_HOSTED_HUB`). `--group` and `--new-group` conflict with `--self-host` (clap `conflicts_with`); `--handle` works with both choices.
  - `--group` goes through self-host's `resolve_group(human, flag.as_deref())` unchanged, so `init`, `agmen group seats` and `agmen group billing` take a group the same way every `agmen group` command does.
  - On a terminal without a choice flag it asks, with the spec's words: `1) Self host (free, requires setup)` and `2) agmen hosted (7-day free trial)`. Off a terminal (`std::io::IsTerminal`) without a flag it exits 2: "choose --self-host or --hosted".
  - Outside a git worktree it exits 2: "run agmen init inside your project's git repository".
  - Self host runs `<current exe>` with `pub(crate) fn host_up_args(project: Option<&str>, handle: Option<&str>) -> Vec<String>` and inherited stdio, passing `--name <project>` on as `host up --project <project>` and `--handle <name>` as `host up --handle <name>` (host up's own `--name` is the host directory and is never set by init).
  - Hosted, in process: `human_at(hub)`, else `device_login(hub)`; read `/v1/me` `groups`; with `--group`, `resolve_group` picks it and it must be owned or administered (else exit 1 "only the group's owner or an admin can add a project"); with no owned or administered group, or with `--new-group`, POST `/v1/groups` named after the repository directory (lowercased, characters outside `[a-z0-9-]` replaced by `-`, cut to 39) with `repo_url` from `git remote get-url origin` only when `RepoUrl::from_str` accepts it; with one, POST `/v1/projects` to it; with several and no `--group`, ask which on a terminal, else exit 2 listing them. Then `join_here(human, root, project, &JoinArgs::with_handle(handle))`, `setup::run`, POST `/v1/invites`, and print the link, the group, its state and the trial end date or the checkout link.

- [ ] **Step 1: Write the failing tests:**

```rust
// tests/cli.rs (no hub)
#[test] fn init_without_choice_off_tty_exits_2()       // stdin not a TTY, no flag: exit 2, stderr contains "--self-host or --hosted"
#[test] fn init_outside_worktree_exits_2()             // temp dir without git: exit 2, stderr contains "inside your project's git repository"
#[test] fn init_flags_conflict()                       // --self-host --hosted: exit 2; --self-host --group x: exit 2; --self-host --new-group: exit 2
// tests/it_init.rs (TestHub::start_hosted, signed in through the CLI test support)
#[tokio::test] async fn it_init_hosted_first_group()           // exit 0; stdout contains "/i#" and "trial"; .agmen/identity.json written; GET /v1/me groups: 1 group, state "trial", trial_ends_at now+7d
#[tokio::test] async fn it_init_hosted_second_group_checkout() // trial used, --new-group: exit 0; stdout contains the FakePaddle checkout url and "read-only"
#[tokio::test] async fn it_init_adds_project_to_own_group()    // one owned group: exit 0; tenants count 1, projects count 2
#[tokio::test] async fn it_init_several_groups_needs_choice()  // two groups, off a TTY, no --group: exit 2 listing both names
#[tokio::test] async fn it_init_group_by_name()                // two groups "andy/acme" and "andy/beta": --group andy/beta: exit 0, the new project is in beta; --group <beta's tenant id>: same (through self-host's resolve_group)
#[tokio::test] async fn it_init_hosted_group_limit()           // owns 3, --new-group: exit 1 (a hub refusal, output.rs:95-113), stderr contains "3 groups"
// crates/agmen-cli/src/cmd/init.rs
#[test] fn host_up_args_map_name_to_project()  // host_up_args(Some("widgets"), Some("sol")) = ["host", "up", "--project", "widgets", "--handle", "sol"]; host_up_args(None, None) = ["host", "up"]
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cargo nextest run -p agmen-cli --locked -E 'test(/^init_/) | test(host_up_args_map_name_to_project)'` and the `it` command with `-p agmen-cli -E 'binary(it_init)'`. Expected: FAIL (`unrecognized subcommand 'init'`).
- [ ] **Step 3: Implement** `init.rs` and wire `init` into `Command`.
- [ ] **Step 4: Run the tests to verify they pass.** Run: the two commands of Step 2 plus `-E 'binary(it_cli)'` (self-host's group tests use `resolve_group`), then the gate. Expected: PASS.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-cli/src/cmd crates/agmen-cli/tests/cli.rs crates/agmen-cli/tests/it_init.rs
git commit -m "Add agmen init with the self host or agmen hosted choice"
```

### Task 13: `agmen group seats|billing`, reminders, and the desktop hand-off

**Files:**
- Modify: `crates/agmen-cli/src/cmd/group.rs` (add `Seats` and `Billing` to self-host's subcommand enum), `crates/agmen-cli/src/cmd/account.rs` (`whoami`, `invite`: print notices), `crates/agmen-cli/src/cmd/init.rs`
- Create: `crates/agmen-cli/tests/it_group.rs`

**Interfaces:**
- Consumes: Task 2 `GET /v1/me` `groups` and `notices`; Task 3 `GET /v1/tenants/{tenant}/seats`; Task 7 `PUT .../seats`, `POST .../plan`, `POST .../portal`; self-host Task 10's `resolve_group(human: &Human, flag: Option<&str>) -> Result<Group>` and its `group` subcommand enum.
- Produces:
  - `agmen group seats [--group <name or id>]` prints `seats: <used> used of <paid> paid (<plan>, <state>)` and today's quota use.
  - `agmen group seats add <n>` and `agmen group seats remove <n>` send `PUT seats {paid: paid + n}` and `{paid: paid - n}`; a trial or Paddle checkout answer prints "Finish checkout to add the seats: <url>".
  - `agmen group billing [--group <name or id>] [--plan team|business] [--seats <n>]` prints the portal link for an active or past-due group, the checkout link for a trial or read-only one, and with `--plan` changes the plan.
  - `pub(crate) fn print_notices(notices: &[String])`: each notice on stderr prefixed `agmen: `, called by `whoami`, `invite`, `init` and every `group` subcommand.
  - On self-host (`/v1/me` group state `self_host`), `seats` and `billing` exit 2: "this hub is self-hosted: no seats or billing".

- [ ] **Step 1: Write the failing tests** in `tests/it_group.rs`:

```rust
#[tokio::test] async fn it_group_seats_shows_use()          // trial with 2 members: stdout "seats: 2 used of 4 paid (trial, trial)"
#[tokio::test] async fn it_group_seats_add_in_trial()       // seats add 1: exit 0, stdout contains the checkout url
#[tokio::test] async fn it_group_seats_remove_below_used()  // exit 1, stderr contains "remove members first"
#[tokio::test] async fn it_group_billing_portal()           // active with sub: stdout contains the portal url
#[tokio::test] async fn it_group_billing_admin_refused()    // group admin: exit 1, stderr contains "owner"
#[tokio::test] async fn it_whoami_prints_trial_notice()     // trial on day 5: stderr contains "agmen: " and "agmen group billing"
#[tokio::test] async fn it_group_seats_self_host()          // self-host hub: exit 2, "self-hosted"
```

- [ ] **Step 2: Run them to verify they fail.** Run: the `it` command with `-p agmen-cli -E 'binary(it_group)'`. Expected: FAIL.
- [ ] **Step 3: Implement** the two subcommands and `print_notices`.
- [ ] **Step 4: Run the tests to verify they pass.** Run: the `it` command with `-p agmen-cli -E 'binary(it_group) | binary(it_cli) | binary(it_init)'`, then the gate. Expected: PASS.
- [ ] **Step 5: Commit:**

```bash
git add crates/agmen-cli/src/cmd crates/agmen-cli/tests/it_group.rs
git commit -m "Add agmen group seats and billing"
```

- [ ] **Step 6: Andy action: hand the desktop screens to the desktop plan.** `desktop/gpui` is checked out in its own worktree with claimed work, so this plan does not edit it. Ask Andy to append this section to `docs/plans/desktop.md` on `desktop/gpui` (or have the session working that branch do it); per the spec, these tasks are built once the desktop app has a live backend instead of its fixture:

  > **Task H1: First-run choice.** On first start with no sign-in, the app shows the two choices with the spec's words, `Self host (free, requires setup)` and `agmen hosted (7-day free trial)`. Hosted signs in through the hub's device flow at `https://api.getagmen.ai`, then calls `POST /v1/groups {name, project, repo_url?}` and shows the answer's `state`, `trial_ends_at` or `checkout_url`, and the invite `link` from `POST /v1/invites`. Self host shows the `agmen host up` command to run. Fixture data for the screenshot harness: one trial group, one read-only group.
  >
  > **Task H2: Group settings: seats, members and billing.** A settings page per group from `GET /v1/me` `groups`: seats from `GET /v1/tenants/{tenant}/seats` (`used`, `paid`, `plan`, `state`, `quota`, `used_today`), add or remove seats with `PUT /v1/tenants/{tenant}/seats {paid}` (open `checkout_url` in the browser when one comes back), members from `GET /v1/tenants/{tenant}/members` with remove (`DELETE .../members/{handle}`) and role (`PUT .../members/{handle}/role`), and, for the owner only, plan change (`POST /v1/tenants/{tenant}/plan`) and the billing portal (`POST /v1/tenants/{tenant}/portal`, opened in the browser). Refusal codes shown in plain words: `group_admin_required`, `owner_required`, `seats_in_use`, `seats_over_50`, `no_subscription`, `not_hosted`. The `notices` from `GET /v1/me` show as a banner.

  Expected: Andy confirms the section is in `docs/plans/desktop.md` on `desktop/gpui`, or says where it went instead.

---

## Phase C: broker, deployment, operations, site

### Task 14: Embedded RMQTT in the hub (only when the spike passed)

Find the spike's ADR by name: `ls docs/adr/*-broker-rmqtt.md` (one file), and read the line under Decision that starts with `Verdict:`. If it reads `Verdict: fail`, skip this task and take branch B in Task 15. If no file matches, stop and tell Andy: the spike has not finished.

**Files:**
- Create: `crates/agmen-hub/src/broker/mod.rs` (`BrokerAdmin`, `BrokerError`), `crates/agmen-hub/src/broker/embedded.rs` (from the spike's `broker.rs`, `acl.rs` and token check), `crates/agmen-hub/src/broker/shim.rs` (the spike's `admin.rs`, compiled only with `test-hooks`), `crates/agmen-hub/tests/it_broker.rs`, `crates/agmen-hub/tests/support/broker.rs`, `crates/agmen-client/tests/it_embedded_first_ack.rs`
- Modify: `crates/agmen-hub/Cargo.toml` (the RMQTT crates and versions the ADR records; dev-dependency `rumqttc` gains feature `websocket`), `Cargo.lock`, `deny.toml` (each exception the ADR's Consequences list, re-scoped from "only through rmqtt in spike s11" to "only through rmqtt in agmen-hub (embedded broker, docs/adr/NNNN-broker-rmqtt.md)"; an exception the hub build no longer needs is removed), `about.toml` (the same license exceptions as `deny.toml` for `agmen-hub`, since onboarding Task 11's `about.toml` accepts exactly the licenses `deny.toml` allows), `THIRD_PARTY_NOTICES.md` (regenerated: `rmqtt`, `rmqtt-plugins` and their dependencies join `agmen-hub`'s graph), `crates/agmen-hub/src/config.rs` (broker settings), `crates/agmen-hub/src/http.rs:54-155` (`Inner.emqx` becomes `Inner.broker: BrokerAdmin`; `Hub::emqx()` becomes `Hub::broker()`), `crates/agmen-hub/src/identities.rs:363-381` (`ban_and_kick` calls `broker()`), `crates/agmen-hub/src/main.rs:125-154` (stop the broker after the server returns), `crates/agmen-hub/src/test_hooks.rs` (`Hub::connect_with_broker`), `crates/agmen-hub/tests/support/mod.rs:247-320` and `:564-652` (TestHub and the admin helpers), `crates/agmen-hub/tests/support/mqtt.rs:22-32` (`mqtt_addr`), `docs/adr/0002-broker-emqx.md` (a "Superseded for agmen hosted by" line naming the spike's ADR)

**Interfaces:**
- Consumes: the spike crate `spikes/s11-rmqtt` (`Acl`, `TokenVerifier`, `BanList`, `client_id_allowed`, `Broker`, `admin::router`) and its ADR's "Consequences" list; `BrokerTokens::jwks(&self) -> JwkSet` (`crates/agmen-hub/src/broker_tokens.rs:261`); self-host Task 5 `AGMEN_DATA_DIR` and `HubSecrets`.
- Produces:
  - Env `AGMEN_BROKER` = `emqx` (default) or `embedded`. With `embedded`: `AGMEN_EMBEDDED_TCP_LISTEN` (default `127.0.0.1:1883`, the hub's own client), `AGMEN_EMBEDDED_WS_LISTEN` (default `0.0.0.0:8083`, path `/mqtt`), `AGMEN_DATA_DIR` required (else `ConfigError::Missing("AGMEN_DATA_DIR")`), `AGMEN_MQTT_URL` defaults to `mqtt://<tcp listen>`, `AGMEN_EMQX_API_URL` is not read, and the EMQX API secret in `HubSecrets` goes unused.
  - `pub struct EmbeddedConfig { pub tcp: SocketAddr, pub ws: SocketAddr, pub data_dir: PathBuf }` (`data_dir` is `<AGMEN_DATA_DIR>/rmqtt`, under the `/var/lib/agmen` the image creates for user 65532); `pub enum BrokerChoice { Emqx(EmqxConfig), Embedded(EmbeddedConfig) }` as `Config.broker`, replacing `Config.emqx`.
  - `pub const MAX_PACKET_BYTES: u32 = 96 * 1024;` (98304).
  - `#[derive(Clone)] pub struct EmbeddedBroker` with `pub async fn start_embedded(config: &EmbeddedConfig, jwks: JwkSet) -> Result<EmbeddedBroker, BrokerError>` (returns once both listeners are bound; verifies tokens against `jwks` in process, binds client ids to identities, applies each token's `acl` claim with everything else denied, caps packets at `MAX_PACKET_BYTES`, keeps shared subscriptions, retained QoS 1, wills and 1-day sessions in sled under `data_dir`, and keeps the ban list there), `kick(&self, client_id: &str) -> Result<bool, BrokerError>`, `ban_username(&self, username: &str, until: Timestamp) -> Result<(), BrokerError>`, `unban_username(&self, username: &str) -> Result<bool, BrokerError>`, `client(&self, client_id: &str) -> Option<ClientInfo>` (the spike's `ClientInfo { connected, subscriptions }` plus `connected_at: Option<Timestamp>`, the time the current connection was accepted, `None` when offline), `delete_retained(&self, topic: &str) -> Result<(), BrokerError>`, `stop(&self) -> Result<(), BrokerError>` (closes the listeners and flushes sled).
  - `pub enum BrokerAdmin { Emqx(EmqxAdmin), Embedded(EmbeddedBroker) }` with `async fn kick(&self, client_id: &str) -> Result<(), BrokerError>`, `async fn ban_username(&self, username: &str, until: Timestamp) -> Result<(), BrokerError>` (the shapes of `EmqxAdmin::kick` and `ban_username`, `crates/agmen-hub/src/emqx.rs:108`, `:148`) and `async fn stop(&self) -> Result<(), BrokerError>` (no-op for EMQX). `#[non_exhaustive] pub enum BrokerError { Emqx(EmqxError), Embedded(String), Io(std::io::Error) }`.
  - Lifecycle: `Hub::connect` starts the embedded broker with `tokens.jwks()` when `config.broker` is `Embedded`, before the HTTP server serves; `start_messaging` connects the hub's own client to the TCP listener as today; `serve` in `main.rs` calls `hub.broker().stop()` after the server and messaging futures return.
  - `#[cfg(feature = "test-hooks")] pub fn admin_shim(broker: EmbeddedBroker, api_key: String, api_secret: Secret) -> Router`: the spike's EMQX-shaped routes (`DELETE /clients/{id}`, `GET /clients/{id}`, `GET /clients/{id}/subscriptions`, `POST /banned`, `DELETE /banned/username/{u}`, `DELETE /mqtt/retainer/message/{topic}`, under `/api/v5`, Basic auth). `GET /clients/{id}` answers `{"clientid", "connected", "connected_at"}`, `connected_at` the RFC 3339 time of the current connection as EMQX gives it (null when offline): `crates/agmen-cli/tests/it_mcp_session.rs` (`stop_mcp_session`, on `mvp/impl` since 3e40dfb) reads it to see a reconnect, and the spike's shim (`docs/plans/broker-spike.md` Task 4) answers only `{"clientid", "connected"}`, so the port adds the field. Never in the hub binary.
  - `#[cfg(feature = "test-hooks")] pub async fn Hub::connect_with_broker(config: &Config, broker: EmbeddedBroker) -> Result<Hub, HubError>`.
  - Test harness switch, in `tests/support/broker.rs`: `pub fn switch(value: Option<&str>) -> Result<ItBrokerKind, String>` with `pub enum ItBrokerKind { Emqx, Embedded }`, read from `AGMEN_IT_BROKER`. Unset or `emqx` keeps the dev EMQX; `embedded` makes the first `TestHub` of the test process start one `ItBroker { broker: EmbeddedBroker, tcp: SocketAddr, shim: Url, _data: TempDir }` (free ports picked as the spike's tests pick them, JWKS read from `deploy/dev/.secrets/jwks/jwks.json` so every dev signing key verifies), stored in a process-wide `OnceLock` (nextest runs each test in its own process); an `Err` from `switch` panics with its message. Every `TestHub` then calls `Hub::connect_with_broker` with that broker, `mqtt::mqtt_addr()` returns `ItBroker.tcp`, and the admin helpers (`unban_username`, `emqx_api`, `gone_from_broker`) use `ItBroker.shim` with the `AGMEN_IT_EMQX_API_KEY` and `AGMEN_IT_EMQX_API_SECRET` values. So the hub's own kick and ban go through `BrokerAdmin::Embedded` in process, and the tests' admin calls go through the shim.

Ported spike tests, each in `tests/it_broker.rs` against a broker started with `start_embedded` on free ports and a temp data dir (they need no `AGMEN_IT_BROKER`), one per spec criterion: `it_bad_signature_refused`, `it_no_acl_claim_is_denied`, `it_expiry_disconnects` (the JWT check); `it_client_ids_bound_to_identity` (cinfo rule); `it_publish_to_own_up_allowed`, `it_publish_to_other_up_refused`, `it_subscribe_other_tenant_refused`, `it_share_prefix_does_not_widen_acl`, `it_will_on_foreign_topic_refused`, and `it_hub_minted_hub_token_scope` and `it_hub_minted_identity_token_acl` (topic rules; the last two use tokens from the hub's own `BrokerTokens`, so the ACL cannot drift from what the hub mints); `it_packet_over_limit_refused` (96 KB); `it_hub_shared_subscription`, `it_retained_qos1_delivered_to_late_subscriber`, `it_will_published_on_abrupt_drop`, `it_durable_session_queues_while_offline`, `it_session_expiry_honored` (sessions); `it_rest_kick_disconnects` (renamed `it_kick_disconnects`, through `BrokerAdmin::kick`), `it_ban_refuses_connect` (through `BrokerAdmin::ban_username`) (kick and ban, with 0x87, 0x8A and 0x98 as the spike asserts). Each keeps the spike's assertions; `it_hub_minted_identity_token_acl` seeds its identity in a `TestDb`, as the spike's Task 6 does.

The spike's R70 and R71 criterion, `it_concurrent_durable_first_ack` (spike Task 7: 8 concurrent durable `agmen-client` Mcp sessions, each posting one FYI, every first ack within 5 s, in 5 of 5 batches), is ported to `crates/agmen-client/tests/it_embedded_first_ack.rs`, since it drives `agmen_client::Session` and `agmen-hub` has no `agmen-client` dev-dependency (`agmen-client` already dev-depends on `agmen-hub` with `test-hooks`). The test starts one `EmbeddedBroker` with `start_embedded` on free ports and a temp data dir (JWKS from `deploy/dev/.secrets/jwks/jwks.json`); each session gets its own `TestDb` and a hub from the client harness's config with `AGMEN_MQTT_URL` set to the broker's TCP address, started with `Hub::connect_with_broker`; the session connects to that TCP address. It keeps the spike's constants, bounds (connect 10 s, ack 5 s, close 3 s) and its `s11-probe` batch lines, and asserts 5 clean batches. It needs no `AGMEN_IT_BROKER`.

- [ ] **Step 1: Write the failing tests:** the ported tests above, plus:

```rust
#[tokio::test] async fn it_embedded_sessions_survive_hub_stop()  // Hub::connect with AGMEN_BROKER embedded on a temp AGMEN_DATA_DIR; identity connects clean_start=false; 1 retained state and 3 queued QoS 1 messages; hub.broker().stop(), drop the hub, connect a new Hub on the same data dir: session present, all 3 messages and the retained state arrive
#[tokio::test] async fn it_embedded_state_survives_hub_kill()    // CARGO_BIN_EXE_agmen-hub serve as a child (TestDb, embedded, temp AGMEN_DATA_DIR, the dev signing key); revoke identity X with DELETE /v1/identities/{id} (which bans it) and queue 3 QoS 1 messages for identity Y; SIGKILL; start again: X's CONNECT gets CONNACK 0x8A; Y's 3 messages arrive
#[tokio::test] async fn it_ws_listener_accepts_identity()        // rumqttc over ws://<ws addr>/mqtt with an identity token: CONNACK success; publish to its own up/: PUBACK success
#[tokio::test] async fn it_shim_matches_broker_admin()           // ban through POST /api/v5/banned on admin_shim, then BrokerAdmin::ban_username of the same name: 400 ALREADY_EXISTS then Ok; GET /clients/{id} on a connected client: connected true and a connected_at that changes after the client reconnects; DELETE /clients/{id} on a connected client: 204 and DISCONNECT 0x98; without Basic auth: 401
// config.rs
#[test] fn broker_choice_parses()      // unset -> Emqx; "embedded" with AGMEN_DATA_DIR=/var/lib/agmen -> Embedded{tcp 127.0.0.1:1883, ws 0.0.0.0:8083, data_dir /var/lib/agmen/rmqtt}, mqtt 127.0.0.1:1883; "embedded" without AGMEN_DATA_DIR -> Missing("AGMEN_DATA_DIR"); "rmqtt" -> Invalid{name:"AGMEN_BROKER"}
// tests/it_broker.rs (no dev stack; runs in the default profile)
#[test] fn broker_switch_parses()      // support::broker::switch(None) = Ok(Emqx); Some("emqx") = Ok(Emqx); Some("embedded") = Ok(Embedded); Some("rmqtt") = Err containing "AGMEN_IT_BROKER"
```

- [ ] **Step 2: Run them to verify they fail.** Run: `cargo nextest run -p agmen-hub --locked -E 'test(broker_choice_parses) | test(broker_switch_parses)'` and the `it` command with `-p agmen-hub -p agmen-client -E 'binary(it_broker) | binary(it_embedded_first_ack)'`. Expected: FAIL to compile (`broker` module missing).
- [ ] **Step 3: Implement** `broker/`, the config, the lifecycle, `connect_with_broker`, the harness switch, and route every `emqx()` caller through `broker()`. Move the spike's code; change its behavior only where the ADR's Consequences say to. Copy each `deny.toml` license exception into `about.toml`, then run `scripts/third-party-notices.sh` (onboarding Task 11) to regenerate `THIRD_PARTY_NOTICES.md`.
- [ ] **Step 4: Run the new tests and the route guard.** Run: the two commands of Step 2, then `cargo nextest run -p agmen-hub --locked -E 'test(isolation_table_covers_every_route)'` (Task 11's guard; it skips `broker/shim.rs`, and any other route this task adds goes into its `ROUTES` or `NOT_SCOPED`), then `scripts/third-party-notices.sh --check` and `cargo deny check`. Expected: PASS; the check exits 0.
- [ ] **Step 5: Run the whole hub `it` profile on the embedded broker, then on EMQX.** Run: `set -a; . deploy/dev/.secrets/dev.env; set +a; AGMEN_IT_BROKER=embedded cargo nextest run -p agmen-hub --locked --profile it`, then the same without `AGMEN_IT_BROKER`. Expected: every test passes both times, the spec's pass condition. Then the gate.

  The client, CLI and e2e `it` profiles do not run with `AGMEN_IT_BROKER=embedded`: their test supports reach the broker only through the `AGMEN_IT_*` addresses, including `AGMEN_IT_MQTT_PROXIED` (through the dev toxiproxy) and `AGMEN_IT_MQTT_WSS` (TLS), and an in-process `ItBroker` on free ports has neither a proxy in front nor a TLS listener. The spike's Task 8 Step 4 ran all three against the same RMQTT code with its own toxiproxy and WSS listener, and this task moves that code with behavior changed only where the ADR's Consequences say. If a Consequence changes what a client sees (a reason code, wills, session handling or the admin answers), stop and ask Andy before going on: those three profiles then need a standalone embedded broker, run the way the spike's Task 8 Steps 3 to 5 run theirs.
- [ ] **Step 6: Commit:**

```bash
git add crates/agmen-hub crates/agmen-client/tests/it_embedded_first_ack.rs Cargo.lock deny.toml about.toml THIRD_PARTY_NOTICES.md docs/adr/0002-broker-emqx.md
git commit -m "Embed the RMQTT broker in the hub for agmen hosted"
```

### Task 15: Hosted deployment in agmen-kubernetes

All changes are in `~/Projects/agmen-kubernetes` (`groupdrive/agmen-kubernetes`, private). `agmen-env/` stays Andy's dogfood hub on EMQX until the spike passes.

**Files:**
- Create: `agmen-hosted/kustomization.yaml`, `agmen-hosted/hub-patch.yaml`, `agmen-hosted/placeholders-patch.yaml`, `agmen-hosted/delete-emqx.yaml`, `agmen-hosted/README.md`, `agmen-hosted/check.sh`
- Branch A adds `agmen-hosted/broker-embedded.yaml`; branch B adds `agmen-hosted/broker-emqx-cloud.yaml`
- Modify: `README.md` (layout), and in the agmen repo for branch B only: `crates/agmen-hub/src/config.rs:147-160` (`mqtt_addr` accepts `mqtts://host:port`) and `crates/agmen-hub/src/mqtt.rs:149-170` (TLS on the hub's own connection)

**Interfaces:**
- Consumes: `agmen-env/` as the base (`agmen-env/kustomization.yaml`, `agmen-env/hub.yaml`, which already sets `fsGroup: 65532`); Task 1 env names; Task 14 env names (branch A); hub image `ghcr.io/agmen-dev/agmen-hub:<version>`.
- Produces: `kubectl kustomize agmen-hosted`, namespace `agmen-hosted`, hub at `https://api.getagmen.ai` with `AGMEN_HUB_MODE=hosted`, `AGMEN_GITHUB_CLIENT_ID` set to the hosted app's id, the five `AGMEN_PADDLE_PRICE_*` from ConfigMap `agmen-paddle-catalog`, `AGMEN_PADDLE_API_KEY` and `AGMEN_PADDLE_WEBHOOK_SECRET` from Secret `agmen-paddle`. The EMQX StatefulSet, its Service, its ConfigMap and the `agmen-mqtt-ws` Ingress are removed with `$patch: delete`. `hub-image` is the newest released semver tag.
  - Branch A (`Verdict: pass`): one hub replica, `strategy: Recreate`, a 5 Gi PVC mounted at `/var/lib/agmen` (pod `fsGroup: 65532`), `AGMEN_DATA_DIR=/var/lib/agmen` (so the hub generates its signing key there and keeps the broker in `/var/lib/agmen/rmqtt`), the base's `AGMEN_JWT_SIGNING_KEY`, `AGMEN_JWT_EXTRA_KEYS`, `AGMEN_EMQX_API_URL`, the `agmen-hub-emqx-api` Secret reference and the `jwt-keys` volume removed, container port 8083, `AGMEN_BROKER=embedded`, `AGMEN_PUBLIC_MQTT_URL=wss://api.getagmen.ai/mqtt`, and an Ingress path `/mqtt` on `api.getagmen.ai` to port 8083.
  - Branch B (`Verdict: fail`): EMQX Cloud Dedicated Flex. `AGMEN_MQTT_URL=mqtts://<deployment host>:8883`, `AGMEN_PUBLIC_MQTT_URL=wss://<deployment host>:8084/mqtt`, `AGMEN_EMQX_API_URL=https://<deployment host>:8443/api/v5`, API key and secret from Secret `agmen-emqx-cloud`; the deployment host is placeholder `emqx-cloud-host`, set when Andy creates the deployment. The cloud deployment's JWT authentication reads `https://api.getagmen.ai/v1/jwks` with the `acl` claim, and authorization `no_match = deny`, as ADR-0002.

- [ ] **Step 1: Check the tools.** Run: `kubectl version --client` and `command -v kubeconform`. Expected: kubectl prints its client version; kubeconform was not installed on this machine on 2026-10-26, so if it is still missing, ask Andy before installing it (`brew install kubeconform`) and stop until he answers.
- [ ] **Step 2: Write `agmen-hosted/check.sh`** (shellcheck and shfmt clean) asserting on `kubectl kustomize agmen-hosted` piped to `yq`: no image matching `emqx/emqx`; no image tag `latest` or missing; the hub env has `AGMEN_HUB_MODE=hosted` and every `AGMEN_PADDLE_*` name; every resource is in namespace `agmen-hosted`; the api Ingress host is `api.getagmen.ai`; branch A: a PVC is mounted at `/var/lib/agmen`, `AGMEN_DATA_DIR` is `/var/lib/agmen`, `fsGroup` is 65532, no `AGMEN_JWT_SIGNING_KEY`, and `replicas` is 1. Run it. Expected: FAIL (no `agmen-hosted/`).
- [ ] **Step 3: Write the overlay** and the branch file the verdict picks. Branch B also changes the hub's broker connection to TLS, with the unit test `mqtts_urls_accepted` (`mqtts://x.emqxsl.com:8883` parses; `mqtts://x` without a port refused) in `config.rs`.
- [ ] **Step 4: Run the checks.** Run: `agmen-hosted/check.sh` and `kubectl kustomize agmen-hosted | kubeconform -strict -ignore-missing-schemas`. Expected: both pass. Branch B: run `cargo nextest run -p agmen-hub --locked -E 'test(mqtts_urls_accepted)'` and the agmen gate too.
- [ ] **Step 5: Write `agmen-hosted/README.md`**: the Secrets and placeholders to fill, `agmen-hub paddle-catalog` against the live API to fill `agmen-paddle-catalog`, the Paddle webhook destination `https://api.getagmen.ai/v1/paddle/webhook` with events `subscription.*` and `transaction.completed`, and Andy's own group: moved only after Task 16 Step 7, created with `agmen init --hosted` and given `agmen-hub set-plan --plan business-10`; the old `agmen` namespace keeps running until he deletes it.
- [ ] **Step 6: Commit** in agmen-kubernetes:

```bash
git add agmen-hosted README.md
git commit -m "Add the agmen hosted overlay"
```

Branch B, first in the agmen repo:

```bash
git add crates/agmen-hub/src/config.rs crates/agmen-hub/src/mqtt.rs
git commit -m "Connect the hub to its broker over TLS"
```

### Task 16: Operations: status probe, restore test and down alerts

**Files:**
- agmen repo: modify `crates/agmen-hub/src/health.rs:19-23` (route), `crates/agmen-hub/src/mqtt.rs:296-345` (`run_connection` sets the broker flag), `crates/agmen-hub/src/http.rs:54-65` (`Inner.broker_up: AtomicBool`), `crates/agmen-hub/tests/it_health.rs` (new case)
- agmen-kubernetes: create `agmen-env/ops/restore-test-job.yaml`, `ops/uptime/wrangler.toml`, `ops/uptime/worker.js`, `ops/uptime/decide.js`, `ops/uptime/decide.test.js`, `ops/uptime/README.md`; modify `agmen-env/README.md` ("Verified")

**Interfaces:**
- Consumes: the nightly dump `s3://$S3_BUCKET/postgres/agmen-<UTC stamp>.dump` (`agmen-env/pg-dump-cronjob.yaml`); `database_ready` (`crates/agmen-hub/src/health.rs`).
- Produces:
  - `GET /statusz` (both modes, no token): 200 `{"database":"ok","broker":"ok"}`, or 503 with `"down"` for each part that fails. `/healthz` is unchanged, so readiness never waits on the broker (EMQX reads the hub's JWKS at cold start, `crates/agmen-hub/src/main.rs:138-141`). The broker flag is true from a ConnAck (`mqtt.rs:320`) until a connection error (`mqtt.rs:340`); with the embedded broker it also needs the embedded listener up.
  - `restore-test-job.yaml` (applied by hand, not in the kustomization): an init container `amazon/aws-cli:2.38.5` downloads the newest dump; a native sidecar `postgres:16.15` on an emptyDir; the main container `postgres:16.15` runs `pg_restore --no-owner` into it and prints the row counts of `tenants`, `projects`, `messages` and the last `_sqlx_migrations.version`, exiting non-zero on any failure.
  - Uptime worker: Cloudflare Worker, cron `*/5 * * * *`, GET `https://api.getagmen.ai/statusz` with a 10 s timeout. `decide(previous: {failures, down}, ok: boolean) -> {next, alert: null | "down" | "recovered"}` turns down after 2 failures in a row and alerts once per change. State lives in KV namespace binding `UPTIME`. The alert is a POST of one line ("agmen hosted is down: <body or error>" or "agmen hosted recovered") to the Worker secret `ALERT_URL` (an ntfy topic URL).

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test] async fn it_statusz_reports_broker()   // hub without messaging: 503, broker "down", database "ok"; after start_messaging: 200 both "ok"
```

```js
// ops/uptime/decide.test.js (node --test)
test('one failure does not alert')          // decide({failures:0,down:false}, false) -> failures 1, alert null
test('two failures alert down once')        // then false again -> down true, alert "down"; a third -> alert null
test('recovery alerts once')                // decide({failures:3,down:true}, true) -> {failures:0,down:false}, alert "recovered"
```

- [ ] **Step 2: Run them to verify they fail.** Run: the `it` command with `-p agmen-hub -E 'test(it_statusz_reports_broker)'`, and `node --test ops/uptime` in agmen-kubernetes. Expected: FAIL.
- [ ] **Step 3: Implement** `/statusz`, the broker flag, `decide.js`, `worker.js`, `wrangler.toml` (KV binding, cron) and the restore Job.
- [ ] **Step 4: Run the tests to verify they pass.** Run: the two commands of Step 2, the agmen gate, and `kubeconform -strict -ignore-missing-schemas agmen-env/ops/restore-test-job.yaml`. Expected: PASS.
- [ ] **Step 5: Restore once and record it.** Run: `kubectl -n agmen apply -f agmen-env/ops/restore-test-job.yaml && kubectl -n agmen wait --for=condition=complete job/agmen-restore-test --timeout=15m && kubectl -n agmen logs job/agmen-restore-test`. Expected: exit 0 and non-zero `tenants` and `messages` counts. Write the date, dump name and counts under "Verified" in `agmen-env/README.md`, then delete the Job.
- [ ] **Step 6: Check wrangler, then deploy the worker.** Run: `npx --yes wrangler@4.45.0 --version`. Expected: `4.45.0` (on npm since 2025-10-23, engines `node >=18`; newer 4.x releases at least two weeks old need Node 22, and this machine had Node 20.13.1 on 2026-10-26). Then from `ops/uptime`: `npx wrangler@4.45.0 deploy` and `npx wrangler@4.45.0 secret put ALERT_URL`. Expected: the deploy prints the worker's URL and the cron trigger.
- [ ] **Step 7: Ask Andy, then test the alerts.** This scales the production hub to 0 for 11 minutes, which drops every connection on it. Run it only after Andy says yes, and only while no group but test groups is on `agmen-hosted` (before his own group moves, Task 15 Step 5). Run: `kubectl -n agmen-hosted scale deploy/agmen-hub --replicas=0`, wait 11 minutes, `kubectl -n agmen-hosted scale deploy/agmen-hub --replicas=1`, wait 10 minutes. Expected: exactly one "down" and one "recovered" message on the ntfy topic. Record the times in `ops/uptime/README.md`.
- [ ] **Step 8: Commit** in agmen:

```bash
git add crates/agmen-hub/src crates/agmen-hub/tests/it_health.rs
git commit -m "Add /statusz with database and broker status"
```

In agmen-kubernetes:

```bash
git add agmen-env/ops agmen-env/README.md ops/uptime
git commit -m "Add the restore test job and the uptime worker"
```

### Task 17: getagmen.ai site and legal pages (runs first)

This task starts before every other task in this plan and before the onboarding plan's release pipeline exists (see **Order**). It needs no hub or CLI code and no task of any plan: the installers appear only as URLs, which answer 404 until onboarding's first release, and checkout is Paddle's own overlay, which opens only once Andy's Paddle account exists. Everything the pages say comes from the spec.

**Files:**
- Create in the agmen repo: `site/index.html`, `site/pricing/index.html`, `site/terms/index.html`, `site/privacy/index.html`, `site/refunds/index.html`, `site/checkout/index.html`, `site/checkout/checkout.js`, `site/style.css`, `site/_redirects`, `site/_headers`, `site/check.sh`, `site/README.md`

**Interfaces:**
- Consumes (text and URLs only, nothing built):
  - The spec: decision 8 (plan table, trial, payment failure timeline, quotas, retention), decision 2 (the hub keys people on the GitHub numeric id and throws the GitHub token away), decision 10 (where agmen hosted runs) and decision 6 under "Decisions Andy made".
  - This plan's Task 1 rule for a canceled subscription (read-only 90 days, then deleted), written here as text; Task 1 need not exist yet.
  - Installer URLs: `https://github.com/agmen-dev/agmen/releases/latest/download/install.sh` and `.../install.ps1` (the asset names onboarding Task 14 will upload) and `brew install agmen-dev/tap/agmen` (onboarding Task 13). The install lines are written exactly as onboarding Task 2's `/i` page writes them.
  - Source text: `https://github.com/basecamp/policies` at commit `05fb1f9ddaaf604fc5816990fa66f3ffd7f0a462` (the repository was archived on 2026-10-06; its `LICENSE.md` is CC BY 4.0), files `terms/index.md`, `privacy/index.md` and `refund/index.md`. The repository's README points to 37signals.com/policies for current text, but that site's license was not checked, so nothing is copied from it.
- Produces:
  - `/`: what agmen is (a hub for a group's people and their MCP agents, sign-in with GitHub), the install lines `curl -fsSL https://getagmen.ai/install.sh | sh`, `powershell -ExecutionPolicy Bypass -c "irm https://getagmen.ai/install.ps1 | iex"` and `brew install agmen-dev/tap/agmen`, then `agmen init`, and links to the docs in `https://github.com/agmen-dev/agmen` (`README.md`, `docs/mcp-setup.md`). One element `<p id="launch-note">`: "agmen's first release and agmen hosted sign-ups are on the way. The install lines work from the first release."
  - `/pricing`: the spec's plan table word for word (Self host, Team, Business); the trial (7 days, no card, Team's 4 seats); quotas per group per day (trial 5,000 messages; Team 10,000 plus 1,000 for each extra seat; Business 50,000 for 10 seats, scaled by the step); retention (Team 90 days, Business 1 year, self host keeps everything unless the host sets a limit); "custom above 50" with `sales@getagmen.ai`; the same launch note.
  - `/terms`: the operator by the placeholder `OPERATOR_REPLACE_ME` (legal name and place; not in the spec, Andy supplies them); agmen hosted as the service; sign-in with GitHub; Paddle as merchant of record for every payment; monthly plans, seats and quotas as on `/pricing`; non-payment: a trial that ends unpaid is read-only for 14 days, then deleted; a failed payment keeps full access for 14 days, then read-only for 90 days, then deleted; a canceled subscription is read-only for 90 days, then deleted; paying at any point restores the group; self hosting is under the MIT license and outside these terms; the use restrictions adapted from Basecamp's terms.
  - `/privacy`: the operator (`OPERATOR_REPLACE_ME`) as controller; what agmen hosted stores: the GitHub numeric account id and login, and the messages between people and their agents, plus the group, project, identity and role records that route them; what it does not keep: card data (Paddle holds it), the GitHub token (thrown away after sign-in), an email address; retention: 90 days of messages on a trial and Team, 1 year on Business; deletion after non-payment on the timeline above; subprocessors: Paddle (payments, merchant of record), Hetzner (agmen hosted's servers, spec decision 10) and Cloudflare (serves this site); contact `privacy@getagmen.ai`; GDPR rights of access and deletion (plus correction and a copy of your data), answered within one month; agmen does not sell data; a self-hosted hub sends agmen nothing.
  - `/refunds`: written to match Paddle's 14-day refund window: a refund on request within 14 days of a charge, by writing to `support@getagmen.ai`; after 14 days, no refund for the rest of a month; canceling stops the next renewal.
  - Every legal page ends with "Last updated <date>" and the credit "Adapted from the Basecamp policies by 37signals (https://github.com/basecamp/policies), CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/). Changed for agmen.", both URLs as links.
  - `/checkout`: `checkout.js` holds the Paddle client-side token in one constant. While the constant is the placeholder `test_replace_me`, the page loads nothing from Paddle and says "Checkout opens once agmen's Paddle account is approved." With a real token it loads `https://cdn.paddle.com/paddle/v2/paddle.js` and calls `Paddle.Initialize({ token })`, which opens the checkout for the `_ptxn` in the URL. No inline script, so the CSP needs no `unsafe-inline`.
  - `_redirects`: `/install.sh https://github.com/agmen-dev/agmen/releases/latest/download/install.sh 302` and `/install.ps1 https://github.com/agmen-dev/agmen/releases/latest/download/install.ps1 302`, so the site serves the newest release's installer and keeps no copy of it.
  - `_headers`: `Referrer-Policy: strict-origin-when-cross-origin`, `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, and a CSP of `'self'` plus the hosts Paddle's Paddle.js documentation lists (check developer.paddle.com and name the page in a comment in `_headers`).

- [ ] **Step 1: Write `site/check.sh`** (POSIX sh, `shellcheck` and `shfmt -d` clean). It exits 1 naming each failed check unless: `index.html` contains the three install lines above exactly and `agmen init`; `/pricing` contains `$25`, `$10`, `$100`, `$200`, `$350`, `7-day`, `5,000`, `10,000`, `50,000`, `90 days`, `1 year`, `sales@getagmen.ai`; each legal page contains `CC BY 4.0`, `https://creativecommons.org/licenses/by/4.0/`, `https://github.com/basecamp/policies`, `Changed for agmen` and `Last updated`; privacy contains `GitHub numeric account id`, `login`, `messages`, `Paddle`, `Hetzner`, `Cloudflare`, `privacy@getagmen.ai`, `GDPR`, `access`, `deletion`, `90 days`, `1 year`, `does not sell`; refunds contains `14 days` and `support@getagmen.ai`; terms contains `read-only`, `deleted`, `14 days`, `90 days`, `Paddle`; `_redirects` sends `/install.sh` and `/install.ps1` to `releases/latest/download/install.sh` and `.ps1`; no file contains U+2013 or U+2014; every internal `href` and `src` resolves to a file under `site/`; no file holds `OPERATOR_REPLACE_ME`; `test_replace_me` appears nowhere but `checkout/checkout.js`. While it is there, check.sh prints `warning: the Paddle client-side token is not set` and still exits 0, since Paddle approves the account only after the site is up. Run: `sh site/check.sh`. Expected: exit 1 (no pages).
- [ ] **Step 2: Read the source policies** at the pinned commit: `for f in terms privacy refund; do gh api "repos/basecamp/policies/contents/$f/index.md?ref=05fb1f9ddaaf604fc5816990fa66f3ffd7f0a462" --jq .content | base64 -d > "$SCRATCH/basecamp-$f.md"; done` (`$SCRATCH` is the session scratchpad; nothing lands in the repo). Expected: three Markdown files. Adapt their structure and plain wording; drop what is Basecamp's own (several products, its support links, Liquid variables, its subprocessors) and add each item Produces lists.
- [ ] **Step 3: Write the pages** in plain HTML with one stylesheet, light and dark through `prefers-color-scheme`, readable at 360 px wide with no sideways scroll. Terms and privacy keep `OPERATOR_REPLACE_ME`; `checkout.js` keeps `test_replace_me`.
- [ ] **Step 4: Run the checks.** Run: `sh site/check.sh; shellcheck site/check.sh; shfmt -d site/check.sh`. Expected: check.sh fails only on `OPERATOR_REPLACE_ME` and prints the token warning; shellcheck and shfmt print nothing.
- [ ] **Step 5: Ask Andy for the operator's legal name and place**, put them in terms and privacy, and rerun `sh site/check.sh`. Expected: exit 0 with only the token warning. If Andy has not answered, commit with the placeholder and say so in the task report; the README's deploy steps require check.sh to pass first.
- [ ] **Step 6: Write `site/README.md`**: (1) check wrangler with `npx --yes wrangler@4.45.0 --version` (expected `4.45.0`; on npm since 2025-10-23 with engines `node >=18`, checked 2026-10-26 on Node 20.13.1); (2) deploy only when `sh site/check.sh` exits 0: `npx wrangler@4.45.0 pages deploy site --project-name getagmen`, then add `getagmen.ai` as the Pages custom domain (Andy registers the domain first, spec "Actions for Andy"); (3) before the first deploy, `privacy@`, `sales@` and `support@getagmen.ai` must deliver to Andy; (4) after Andy's Paddle account is approved, paste its client-side token into `checkout.js`, set Paddle's default payment link to `https://getagmen.ai/checkout`, and redeploy; (5) after onboarding's first release, `curl -fsSIL https://getagmen.ai/install.sh` ends in `HTTP/2 200` from `objects.githubusercontent.com` or `release-assets.githubusercontent.com`, and the same for `install.ps1`; (6) when hosted sign-ups open (after Task 16), remove `#launch-note` from both pages and redeploy; (7) if Task 15 takes branch B (EMQX Cloud), add EMQ to the privacy page's subprocessors before sign-ups open; (8) the adapted text is not legal advice; a lawyer reviews it once there are paying groups (about $770, spec decision 6).
- [ ] **Step 7: Commit:**

```bash
git add site
git commit -m "Add the getagmen.ai site and legal pages"
```

## Spec coverage

| Spec item | Task |
|---|---|
| Decision 8 onboarding choice, `agmen init`, `--new-group` | 12 |
| Group and seats, roles for billing | 2, 3, 7, 13 |
| Plans, catalog in Paddle | 1, 5 |
| Trial: no card, one per GitHub id, 4 seats, reminders days 5 and 7, read-only 14 days, deletion | 1, 2, 8, 13 |
| Payment failure timeline | 1, 6, 8 |
| Seat changes (Team quantity, Business steps) | 7, 13 |
| Quotas, 80% ALERT, `quota_exceeded` until 00:00 UTC | 4 |
| Billing: Paddle checkout, portal, signed webhooks, plan state | 5, 6, 7 |
| Controls for owner and admins (seats, billing) | 3, 7, 13 |
| Desktop first-run choice and settings screens on the same API | 13 (Step 6, Andy action into the desktop plan) |
| History retention, including the self-host limit | 1, 9 (self-host Task 7 passes the variable) |
| Decision 10: quota, pruning, roster fix, abuse limits, isolation tests | 4, 9, 10, 2, 11 |
| Decision 10: backup restore, down alerts | 16 |
| Decision 10 and "Decisions Andy made" 6: public site on Cloudflare Pages, pricing, terms, privacy (what agmen stores, retention, deletion after non-payment, Paddle as subprocessor, privacy contact, GDPR access and deletion), refunds (Paddle's 14 days), Basecamp CC BY 4.0 credit; `getagmen.ai/install.sh` and `install.ps1` | 17 (runs first) |
| Where hosted runs, Andy's group as a tenant | 15 |
| Decision 9 results (embedded broker or EMQX Cloud) | 14, 15 |

Left to other plans: tenant roles, `agmen group members|remove|role|transfer|revoke` and `resolve_group`, project creation, the claim token and the compose file's retention variable (self-host plan); device flow, invite links, join, setup, the installers and their release assets (onboarding plan); building the desktop screens (desktop plan, from Task 13 Step 6).
