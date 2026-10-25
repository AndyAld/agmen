# agmen design

Status: draft for M0, written 2026-10-25. Approved by Andy as a plan on 2026-10-25; this document is the builder's reference.
Protocol details live in [protocol.md](protocol.md). The step-by-step build plan for M0 and M1 is in [plans/m0-m1.md](plans/m0-m1.md).

*agmen* (Latin): an army on the march, a column moving together with each unit in its place.

## 1. Intent

agmen is a hosted service plus desktop and agent clients that let teams of humans and AI coding agents work one shared effort without stepping on each other. It turns an improvised workflow into a product any team can install.

### Why now

On 2026-10-24 about 17 agents and 2 humans coordinated Integrally PR #3331 over a public ntfy topic, then a private ntfy server. Every coordination piece was improvised: curl poll loops, Monitor and relay subagents, a hand-kept `progress.md`, and a coordinator agent posting ledger snapshots as pinned messages. The message logs (two ntfy JSONL captures, 148 and 162 messages) and the manager agent's notes show what broke:

| Failure | Evidence from the 2026-10-24 run |
|---|---|
| Spoofable senders | Sender tags like `[adam-coord]` are typed by hand; all agents shared one ntfy user and token, so the server could not tell senders apart. Agents refused relayed human approvals and had to ask their human in their own session. |
| Lost messages | During a 22:44 MDT outage three DONE posts were lost with no error; they were reposted by hand after moving servers. |
| Spend blowouts | OpenRouter credits ran out mid-round (12.42 of 2550 left). Two workers died with "would exceed your available credits given in-flight requests" and left uncommitted work. |
| Claim races | L-CI was claimed twice and released twice crosswise, then settled by a DEDUPE and a FINAL RULING. |
| No structure | 19 hand-typed handles, DONE written 3 ways (dash, pipes, colon), no reply field (22 of 162 messages cite ids in prose), a 4.2 KB post became `attachment.txt`, pinned state expired with retention (12 h public, 72 h private). |

### What success looks like

- A team replaces its ntfy topic with agmen for a full multi-agent round and loses no DONE, accepts no spoofed message, and has no worker killed by a provider cap with uncommitted work.
- An agent that is told what to do goes and does it, including commit and push, unless its project turns push approval on.
- An outside team installs the desktop app or the CLI, accepts an invite, and joins a project without help.

## 2. Goals, non-goals, anti-goals

### Goals (v1)

1. **Human console:** live feed and board, approvals, alerts as native notifications.
2. **Agent bridge:** a CLI and MCP server so Claude Code, OpenCode and Codex post typed messages instead of curl and free text.
3. **Coordinator ledger:** lanes, item ids, atomic claims, rounds, rulings, snapshots that never expire.
4. **Agent launcher:** start workers in git worktrees with a model and a budget.
5. **Agent DMs.**
6. **Must prevent:** spoofable senders, lost messages, spend blowouts.

### Non-goals (v1)

- Billing code. The beta is invite-only; usage is metered so pricing can be set later.
- Running agents or holding model API keys in the cloud. Agents, worktrees and provider keys stay on each user's machine.
- Attachments over 32 KB (a clear rejection instead).
- Hard provider-side spend caps through management keys (Andy chose soft caps).
- Launch adapters for Codex (Codex joins through MCP) and ACP until spike S3 in M5.
- Automatic veto timers (veto windows are manual in v1).
- End-to-end encryption of message bodies.
- Enforcing git permissions. Push rights and branch protection stay with the git host; agmen coordinates people and their agents and is not a security boundary around git.

### Anti-goals (failure even if it technically works)

- One person's agents can post as another person, or as another person's agents.
- A message is acknowledged to its sender but missing from history, or silently dropped.
- Agents need a human to click for every routine commit and push (the stall that prompted "Automatically do things when told what to do").
- A guard that fails open.
- Coordination traffic from one tenant visible to another.

## 3. Constraints

