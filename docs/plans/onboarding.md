# Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A friend gets from an invite link to a joined worktree with their agent connected in three steps, on any hub, signed in with GitHub, with any MCP agent, from prebuilt binaries of a public MIT repository.

**Architecture:** The hub runs GitHub's device flow itself on a shared public client id, so no hub holds a client secret and the hub never takes a GitHub token from a client. Invites become `https://<hub>/i#<code>` links served by a static page; `agmen join <link>` signs in, accepts, writes the identity and runs `agmen setup`, which writes each installed agent's MCP entry. `agmen mcp` carries the parts the Claude Code plugin gave only Claude: pending messages at the end of tool results and presence, both only while no `agmen watch` runs for the identity. `dist` builds the CLI on version tags in GitHub Actions; a reusable job publishes the hub image.

**Tech Stack:** Rust 1.99.1 (edition 2024), axum 0.8, sqlx 0.9 with Postgres, reqwest 0.13, rmcp 3.7, rumqttc-next 0.34 (agmen-client), toml_edit (new, Task 5), dist 0.33.1, GitHub Actions, gitleaks 8.30.1, cargo-about (new, Task 11), POSIX sh and PowerShell installers.

**Spec:** `docs/specs/2026-10-26-onboarding-and-hosting-design.md` (decisions 1 to 6, "Decisions Andy made" 1, 2 and 4). Cross-plan names: `.superpowers/sdd/onboarding/plan-contracts.md`. This is plan 1 of 4; `self-host.md`, `broker-spike.md` and `hosted.md` follow and consume the names in each task's Produces block.

## Global Constraints

Copied from the spec and the contracts; every task's requirements include these.