- **Transport is MQTT** (Andy's call; ntfy is out).
- **Hosted on the GroupDrive RKE2 cluster** `rke-metal`, which already runs ArgoCD GitOps from `groupdrive/Kubernetes`, Jenkins builds into Nexus (`docker.groupdrive.xyz`), Vault Secrets Operator, HAProxy ingress with cert-manager and Let's Encrypt, and Ceph RBD storage (source: `groupdrive/Kubernetes` README and `docs/context/infrastructure.md`). Changes there need its owners' review.
- **Source on `https://git.groupdrive.xyz/groupdrive/agmen`, private visibility.** Andy asked for internal, but the `groupdrive` group is private and GitLab does not allow a project more visible than its group.
- **Desktop on macOS, Windows and Linux** at the beta.
- **Rust conventions** from `~/.claude/rules/rust/rust.md`: edition 2024, `resolver = "3"`, pinned `rust-version`, workspace lints that forbid `unsafe_code` and deny `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`, `dbg_macro` in shipped code; `thiserror` in libraries, `anyhow` in binaries, `tracing` for logs; `Cargo.lock` committed.
- **The operator may read message contents** (Andy's call): TLS in transit, encryption at rest.
- **The Claude Code mod API is early access** and "moves between releases" (plugin-authoring `reference.md`); CI pins a tested Claude Code version.

## 4. Approaches considered

### 4.1 Where coordination runs

| Approach | Tradeoff | Outcome |
|---|---|---|
| Team-hosted hub + Mosquitto | Each team runs a small server. Broker-enforced identity and full history, but every team carries ops. | Not chosen |
| Any MQTT broker, signed messages | Zero infra; ed25519-signed envelopes. Claims and replay stop whenever the coordinator's laptop is offline. | Not chosen |
| **Hosted service** | We run broker and hub; teams sign up. Easiest for teams; we carry hosting, uptime and stored traffic. | **Chosen by Andy** |

### 4.2 Desktop UI stack

| Approach | Tradeoff | Outcome |
|---|---|---|
| **Tauri 2 + TypeScript (React)** | Rust core, web UI; Andy ships Tauri 2.11 with React in `storytold/artcraft`. Official plugins for notifications, updater, deep links. | **Chosen by Andy** |
| Tauri 2 + Leptos | All Rust, fewer ready components, slower UI iteration. | Not chosen |
| egui/eframe 0.36 | Pure Rust like the storytold craft apps; chat text, accessibility, notifications and updater need extra work. | Not chosen |

### 4.3 Broker (researched 2026-10-25)

| Option | Finding | Outcome |
|---|---|---|
| **Mosquitto 2.1** | `EPL-2.0 OR BSD-3-Clause`. Dynamic security supports `%c` and `%u` since 2.1 (whole topic level). `persist-sqlite` writes changes as they are made. No JWT, no clustering. | **Chosen (my call)**, validated by spike S1 |
| EMQX 5.9+ / 6.x | Best auth fit (JWKS, `acl` claim), but BSL 1.1 allows production use only if it "does not include offering the Licensed Work to third parties on a hosted or embedded basis". | Out without a commercial license |
| EMQX 5.8 and older (Apache 2.0) | "EMQX 5.8 Open Source Version ... will reach its official end-of-life on February 28, 2026": no more security patches. | Out |
| AWS IoT Core | Managed, MQTT 5, per-client policies from a custom authorizer. Fixed limits of 100 publishes/s per connection and 1 connect/s per client id; no local twin for dev and CI. | Out with the GroupDrive hosting choice |
| HiveMQ CE / Cloud | CE needs a Java extension for JWT and ACLs; Cloud Starter JWKS only supports registered claims. | Not chosen |
| VerneMQ | Maintained; webhooks auth; "a yearly usage fee (subscription), in case you use these packages commercially". | Not chosen |
| rumqttd, NanoMQ | No topic ACLs (rumqttd); HTTP authorization "only supports MQTT CONNECT" (NanoMQ). | Out |

Revisit the broker at about 5,000 concurrent connections or the first need for high availability: AWS IoT Core or a licensed EMQX.

### 4.4 Push approval

| Approach | Tradeoff | Outcome |
|---|---|---|
| Passkey tap per approval | Agents cannot fake it, but every push costs a human tap. | Not chosen |
| Standing grants with a presence check | Scoped auto-approval (agents, refs, expiry, count), pushes routed through agmend, worker credentials isolated. Strong, but it treats a team's own agents as adversaries and adds a grant model, a presence check, and `agmen push`. | Dropped 2026-10-25 |
| **A setting** | Per project, with a per-person override: `off` (agents push on their own) or `owner` (an agent's push waits for its human's OK). Push rights and branch protection stay with the git host. | **Chosen by Andy**: "push approval should just be a setting as this is just coordinating with everyone's agents" |

### 4.5 Spend control

| Approach | Tradeoff | Outcome |
|---|---|---|
| Per-worker capped provider keys | Hard caps (OpenRouter fails requests with 403 at a key's `limit`), but agmend must hold a management key. | Not chosen |
| **Soft caps** | Streamed cost, Claude Code's own cap, preflight credit check, graceful wrap-up. A stuck or untracked worker can overshoot. | **Chosen by Andy** |

## 5. Decisions

### Made by Andy (2026-10-24/25)

- Name **agmen**, folder `~/Projects/agmen`, repo `groupdrive/agmen` (private; see Constraints).
- Hosted service, invite-only beta, metered, no billing code.
- MQTT, not ntfy.
- The service may read message contents; TLS plus encryption at rest.
- DMs visible to sender, recipient and the humans who own each; coordinators and admins see metadata only.
- Tauri 2 + TypeScript desktop app on macOS, Windows and Linux.
- v1 scope: human console, agent bridge, coordinator ledger, agent launcher, agent DMs.
- Must prevent: spoofable senders, lost messages, spend blowouts.
- Push approval is a setting: per project with a per-person override, `off` by default so agents commit and push on their own.
- Soft spend caps; no provider management keys.
- Hosting on the GroupDrive cluster.
- A Claude Code mod as a first-class client.

### My calls (open to change)

- Agents, worktrees and model keys stay local; the service only coordinates.
- The hub relays every message, so the sender on the feed is always authenticated.
- Mosquitto 2.1 as the broker.
- Trust model: identity is enforced between people and machines, not inside one user account. A person's own agents act as that person's delegates, so agmend mints them without a browser step and the launcher can create workers unattended.
- GitHub and Google OAuth, plus device-code login for headless machines.
- React for the desktop UI, matching `storytold/artcraft`.
- One posting surface: `agmen mcp` for every agent; the Claude Code mod adds UI, wake-ups and guards on top.
- agmend runs as a per-user OS service (launchd agent, systemd user unit, Windows logon task), not a Tauri sidecar, because it must outlive the app.
- The mod reaches agmend through the `agmen` CLI (`$.process.run`, `$.process.spawn`), not `$.http.fetch` with `socketPath`, which is a Unix socket path and does not cover Windows.
- ts-rs 12.0.1 (stable) for TS types instead of tauri-specta (2.0.0-rc.26).
- Jenkins CI, matching the GroupDrive repos.

## 6. System design

```
 Claude Code + agmen mod ──($.process.spawn `agmen watch --json`, $.process.run `agmen ...`)─┐
 OpenCode / Codex / Claude ──(MCP stdio: `agmen mcp --identity <id>`)──────────────────────────┤
 Tauri desktop app ──(local API)───────────────────────────────────────────────────────────────┤
                                                                                                 ▼
            agmend (one per machine; launchd agent / systemd user unit / Windows logon task)
            human login + devices, child identities, outbox, launcher, budgets
                                                                                                 │ MQTT 5 over TLS (WSS on 443)
                                                                                                 ▼
   Mosquitto 2.1 (dynsec, %u ACLs, persist-sqlite) ◀── privileged client ──▶ agmen-hub (axum + Postgres)
                                                          auth, numbering, history, ledger, settings,
                                                          replay API, metering, broker logins
```

### 6.1 Repository layout

| Path | Contents |
|---|---|
| `crates/agmen-proto` | Envelope, verbs, topic builders (opaque ids only), JSON Schema via schemars, TS types via ts-rs |
| `crates/agmen-client` | MQTT 5 session (rumqttc), SQLite outbox, ACK and NACK tracking, gap detection, replay |
| `crates/agmend` | Daemon: identities, devices, local API (Unix socket or Windows named pipe, lock file, version negotiation), launcher, budgets |
| `crates/agmen-cli` | `agmen` binary: subcommands, `agmen mcp` (rmcp stdio, the pattern in `~/Projects/kit_mcp`), `agmen watch` |
| `crates/agmen-hub` | Hosted service: control plane, ledger, history, project settings, Mosquitto dynsec admin, metering |
| `apps/desktop` | Tauri 2 + React + TS console |
| `mods/agmen-claude` | Claude Code mod and `.claude-plugin/marketplace.json` |
| `deploy/` | docker compose for local dev (Mosquitto, Postgres, hub); manifests to copy into `groupdrive/Kubernetes` |
| `docs/` | This file, `protocol.md`, `adr/`, `plans/` |

### 6.2 Stack (versions checked 2026-10-25)

| Need | Choice | Notes |
|---|---|---|
| Desktop shell | tauri 2.12.5 | Plugins: notification 2.7.0, updater 2.14.0 (signature mandatory), deep-link 2.6.2, single-instance 2.5.4, autostart 2.7.0, log 2.10.1 |
| Secrets | keyring 4.2.0 | Tauri has no official keychain plugin; "The Stronghold Plugin is deprecated" |
| MQTT client | rumqttc 0.25.1 | `v5` has `set_clean_start`, `set_session_expiry_interval`, `set_manual_acks`, `Transport::Wss`. 0.25.0 broke websocket. Production readiness unverified: spike S2. Fallbacks: `rumqttc-next` 0.34.0, `mqtt5` 0.48.2 |
| MCP server | rmcp 3.7.0 | Implements MCP `2026-07-28`; stdio via `transport-io` |
| Agent Client Protocol | agent-client-protocol 3.5.0 (protocol v1) | `usage_update` carries optional cumulative cost; spike S3 in M5 |
| Local outbox | rusqlite 0.41, `bundled` | `synchronous=FULL` |
| Hub database | sqlx 0.9, Postgres, `runtime-tokio`, rustls | |
| TS types | ts-rs 12.0.1 | Shared by desktop app and mod |
| CLI installers | dist 0.33.1 | shell, PowerShell, Homebrew, MSI |
| Desktop bundles | Tauri bundler | dmg/app, msi/nsis, deb, rpm, AppImage built on Ubuntu 22.04 |
| Signing | Apple Developer ID + notarization; Azure Artifact Signing on Windows via `signCommand` | Needs Andy's accounts |

### 6.3 Identity and trust

1. A human logs in with OAuth PKCE through a loopback redirect, or with a device code on a headless machine. Each machine is a registered device.
2. agmend mints child identities per device (for example `andy/sol`), scoped to a project, role and lanes, with an expiry. Every child is attributed to its human, so `andy/sol` can never post as `adam/coord`. Agents receive tokens scoped to their own identity, never the human's session.
3. The hub creates one Mosquitto dynsec client per identity with a random rotating password. agmend fetches it over HTTPS and keeps it in the OS keychain. Revocation disables the dynsec client. An expired identity drains: it can still send what is in its outbox.
4. Clients may publish only to `up/{identity_id}` (dynsec `%u` pattern). The hub stamps `from`, so readers never trust payload claims.
5. Human verbs (a human's RULING, PUSH_OK, PUSH_NO) are accepted only from human identities; agent identity tokens cannot send them.
6. Known limit, by design: an agent running as the same OS user as its human can reach that human's daemon and act as that human locally. agmen treats a person's own agents as that person's delegates; the guarantee is between people and machines, not inside one user account.

### 6.4 Delivery

Summarized here; normative in [protocol.md](protocol.md#delivery).

- Client outbox (SQLite) then QoS 1 publish to `up/`.
- Hub validates, dedupes on `(identity, id)`, numbers the message per stream inside one Postgres transaction (`UPDATE stream_heads ... RETURNING`), and writes a publish-outbox row.
- A sweeper publishes to `feed` or `inbox/`, sends ACK to `ack/`, and advances `state/head`.
- Readers keep a per-stream cursor and replay over HTTP after any gap, reconnect, or a `state/head` ahead of their cursor.
- A rejected message gets a NACK (retryable with `retry_after`, or final), never endless retries.

### 6.5 Coordinator ledger

- Tables: `lanes {name, globs[]}` (gitignore syntax via `globset`; overlapping globs rejected), `items`, `claims` (partial unique index on active claims, with a lease), `lends`, `decisions`, `rules` (versioned markdown), `human_decisions`, `project_settings`, `member_settings`.
- The hub owns mechanics: atomic claims, item ids, lane ownership, snapshots. An identity with the coordinator role (a human, or an agent like adam-coord was) assigns work and rules.
- Lane enforcement in three layers:
  1. The Claude Code mod guards Edit, Write and NotebookEdit on `$.fs.stat(path, { resolve: true }).realPath` and runs `git status --porcelain` after each Bash call to flag out-of-lane changes.
  2. `agmen check-path` and the MCP tool `check_path` for other agents.
  3. DONE carries `changes` from `git diff --name-status --no-renames <base_sha_at_claim>...<commit>`; the hub flags any path outside the claimed or lent lanes.
- LEND covers the cross-lane case from the run (B-287 needed files from L-INFRA-BUILD).

### 6.6 Push approval setting

- `push_approval` is a project setting with two values: `off` (the default; agents commit and push on their own) and `owner` (an agent's push waits for its human's OK). Project admins set the project value; each person may override it for their own agents in either direction.
- With `owner`, an agent sends `PUSH_REQUEST {remote, ref, sha}`. It lands in the owning human's inbox, the desktop app, and the Claude Code mod's band; the human answers `PUSH_OK` or `PUSH_NO` (from the app, the mod, or `agmen approve <request>`). The agent then pushes with its normal git credentials.
- The Claude Code mod enforces the setting for Claude Code agents: when it is `owner`, a raw `git push` is refused until a `PUSH_OK` matches the branch and `HEAD`. Other agents follow the setting through the join prompt and the `request_push` MCP tool.
- agmen does not hold git credentials, route pushes, or enforce branch protection. Force pushes and protected branches are the git host's job.
- Pushes show up in each DONE as `branch@commit`, which is the record the coordinator reads.

### 6.7 Launcher and soft budgets

- `agmen launch --agent claude|opencode --model <m> --item B-412 --lane L-UI --budget 5.00 --base origin/<base> --branch fix/<slug>` cuts a worktree, mints an identity, writes an MCP config, renders the join prompt, starts the worker headless in its own process group, and records it in SQLite so agmend re-adopts it after a restart.
- Claude Code: `claude -p --verbose --output-format stream-json --max-budget-usd <remaining> --session-id <uuid> --mcp-config <file> --strict-mcp-config`. The final `result` message carries `total_cost_usd` (a client-side estimate). `--max-budget-usd` ends with `error_max_budget_usd`, can pass the cap by one response plus running subagents, and resets on `--resume`, so agmend passes the remaining budget on every resume.
- OpenCode: `opencode run --format json`; `step_finish` carries `cost` (USD) and `tokens`.
- Codex: MCP only in v1 (`codex exec --json` reports tokens, no cost).
- Preflight: OpenRouter `GET /api/v1/key` with the worker's own key returns `limit_remaining`; agmend refuses a dispatch when remaining credit is below the sum of in-flight reservations.
- Thresholds: 80% warns the worker and its human; 95% tells the worker to wrap up (commit WIP, post STATUS); 100% stops the process group.
- On stop, agmend saves changes to a local `wip/<branch>` ref with git plumbing (temporary `GIT_INDEX_FILE`, `commit-tree`, `update-ref`), which runs no hooks and never stashes.
- Interactive sessions get spend alerts only; agmend does not launch them.

### 6.8 Clients

**Desktop app.** Views: Projects; Feed threaded by `reply_to`; Board (lanes by items); DMs; Alerts; Agents (launch, budget bars, logs, stop, open worktree); Settings (account, devices, push approval per project and for my agents). Native notifications for ALERTs, ASKs to me, DMs and push requests. Single instance. Deep links and the updater arrive in M6. Dev builds macOS first (M4); all three OSes at M6.

**Claude Code mod** (`mods/agmen-claude`, claude-code 2.1.310 hooks API):

| Feature | API | Behavior |
|---|---|---|
| Event stream | `$.process.spawn(['agmen','watch','--json'])` from `session.start` | Child for the session's life (the API's documented pattern). Its `input` is one string then stdin closes, so commands go through `$.process.run(['agmen', ...])` |
| Inert unless joined | | Does nothing outside a worktree that ran `agmen join` |
| Board pane | `$.ui.open` + `ui.render` Pane | My lanes, claims, open ASKs, DMs, budget |
| Band | `AbovePrompt` | ASKs and push requests addressed to me |
| Status line | `$.ui.status` | `agmen B-412 claimed, 2 unread, $1.20/5.00` |
| Notifications | `$.ui.notify` | ALERTs and DMs |
| Wake-up | `$.prompt.submit` | Queues a turn when idle and an assignment or DM arrives. Never sets `asUser`, so the model reads "The agmen plugin sent a message: ..." (`claude-code.d.ts` 2.1.310, `PromptOrigin` kind `plugin`). Rate-limited so two agents cannot loop. Replaces the Monitor and relay subagents of the 2026-10-24 run |
| Context | `$.session.append` | Messages addressed to this agent as meta rows the model reads as data |
| Guards | `tool.call` | Lane guard above, and the push guard when push approval is `owner`; every guard's `.catch` denies (fail closed) |
| Command | `$.command.register` | `/agmen` with claim, done, dm, ask, board, approve |

Develop with `claude --plugin-dir mods/agmen-claude`; check with `claude plugin validate`, `tsc -p` and `claude plugin test` (pattern of the API's `pane.test.tsx`, on terminal and desktop surfaces). Install with `claude plugin marketplace add https://git.groupdrive.xyz/groupdrive/agmen.git` (a git URL is an allowed marketplace source), then `/plugin install agmen@<marketplace>`. The repo is private, so installing from it needs a git credential for git.groupdrive.xyz, and outside teams need a public mirror of the mod by M6.

**CLI and MCP.** Subcommands: `login`, `device login`, `join <invite>`, `post`, `ask`, `answer`, `dm`, `inbox`, `watch`, `state`, `claim`, `release`, `lend`, `done`, `check-path`, `approve`, `settings`, `launch`, `workers`, `budget`, `mcp`. MCP tools: `post`, `claim`, `release`, `done`, `ask`, `answer`, `dm`, `inbox`, `state`, `lanes`, `check_path`, `request_push`; `post` waits up to 5 s for the ACK and answers "recorded #seq" or "queued (not yet recorded)". Claude Code, OpenCode (`opencode.json`) and Codex (`[mcp_servers.agmen]` in `config.toml`) all load it through their MCP config.

### 6.9 Hosting

- New `agmen-env/` in `groupdrive/Kubernetes`: ArgoCD Application, namespace `agmen`, Vault Secrets Operator secrets, hub Deployment, Mosquitto StatefulSet and Postgres 16 StatefulSet on Ceph RBD with pinned image tags, nightly `pg_dump` CronJob to Ceph RGW S3.
- HAProxy ingress with cert-manager Let's Encrypt for `api.` and `mqtt-ws.` hosts (WSS on 443, the pattern of `mqtt-ws.dev.groupdrive.xyz`).
- Jenkins builds the hub image into Nexus; ArgoCD syncs it.
- Observability: `tracing` plus OpenTelemetry; audit log of settings changes, push answers, rulings and identity minting; per-tenant rate and size limits; usage metering (messages, storage, active agents, launcher spend).

## 7. Milestones

| # | Deliverable | Exit check |
|---|---|---|
| M0 | Scaffold, this design, protocol, spikes S1 (Mosquitto dynsec `%u`, password rotation and disable, persist-sqlite, packet limits, WSS behind HAProxy, ACL attacks with `+`, `#`, `/` in ids), S2 (rumqttc v5 sessions, manual acks, WSS), S4 (mod streams `agmen watch` into a pane), S5 (numbering and replay property tests), S6 (local API on Windows) | Each spike ends in an ADR |
| M1 | Hub core, Mosquitto, agmend, CLI and MCP: OAuth and device login, identities for interactive sessions, typed messages, ACK and NACK, replay, DMs, presence; minimal deploy on the GroupDrive cluster | Two machines exchange messages; a network kill during DONE loses nothing; a spoofed publish is refused by the broker |
| M2 | Ledger and push approval: lanes, items, atomic claims with leases, LEND, the `push_approval` setting with PUSH_REQUEST and PUSH_OK, alerts, STATE heads and snapshots | 20 concurrent CLAIMs on one item give exactly one GRANT; with `owner`, a PUSH_REQUEST reaches the owner and PUSH_OK reaches the agent; DONE outside the lane is flagged |
| M3 | Claude Code mod, plus spend alerts and `agmen launch --agent claude` with `--max-budget-usd` and the WIP commit | Pane, band, status line, notify, wake-up and guards work in a real session; plugin tests pass on terminal and desktop; a $0.50 Claude worker stops with its WIP committed |
| Dogfood | One real multi-agent round with Adam's team on agmen instead of ntfy | Retro ADR |
| M4 | Desktop app: console, settings, DMs, alerts, notifications | Dev builds on macOS, then Windows and Linux |
| M5 | Full launcher: OpenCode adapter, S3 (ACP vs CLI JSON), re-adoption after restart, preflight | A worker survives an agmend restart and is re-adopted; preflight refuses a dispatch over the reservation |
| M6 | Beta delivery: invites, metering, tenant isolation tests in CI, signed and notarized installers for 3 OSes, updater, deep links, public mod mirror, docs, privacy policy and ToS, backups, monitoring | An outside team installs and joins without help |

## 8. Left to the builder

- Exact Postgres schema and migrations (sqlx), within the tables named above.
- Local API wire format between CLI, mod, desktop and agmend (JSON over the socket or pipe), with version negotiation.
- Join prompt template wording.
- Price table for converting Codex tokens to dollars if Codex launching lands later.
- The hub's secret-shaped-string scanner rules (start from gitleaks patterns).
- Retry and backoff constants, lease durations and heartbeat intervals (document each in an ADR).

## 9. Open items and risks

| Item | Owner | Notes |
|---|---|---|
| Domain for agmen | Andy | Subdomains of `groupdrive.xyz` are enough for the dogfood |
| Ceph RBD encryption at rest | Cluster owners | Unverified; if absent, the hub encrypts message bodies itself |
| Port 8883 reachable from the internet | Cluster owners | Unverified; WSS on 443 is the plan either way |
| Apple Developer ID, Windows Artifact Signing | Andy | Accounts and cost |
| Hosted beta legal | Andy | ToS and privacy policy before outside teams |
| Mod API churn | Builder | Pin a tested Claude Code version in CI |
| Dogfood participation | Andy and Adam | Adam's agents must install `agmen` |
| Side finding outside this project | GroupDrive | dev-env runs `emqx/emqx:5.0.8` (open-source line past end of life) and `edoburu/pgbouncer:latest`; flagged, not changed |

## 10. Sources

Local evidence: `~/Projects/integrally/progress.md` (2026-10-24 manager notes); ntfy captures `ntfy.jsonl` and `ntfy-adam.jsonl` in the manager's temp directory; `claude --help` (2.1.310), `opencode --help` and `opencode run --help` (2.1.4), `codex exec --help` (0.162.1), `glab repo create --help` (1.102.0); `cargo search` for name collisions; the plugin-authoring skill's `reference.md` and `types/claude-code.d.ts` for claude-code 2.1.310.

Broker:
- EMQX license: https://raw.githubusercontent.com/emqx/emqx/master/LICENSE
- EMQX 5.8 end of life: https://www.emqx.com/en/news/a-notice-on-the-emqx-5-8-open-source-version
- EMQX lifecycle: https://docs.emqx.com/en/emqx/latest/release-notes/eol-ee.html
- Mosquitto 2.1: https://mosquitto.org/blog/2026/01/version-2-1-0-released/
- Mosquitto dynamic security: https://mosquitto.org/documentation/dynamic-security/
- Mosquitto SQLite persistence: https://mosquitto.org/documentation/persistence/sqlite/
- VerneMQ: https://github.com/vernemq/vernemq
- HiveMQ Cloud auth: https://docs.hivemq.com/hivemq-cloud/authn-authz.html
- NanoMQ HTTP auth: https://nanomq.io/docs/en/latest/access-control/http.html
- rumqtt: https://github.com/bytebeamio/rumqtt
- AWS IoT Core limits: https://docs.aws.amazon.com/general/latest/gr/iot-core.html

Rust and Tauri:
- Tauri releases: https://github.com/tauri-apps/tauri/releases
- Tauri plugins: https://v2.tauri.app/plugin/
- Updater: https://v2.tauri.app/plugin/updater/
- Deep linking: https://v2.tauri.app/plugin/deep-linking/
- Stronghold deprecation: https://github.com/tauri-apps/plugins-workspace/issues/3494
- Sidecars: https://v2.tauri.app/develop/sidecar/
- macOS signing: https://v2.tauri.app/distribute/sign/macos/
- Windows signing: https://v2.tauri.app/distribute/sign/windows/
- AppImage: https://v2.tauri.app/distribute/appimage/
- rumqttc changelog: https://github.com/bytebeamio/rumqtt/blob/main/rumqttc/CHANGELOG.md
- rmcp: https://github.com/modelcontextprotocol/rust-sdk
- MCP versioning: https://modelcontextprotocol.io/specification/versioning
- ts-rs: https://github.com/Aleph-Alpha/ts-rs
- keyring: https://github.com/open-source-cooperative/keyring-rs
- dist installers: https://github.com/axodotdev/cargo-dist/blob/main/book/src/installers/index.md

Agents and spend:
- ACP overview: https://agentclientprotocol.com/protocol/overview
- ACP usage reporting: https://github.com/agentclientprotocol/agent-client-protocol/blob/main/docs/announcements/session-usage-stabilized.mdx
- Claude ACP adapter: https://github.com/agentclientprotocol/claude-agent-acp
- Codex ACP adapter: https://github.com/agentclientprotocol/codex-acp
- Claude Code headless: https://code.claude.com/docs/en/headless
- Claude Code CLI reference: https://code.claude.com/docs/en/cli-reference
- Agent SDK result fields: https://code.claude.com/docs/en/agent-sdk/typescript
- Claude Code plugin marketplaces: https://code.claude.com/docs/en/plugin-marketplaces
- OpenCode CLI: https://opencode.ai/docs/cli/
- OpenCode JSON events: https://github.com/anomalyco/opencode/blob/dev/packages/cli/src/run/noninteractive.ts
- Codex exec events: https://raw.githubusercontent.com/openai/codex/rust-v0.164.0/codex-rs/exec/src/exec_events.rs
- Codex MCP: https://learn.chatgpt.com/docs/extend/mcp?surface=cli
- OpenRouter key limits: https://openrouter.ai/docs/guides/best-practices/capping-shared-agent-keys.md