- **GitHub sign-in:** "humans are keyed on GitHub's numeric account id, never the login (ruling R39, `crates/agmen-hub/src/auth/oauth.rs`)."
- **Device flow:** "The hub signs people in with GitHub's own device flow, which needs no client secret ("The `client_secret` is not needed for the device flow", GitHub docs above). So no self-hoster creates an OAuth app."
- **Flow:** "the CLI asks its hub to start a login. The hub calls `github.com/login/device/code` with the client id, and the CLI shows the user code and opens `github.com/login/device`. The hub polls `github.com/login/oauth/access_token`, calls `GET api.github.com/user` once, keys the person on the numeric `id`, and throws the GitHub token away. The hub then issues its own tokens as today."
- **"The hub never accepts a GitHub token from a client."**
- **Client ids:** "Two agmen-owned GitHub OAuth apps, both with "Enable Device Flow" on and no scopes." `AGMEN_GITHUB_CLIENT_ID` is optional; its default is the compiled constant `DEFAULT_GITHUB_CLIENT_ID` in `crates/agmen-hub/src/config.rs`, a placeholder until Andy creates the self-host app in `agmen-dev`. `AGMEN_GITHUB_CLIENT_SECRET` is removed.
- **What gets removed:** "the hub's web OAuth callback and its device page that redirects through it (`crates/agmen-hub/src/auth/device.rs`, `oauth.rs`). That also removes `AGMEN_GITHUB_CLIENT_SECRET` and the exact-callback requirement."
- **Timing:** "the hub's own device-login window moves from 600 s to GitHub's 900 s, and polling follows GitHub's interval and `slow_down`."
- **Device flow limit:** GitHub allows "50 submissions in an hour per application".
- **Invite form:** "`https://<hub>/i#<code>`. The code sits in the URL fragment, so browsers never send it to the hub, to server logs or in a Referer header."
- **The page:** "`GET /i` serves a small static page. Its inline script reads the fragment and shows the install line and the exact `agmen join` command for the visitor's platform. The page sends `Referrer-Policy: no-referrer` and `Cache-Control: no-store`, and it never calls the hub with the code."
- **Invites:** "single-use secrets stored only as SHA-256, valid 1 hour to 30 days, with role limits" (R55, R58). "The hub sees it only in the accept call, as today."
- **Identity trust:** "an identity file is trusted only at `<worktree root>/.agmen/identity.json`, untracked, exact names, no symlinks (R42 to R42b). Tokens are per machine in the OS keychain, keyed by hub origin plus identity (R17, R53)."
- **Join:** "Outside a git worktree, join stops and says to run it inside a clone of the project's repository. If the project records a repository URL (a new optional project field), join prints the `git clone` command." "`agmen join --project` for second worktrees stays as built, and defaults to the person's only project when they have exactly one." "`agmen login --hub` stays."
- **Launch agents:** "Claude Code: the plugin through its marketplace, or `.mcp.json` when the plugin is declined." "Codex: `~/.codex/config.toml`, with `cwd`." "OpenCode: `opencode.json` in the worktree, added to `.git/info/exclude`." "Cursor and Gemini CLI: their MCP config formats are not yet verified; the builder confirms them from current docs before writing them."
- **Pending:** "every tool result ends with the messages waiting for that identity. This is design.md section 6.10, which R19 dropped for the MVP." This plan amends R19.
- **Presence:** "`agmen mcp` publishes presence when no `agmen watch` runs for that identity. This amends R65, which made watch the only publisher."
- **Lane guard:** "the `check_path` tool is there for any agent. A hook that blocks out-of-lane edits stays Claude Code only, and setup says so."
- **Binaries:** "`agmen` for macOS (arm64, x86_64), Linux (x86_64, arm64) and Windows (x86_64), built by `dist` 0.33.1." "a one-line shell installer, a PowerShell installer and a Homebrew tap. Each installer checks the binary's SHA-256 from the release."
- **Images:** "the hub image and, for self-host, the broker image are published with pinned version tags, never `latest`." This plan publishes `ghcr.io/agmen-dev/agmen-hub:<version>`. The self-host broker is EMQX's own `emqx/emqx:6.3.2`, which EMQ publishes with that pinned tag and the self-host plan's compose pins (contracts: "EMQX 6.3.2 single node (pinned)"); agmen does not republish EMQX under `agmen-dev`, because whether it may is the question still with EMQ (spec, "Decisions Andy made" 5). Andy's ruling on that reading is listed at the end. Releases run from the dist-generated GitHub Actions workflow on version tags only; the per-commit gate stays on Jenkins (R49a).
- **Names:** domain `getagmen.ai`; install scripts `https://getagmen.ai/install.sh` and `https://getagmen.ai/install.ps1`; GitHub organization `agmen-dev`; repository `https://github.com/agmen-dev/agmen`.
- **License:** "The agmen repository becomes public under MIT. Before it does: a secret scan of the full history; a `LICENSE` file; a notice listing bundled third-party licenses (fonts under OFL, Lucide under ISC, GPUI under Apache 2.0, each Zed theme under its own)."
- **Publishing:** "a new public repository `github.com/agmen-dev/agmen` with one squashed first commit, authored with Andy's GitHub no-reply address. Before it: GroupDrive and other private text removed from the tree, Integrally reworded as "a 17-agent PR", repository URLs pointed at the public repo, and the mockups regenerated with Lucide icons in place of Zed's and Material's. The existing private `AndyAld/agmen` stays private. The local `mvp/t14` branch, which holds Claude Code's early-access type files in its history, is never published." Task 15 prepares the tree; making the squashed commit and pushing it is Andy's action, not a task.
- **Anti-goals:** "A self-hoster has to create a GitHub OAuth app, register a callback URL or hold a client secret." "An invite carries a secret that lands in server logs, browser history sent to third parties, or a Referer header." "A free self-hosted hub phones home." "The hub trusts a GitHub token that a client hands it."
- **The gate** (every task's last run): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`, `cargo nextest run --workspace --locked`, `cargo test --doc --workspace --locked`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`, `cargo deny check`. Integration tests: `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run --workspace --locked --profile it` against the dev stack.
- **Tests leave no trace:** tests never touch the user's keychain (R17), clipboard, browser, real agent configs or real `claude`, `codex`, `opencode`, `cursor` or `gemini` programs. Secret-shaped test strings are built at runtime (R22).
- **Writing:** no em or en dashes in code, comments, docs or commit messages; one-line commit messages with no trailers.

## Review Focus

1. **A pasted invite link in the shapes chat apps produce:** surrounding whitespace, angle brackets, uppercase hex, `http://` to a public host, an `/i` link with no fragment, a bare code with no known hub. Expected: whitespace and brackets are stripped, hex is lowercased, plain `http` is refused unless the host is loopback, the rest exit 2 naming what is missing. Test: Task 7 `invite_links_and_codes_parse`.
2. **A crafted fragment on the `/i` page** (`#<img src=x onerror=...>`, a short code). Expected: the page renders it as text or shows "This link is missing its invite code", and the page can send no request at all (CSP `default-src 'none'`). Test: Task 2 `page_reads_the_code_only_from_the_fragment` and `it_invite_page_headers`.
3. **`agmen join <link>` run outside the clone, then again inside it.** Expected: the first run accepts, makes no identity, exits 1 with the `git clone` line and `agmen join --project <id>`; the second run with the used link says "already used" and how to join by project. Test: Task 7 `it_join_outside_a_worktree_points_to_the_clone`.
4. **The person's own agent config:** a tracked `.mcp.json` or `opencode.json`, a file that is not plain JSON, an `opencode.jsonc`, a Codex entry pointing at another worktree, and running setup twice. Expected: tracked and unreadable files stay byte for byte and setup says why, the Codex entry is repointed and setup names the old worktree, a second run reports "already set up". Tests: Task 4 `setup_leaves_a_tracked_mcp_json_alone`, `refuses_files_that_are_not_json_objects`; Task 5 `another_worktree_is_repointed_and_reported`, `setup_skips_opencode_jsonc`.
5. **GitHub's side of sign-in going wrong:** the person declines on github.com, the code expires, GitHub answers `slow_down`, GitHub is unreachable or has device flow disabled. Expected: the hub passes `access_denied`, `expired_token` and `slow_down` through, answers 502 `upstream_unavailable` for the rest, never polls GitHub faster than its interval, and the CLI says what happened in one line. Tests: Task 1 `it_device_flow_access_denied`, `it_device_flow_expired`, `it_device_flow_slow_down_is_passed_on`, `it_device_poll_follows_github_interval`, `it_device_start_github_down_is_502`; Task 1b `it_login_declined_on_github`.

## Files

| Path | Responsibility | Task |
|---|---|---|
| `crates/agmen-hub/src/auth/github.rs` (replaces `oauth.rs`) | GitHub device-flow client, `/user`, human upsert, `account_by_login` | 1 |
| `crates/agmen-hub/src/auth/device.rs` | `POST /v1/auth/device` and the device grant | 1 |
| `crates/agmen-hub/src/test_github.rs` (moved from `tests/support/fake_github.rs`) | `FakeGithub` for every crate's tests, behind `test-hooks` | 1 |
| `crates/agmen-hub/migrations/0006_github_device.{up,down}.sql` | device rows hold GitHub's device code and poll timing | 1 |
| `crates/agmen-hub/src/invite_page.rs`, `invite_page.html` | `GET /i` | 2 |
| `crates/agmen-proto/src/repo_url.rs`, `crates/agmen-hub/migrations/0007_project_repo_url.{up,down}.sql` | optional project repository URL, one parser for hub and CLI | 3 |
| `crates/agmen-cli/src/cmd/account.rs` | `agmen login` through GitHub's device page | 1, 1b |
| `crates/agmen-cli/src/open.rs` | open a URL, copy text, only on a terminal | 1b, 2 |
| `crates/agmen-cli/src/setup/{mod,json,claude,codex,opencode}.rs` | `agmen setup` | 4, 5, 6 |
| `crates/agmen-cli/src/cmd/join.rs` | `agmen join <link|code>` does everything | 7 |
| `crates/agmen-client/src/session.rs` | presence chosen per session, quiet close | 8 |
| `crates/agmen-cli/src/watch_lock.rs`, `crates/agmen-cli/src/mcp/mod.rs` | presence from mcp | 9 |
| `crates/agmen-cli/src/mcp/{mod,tools}.rs`, `cmd/inbox.rs` | pending messages in tool results | 10 |
| `LICENSE`, `THIRD_PARTY_NOTICES.md`, `about.toml`, `about.hbs`, `scripts/third-party-notices.sh` | MIT and notices | 11 |
| `.gitleaksignore` | reviewed false positives only | 12 |
| `dist-workspace.toml`, `.github/workflows/{release,publish-hub-image}.yml` | release pipeline | 13 |
| `install/{install.sh,install.ps1,test-install.sh,test-install.ps1}`, `.github/workflows/test-installers.yml` | installers, uploaded to every release as `install.sh` and `install.ps1` | 14 |
| `scripts/public-tree-check.sh`, `docs/mockups/` | the tree the public repository's squashed commit is made from | 15 |

Task numbers stay as the other plans cite them; the CLI half of sign-in is Task 1b so Tasks 2 to 14 keep their numbers, and Task 15 comes last because it rewords the finished plans.

---

## Phase A: hub

### Task 1: Hub-run GitHub device flow on a shared client id

**Files:**
- Create: `crates/agmen-hub/src/auth/github.rs`, `crates/agmen-hub/src/auth/github_tests.rs`, `crates/agmen-hub/src/test_github.rs`, `crates/agmen-hub/migrations/0006_github_device.up.sql`, `crates/agmen-hub/migrations/0006_github_device.down.sql`
- Delete: `crates/agmen-hub/src/auth/oauth.rs` (keep lines 42 to 80 `GithubId`, 93 to 98 `GithubUser`, 160 to 185 `account_by_login`, 320 to 395 `upsert_human`, `claim_bootstrap_admin`, `is_human_handle` and their tests in `github.rs`; drop the web flow at 82 to 158 and 187 to 318), `crates/agmen-hub/tests/support/fake_github.rs`
- Modify: `crates/agmen-hub/src/auth/device.rs:1-289` (rewrite: no form, approve page or user-code minting), `crates/agmen-hub/src/auth/mod.rs:1-16,39-46`, `crates/agmen-hub/src/config.rs:35-48,237-358` and tests `361-513`, `crates/agmen-hub/src/http.rs:16,54-65,85-114,154-156`, `crates/agmen-hub/src/lib.rs:21-61`, `crates/agmen-hub/src/main.rs:125-137`, `crates/agmen-hub/src/test_hooks.rs:134-151`, `crates/agmen-hub/Cargo.toml` (drop `oauth2`), `crates/agmen-cli/src/cmd/account.rs:35-63` (`DeviceStart` and the second printed line only, so `agmen login` keeps working against the new answer)
- Test: `crates/agmen-hub/src/auth/github_tests.rs`, `crates/agmen-hub/tests/it_auth.rs:1-193`, `crates/agmen-hub/tests/support/mod.rs:194-290,388-490`, `crates/agmen-cli/tests/it_cli.rs:65-110`, `crates/agmen-cli/tests/support/mod.rs:140-200`, `crates/agmen-client/tests/support/mod.rs:188-208`

**Interfaces:**
- Consumes: `Hub::public_link`, `issue_human_tokens(tx, human)`, `db::{new_secret, hash_secret}`, `upstream_client()` (no redirects, 10 s timeout, User-Agent).
- Produces:
  - `pub const DEFAULT_GITHUB_CLIENT_ID: &str = "agmen-dev-self-host-placeholder";` in `config.rs`, re-exported from `lib.rs`. `Hub::connect` logs one `warn!` when the configured id equals it.
  - `pub struct GithubConfig { pub client_id: String, pub url: Url, pub api_url: Url }` (no `client_secret`).
  - `pub(crate) struct Github`; `Github::new(config: &GithubConfig, http: reqwest::Client) -> Result<Github, GithubError>`; `async fn start_device(&self) -> Result<DeviceCode, GithubError>`; `async fn poll_device(&self, device_code: &str) -> Result<DevicePoll, GithubError>`; `async fn user(&self, token: &GithubToken) -> Result<GithubUser, GithubError>`.
  - `pub(crate) struct DeviceCode { pub(crate) device_code: String, pub(crate) user_code: String, pub(crate) verification_uri: Url, pub(crate) expires_in: u32, pub(crate) interval: u32 }`.
  - `pub(crate) enum DevicePoll { Pending, SlowDown { interval: u32 }, Expired, Denied, Approved(GithubToken) }`; `pub(crate) struct GithubToken` whose `Debug` prints `GithubToken(<redacted>)`.
  - `pub enum GithubError` (still `#[non_exhaustive]`): `BadUrl(String)`, `Request(#[source] reqwest::Error)`, `User(#[source] reqwest::Error)`, `Refused { code: String }`.
  - `Hub::github(&self) -> &Github`.
  - `POST /v1/auth/device` answers `{device_code, user_code, verification_uri, expires_in, interval}`: `device_code` is the hub's own secret; the other four come from GitHub, `expires_in` capped at 900. `verification_uri_complete` is gone.
  - `POST /v1/auth/token` with the device grant answers tokens, or 400 `authorization_pending`, `slow_down`, `expired_token`, `access_denied`, `invalid_grant`; 502 `upstream_unavailable` when GitHub fails.
  - Routes removed: `GET /v1/auth/device`, `POST /v1/auth/device/approve`, `GET /v1/auth/oauth/github/start`, `GET /v1/auth/oauth/github/callback`.
  - `pub struct FakeGithub` (feature `test-hooks`, re-exported from `lib.rs`), with `pub const FAKE_GITHUB_CLIENT_ID: &str = "agmen-test-client"`: `async fn start() -> Result<FakeGithub, HookError>`, `fn base(&self) -> &Url`, `fn set_interval(&self, secs: u32)` (default 1), `fn approve(&self, user_code: &str, id: u64, login: &str)`, `fn deny(&self, user_code: &str)`, `fn expire(&self, user_code: &str)`, `fn slow_down_next(&self, user_code: &str)` (answers `slow_down` with `interval` + 5 once), `fn refuse_next_poll(&self, error: &str)`, `fn fail_device_code(&self, status: u16)`, `fn register(&self, id: u64, login: &str)`, `fn token_polls(&self) -> usize`, `fn last_device_form(&self) -> Option<Vec<(String, String)>>`, `fn last_token_form(&self) -> Option<Vec<(String, String)>>`, `fn last_device_accept(&self) -> Option<String>`. It answers JSON only when the request sends `Accept: application/json` (GitHub's default is form encoding), answers token errors with HTTP 200 as GitHub does, serves `/user` and `/users/{login}` as today, and never uses `unwrap`, `expect` or `panic` (it compiles as library code under `--all-features`; take poisoned locks with `PoisonError::into_inner`).
  - `GithubId`, `GithubUser`, `upsert_human`, `claim_bootstrap_admin`, `account_by_login` and `is_human_handle` live in `crates/agmen-hub/src/auth/github.rs` with unchanged signatures; `auth/oauth.rs` no longer exists (the self-host plan's tenant-role change edits `claim_bootstrap_admin` there).
  - CLI: `DeviceStart` in `cmd/account.rs` reads `verification_uri: Url` in place of `verification_uri_complete`; `agmen login` prints `Open <verification_uri> and enter the code. It expires in <n> minutes.` after `Your code is <code>`.
  - Test harnesses: `crates/agmen-cli/tests/support/mod.rs` and `crates/agmen-client/tests/support/mod.rs` start `agmen_hub::FakeGithub`, set `AGMEN_GITHUB_URL` and `AGMEN_GITHUB_API_URL` to its base, set no secret, and keep it as `Ctx.github`.

- [ ] **Step 1: Write the failing tests**

Unit tests in `crates/agmen-hub/src/auth/github_tests.rs` (no database; they run in the default gate):

```rust
#[tokio::test] async fn start_device_sends_only_the_client_id()        // last_device_form == [("client_id","agmen-test-client")], no "client_secret" key; last_device_accept == "application/json"; answer user_code.len() == 9, verification_uri == base.join("login/device"), expires_in == 900, interval == 1
#[tokio::test] async fn poll_sends_the_device_grant_without_a_secret()  // last_token_form has grant_type "urn:ietf:params:oauth:grant-type:device_code", device_code == start's, client_id, and no client_secret
#[tokio::test] async fn poll_maps_every_github_answer()                 // fresh code: Pending; slow_down_next: SlowDown { interval: 6 }; approve(code, 4242, "Andy"): Approved(_); deny: Denied; expire: Expired (a fresh code each)
#[tokio::test] async fn user_reads_the_numeric_id()                     // Approved token -> user(): id.get() == 4242, login == "Andy"
#[tokio::test] async fn other_github_errors_are_refusals()              // refuse_next_poll("device_flow_disabled") -> Err(GithubError::Refused { code }) with code == "device_flow_disabled"
#[tokio::test] async fn unreachable_github_is_a_request_error()         // url http://127.0.0.1:9/ -> start_device() is Err(GithubError::Request(_))
#[test] fn github_token_debug_is_redacted()                             // format!("{token:?}") == "GithubToken(<redacted>)"
```

Config unit tests in `config.rs`:

```rust
#[test] fn github_client_id_defaults_to_the_shared_app()  // unset and "" -> DEFAULT_GITHUB_CLIENT_ID; "Ov23liexample" -> "Ov23liexample"
#[test] fn client_secret_is_not_read()                    // a lookup with neither AGMEN_GITHUB_CLIENT_ID nor AGMEN_GITHUB_CLIENT_SECRET is Ok
```

Integration tests in `crates/agmen-hub/tests/it_auth.rs` (support `TestHub::start_with_lease` points `GithubConfig.url` and `api_url` at `FakeGithub::base()`; `support::login(ctx, sub, login)` becomes start, `ctx.github.approve(user_code, sub, login)`, poll once). `it_device_flow_issues_human_token` asserts the old 600 s window and `verification_uri_complete`; the spec replaces both, so those assertions change as written here. `it_login_pages_refuse_framing` tests the removed pages and is replaced by `it_responses_refuse_framing`.

```rust
#[tokio::test] async fn it_device_flow_issues_human_token()        // start: expires_in == 900, interval == 1, verification_uri ends "/login/device", body has no "verification_uri_complete"; poll -> 400 authorization_pending; approve(user_code, 4242, "Andy"); poll after 1 s -> 200, token_type "Bearer", expires_in 3600; humans with provider_sub "4242" == 1; access token mints andy/sol (201); device_authorizations count == 0; same device_code again -> 400 invalid_grant
#[tokio::test] async fn it_device_flow_new_human_gets_lowercase_handle() // login(777, "Adam-K"): one human with handle "adam-k" (kept)
#[tokio::test] async fn it_device_poll_follows_github_interval()  // set_interval(2); poll twice 100 ms apart -> both authorization_pending, token_polls() == 1; after 2 s one more poll -> token_polls() == 2
#[tokio::test] async fn it_device_flow_slow_down_is_passed_on()   // slow_down_next(code); poll -> 400 slow_down; SELECT interval_secs == 6
#[tokio::test] async fn it_device_flow_access_denied()            // deny(code); poll -> 400 access_denied; next poll -> 400 invalid_grant
#[tokio::test] async fn it_device_flow_expired()                  // expire(code); poll -> 400 expired_token; next poll -> 400 invalid_grant
#[tokio::test] async fn it_device_start_github_down_is_502()      // fail_device_code(503); POST /v1/auth/device -> 502 upstream_unavailable; device_authorizations count == 0
#[tokio::test] async fn it_open_device_logins_are_capped()        // insert 1000 open rows by SQL; POST /v1/auth/device -> 503 temporarily_unavailable
#[tokio::test] async fn it_web_oauth_routes_are_gone()            // GET /v1/auth/device -> 405; GET /v1/auth/oauth/github/start?user_code=BCDF-GHJK -> 404; GET /v1/auth/oauth/github/callback?state=x -> 404; POST /v1/auth/device/approve -> 404
#[tokio::test] async fn it_hub_refuses_github_tokens()            // POST /v1/auth/token grant_type "urn:ietf:params:oauth:grant-type:token-exchange", subject_token format!("gho_{}", "a".repeat(36)) -> 400 unsupported_grant_type; GET /v1/me with that bearer -> 401
#[tokio::test] async fn it_responses_refuse_framing()             // POST /v1/auth/device and GET /v1/jwks carry X-Frame-Options DENY and CSP "frame-ancestors 'none'"
```

`it_me.rs:134-170` `it_device_flow_approved_by_test_hook` stays as written: `approve_device` matches the stored user code without regard to case.

CLI (`crates/agmen-cli/tests/it_cli.rs`, harness as in Produces; it approves with `ctx.github.approve(code, sub, login)` where it followed the hub's page before):

```rust
#[tokio::test] async fn it_login_device_flow() // as today, plus: stdout has the line format!("Open {}login/device and enter the code. It expires in 15 minutes.", ctx.github.base())
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p agmen-hub --locked -E 'test(github) | test(client_id) | test(client_secret)'`
Expected: FAIL to compile (`Github`, `FakeGithub`, `DEFAULT_GITHUB_CLIENT_ID` not defined).

- [ ] **Step 3: Write the migration** `0006_github_device.up.sql`: `DELETE FROM device_authorizations` (logins in progress are dropped), drop `oauth_state_hash` and `approval_hash`, add `github_device_code text NOT NULL` (GitHub's code, kept because the hub must send it to GitHub; the row lives at most 900 s), `interval_secs integer NOT NULL CHECK (interval_secs BETWEEN 1 AND 600)`, `next_poll_at timestamptz NOT NULL`. `human_id` and `approved_at` stay for the test hook only. The down migration reverses it, deleting rows first.

- [ ] **Step 4: Implement `Github` in `auth/github.rs`** with the Produces signatures. Requests send `Accept: application/json`; `start_device` posts form `client_id` only; `poll_device` posts `client_id`, `device_code`, `grant_type`; an `error` field maps `authorization_pending`, `slow_down` (reads `interval`), `expired_token`, `access_denied` to `DevicePoll`, any other code to `GithubError::Refused`. Move `upsert_human`, `claim_bootstrap_admin`, `account_by_login`, `is_human_handle`, `GithubId`, `GithubUser` here unchanged. Remove `oauth2` from `Cargo.toml` and refresh `Cargo.lock` with a plain `cargo build`.

- [ ] **Step 5: Implement `config.rs`:** `client_id` from `AGMEN_GITHUB_CLIENT_ID` or `DEFAULT_GITHUB_CLIENT_ID`; nothing reads the secret; update the `from_lookup` doc example (drop the two GitHub lines). `main.rs` `serve` logs one `warn!("AGMEN_GITHUB_CLIENT_SECRET is set but no longer used")` when that variable is non-empty.

- [ ] **Step 6: Rewrite `auth/device.rs`:** `start(State<Hub>) -> Result<Json<DeviceStart>, ApiError>` deletes expired rows, answers 503 `temporarily_unavailable` when 1000 rows are open (`MAX_OPEN_DEVICE_LOGINS: i64 = 1000`), calls `start_device`, stores the row with `next_poll_at = now()` and `expires_at = now() + min(expires_in, 900)` (`MAX_EXPIRES_IN_SECS: u32 = 900`), and answers 503 on a `user_code` conflict. `redeem(hub: &Hub, device_code: &str) -> Result<TokenResponse, ApiError>` locks the row `FOR UPDATE`, then in this order: unknown is `invalid_grant`; expired deletes and is `expired_token`; a row the test hook approved issues tokens; `next_poll_at > now()` is `authorization_pending` without calling GitHub; otherwise poll GitHub and set `next_poll_at = now() + interval`, with `SlowDown` storing the new interval and answering `slow_down`, `Expired` and `Denied` deleting the row, and `Approved` calling `user`, `upsert_human`, deleting the row and issuing tokens in the same transaction. The GitHub token is dropped there. `approve_as` matches `upper(user_code) = upper($2)` and no longer touches the dropped columns. Remove `/v1/auth/device` GET, `/approve` and both `/oauth/github` routes from `auth/mod.rs`.

- [ ] **Step 7: Implement `FakeGithub` in `test_github.rs`** from the old `tests/support/fake_github.rs`, with the Produces methods; user codes are 8 consonants shown as `BCDF-GHJK`. Replace `tests/support/fake_github.rs` and the helpers `approve_in_browser`, `open_confirm_page`, `post_approve`, `hidden_input`, `location` in `tests/support/mod.rs` with `FakeGithub` calls.

- [ ] **Step 8: Point the CLI and both client-side harnesses at the new answer:** the `DeviceStart` and printed-line change in Produces, and the CLI and client harness changes in Produces. Nothing else in the CLI changes in this task.

- [ ] **Step 9: Run the tests to verify they pass**

Run: `cargo nextest run -p agmen-hub -p agmen-cli --locked`, then `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run --workspace --locked --profile it -E 'test(it_device) | test(it_login) | test(it_web_oauth) | test(it_hub_refuses) | test(it_responses_refuse) | test(it_open_device) | test(it_bootstrap)'`, then the gate.
Expected: PASS; `rg -n 'client_secret|CLIENT_SECRET|oauth2' crates` prints only the `main.rs` warning and test assertions that the secret is absent.

- [ ] **Step 10: Commit**

```bash
git add -A crates/agmen-hub crates/agmen-cli crates/agmen-client/tests Cargo.lock
git commit -m "Run GitHub's device flow in the hub on a shared client id"
```

### Task 1b: `agmen login` opens GitHub's device page

**Files:**
- Create: `crates/agmen-cli/src/open.rs`
- Modify: `crates/agmen-cli/src/cmd/account.rs:44-108` (split `login` into `device_login` plus the `signed in as` line), `crates/agmen-cli/src/lib.rs` (add `mod open`)
- Test: `crates/agmen-cli/src/open.rs` (unit), `crates/agmen-cli/tests/it_cli.rs`

**Interfaces:**
- Consumes: Task 1 `DeviceStart.verification_uri`, `Ctx.github`, `FakeGithub::deny`; the hub's 400 `access_denied`.
- Produces:
  - `pub(crate) async fn device_login(hub: &Url) -> Result<Human>` in `cmd/account.rs`: prints `Your code is <code>` and the `Open ...` line, calls `open_url` through `blocking`, polls, saves the login, and maps `access_denied` to `the sign-in was declined on GitHub; run agmen login again`. `login` is `device_login` plus `signed in as <handle>`.
  - `pub(crate) fn open_url(url: &Url) -> bool` and `fn openable(url: &Url) -> bool` in `open.rs`. `open_url` returns `false` without running anything unless stdout is a terminal (`std::io::IsTerminal`) and `openable(url)` holds; it runs the first of `open` (macOS), `xdg-open` (Linux), `rundll32 url.dll,FileProtocolHandler` (Windows), waiting at most 5 s (`OPEN_TIMEOUT`).

- [ ] **Step 1: Write the failing tests**

```rust
// open.rs
#[test] fn only_https_or_loopback_http_opens() // openable: "https://github.com/login/device" true, "http://127.0.0.1:8080/x" true, "http://example.com" false, "file:///etc/passwd" false, "javascript:alert(1)" false
// it_cli.rs
#[tokio::test] async fn it_login_declined_on_github() // ctx.github.deny(code): exit 1, stderr contains "declined on GitHub"
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'test(only_https)'`, then `... --profile it --test it_cli -E 'test(it_login_declined)'` (`--test it_cli` leaves out the unit tests that do not compile yet).
Expected: the first fails to compile (`open` module missing); the second fails because stderr says `signing in failed`, not `declined on GitHub`.

- [ ] **Step 3: Implement** `open.rs` and `device_login` as in Produces.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p agmen-cli --locked`, then `... --profile it -E 'test(it_login)'`, then the gate.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-cli
git commit -m "Open GitHub's device page from agmen login and name a declined sign-in"
```

### Task 2: Invite links and the /i page

**Files:**
- Create: `crates/agmen-hub/src/invite_page.rs`, `crates/agmen-hub/src/invite_page.html`
- Modify: `crates/agmen-hub/src/invites.rs:33-40,99-108`, `crates/agmen-hub/src/http.rs:117-140,203-214`, `crates/agmen-hub/src/lib.rs:21-44`, `crates/agmen-cli/src/cmd/account.rs:214-258`, `crates/agmen-cli/src/open.rs`
- Test: `crates/agmen-hub/src/invite_page.rs` (unit), `crates/agmen-hub/tests/it_auth.rs`, `crates/agmen-cli/tests/it_cli.rs:112-190`

**Interfaces:**
- Consumes: `Hub::public_link(path) -> Url` (`http.rs:178`); Task 1b `open.rs` and its `OPEN_TIMEOUT`.
- Produces:
  - `GET /i` (outside `/v1`): `invite_page::page() -> Response`, body `INVITE_PAGE: &str = include_str!("invite_page.html")`, headers `Content-Type: text/html; charset=utf-8`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`, `Content-Security-Policy: INVITE_PAGE_CSP` where `pub(crate) const INVITE_PAGE_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"`.
  - `refuse_framing` (`http.rs:206`) keeps a CSP a handler set and inserts `frame-ancestors 'none'` only when none is present; `X-Frame-Options: DENY` stays on every response.
  - `POST /v1/invites` answers `{code, link, project, role, expires_at}` with `link = hub.public_link("i")` plus fragment `code`, for example `https://api.getagmen.ai/i#<64 hex>`. Self-host and hosted plans and the desktop app print `link`.
  - CLI: `pub(crate) fn copy_text(text: &str) -> bool` in `open.rs` (terminal only; first of `pbcopy`, `wl-copy`, `xclip -selection clipboard`, `xsel --clipboard --input`, `clip`; 5 s cap).

The page (layout is the builder's): the script reads `location.hash`, accepts only `/^[0-9a-f]{64}$/` after lowercasing, builds the join command from `location.origin + location.pathname + '#' + code`, writes every value with `textContent`, picks the install line from `navigator.userAgent` (Windows: `powershell -ExecutionPolicy Bypass -c "irm https://getagmen.ai/install.ps1 | iex"`; otherwise `curl --proto '=https' --tlsv1.2 -fsSL https://getagmen.ai/install.sh | sh`, plus `brew install agmen-dev/tap/agmen` on macOS), and otherwise shows `This link is missing its invite code. Ask for a new link.` It says to run the join command in a clone of the project's repository.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/agmen-hub/src/invite_page.rs
#[test] fn page_reads_the_code_only_from_the_fragment() // INVITE_PAGE contains "location.hash" and "/^[0-9a-f]{64}$/"; contains none of "fetch(", "XMLHttpRequest", "sendBeacon", "innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "location.search"
#[test] fn page_shows_the_install_lines_and_join() // contains "curl --proto '=https' --tlsv1.2 -fsSL https://getagmen.ai/install.sh | sh", "irm https://getagmen.ai/install.ps1 | iex", "brew install agmen-dev/tap/agmen", "agmen join "
#[test] fn csp_blocks_every_request()              // INVITE_PAGE_CSP contains "default-src 'none'", "frame-ancestors 'none'", "base-uri 'none'", "form-action 'none'"; no "connect-src", no "img-src"

// crates/agmen-hub/tests/it_auth.rs
#[tokio::test] async fn it_invite_page_headers()          // GET /i and GET /i?x=1: 200, content-type "text/html; charset=utf-8", referrer-policy "no-referrer", cache-control "no-store", x-frame-options "DENY", content-security-policy == INVITE_PAGE_CSP's text; both bodies identical
#[tokio::test] async fn it_invite_answer_carries_the_link() // POST /v1/invites: link == format!("{}i#{}", base, code); code matches ^[0-9a-f]{64}$
```

CLI: `it_invite_and_join` reads the link line `  <base>i#<code>` instead of `agmen join <code>` and joins with the fragment's code until Task 7 teaches join links; add `assert!(!text.contains("copied"))` (stdout is not a terminal). `open.rs` unit test (`#[cfg(unix)]`):

```rust
#[test] fn run_first_falls_through_to_the_next_candidate() // [("agmen-no-such-program", &[]), ("sh", &["-c", "cat > \"$F\""])] with input "x" and F in a tempdir -> true; the file holds "x"
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p agmen-hub -p agmen-cli --locked -E 'test(page_) | test(csp_) | test(run_first)'`
Expected: FAIL to compile (`invite_page`, `copy_text` missing).

- [ ] **Step 3: Implement** `invite_page.rs` and `invite_page.html` as specified, the `/i` route in `Hub::router`, the `refuse_framing` change, `link: Url` on `invites::Created`, and `copy_text` with the private `run_first(candidates: &[(&str, &[&str])], input: Option<&str>) -> bool` that `open_url` also uses.

- [ ] **Step 4: Implement `agmen invite` output** (`account.rs:247-256`): the existing heading line, then `  <link>`, then `(copied to the clipboard)` only when `copy_text` returned true, then `Send it. They open it and follow the steps on the page.` The `Created` struct reads `link`.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo nextest run -p agmen-hub -p agmen-cli --locked`, then `... --profile it -E 'test(it_invite) | test(it_responses_refuse)'`, then the gate.
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/agmen-hub crates/agmen-cli
git commit -m "Serve invite links at /i with the code in the fragment"
```

### Task 3: Optional project repository URL

**Files:**
- Create: `crates/agmen-proto/src/repo_url.rs`, `crates/agmen-hub/migrations/0007_project_repo_url.up.sql`, `crates/agmen-hub/migrations/0007_project_repo_url.down.sql`
- Modify: `crates/agmen-proto/src/lib.rs:38-58` (`mod repo_url;`, `pub use repo_url::{BadRepoUrl, RepoUrl};`), `crates/agmen-hub/src/invites.rs:120-196`, `crates/agmen-hub/src/bootstrap.rs:65-115,158-215`, `crates/agmen-hub/src/main.rs:10-37,71-123` and tests `211-280`, `crates/agmen-hub/src/test_hooks.rs`
- Test: `crates/agmen-proto/src/repo_url.rs` (unit), `crates/agmen-hub/src/main.rs` (unit), `crates/agmen-hub/tests/it_auth.rs`

**Interfaces:**
- Produces:
  - `projects.repo_url text NULL CHECK (repo_url IS NULL OR length(repo_url) <= 512)`, added only by `0007`; no later migration adds the column again.
  - `pub struct RepoUrl(String)` in `agmen-proto` (`agmen_proto::RepoUrl`, so the hub, the client and the CLI share one rule; `agmen-cli` depends on `agmen-hub` only as a dev-dependency) with `FromStr<Err = BadRepoUrl>`, `as_str(&self) -> &str`, `Display`, `Serialize`, `Deserialize` through `TryFrom<String>`; `pub struct BadRepoUrl` (`thiserror`, message says which forms are allowed). Accepted forms: `https://host/path` with no user info, `ssh://user@host/path` with no password, scp form `user@host:path`; at most 512 bytes; no whitespace or control characters; never starting with `-`. The parser is plain `str` code: `agmen-proto` has no `url` dependency and gains none, and the type has doc comments (`lib.rs:36` warns on missing docs). `crates/agmen-hub` has no `repo_url.rs` and no re-export; the hub names `agmen_proto::RepoUrl`. Every API that takes a repository URL (the self-host plan's `POST /v1/projects`, the hosted plan's `POST /v1/groups`) names the field `repo_url`, parses it with `RepoUrl::from_str` and refuses with 422 `bad_repo_url`; a CLI that sends `git remote get-url origin` sends it only when `RepoUrl::from_str` accepts it, so a token embedded in an origin URL never reaches the hub.
  - `POST /v1/invites/{code}/accept` answers `{tenant, project, role, name, repo_url}` (`repo_url` is `null` when unset).
  - `Bootstrap::with_repo_url(self, repo_url: RepoUrl) -> Self`; `agmen-hub bootstrap ... --repo-url <url>`; `Bootstrapped.repo_url: Option<RepoUrl>` serialized as `repo_url`.
  - Test hook: `Hub::set_repo_url(&self, topics: &Topics, repo_url: &RepoUrl) -> Result<(), HookError>`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/agmen-proto/src/repo_url.rs
#[test] fn repo_urls_accept_https_ssh_and_scp_forms() // "https://github.com/agmen-dev/agmen", "https://github.com/agmen-dev/agmen.git", "ssh://git@github.com/agmen-dev/agmen.git", "git@github.com:agmen-dev/agmen.git" parse and round-trip through as_str
#[test] fn repo_urls_refuse_secrets_options_and_local_paths() // "", "http://github.com/a/b", "https://u:p@github.com/a/b", "https://u@github.com/a/b", "ssh://git:pw@github.com/a", "file:///tmp/r", "/tmp/r", "-oProxyCommand=x", "git@github.com:a b", "https://github.com/a\nb", format!("https://github.com/{}", "a".repeat(495)) all Err
// main.rs
#[test] fn bootstrap_takes_a_repo_url()               // parse_args bootstrap --tenant t --project p --admin-github-id 1 --repo-url https://github.com/agmen-dev/agmen -> Bootstrap with that RepoUrl; a bad URL is an error naming --repo-url
// it_auth.rs
#[tokio::test] async fn it_accept_answers_project_name_and_repo_url() // first accept: name == "project <id>", repo_url null; set_repo_url(https://github.com/agmen-dev/agmen); a second invite accepted by another human: repo_url == "https://github.com/agmen-dev/agmen"
#[tokio::test] async fn it_bootstrap_sets_repo_url()                 // bootstrap with --repo-url: projects.repo_url stored, JSON "repo_url" equals it; rerun without it keeps it
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p agmen-proto -p agmen-hub --locked -E 'test(repo_url) | test(bootstrap_takes)'`
Expected: FAIL to compile (`RepoUrl` missing).

- [ ] **Step 3: Implement** the migration, `RepoUrl`, the accept answer (one `JOIN projects` in the existing `SELECT ... FOR UPDATE`), the bootstrap option (an `UPDATE` only when given) and the hook.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo nextest run -p agmen-proto -p agmen-hub --locked`, then `... --profile it -E 'test(it_accept) | test(it_bootstrap) | test(it_invite)'`, then the gate.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-proto crates/agmen-hub
git commit -m "Record an optional repository URL per project and return it on accept"
```

## Phase B: CLI and agents

### Task 4: `agmen setup` with Claude Code

**Files:**
- Create: `crates/agmen-cli/src/setup/mod.rs`, `crates/agmen-cli/src/setup/json.rs`, `crates/agmen-cli/src/setup/claude.rs`
- Modify: `crates/agmen-cli/src/cmd/mod.rs:26-73,95-122`, `crates/agmen-cli/src/lib.rs`, `crates/agmen-cli/src/git.rs` (add `is_tracked`, take `add_exclude` from `cmd/join.rs:253-278` and its test `join.rs:346-359`), `crates/agmen-cli/src/cmd/join.rs:161-162`, `docs/mcp-setup.md:1-33`
- Test: `crates/agmen-cli/src/setup/json.rs`, `crates/agmen-cli/src/setup/mod.rs` (unit), `crates/agmen-cli/tests/cli.rs`, `crates/agmen-cli/tests/support/mod.rs:300-410`

**Interfaces:**
- Consumes: `agmen_client::{find_identity, IdentityFile}`, `crate::joined::joined()`, `crate::client::blocking`.
- Produces:
  - `agmen setup [--agent <agent>] [--no-plugin]`, about text "Set up your installed agents to use agmen in this worktree." Outside a joined worktree it fails like `agmen inbox` (exit 3, `NOT_JOINED`).
  - `#[derive(ValueEnum)] pub(crate) enum AgentKind` holds only the agents that have a writer: `Claude` (display `Claude Code`) in this task; Task 5 adds `Codex`, `Opencode` (`Codex`, `OpenCode`) and Task 6 adds `Cursor`, `Gemini` (`Cursor`, `Gemini CLI`). Until a variant exists, `--agent <that name>` is clap's usage error (exit 2) listing the possible values, and setup never detects that agent.
  - `pub(crate) enum Plugin { Install, Decline }`; `pub(crate) fn plugin_choice(no_plugin: bool) -> Result<Plugin>` holds the prompt rule below, and both `agmen setup` and `agmen join` call it.
  - `pub(crate) struct AgentEnv { path: Vec<PathBuf>, home: PathBuf }`; `AgentEnv::from_process() -> Result<AgentEnv>` (`PATH`, and `HOME` or `USERPROFILE`); `fn find_program(&self, name: &str) -> Option<PathBuf>` (`name`, and `name.exe` on Windows).
  - `pub(crate) async fn run(root: &Path, identity: &IdentityFile, only: Option<AgentKind>, plugin: Plugin) -> Result<Vec<Change>>`; `pub(crate) struct Change { pub(crate) agent: AgentKind, pub(crate) outcome: Outcome }`; `pub(crate) enum Outcome { Configured(String), Unchanged, Skipped(String), Failed(String) }`; `Change` displays as `<agent>: <what>`, and `Unchanged` as `<agent>: already set up`.
  - `pub(crate) struct Merged { pub(crate) text: String, pub(crate) changed: bool }`; `pub(crate) enum SetupError { Unreadable { file: PathBuf, why: String } }`.
  - `pub(crate) fn merge_mcp_servers(existing: Option<&str>) -> Result<Merged, SetupError>`: sets `mcpServers.agmen = {"command":"agmen","args":["mcp"]}`, keeps every other key.
  - `pub(crate) async fn write_worktree_file(root: &Path, rel: &str, merge: impl FnOnce(Option<&str>) -> Result<Merged, SetupError>) -> Result<Outcome>`: a tracked file is `Skipped("<rel> is tracked by git; add the agmen entry by hand (docs/mcp-setup.md)")`; otherwise writes when changed and adds `rel` to `info/exclude`.
  - `pub(crate) async fn is_tracked(root: &Path, rel: &str) -> Result<bool>` (`git ls-files --error-unmatch -- <rel>`) and `pub(crate) fn add_exclude(path: &Path, line: &str) -> Result<()>` in `git.rs`.
  - Claude Code: detected by `claude` on `PATH` or `~/.claude`. `Plugin::Install`: `claude plugin list --json`; when it lists `agmen@agmen`, `Unchanged`; otherwise `claude plugin install agmen --marketplace agmen-dev/agmen --scope user` (120 s cap). A failed install writes `.mcp.json` and reports `plugin install failed (<first stderr line>); wrote .mcp.json instead`. `Plugin::Decline` writes `.mcp.json`. On a terminal, `agmen setup` asks `Install the agmen plugin for Claude Code (wake-ups and the lane guard)? [Y/n]` unless `--no-plugin`; without a terminal it installs unless `--no-plugin`.
  - Test harness (`crates/agmen-cli/tests/support/mod.rs:303-371`), the one PATH rule for every CLI test and the one fake-program mechanism (the self-host plan's fake `docker` and `tailscale` use it too): `Home::new` makes `<home>/bin` with a `git` symlink to the git found on the test's `PATH`; `apply_env` sets `PATH` to `<home>/bin:/usr/bin:/bin` (so fake scripts and the CLI can run `sh`, `cat`, `mv` by bare name) and removes `CODEX_HOME`; `Home::fake_program(&self, name: &str, script: &str)` writes an executable sh script `<home>/bin/<name>` that appends `$*` to `$HOME/calls/<name>.log` before running `script`. `Home::new` panics with `an agent program is installed in /usr/bin or /bin; the setup tests cannot hide it` when any of `claude`, `codex`, `opencode`, `cursor`, `cursor-agent`, `gemini` exists in `/usr/bin` or `/bin`, so no test ever runs a real agent.

- [ ] **Step 1: Read `claude plugin list --json` once on this machine** (read-only) and record the shape that names an installed plugin in `setup/claude.rs`'s module doc; the fake in the tests prints the same shape.

- [ ] **Step 2: Write the failing tests**

```rust
// setup/json.rs
#[test] fn adds_agmen_to_a_missing_file()         // None -> {"mcpServers":{"agmen":{"command":"agmen","args":["mcp"]}}}, changed
#[test] fn keeps_other_servers_and_keys()          // {"mcpServers":{"x":{"command":"x"}},"other":1} -> "x" and "other" kept, agmen added
#[test] fn second_run_is_unchanged()               // merging its own output: changed == false, same text
#[test] fn replaces_a_stale_agmen_entry()          // {"mcpServers":{"agmen":{"command":"old"}}} -> the agmen entry above, changed
#[test] fn refuses_files_that_are_not_json_objects() // "[]", "{,}", "// note\n{}", "{\"mcpServers\":[]}" -> Err(SetupError::Unreadable { .. })
// setup/mod.rs
#[test] fn detects_agents_by_program_or_config_dir() // empty PATH and home: none; claude on PATH: Claude; ~/.claude dir only: Claude
// tests/cli.rs, #[cfg(unix)]
#[test] fn setup_needs_a_joined_worktree()                 // exit 3, stderr contains "agmen join"
#[test] fn setup_refuses_an_agent_it_cannot_set_up_yet()   // --agent codex: exit 2, stderr contains "possible values: claude"
#[test] fn fake_programs_can_run_system_tools()            // fake_program("probe", "cat \"$1\" > \"$HOME/out\"; mv \"$HOME/out\" \"$HOME/moved\""), run as Command::new("probe").arg(<file>) with apply_env: exit 0; <home>/moved holds the file's text; calls/probe.log has one line
#[test] fn setup_with_no_agents_says_so()                  // exit 0, stdout "no agents found: install one, then run agmen setup"
#[test] fn setup_installs_the_claude_plugin()              // calls/claude.log lines: "plugin list --json", "plugin install agmen --marketplace agmen-dev/agmen --scope user"; stdout "Claude Code: installed the agmen plugin"; no .mcp.json
#[test] fn setup_skips_an_installed_plugin()               // fake list names agmen@agmen: no install line; stdout "Claude Code: already set up"
#[test] fn setup_no_plugin_writes_mcp_json()               // --no-plugin: .mcp.json equals the merged entry; info/exclude has ".mcp.json"; second run "Claude Code: already set up"
#[test] fn setup_falls_back_when_plugin_install_fails()    // fake install exits 1 with stderr "offline": .mcp.json written; stdout contains "plugin install failed (offline)"
#[test] fn setup_leaves_a_tracked_mcp_json_alone()         // committed .mcp.json: bytes unchanged; stdout contains ".mcp.json is tracked by git"
```

The tests write `IDENTITY` (`tests/cli.rs:12`) to `.agmen/identity.json` with `.agmen/` in `info/exclude`, so the trust test passes without a hub.

- [ ] **Step 3: Run to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'test(setup) | test(json::) | test(detects_agents) | test(fake_programs)'`
Expected: FAIL to compile (`setup` module and `Home::fake_program` missing).

- [ ] **Step 4: Implement** the module, the command, the Claude Code writer and the harness changes as specified; `join` calls `git::add_exclude(&exclude, ".agmen/")`. Each line goes to stdout through `clean`.

- [ ] **Step 5: Update `docs/mcp-setup.md`:** step 2 becomes `agmen setup` (runs after join, writes each agent's entry); the Claude Code section says setup installs the plugin or writes `.mcp.json`; the hand-written entries stay as the fallback.

- [ ] **Step 6: Run to verify they pass**

Run: `cargo nextest run -p agmen-cli --locked`, then `... --profile it -p agmen-cli` (the PATH change touches every CLI test: a test or a CLI path that runs a program outside `<home>/bin`, `/usr/bin` and `/bin` fails here, and the fix is a `fake_program` or a symlink in `Home::new`, never a wider PATH), then the gate.
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/agmen-cli docs/mcp-setup.md
git commit -m "Add agmen setup with Claude Code"
```

### Task 5: `agmen setup` for Codex and OpenCode

**Files:**
- Create: `crates/agmen-cli/src/setup/codex.rs`, `crates/agmen-cli/src/setup/opencode.rs`
- Modify: `crates/agmen-cli/src/setup/mod.rs`, `crates/agmen-cli/Cargo.toml` (add `toml_edit`), `Cargo.lock`, `docs/mcp-setup.md:35-64`
- Test: `crates/agmen-cli/src/setup/codex.rs`, `crates/agmen-cli/src/setup/opencode.rs` (unit), `crates/agmen-cli/tests/cli.rs`

**Interfaces:**
- Consumes: Task 4's `run`, `AgentKind`, `AgentEnv`, `Merged`, `SetupError`, `write_worktree_file`, `Outcome`.
- Produces:
  - `AgentKind::Codex` (display `Codex`) and `AgentKind::Opencode` (display `OpenCode`).
  - `pub(crate) struct CodexEntry { pub(crate) cwd: PathBuf, pub(crate) identity: IdentityId }`; `pub(crate) enum CodexChange { Added, Unchanged, Repointed { from: PathBuf } }`; `pub(crate) fn merge_codex(existing: Option<&str>, entry: &CodexEntry) -> Result<(String, CodexChange), SetupError>`. It writes `[mcp_servers.agmen]` with `command = "agmen"`, `args = ["mcp", "--identity", "<id>"]`, `cwd = "<worktree root>"`, keeping comments and every other table. The file is `$CODEX_HOME/config.toml`, else `~/.codex/config.toml`. Codex holds one `agmen` entry, so a second worktree repoints it and setup says `Codex: now points at <root> (was <old root>)`; the `--identity` pin makes a session in the old worktree refuse rather than act as the new identity.
  - `pub(crate) fn merge_opencode(existing: Option<&str>) -> Result<Merged, SetupError>`: sets `mcp.agmen = {"type":"local","command":["agmen","mcp"],"enabled":true}`, adds `"$schema": "https://opencode.ai/config.json"` to a new file. With `opencode.jsonc` present, `Skipped("opencode.jsonc exists; add the agmen entry by hand (docs/mcp-setup.md)")`.
  - After any agent other than Claude Code with the plugin is configured, setup prints `lane guard: only the Claude Code plugin blocks edits outside your lanes; other agents should call check_path before editing`.
  - Detection: Codex by `codex` on `PATH` or the Codex home; OpenCode by `opencode` on `PATH` or `~/.config/opencode`.

`toml_edit` is new: the workspace's `toml` 1.1.8 drops comments when it writes, and the user's Codex file is theirs. Add it with `cargo add -p agmen-cli toml_edit` and keep `cargo deny check` green.

- [ ] **Step 1: Write the failing tests**

```rust
// codex.rs
#[test] fn adds_the_agmen_server()                  // None -> parses to command "agmen", args ["mcp","--identity","idn7k2q9"], cwd "/work/repo"; Added
#[test] fn keeps_comments_and_other_servers()       // "# mine\n[mcp_servers.other]\ncommand = \"x\"\n" -> output starts with that text byte for byte
#[test] fn same_worktree_is_unchanged()             // merging its own output -> Unchanged, same text
#[test] fn another_worktree_is_repointed_and_reported() // cwd "/work/a" then entry "/work/b" -> Repointed { from: "/work/a" }
#[test] fn refuses_unusable_toml()                  // "[[", "mcp_servers = 1", "[mcp_servers]\nagmen = 1" -> Err(SetupError::Unreadable { .. })
// setup/mod.rs
#[test] fn detects_codex_and_opencode()            // ~/.codex dir only: Codex; ~/.config/opencode dir only: OpenCode; opencode on PATH: OpenCode
// opencode.rs
#[test] fn opencode_entry_is_local_and_enabled()   // None -> $schema plus mcp.agmen as above; other keys kept on an existing file; second run unchanged
// tests/cli.rs, #[cfg(unix)]
#[test] fn setup_writes_codex_config_with_cwd()     // fake codex: <home>/.codex/config.toml has cwd == canonical worktree root and args containing "idn7k2q9"; stdout "Codex: wrote"
#[test] fn setup_writes_opencode_json_and_excludes_it() // fake opencode: opencode.json written; info/exclude has "opencode.json"
#[test] fn setup_skips_opencode_jsonc()             // opencode.jsonc present: no opencode.json; stdout contains "opencode.jsonc exists"
#[test] fn setup_agent_flag_picks_one()             // fake codex and opencode, --agent opencode: no codex config file
#[test] fn setup_names_the_lane_guard_limit()       // fake codex: stdout contains "check_path before editing"
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'test(codex) | test(opencode) | test(setup_)'`
Expected: FAIL to compile (`merge_codex`, `merge_opencode`, `AgentKind::Codex` missing).

- [ ] **Step 3: Implement** both writers, the two variants, and register them in `run`. Task 4's `setup_refuses_an_agent_it_cannot_set_up_yet` changes to `--agent cursor` (stderr contains "possible values: claude, codex, opencode").

- [ ] **Step 4: Update `docs/mcp-setup.md`:** the Codex and OpenCode sections lead with `agmen setup`, show what it writes, and keep the hand-written forms.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo nextest run -p agmen-cli --locked`, then the gate.
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/agmen-cli Cargo.lock docs/mcp-setup.md
git commit -m "Set up Codex and OpenCode with agmen setup"
```

### Task 6: `agmen setup` for Cursor and Gemini CLI

**Files:**
- Modify: `crates/agmen-cli/src/setup/mod.rs`, `crates/agmen-cli/src/setup/json.rs`, `docs/mcp-setup.md`
- Test: `crates/agmen-cli/tests/cli.rs`

**Interfaces:**
- Consumes: Task 4's `merge_mcp_servers`, `write_worktree_file`, `AgentKind`.
- Produces: `AgentKind::Cursor` (display `Cursor`) and `AgentKind::Gemini` (display `Gemini CLI`). Cursor writes `.cursor/mcp.json` in the worktree (exclude line `.cursor/mcp.json`), detected by `cursor` or `cursor-agent` on `PATH` or `~/.cursor`; Gemini CLI writes `.gemini/settings.json` in the worktree (exclude line `.gemini/settings.json`), detected by `gemini` on `PATH` or `~/.gemini`. Both use the `mcpServers` map unless Step 1 finds otherwise.

- [ ] **Step 1: Confirm both formats from current docs** (Cursor's MCP documentation at docs.cursor.com and the Gemini CLI repository's MCP server doc under `github.com/google-gemini/gemini-cli/docs`): file location, top-level key, entry fields, and whether the server starts in the project directory. Write the URLs and the date in `setup/mod.rs`'s module doc. If a format differs from `mcpServers` with `command` and `args`, add its own merge function beside `merge_mcp_servers` with the same unit tests as Task 4's, and write the tests below to the documented shape.

- [ ] **Step 2: Write the failing tests**

```rust
#[test] fn setup_writes_cursor_mcp_json()                  // fake cursor-agent: .cursor/mcp.json has the agmen entry; info/exclude has ".cursor/mcp.json"
#[test] fn setup_writes_gemini_settings_and_keeps_others() // fake gemini, existing .gemini/settings.json {"theme":"x"}: theme kept, agmen entry added; info/exclude has ".gemini/settings.json"
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'test(cursor) | test(gemini)'`
Expected: FAIL (no file written).

- [ ] **Step 4: Implement** both variants and agents in `run`, and add their sections to `docs/mcp-setup.md`. Every agent then has a writer, so delete `setup_refuses_an_agent_it_cannot_set_up_yet`; clap's own value check covers unknown names.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo nextest run -p agmen-cli --locked`, then the gate.
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/agmen-cli docs/mcp-setup.md
git commit -m "Set up Cursor and Gemini CLI with agmen setup"
```

### Task 7: `agmen join <link>` does everything

**Files:**
- Modify: `crates/agmen-cli/src/cmd/join.rs:1-200`, `crates/agmen-cli/src/client.rs:234-327`, `crates/agmen-cli/src/cmd/account.rs:123-161,247-256`
- Test: `crates/agmen-cli/src/cmd/join.rs` (unit), `crates/agmen-cli/tests/cli.rs`, `crates/agmen-cli/tests/it_cli.rs:112-190`

**Interfaces:**
- Consumes: Task 1b `device_login(hub: &Url) -> Result<Human>`; Task 2 `link` on `POST /v1/invites`; Task 3 accept answer `{tenant, project, role, name, repo_url}` (read as `repo_url: Option<agmen_proto::RepoUrl>`, so a hub cannot put an option-shaped or secret-bearing string into the printed `git clone` line) and `Hub::set_repo_url`; Task 4 `setup::run(root, identity, None, setup::plugin_choice(false)?)`.
- Produces:
  - `agmen join [<link|code>] [--hub <url>] [--project <id>] [--handle <name>] [--kind agent|coordinator]`; `--hub` has `env = "AGMEN_HUB"` and applies to a bare code only.
  - `pub(crate) enum Invite { Link { hub: Url, code: String }, Code(String) }`; `pub(crate) fn parse_invite(text: &str) -> Result<Invite, Fail>`: trims whitespace and one pair of `<` `>`; a URL must be `https`, or `http` to `localhost`, `127.0.0.1` or `[::1]`, with last path segment `i`, no query, and a 64-hex fragment (lowercased); its hub is the URL without that segment and fragment. A bare 64-hex string is `Code`. Anything else is a usage error naming the problem.
  - `pub(crate) async fn human_at(hub: &Url) -> Result<Option<Human>>` in `client.rs`: the signed-in person when the login file names that hub (same origin and path) and the token works, else `None`. The self-host plan's `agmen host up` and the hosted plan's `agmen init` call it, then `device_login(hub)` when it is `None`, rather than reading the login file themselves.
  - `pub(crate) async fn join_here(human: &Human, root: &Path, project: &str, args: &JoinArgs) -> Result<IdentityFile>` in `cmd/join.rs`: mints the identity for `project` in the worktree at `root` and writes `.agmen/identity.json`, as `join --project` does today. Both the self-host plan's `agmen host up` (its Task 9) and the hosted plan's `agmen init` (its Task 12) call it, then `setup::run(root, &identity, None, plugin)`, in process; `host up` runs only `agmen invite` as a child.
  - `pub(crate) struct JoinArgs` (today `join.rs:39-54`) keeps `invite: Option<String>` (now a link or a code), `project`, `handle` and `kind`, and gains `hub: Option<Url>`. The `how` group (`join.rs:40`) is no longer `required(true)`, so `agmen join` alone reaches the only-project rule below; it keeps `invite` and `project` mutually exclusive. The self-host plan (its Task 9) adds `JoinArgs::with_handle` on these fields, and the hosted plan's `agmen init` uses it.
  - Flow: parse; resolve the hub; `human_at` or `device_login` (when another hub's login is replaced, print `signed out of <old hub>: agmen keeps one sign-in per machine`); accept; then outside a git worktree stop with exit 1 and `joined project <name> (<id>). This folder is not a clone of its repository.`, then `  git clone <repo_url>` when the project has one, then `then, inside the clone: agmen join --project <id>`; inside one, mint, write `.agmen/identity.json` as today, run setup (a setup error prints `agent setup failed: <why>; run agmen setup` and join still exits 0), and print `joined project <name> (<id>) as <handle> (<identity id>) at <hub>`, the setup lines, and `Next: start your agent in this folder; it has the agmen tools.`
  - With no link, code or `--project`: the person's only project; none is a usage error `no projects yet: run agmen join <invite link>`; several list each id and name and ask for `--project`.
  - `invite_used` says `the invite was already used; if you used it, run agmen join --project <id> in your clone (agmen whoami lists your projects)`.
  - `agmen invite` adds the line `They run: agmen join <link>`; `whoami`'s empty line says `no projects yet: accept an invite with agmen join <invite link>`.

- [ ] **Step 1: Write the failing tests**

```rust
// join.rs, CODE = "ab".repeat(32)
#[test] fn invite_links_and_codes_parse()
// Link: "https://api.getagmen.ai/i#CODE" (hub "https://api.getagmen.ai/"); "  <https://h.example/hub/i#CODE>\n" (hub "https://h.example/hub/");
//       "https://h.example/i#" + CODE.to_uppercase() (code lowercased); "http://127.0.0.1:8080/i#CODE"; "http://localhost:8080/i#CODE"
// Code: CODE
// usage errors: "http://h.example/i#CODE" ("must be https"), "https://h.example/i", "https://h.example/i#abc", "https://h.example/x#CODE", "https://h.example/i?c=1#CODE", ""
// tests/cli.rs
#[test] fn join_bare_code_needs_a_hub()      // no login, `agmen join <CODE>` in a worktree: exit 2, stderr contains "--hub"
// it_cli.rs
#[tokio::test] async fn it_join_link_signs_in_accepts_and_sets_up() // newcomer home with no login, fake codex: spawn `join <link> --handle sol`; read "Your code is X"; ctx.github.approve(X, 9001, "Newbie"); exit 0; identity.handle == "newbie/sol"; login file hub_url == base; <home>/.codex/config.toml cwd == worktree root; stdout contains "joined project", "Codex: wrote", "Next: start your agent"
#[tokio::test] async fn it_join_reuses_the_sign_in_for_the_same_hub() // signed in at base: no "Your code is" line; exit 0
#[tokio::test] async fn it_join_outside_a_worktree_points_to_the_clone() // set_repo_url(https://github.com/agmen-dev/agmen); `join <link>` in a plain tempdir: exit 1, stderr has "git clone https://github.com/agmen-dev/agmen" and format!("agmen join --project {project}"); same link in a worktree: exit 1 "already used"; `join --project <id>` there: exit 0
#[tokio::test] async fn it_join_without_arguments_uses_the_only_project() // member of one project: `agmen join` exit 0 into it; member of two: exit 2, stderr names both ids
#[tokio::test] async fn it_invite_and_join()  // updated: joins with the "They run: agmen join <link>" line's link; the rest as today
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'test(invite_links) | test(join_bare)'`, then `set -a; . deploy/dev/.secrets/dev.env; set +a; cargo nextest run -p agmen-cli --locked --profile it --test it_cli -E 'test(it_join) | test(it_invite_and_join)'` (`--test it_cli` builds that target and the `agmen` binary only, not the unit tests that do not compile yet).
Expected: the first fails to compile (`parse_invite` missing). The second runs and the five integration tests fail: `join` refuses a link as an invalid code (`it_join_link_signs_in_accepts_and_sets_up`, `it_join_reuses_the_sign_in_for_the_same_hub`, `it_join_outside_a_worktree_points_to_the_clone`), `agmen join` with no argument is a usage error (`it_join_without_arguments_uses_the_only_project`), and `agmen invite` prints no `They run:` line (`it_invite_and_join`).

- [ ] **Step 3: Implement** `parse_invite`, `human_at`, `join_here`, the flow and the messages as specified.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo nextest run -p agmen-cli --locked`, then `... --profile it -E 'test(it_join) | test(it_invite)'`, then the gate.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-cli
git commit -m "Make agmen join take a link, sign in, accept, and set up agents"
```

### Task 8: Presence chosen per session in agmen-client

**Files:**
- Modify: `crates/agmen-client/src/session.rs:76-120,225-310,380-480,550-604,625-651` and tests `760-800`, `crates/agmen-client/src/lib.rs:30`, `crates/agmen-client/src/link.rs:160-170`
- Test: `crates/agmen-client/src/session.rs` (unit), `crates/agmen-client/tests/it_client.rs`

**Interfaces:**
- Produces:
  - `pub enum Presence { Publish, Skip }` (Debug, Clone, Copy, PartialEq, Eq, Hash), exported from `lib.rs`.
  - `ClientKind::default_presence(self) -> Presence` (`Watch` is `Publish`, the others `Skip`) replaces `publishes_presence`.
  - `SessionConfig::with_presence(self, presence: Presence) -> Self`; a `Cli` session never publishes, whatever it is given.
  - `Session::presence(&self) -> Presence`.
  - `Session::close_quietly(self) -> Result<(), ClientError>`: DISCONNECT with reason 0x00 even for a session that publishes presence, so the broker drops the will and the retained presence stays as it is.

- [ ] **Step 1: Write the failing tests**

```rust
// session.rs
#[test] fn presence_follows_the_config() // Mcp + Publish: will on the presence topic, payload {"status":"offline"}, retained, QoS 1; Mcp default: no will; Watch default: will; Cli + Publish: no will
// it_client.rs
#[tokio::test] async fn it_mcp_session_can_publish_presence()    // Mcp session with Publish: retained presence/<id> == {"status":"online"}; close(): == {"status":"offline"} within WAIT
#[tokio::test] async fn it_quiet_close_keeps_the_presence_online() // Mcp session with Publish; close_quietly(): retained stays {"status":"online"} for 2 s
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p agmen-client --locked -E 'test(presence)'`
Expected: FAIL to compile (`Presence` missing).

- [ ] **Step 3: Implement** the field on `SessionConfig`, its use where `kind.publishes_presence()` is read today (`session.rs:397`), and `close_quietly` beside `close` sharing its body with `close_disconnect(false)`. Update the R65 doc comment at `session.rs:111-117` to say who chooses now.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo nextest run -p agmen-client --locked`, then `... --profile it -p agmen-client -E 'test(presence)'`, then the gate.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-client
git commit -m "Let each agmen-client session choose whether it publishes presence"
```

### Task 9: Presence from `agmen mcp` when no watch runs (amends R65)

**Files:**
- Create: `crates/agmen-cli/src/watch_lock.rs`
- Modify: `crates/agmen-cli/src/cmd/watch.rs:1-61`, `crates/agmen-cli/src/mcp/mod.rs:1-24,55-60,103-165,235-264,405-452`, `crates/agmen-cli/src/lib.rs`, `docs/mcp-setup.md:91`
- Test: `crates/agmen-cli/src/watch_lock.rs` (unit), `crates/agmen-cli/tests/it_mcp.rs`

**Interfaces:**
- Consumes: Task 8 `Presence`, `SessionConfig::with_presence`, `Session::close_quietly`, `Session::presence`.
- Produces:
  - `pub(crate) const LOCK_FILE: &str = "watch.lock"` in the identity's state folder.
  - `pub(crate) struct WatchLock(std::fs::File)`; `WatchLock::hold(state_dir: &Path) -> anyhow::Result<WatchLock>` takes a shared lock (`File::lock_shared`, so two watches can run); `pub(crate) fn watch_running(state_dir: &Path) -> anyhow::Result<bool>` tries an exclusive lock (`File::try_lock`) and releases it at once. Both through `blocking`.
  - `agmen watch` holds a `WatchLock` from before it connects until it exits.
  - `agmen mcp` connects with `Presence::Publish` when `watch_running` is false. Every `PRESENCE_RECHECK = 60 s` it compares: a publishing session that now sees a watch closes with `close_quietly` and reconnects with `Skip`; a skipping session that sees no watch closes normally and reconnects with `Publish`. The reconnect goes through the slot lock tool calls already take.
  - `McpOptions::with_presence_recheck(self, every: Duration) -> Self` (at least 1 s).

- [ ] **Step 1: Write the failing tests**

```rust
// watch_lock.rs
#[test] fn no_watch_without_a_lock()         // fresh dir: watch_running == false
#[test] fn a_held_lock_means_a_watch_runs()  // hold() then watch_running == true
#[test] fn two_watches_share_the_lock()      // hold() twice: both Ok
#[test] fn dropping_the_lock_ends_it()       // hold() dropped: watch_running == false
// it_mcp.rs
#[tokio::test] async fn it_mcp_publishes_presence_without_watch()   // state tool roster: sol's entry has "status":"online"; mcp.stop(): luna reads sol offline within WAIT
#[tokio::test] async fn it_mcp_hands_presence_to_watch()           // recheck 1 s; mcp starts (publishing); spawn `agmen watch --json` for sol; after 3 s mcp.stop(); sol stays online for 2 s
#[tokio::test] async fn it_mcp_takes_presence_back_when_watch_stops() // spawn `agmen watch --json` for sol; mcp starts with recheck 1 s (skipping); stop watch with SIGINT: sol goes offline, then online again within 5 s
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'test(watch_lock::)'`
Expected: FAIL to compile (`watch_lock` missing).

- [ ] **Step 3: Implement** the lock, the watch change, and in `mcp/mod.rs` the presence choice in `connect`, a `presence: Task` on `Connected` running `async fn recheck_presence(shared: Weak<Shared>, every: Duration)`, and the doc changes: `mcp/mod.rs:5-6` and `docs/mcp-setup.md:91` say mcp publishes presence while no `agmen watch` runs for the identity, and why (R65 amended).

- [ ] **Step 4: Run to verify they pass**

Run: `cargo nextest run -p agmen-cli --locked`, then `... --profile it -E 'test(it_mcp) | test(it_watch)'`, then the gate.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-cli docs/mcp-setup.md
git commit -m "Publish presence from agmen mcp while no agmen watch runs"
```

### Task 10: Pending messages at the end of tool results (amends R19)

**Files:**
- Modify: `crates/agmen-cli/src/mcp/mod.rs:21-24,329-343`, `crates/agmen-cli/src/mcp/tools.rs:24-31,156-333`, `crates/agmen-cli/src/cmd/inbox.rs:71-160`, `docs/mcp-setup.md:66-91`
- Test: `crates/agmen-cli/src/cmd/inbox.rs` (unit), `crates/agmen-cli/tests/it_mcp.rs`

**Interfaces:**
- Consumes: Task 9 `watch_running(state_dir)`; `inbox::unread(agent, None)`, `Unread::mark_read`, `message_line_with_id`, `clean`.
- Produces:
  - `pub(crate) const PENDING_MAX: usize = 20` in `mcp/mod.rs`.
  - `pub(crate) fn capped_marks(messages: &[StampedEnvelope], max: usize, marks: &[(Stream, u64)]) -> Vec<(Stream, u64)>`: for each stream with a message past `max`, the mark becomes the smaller of its mark and that stream's first dropped seq minus 1, so a dropped message is listed again next time.
  - `Unread::keep_first(self, max: usize) -> (Unread, usize)` returns the oldest `max` messages and how many were dropped.
  - `Server::respond_with_pending` (same signature as `respond`): every tool except `inbox` uses it. When the work answered and no watch runs, it appends `\n\nfor you (marked read):\n` and one `message_line_with_id` per message, then `<n> more: call inbox` when capped, and marks them read. GRANT and DENY are left out of the block (the claim tool answers them) but still marked read. A failure to read pending is logged at warn and the tool answers without the block. A "not joined" answer never gets one.
  - `INSTRUCTIONS` (`tools.rs:25`) gains: `Messages addressed to you are added at the end of tool results under "for you"; when you have no other agmen call to make, call inbox between steps.`

- [ ] **Step 1: Write the failing tests**

```rust
// inbox.rs
#[test] fn keep_first_keeps_the_oldest()             // 25 messages -> 20 kept in hub_at order, 5 dropped
#[test] fn capped_marks_stop_before_the_first_dropped() // in hub_at order feed seqs 1..=15 then inbox seqs 1..=10, max 20, marks [(Feed, 15), (Inbox, 10)] -> [(Feed, 15), (Inbox, 5)]; with max 30 -> marks unchanged
// it_mcp.rs
#[tokio::test] async fn it_tool_result_carries_pending()    // luna DMs sol "ping"; sol's `lanes` result ends with "\n\nfor you (marked read):\n" then a line with "DM from <luna>" and "ping"; the next `lanes` result has no "for you"; inbox says "no new messages"
#[tokio::test] async fn it_pending_skips_grant_and_deny()   // sol claims B-1 (GRANT); the next `lanes` result has no "for you"
#[tokio::test] async fn it_pending_waits_while_watch_runs() // lock held as in Task 9; luna DMs sol; `lanes` has no "for you"; inbox lists the DM
#[tokio::test] async fn it_pending_capped_at_20()           // 25 DMs; `lanes` shows 20 message lines and "5 more: call inbox"; inbox then lists exactly the other 5
#[tokio::test] async fn it_pending_sanitizes_peer_text()    // DM body with ESC, CR and U+202E: the block holds none of them; the second line sits under "    | "
```

The existing `it_mcp` tests (`it_ask_answer_dm_and_inbox`, `it_found_claim_done_release` and the rest) keep their assertions: messages for the identity there are read with `inbox` first, and GRANT and DENY stay out of the block.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p agmen-cli --locked -E 'test(keep_first) | test(capped_marks)'`
Expected: FAIL to compile.

- [ ] **Step 3: Implement** as specified. Update `docs/mcp-setup.md` ("Every tool result except inbox ends with the messages for you...", and that it stops while `agmen watch` runs, which the Claude Code plugin starts).

- [ ] **Step 4: Run to verify they pass**

Run: `cargo nextest run -p agmen-cli --locked`, then `... --profile it -E 'test(it_mcp) | test(it_pending) | test(it_tool_result)'`, then the gate.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/agmen-cli docs/mcp-setup.md
git commit -m "End agmen mcp tool results with pending messages when no watch runs"
```

## Phase C: open source and release

### Task 11: MIT license and third-party notices

**Files:**
- Create: `LICENSE`, `THIRD_PARTY_NOTICES.md`, `about.toml`, `about.hbs`, `scripts/third-party-notices.sh`
- Modify: `Cargo.toml:16-19` (`license = "MIT"`, `repository = "https://github.com/agmen-dev/agmen"`), `deny.toml:30-32` (comment), `mods/agmen-claude/.claude-plugin/plugin.json` (`"license": "MIT"`, repository), `mods/agmen-claude/package.json` (`"license": "MIT"`), `Jenkinsfile:6-12,29-56`, `README.md:366-368` (the License section, which today says no license is chosen, becomes "MIT; see `LICENSE` and `THIRD_PARTY_NOTICES.md`")

**Interfaces:**
- Produces: `THIRD_PARTY_NOTICES.md` at the root (Task 13 ships it inside every archive); `scripts/third-party-notices.sh [--check]` regenerates it, and `--check` exits 1 with a diff when it is stale; Jenkins Gate runs `scripts/third-party-notices.sh --check`. A later task that adds a dependency to `agmen-cli` or `agmen-hub` (self-host Tasks 5 and 6, hosted Tasks 6 and 14) runs the script and commits the regenerated notice.

`LICENSE` is the MIT text with `Copyright (c) 2026 Andy Aldrich and the agmen contributors`. The notice has two parts:
1. **Rust crates**, generated by cargo-about for the graphs of `agmen-cli` and `agmen-hub` (no dev-dependencies), with each crate's license text. `about.toml` accepts the licenses `deny.toml:12-21` allows, plus `CDLA-Permissive-2.0` for `webpki-root-certs` only, for the five targets of Task 13.
2. **Bundled assets**, hand-written in `about.hbs`: the hub image's base `gcr.io/distroless/cc-debian12` (Apache-2.0); and for the desktop app (`crates/agmen-desktop`, on branch `desktop/gpui` until it merges): IBM Plex Sans (OFL-1.1, `assets/fonts/ibm-plex-sans/license.txt`), Lilex (OFL-1.1, `assets/fonts/lilex/OFL.txt`), Lucide icons (ISC, `assets/icons/LICENSE-lucide.txt`), GPUI (Apache-2.0), and each Zed theme or icon theme file under `tests/fixtures/zed*` with its upstream license, found with `git log --follow` on `desktop/gpui` and the upstream repository. A fixture with no upstream source gets no entry.

- [ ] **Step 1: Ask Andy before installing cargo-about** (not installed here; `~/.claude/rules/rust/rust.md` says to ask). Then install the newest release (`cargo search cargo-about --limit 1` names it) with `cargo install --locked cargo-about --version <that version>`, pin that exact version as `CARGO_ABOUT_VERSION` in the script and the Jenkinsfile environment, and write it in the task report so the plan's open version is closed in writing.

- [ ] **Step 2: Write the script check first:** `scripts/third-party-notices.sh --check` with no notice file exits 1.

Run: `scripts/third-party-notices.sh --check; echo $?`
Expected: `1`.

- [ ] **Step 3: Write** `LICENSE`, `about.toml`, `about.hbs`, the script and the manifest changes, then generate the notice.

- [ ] **Step 4: Verify**

Run: `scripts/third-party-notices.sh --check && echo current`; append a line to the notice, rerun, expect exit 1, restore it; `shellcheck scripts/third-party-notices.sh && shfmt -d scripts/third-party-notices.sh`; `cargo deny check`; `claude plugin validate mods/agmen-claude`; the gate.
Expected: `current`, then 1, no shellcheck or shfmt output, deny and validate pass.

- [ ] **Step 5: Commit**

```bash
git add LICENSE THIRD_PARTY_NOTICES.md about.toml about.hbs scripts/third-party-notices.sh Cargo.toml deny.toml mods/agmen-claude Jenkinsfile README.md
git commit -m "License agmen under MIT and list third-party licenses"
```

### Task 12: Secret scan of the full history

**Files:**
- Create (only if Step 4 finds false positives): `.gitleaksignore`
- Report: `<scratchpad>/gitleaks-history.json` (never committed)

**Interfaces:**
- Produces: a clean `gitleaks git` run over every ref, recorded in the task report with the gitleaks version, the ref count and the commit count. The public repository starts from one squashed commit (Task 15), so only today's tree is published; the spec still asks for a scan of the full history, because a secret committed once may still be live.

- [ ] **Step 1: Fetch every ref:** `git fetch origin` and `git fetch github`, then `git for-each-ref --format='%(refname)' | wc -l` and `git rev-list --all | wc -l` for the report.

- [ ] **Step 2: Scan.**

Run: `gitleaks version` (expect 8.30.1), then `gitleaks git --log-opts="--all" --redact --no-banner --report-format json --report-path <scratchpad>/gitleaks-history.json .`
Expected: exit 0, or exit 1 with findings to triage.

- [ ] **Step 3: Check files that never belong in history.**

Run: `git log --all --oneline -- '*.pem' '*.key' '*.p12' '.env' '.env.*' 'deploy/dev/.secrets/*'`
Expected: no output (the dev keys live under the ignored `deploy/dev/.secrets/`).

- [ ] **Step 4: Triage each finding** by file, commit and rule (the report is redacted; never print a value). A test string built at runtime (R22) or a documented placeholder is a false positive: add its fingerprint to `.gitleaksignore` with a comment line above it naming the file and why. Anything else is a real secret: stop, report the commit, file and rule to Andy, and change nothing; rotating it is his call. No history rewrite is needed, since the public repository gets only the squashed tree, but a secret still in the tree blocks Task 15 until it is removed.

- [ ] **Step 5: Re-run Step 2.**
Expected: exit 0.

- [ ] **Step 6: Commit** (only when `.gitleaksignore` changed)

```bash
git add .gitleaksignore
git commit -m "Record reviewed gitleaks false positives from the full-history scan"
```

### Task 13: Release pipeline with dist and the hub image

**Files:**
- Create: `dist-workspace.toml`, `.github/workflows/release.yml` (generated by `dist init`), `.github/workflows/publish-hub-image.yml`
- Modify: `crates/agmen-cli/Cargo.toml`

**Interfaces:**
- Consumes: Task 11 `LICENSE`, `THIRD_PARTY_NOTICES.md`; `crates/agmen-hub/Dockerfile`.
- Produces:
  - `dist-workspace.toml` `[dist]`: `cargo-dist-version = "0.33.1"`, `ci = "github"`, `targets = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]`, `installers = ["homebrew"]`, `tap = "agmen-dev/homebrew-tap"`, `formula = "agmen"`, `publish-jobs = ["homebrew", "./publish-hub-image"]`, `checksum = "sha256"`, `include = ["LICENSE", "THIRD_PARTY_NOTICES.md"]`, `pr-run-mode = "skip"`, `dist = false`. The shell and PowerShell installers are agmen's own (Task 14), so dist generates none.
  - One rule picks what dist builds: `dist = false` in `[dist]` makes every package opt-in (dist's reference: "`dist = false` on a whole workspace defaults all packages to do-not-distribute"), and only `agmen-cli` opts in with `[package.metadata.dist] dist = true`. No other package carries a dist flag, so `agmen-hub`, `agmen-e2e` (`tests/e2e`, which has a binary), the spikes and the broker-spike plan's `spikes/s11-rmqtt` stay out without edits, and a package added later stays out until it opts in.
  - Release archives: one per target holding `agmen` (or `agmen.exe`), `LICENSE` and `THIRD_PARTY_NOTICES.md`, each with a `<archive>.sha256`. Task 14 reads the archive names `dist plan` prints.
  - `.github/workflows/publish-hub-image.yml`: `on: workflow_call` with required string input `plan`; `permissions: packages: write, contents: read`; checks out the tag; builds `crates/agmen-hub/Dockerfile` for `linux/amd64,linux/arm64`; pushes only `ghcr.io/agmen-dev/agmen-hub:<version>`, where version is `fromJson(inputs.plan).announcement_tag` without its leading `v`; labels `org.opencontainers.image.source=https://github.com/agmen-dev/agmen` and `org.opencontainers.image.licenses=MIT`. Third-party actions are pinned to full commit SHAs with the version in a comment.
  - `release.yml` runs on `push` of version tags only.

- [ ] **Step 1: Ask Andy before installing dist** (`cargo install --locked cargo-dist --version 0.33.1`). The `[dist]` keys above were not checked against 0.33.1's config reference in planning; `dist plan` and `dist init` reject unknown keys, so check each one there and report any that changed name.

- [ ] **Step 2: Write the checks first** and run them on the current tree.

Run: `dist plan` (expect: not configured), `test -f .github/workflows/publish-hub-image.yml` (expect exit 1).

- [ ] **Step 3: Write** `dist-workspace.toml` and the package metadata, run `dist init --yes` to generate `release.yml` (keep the generated file as is), and write `publish-hub-image.yml`.

- [ ] **Step 4: Verify**

Run:
- `dist plan`: lists one app, `agmen`, for the five targets, plus the Homebrew formula; no `agmen-hub`, no `agmen-e2e` and no spike.
- `yq '.on | keys' .github/workflows/release.yml`: only `push`; `yq '.on.push | keys'`: only `tags`.
- `rg -n ':latest' .github/workflows`: no output.
- `yq '.jobs[].steps[] | select(.uses) | .uses' .github/workflows/publish-hub-image.yml`: every value ends in `@<40 hex>`.
- `docker build -f crates/agmen-hub/Dockerfile -t agmen-hub:release-check .`: builds (local only, nothing pushed).
- The gate.
Expected: as listed.

- [ ] **Step 5: Commit**

```bash
git add dist-workspace.toml .github crates/agmen-cli/Cargo.toml Cargo.lock
git commit -m "Build agmen releases with dist and publish the hub image on tags"
```

### Task 14: Installers that check SHA-256

**Files:**
- Create: `install/install.sh`, `install/install.ps1`, `install/test-install.sh`, `install/test-install.ps1`, `.github/workflows/test-installers.yml`
- Modify: `dist-workspace.toml`, `Jenkinsfile:44-56`, `mods/agmen-claude/hooks/register.tsx:119-120`, `mods/agmen-claude/hooks/register.test.tsx:522-536` (`notifies_when_cli_missing`; lines as on `mvp/impl` at 2329e12), `README.md` (an Install section above "Clients"), `docs/mcp-setup.md:5-8`

**Interfaces:**
- Consumes: Task 13 archive and checksum names from `dist plan`.
- Produces:
  - `install/install.sh` (POSIX sh) and `install/install.ps1`, uploaded to every GitHub release as assets named `install.sh` and `install.ps1` by a dist extra artifact in `dist-workspace.toml`: `[[dist.extra-artifacts]]` with `artifacts = ["install/install.sh", "install/install.ps1"]` and `build = ["true"]` (the files exist already; dist's reference: "Every file in this list will be uploaded individually to your release as its own artifact"). Their stable URLs are `https://github.com/agmen-dev/agmen/releases/latest/download/install.sh` and `.../install.ps1`. The hosted plan's site serves `https://getagmen.ai/install.sh` and `install.ps1` by redirecting to exactly those two URLs; no release asset is named `agmen-cli-installer.*`. Variables: `AGMEN_VERSION` (default: the latest release), `AGMEN_INSTALL_DIR` (default `$HOME/.local/bin`; Windows `$env:LOCALAPPDATA\agmen\bin`, added to the user `PATH`), `AGMEN_DOWNLOAD_BASE` (default `https://github.com/agmen-dev/agmen/releases`; a `file:` base is copied, for the tests).
  - Each script picks the archive for the platform (`uname -s` and `uname -m`; an unknown pair exits 1 naming it), downloads the archive and `<archive>.sha256`, computes SHA-256 with `sha256sum` or `shasum -a 256` (`Get-FileHash` on Windows), refuses to install on a mismatch, a missing checksum file or no hashing tool (exit 1, `checksum mismatch` or what is missing, nothing written), then installs `agmen` and prints `agmen <version> installed to <dir>` and, when the dir is not on `PATH`, the line to add it.
  - The plugin's `INSTALL` hint becomes `curl -fsSL https://getagmen.ai/install.sh | sh (Windows: irm https://getagmen.ai/install.ps1 | iex)`.
  - `.github/workflows/test-installers.yml` (`on: workflow_call`, input `plan`) runs `install/test-install.sh` on `ubuntu-24.04` and `macos-14` and `install/test-install.ps1` on `windows-2022`; `dist-workspace.toml` lists it in `plan-jobs` so a failure stops the release before any build. If dist 0.33.1 has no `plan-jobs`, list it first in `publish-jobs`, which still runs before the release is announced, and say so in the task report.

- [ ] **Step 1: Write the failing tests** in `install/test-install.sh` (and the same three cases in `test-install.ps1`), each in a fresh temp dir with a fake release built by the test (a fake `agmen` script that prints `agmen 9.9.9`, archived under Task 13's name, with its `.sha256`):

```sh
installs_a_good_archive     # exit 0; "$AGMEN_INSTALL_DIR/agmen" is executable and prints "agmen 9.9.9"
refuses_a_bad_checksum      # .sha256 for other bytes: exit 1; stderr has "checksum mismatch"; install dir empty
refuses_a_missing_checksum  # no .sha256 file: exit 1; install dir empty
```

Run: `sh install/test-install.sh`
Expected: FAIL (`install/install.sh` missing).

- [ ] **Step 2: Implement** both installers, the workflow and the extra artifact; add `sh install/test-install.sh` to the Jenkins Gate stage after `cargo deny`. If `dist plan` refuses `build = ["true"]` or names the asset by its full path, use the form dist 0.33.1's reference gives for copying a file as is, keep the asset names `install.sh` and `install.ps1`, and say which form in the task report.

- [ ] **Step 3: Update** the plugin `INSTALL` text and `notifies_when_cli_missing` (add `expect(w.notified[0]).toContain('getagmen.ai/install.sh')`), the README Install section (the three install lines, then `agmen join <invite link>` in your clone), and `docs/mcp-setup.md` step 1.

- [ ] **Step 4: Verify**

Run: `sh install/test-install.sh`; `shellcheck install/*.sh && shfmt -d install`; `claude plugin validate mods/agmen-claude && claude plugin test mods/agmen-claude`; `dist plan`; the gate. `install/test-install.ps1` is not run here (no `pwsh` on this machine); it first runs in `test-installers.yml` on the first tag, so the task report says so.
Expected: three passes, no shellcheck or shfmt output, the plugin tests pass, `dist plan` lists `./test-installers` and two release artifacts named `install.sh` and `install.ps1`.

- [ ] **Step 5: Commit**

```bash
git add install .github/workflows/test-installers.yml dist-workspace.toml Jenkinsfile mods/agmen-claude README.md docs/mcp-setup.md
git commit -m "Add shell and PowerShell installers that check SHA-256"
```

### Task 15: The tree for the public repository

**Files:**
- Create: `scripts/public-tree-check.sh`
- Modify: every tracked file the check lists after Tasks 11 and 14 (which fix `Cargo.toml`, `plugin.json`, `register.tsx` and the README's install lines). On 149c67d the rest are `README.md:229,342`, `docs/design.md`, `docs/adr/0001-dev-stack.md:8`, `docs/plans/m0-m1.md`, `docs/plans/mvp.md:268-278`, `docs/specs/2026-10-26-onboarding-and-hosting-design.md`, the four onboarding-and-hosting plans (this one included) and `docs/mockups/scripts/roles.py:179`. Also `docs/mockups/scripts/console.py:740-800` (icons), `docs/mockups/README.md`, and the regenerated `docs/mockups/agmen-{console,roles}.figcraft` and `docs/mockups/{console,roles}/*.webp`.

**Interfaces:**
- Consumes: Task 11 `LICENSE`, the notice and the repository URLs; Task 12's clean history scan; Task 14's install lines; the 27 Lucide SVGs (ISC) at `crates/agmen-desktop/assets/icons/lucide/` on `desktop/gpui`.
- Produces:
  - `scripts/public-tree-check.sh <term>...` (POSIX sh): for each term, a fixed string matched in any case, it prints `<file>:<line>` for every match in tracked files (`git grep -n -i -F`, so ignored files such as `deploy/dev/.secrets/` never count) and `<file>: contains <term>` for every tracked `.figcraft` archive whose unzipped text holds it; it also prints any tracked path under `mods/*/.claude-plugin/types/`. It exits 1 when it printed anything, 0 otherwise. The terms are arguments, never stored in the tree; they are `groupdrive`, `integrally` and `kit_mcp` (a private project's path), plus any Andy adds in Step 1, and the task report records the exact command for Andy's squash.
  - One rewrite per kind of hit: `https://git.groupdrive.xyz/groupdrive/agmen` (with or without `.git`) becomes `https://github.com/agmen-dev/agmen`; `groupdrive/agmen-kubernetes` becomes `agmen-kubernetes` (agmen's private deployment repository); in finished plans and ADRs, GroupDrive's cluster, registry and hosts become "agmen's cluster", "agmen's registry" and drop the host names; any other GroupDrive mention goes with the clause that holds it. "Integrally PR #3331" and the mockup's `integrally · pr-3331` become "a 17-agent PR", the spec's words. A private path goes with the clause that names it. The spec and the four plans get the same rules, including the spec's own publishing sentence and this task's term list, which becomes "the private terms in the task report".
  - Mockups: `console.py` reads every icon from one folder named by `LUCIDE_ICONS` and drops `ZED_ICONS`, `MATERIAL_ICON_THEME` and the Material mapping. UI icons map Zed names to Lucide files: `file_tree` folder-tree, `git_branch` git-branch, `magnifying_glass` search, `terminal` square-terminal, `chat` messages-square, `bell` bell, `settings` settings, `user_group` users, `person` user, `chevron_down` chevron-down, `chevron_right` chevron-right, `sparkle` sparkle, `diff` diff, `pencil` pencil (all 14 are among the 27). File icons are `folder` and `folder-open` for folders and `file` for every file. The screens' words about following Zed themes stay; only the icons change. `docs/mockups/README.md` says the icons are Lucide (ISC) and its Rebuild section uses `LUCIDE_ICONS`.

- [ ] **Step 1: Ask Andy** whether any other name or text counts as private. The spec names GroupDrive and Integrally; people's first names appear in the tree as test handles and are left alone unless he says otherwise.

- [ ] **Step 2: Write the check and run it on the current tree.**

Run: `sh scripts/public-tree-check.sh groupdrive integrally kit_mcp; echo $?`
Expected: lines for the files listed above, including `docs/mockups/agmen-roles.figcraft: contains integrally`, then `1`.

- [ ] **Step 3: Reword** every hit by the rules above.

- [ ] **Step 4: Rebuild the mockups** with the Lucide folder taken from `desktop/gpui` without a checkout, and the scripts' JSON and icon cache kept out of the tree:

```bash
mkdir -p <scratchpad>/lucide
git archive desktop/gpui crates/agmen-desktop/assets/icons/lucide | tar -x -C <scratchpad>/lucide
cd docs/mockups
LUCIDE_ICONS=<scratchpad>/lucide/crates/agmen-desktop/assets/icons/lucide uv run scripts/console.py <scratchpad>/console.json
figcraft-cli run <scratchpad>/console.json --out agmen-console.figcraft
figcraft-cli export agmen-console.figcraft -o console --format webp
uv run scripts/roles.py <scratchpad>/roles.json
figcraft-cli run <scratchpad>/roles.json --out agmen-roles.figcraft
figcraft-cli export agmen-roles.figcraft -o roles --format webp
```

A preview the export no longer writes is deleted.

- [ ] **Step 5: Verify**

Run: `sh scripts/public-tree-check.sh groupdrive integrally kit_mcp; echo $?`; `shellcheck scripts/public-tree-check.sh && shfmt -d scripts/public-tree-check.sh`; `rg -n 'ZED_ICONS|MATERIAL_ICON_THEME' docs/mockups`; look at `console/desktop-project.webp` and `console/desktop-settings.webp` with the Read tool; the gate.
Expected: `0` with no other output, no shellcheck, shfmt or rg output, Lucide icons in both previews (not dots or squares), the gate passes.

- [ ] **Step 6: Commit**

```bash
git add scripts/public-tree-check.sh README.md docs
git commit -m "Remove private names from the tree and redraw the mockups with Lucide icons"
```

- [ ] **Step 7: Scan the exact tree the squash publishes**

Run: `mkdir -p <scratchpad>/public-tree && git archive --format=tar HEAD | tar -x -C <scratchpad>/public-tree`, then `gitleaks dir --redact --no-banner <scratchpad>/public-tree`.
Expected: exit 0. A finding is triaged as in Task 12 Step 4.

---

## Spec coverage

| Spec item | Task |
|---|---|
| Decision 1: one client, two kinds of hub (links carry the hub address; nothing branches on hub kind) | 2, 7 |
| Decision 2: device flow run by the hub, shared client id, secret and web callback removed, 900 s, interval and `slow_down` | 1, 1b |
| Decision 3: `https://<hub>/i#<code>`, the `/i` page and its headers, `agmen invite` prints and copies the link | 2 |
| Decision 4: `agmen join <link>` signs in, accepts, writes the identity, runs setup, prints next steps; outside a worktree; `--project` default; `login --hub` stays | 1, 3, 7 |
| Decision 5: `agmen setup` for Claude Code, Codex, OpenCode, Cursor, Gemini CLI; pending in tool results; presence from mcp; lane guard note | 4, 5, 6, 8, 9, 10 |
| Decision 6: dist 0.33.1 binaries for five targets, shell, PowerShell and Homebrew installers with SHA-256 (installers uploaded as release assets for `getagmen.ai` to redirect to), pinned hub image, public releases | 13, 14 |
| Decision 6: the self-host broker image with a pinned tag | none: upstream `emqx/emqx:6.3.2`, pinned by `self-host.md`; waits on Andy's ruling below |
| Decided 1: MIT `LICENSE`, third-party notice, full-history secret scan | 11, 12 |
| Decided 1, publishing: private text removed, Integrally reworded, repository URLs at the public repository, mockups redrawn with Lucide icons, the published tree scanned | 11, 14, 15; the squashed commit, its author and the push are Andy's (below) |
| Decided 2 and 4: `getagmen.ai`, `agmen-dev` names in links, installers, images and manifests | 2, 11, 13, 14 |

Not in this plan: the seat check at invite acceptance (decision 3) needs seats and is built in `hosted.md`; `AGMEN_HUB_MODE`, tenant roles and project creation are `self-host.md`; the static site whose `_redirects` sends `/install.sh` and `/install.ps1` to Task 14's release assets is `hosted.md`.

## Andy's actions this plan waits on

- Create the `agmen-dev` organization, its two GitHub OAuth apps with "Enable Device Flow" and no scopes; put the self-host app's client id into `DEFAULT_GITHUB_CLIENT_ID` (a one-line follow-up commit) and the hosted app's id into the hosted deployment's `AGMEN_GITHUB_CLIENT_ID`.
- Create `agmen-dev/homebrew-tap` and the `HOMEBREW_TAP_TOKEN` repository secret before the first tag.
- Approve installing cargo-about (Task 11) and dist 0.33.1 (Task 13).
- Rule on decision 6's broker image: either self-host's pinned upstream `emqx/emqx:6.3.2` meets "the broker image are published with pinned version tags" and the spec says so, or agmen republishes a broker image, which needs EMQ's answer first (or the broker spike's embedded broker, which would need no separate image). Until then no task here publishes a broker image.
- Decide on any real secret Task 12 or Task 15 finds.
- After Task 15, publish: rerun the check command from Task 15's report (exit 0), create the public `agmen-dev/agmen`, make one commit from `main`'s tree authored with his GitHub no-reply address, push only that commit (no branch of the private repository, and never `mvp/t14`), keep `AndyAld/agmen` private, then push the first version tag.
