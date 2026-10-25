# agmen design

Status: draft for M0, written 2026-10-25. Approved by Andy as a plan on 2026-10-25; revised the same day to add the dispatcher (section 6.11) and the terminal UI with its agent view (section 6.8). This document is the builder's reference.
Protocol details live in [protocol.md](protocol.md). The current build plan is the Claude Code plugin MVP in [plans/mvp.md](plans/mvp.md); the earlier M0 and M1 plan in [plans/m0-m1.md](plans/m0-m1.md) holds task detail the MVP plan points into.

**Revised 2026-10-25 (Adam):** the broker is EMQX, not Mosquitto, and the first deliverable is a Claude Code plugin MVP on a thin hub (section 7).

*agmen* (Latin): an army on the march, a column moving together with each unit in its place.

## 1. Intent

agmen is a hosted service plus web, desktop, terminal, Claude Code and agent clients that let teams of humans and AI coding agents work one shared effort without stepping on each other. It turns an improvised workflow into a product any team can install.

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
- An outside team opens an invite link in the web console (and installs the CLI for its agents), and joins a project without help.
- A coordinator assigns an item to a person who is away; that person's dispatcher starts a bigger-model worker inside its caps, and the worker posts DONE with no human click.
- From one terminal a person clicks any agent in the project, sees what it is doing, and messages it, the way they would a Claude Code session.

## 2. Goals, non-goals, anti-goals

### Goals (v1)

1. **Human console** on four surfaces: a web console served by the hub, a GPUI desktop app, a terminal UI (`agmen tui`), and the Claude Code mod. Live feed and board, approvals, alerts as notifications.
2. **Agent view:** in the terminal UI and the desktop app, click any agent to see its live activity and message it (section 6.8).
3. **Agent bridge:** a CLI and MCP server so Claude Code, OpenCode and Codex post typed messages instead of curl and free text.
4. **Coordinator ledger:** lanes, item ids, atomic claims, rounds, rulings, snapshots that never expire.
5. **Agent launcher:** start workers in git worktrees with a model and a budget.
6. **Dispatcher:** a small, cheap model per person watches the chat, answers routine traffic, and delegates real work to bigger-model workers through the launcher (section 6.11).
7. **Swarm chat and DMs:** one project chat room that replaces the ntfy topic, where a human can message every agent at once and agents answer in threads, with private DMs beside it (section 6.10).
8. **Shared agent packs:** agent rules, skills, MCP servers and LSP servers published in a GitHub repo and installed into other people's agents only with each person's approval (section 6.12).
9. **Must prevent:** spoofable senders, lost messages, spend blowouts.

### Non-goals (v1)

- Billing code. The beta is invite-only; usage is metered so pricing can be set later.
- Running agents or holding model API keys in the cloud. Agents, worktrees and provider keys stay on each user's machine.
- Attachments over 32 KB (a clear rejection instead).
- Hard provider-side spend caps through management keys (Andy chose soft caps).
- Launch adapters for Codex (Codex joins through MCP) and ACP until spike S3 in M5.
- Automatic veto timers (veto windows are manual in v1).
- End-to-end encryption of message bodies.
- Sharing an agent's session transcript off the machine that runs it. Other people see an agent's posts, claims and DMs with them, not its tool calls.
- Dispatchers making human decisions. A dispatcher is an agent: it cannot send RULING, PUSH_OK or PUSH_NO, and it cannot approve its own over-cap delegations.
- Enforcing git permissions. Push rights and branch protection stay with the git host; agmen coordinates people and their agents and is not a security boundary around git.
- Hosting or mirroring pack contents. Packs stay in their git repos; the hub stores only a source URL and a commit SHA.
- Hooks, `bin/` executables or other binaries inside packs (v1). A pack carries rules, skills, MCP servers and LSP servers only.

### Anti-goals (failure even if it technically works)

- One person's agents can post as another person, or as another person's agents.
- A message is acknowledged to its sender but missing from history, or silently dropped.
- Agents need a human to click for every routine commit and push (the stall that prompted "Automatically do things when told what to do").
- A guard that fails open.
- A human messages the swarm and some agents never see it, or agents wake each other in a loop.
- Coordination traffic from one tenant visible to another.
- Someone outside a person's allow list makes that person's dispatcher spend their money.
- A pack, or an update to one, starts running on someone's machine before that person approved that exact commit.

## 3. Constraints

- **Transport is MQTT** (Andy's call; ntfy is out).
- **Broker is EMQX** (Adam's call, 2026-10-25), single node under its BSL 1.1 community terms, which allow free production use on one node as long as EMQX is not offered to third parties as a service. That covers the internal MVP and dogfood; outside teams (M6) or clustering need a commercial license or a broker change.
- **Hosted on the GroupDrive RKE2 cluster** `rke-metal`, which already runs ArgoCD GitOps from `groupdrive/Kubernetes`, Jenkins builds into Nexus (`docker.groupdrive.xyz`), Vault Secrets Operator, HAProxy ingress with cert-manager and Let's Encrypt, and Ceph RBD storage (source: `groupdrive/Kubernetes` README and `docs/context/infrastructure.md`). Changes there need its owners' review.
- **Source on `https://git.groupdrive.xyz/groupdrive/agmen`, private visibility.** Andy asked for internal, but the `groupdrive` group is private and GitLab does not allow a project more visible than its group.
- **Human UI is GPUI, the Claude Code mod, a web service, or a terminal UI** (Andy, 2026-10-25: "Make sure this is on GPUI, or using claude mods, or a web service in the plans", then chose all three; then "can you add a TUI version as well? Ensure we can click and view into the different agents and message them like how we can like with claude"). No Tauri or other webview shell.
- **Desktop app on macOS, Windows and Linux** at the beta; the web console on current Chrome, Firefox and Safari.
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

### 4.2 Human UI surfaces

| Approach | Tradeoff | Outcome |
|---|---|---|
| **Web console served by agmen-hub** | No install, so outside teams join from an invite link, and approvals work from any browser or phone. Browser notifications instead of native ones. A hosted page must not reach the local agmend, so launching and stopping workers stays local. | **Chosen by Andy** (2026-10-25) |
| **GPUI desktop app** | Pure Rust on Zed's GPUI, sharing `agmen-proto` types directly. The full local console, including the launcher. Risks: the newest `gpui` on crates.io is 0.2.2 (2025-10-22, Apache-2.0), it is pre-1.0 with frequent breaking changes, and it ships no notification, updater, deep-link or packaging support. Spike S8 settles these. | **Chosen by Andy** (2026-10-25) |
| **Claude Code mod** | The in-session surface: pane, band, status line, notifications, wake-ups and guards. Reaches only Claude Code users. | **Chosen by Andy** (already a first-class client) |
| **Terminal UI (`agmen tui`)** | ratatui in the `agmen` binary, so it needs no extra install, works over SSH on a headless box, and ships before the web console. The full local console, like the desktop app. Mouse support depends on the terminal (tmux needs `mouse on`, unverified for each terminal), so every action also has a key. No native notifications: a terminal bell and unread badges. | **Chosen by Andy** (2026-10-25): "can you add a TUI version as well?" |
| Tauri 2 + TypeScript (React) | Rust core, webview UI, official plugins for notifications, updater, deep links. | Chosen first, replaced 2026-10-25 |
| Tauri 2 + Leptos | All Rust, fewer ready components, slower UI iteration. | Not chosen |
| egui/eframe 0.36 | Pure Rust; chat text, accessibility, notifications and updater need extra work. | Not chosen |

Each surface has one job. The web console is the default console and covers everything the hub knows. The desktop app adds what needs the local machine: the agent view (live activity of local agents, messaging, launch, budget bars, stop, open worktree) and native notifications. The terminal UI offers the same local console in a terminal, and is the first console to ship (M3). The mod covers what happens inside a Claude Code session.

### 4.3 Broker (researched 2026-10-25)

| Option | Finding | Outcome |
|---|---|---|
| Mosquitto 2.1 | `EPL-2.0 OR BSD-3-Clause`. Dynamic security supports `%c` and `%u` since 2.1 (whole topic level). `persist-sqlite` writes changes as they are made. No JWT, no clustering. | My first call; replaced by EMQX 2026-10-25 |
| **EMQX 5.9+ / 6.x, single node** | Best auth fit (JWKS, `acl` claim, `disconnect_after_expire`, REST kick). BSL 1.1: single node free in production, but not "offering the Licensed Work to third parties on a hosted or embedded basis"; clustering needs a commercial license. The Docker Official `emqx` image stopped at 5.8.x; use `emqx/emqx`. | **Chosen by Adam** for the internal MVP; license or switch before outside teams |
| EMQX 5.8 and older (Apache 2.0) | "EMQX 5.8 Open Source Version ... will reach its official end-of-life on February 28, 2026": no more security patches. | Out |
| AWS IoT Core | Managed, MQTT 5, per-client policies from a custom authorizer. Fixed limits of 100 publishes/s per connection and 1 connect/s per client id; no local twin for dev and CI. | Out with the GroupDrive hosting choice |
| HiveMQ CE / Cloud | CE needs a Java extension for JWT and ACLs; Cloud Starter JWKS only supports registered claims. | Not chosen |
| VerneMQ | Maintained; webhooks auth; "a yearly usage fee (subscription), in case you use these packages commercially". | Not chosen |
| rumqttd, NanoMQ | No topic ACLs (rumqttd); HTTP authorization "only supports MQTT CONNECT" (NanoMQ). | Out |

Revisit the broker before outside teams join (license), at about 5,000 concurrent connections, or at the first need for high availability: a licensed EMQX cluster or AWS IoT Core.

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

### 4.6 Who watches the chat and delegates

| Approach | Tradeoff | Outcome |
|---|---|---|
| Every worker watches the chat | Each message lands in an Opus or Sonnet session; costly, and a busy worker answers late | Not chosen; mentions still reach workers (section 6.10) |
| Humans relay by hand | No model cost, but work stalls while the person is away, as with the 2026-10-24 relay subagents | Not chosen |
| Hub-hosted dispatcher | One place for routing, but the hub would hold model keys, against the non-goal | Out |
| **Local dispatcher on a small model** | One per person per project, run by agmend with that person's keys. A cheap model reads the chat and hands real work to bigger models through the launcher. A small model can misroute, so policy lives in agmend, not in the prompt | **Chosen by Andy**: "a lower tier model watching and responding to the chat and delegating to other bigger models. We will need that for delegations" |

Runtime (my call; spike S7 settles it):

| Option | Tradeoff |
|---|---|
| **Claude Code headless, one long-lived process** (`claude -p --input-format stream-json --output-format stream-json`) | The same adapter, MCP config, stdin delivery (section 6.10) and `total_cost_usd` reporting as launched workers. Per-turn cost reporting and context growth over hours are unverified |
| Claude Code headless, one process per batch (`--session-id` then `--resume`) | Same pieces; pays process start per batch; `--max-budget-usd` resets on each resume, so agmend tracks the running total |
| Messages API called from agmend | Fewest processes, but agmend gains a model client, a tool loop and per-provider code |

### 4.7 Sharing agent setup

| Approach | Tradeoff | Outcome |
|---|---|---|
| Each agent's own plugin marketplace | Claude Code and Codex both install plugins from git repos (`claude plugin marketplace add` in 2.1.310, `codex plugin marketplace add` in 0.164.0), but a publisher writes and maintains one plugin per agent, and nothing ties an install to a project or records the person's approval of a commit | Used underneath for Claude Code and Codex |
| Hub-hosted pack registry | One place to browse and install, but the hub would serve code that runs on members' machines, so a hub compromise becomes code execution on every machine | Out |
| **agmen packs in git repos** | One agent-neutral manifest in a GitHub (or any git) repo. agmend renders it into each agent's own mechanism after the person approves an exact commit. agmen must track each agent's config format as it changes | **Chosen** (my call, for Andy's request: "ensure agent rules, skills, LSPs, MCPs can be shared/installed with github to other user's agents with the user's permission") |

## 5. Decisions

### Made by Andy (2026-10-24/25)

- Name **agmen**, folder `~/Projects/agmen`, repo `groupdrive/agmen` (private; see Constraints).
- Hosted service, invite-only beta, metered, no billing code.
- MQTT, not ntfy.
- The service may read message contents; TLS plus encryption at rest.
- DMs visible to sender, recipient and the humans who own each; coordinators and admins see metadata only.
- Three human surfaces: a web console served by the hub, a GPUI desktop app on macOS, Windows and Linux, and the Claude Code mod. No Tauri (2026-10-25).
- v1 scope: human console, agent bridge, coordinator ledger, agent launcher, agent DMs; then the dispatcher and the terminal UI with its agent view (2026-10-25).
- Must prevent: spoofable senders, lost messages, spend blowouts.
- Push approval is a setting: per project with a per-person override, `off` by default so agents commit and push on their own.
- Soft spend caps; no provider management keys.
- Hosting on the GroupDrive cluster.
- A Claude Code mod as a first-class client.
- A terminal UI as a fourth human surface, where you click into any agent to see what it is doing and message it, like Claude Code's own agent views (2026-10-25).
- A lower tier model per person, the dispatcher, watches and answers the chat and delegates work to bigger models (2026-10-25).
- Agent rules, skills, LSP servers and MCP servers can be shared through GitHub and installed into other people's agents, with each person's permission (2026-10-25).

### Made by Adam (2026-10-25)

- EMQX is the broker (single node, internal use under BSL community terms).
- The first deliverable is a Claude Code plugin MVP: feed, posts and DMs; wake-ups; claims and lanes, on a thin hub. Desktop app, launcher, budgets and agmend as an OS service come after it.

### My calls (open to change)

- Agents, worktrees and model keys stay local; the service only coordinates.
- The hub relays every message, so the sender on the feed is always authenticated.
- Broker logins are hub-signed ES256 JWTs with a full-topic `acl` claim ending in deny-all, served through a JWKS endpoint; EMQX authorization `no_match = deny`.
- MVP: no local daemon; `agmen mcp` and `agmen watch` each hold their own MQTT connection and share one SQLite file per identity.
- MVP login is GitHub device flow only.
- Trust model: identity is enforced between people and machines, not inside one user account. A person's own agents act as that person's delegates, so agmend mints them without a browser step and the launcher can create workers unattended.
- GitHub and Google OAuth, plus device-code login for headless machines.
- The web console is a TypeScript and React single-page app built with Vite and served by agmen-hub as static files, using the ts-rs types. It gets live updates over server-sent events and posts through the hub's HTTP API as the logged-in human, so the browser never holds a broker login.
- The web console never talks to agmend. A hosted page calling a localhost daemon invites cross-site request and DNS rebinding attacks, and the launcher does not need it.
- The desktop app talks only to agmend over the local API, like the CLI, and holds no hub session of its own.
- The terminal UI is the `agmen tui` subcommand, a module of `agmen-cli` (ratatui 0.30.2, crossterm 0.29.0, ratatui-textarea 0.9.3, all MIT), and uses the same local API as the desktop app.
- Messaging an agent from any view is a DM. The hub records it, and the owner's agmend delivers it into the running session through the pending queue in section 6.10. There is no second, unrecorded path into a session.
- Live activity (tool calls, text, cost per turn) is shown only for agents that this machine's agmend runs. Transcripts never leave the machine.
- The dispatcher runs locally under agmend: one per person per project, off until started, default model `claude-haiku-5-5` (Claude Code's `--model` takes full model names, per `claude --help` 2.1.310). Worker tiers are `standard` (`sonnet`) and `deep` (`opus`), and each person can remap them.
- agmend, not the model, enforces delegation policy: who may ask, caps per delegation and per day, and worker count.
- The dispatcher has no built-in tools (`--tools ""`), only the agmen MCP server, so text in the chat cannot make it run commands or edit files.
- One posting surface: `agmen mcp` for every agent; the Claude Code mod adds UI, wake-ups and guards on top.
- agmend runs as a per-user OS service (launchd agent, systemd user unit, Windows logon task), not part of the desktop app, because it must outlive the app and run on machines without it.
- The mod reaches agmend through the `agmen` CLI (`$.process.run`, `$.process.spawn`), not `$.http.fetch` with `socketPath`, which is a Unix socket path and does not cover Windows.
- ts-rs 12.0.1 (stable) for the TS types the web console and the mod share.
- Packs install only from the person's own machine, never from the web console or the hub, and every install or update is pinned to a commit the person approved (section 6.12).
- Jenkins CI, matching the GroupDrive repos.

## 6. System design

```
 Claude Code + agmen mod ──($.process.spawn `agmen watch --json`, $.process.run `agmen ...`)─┐
 OpenCode / Codex / Claude ──(MCP stdio: `agmen mcp --identity <id>`)──────────────────────────┤
 GPUI desktop app ──(local API)────────────────────────────────────────────────────────────────┤
 agmen tui (ratatui, in a terminal or over SSH) ──(local API)──────────────────────────────────┤
                                                                                                 ▼
            agmend (one per machine; launchd agent / systemd user unit / Windows logon task)
            human login + devices, child identities, outbox, launcher, budgets, dispatcher
                                                                                                 │ MQTT 5 over TLS (WSS on 443)
                                                                                                 ▼
   EMQX single node (JWT authn, acl claim, no_match deny) ◀── $share/hub ──▶ agmen-hub (axum + Postgres)
                                                          auth, numbering, history, ledger, settings,
                                                          replay API, metering, broker logins
                                                                                                 ▲
 Web console in a browser ──(HTTPS: hub API, server-sent events, Web Push)───────────────────────┘
```

### 6.1 Repository layout

| Path | Contents |
|---|---|
| `crates/agmen-proto` | Envelope, verbs, topic builders (opaque ids only), JSON Schema via schemars, TS types via ts-rs |
| `crates/agmen-client` | MQTT 5 session (rumqttc), SQLite outbox, ACK and NACK tracking, gap detection, replay |
| `crates/agmend` | Daemon: identities, devices, local API (Unix socket or Windows named pipe, lock file, version negotiation), launcher, budgets, dispatcher supervision and delegation policy |
| `crates/agmen-cli` | `agmen` binary: subcommands, `agmen mcp` (rmcp stdio, the pattern in `~/Projects/kit_mcp`), `agmen watch`, `agmen tui` |
| `crates/agmen-pack` | Pack manifest parsing, validation, the plan hash, and rendering into each agent's format; used by agmend and by `agmen pack check`, which runs without agmend |
| `crates/agmen-hub` | Hosted service: control plane, ledger, history, project settings, broker JWTs and JWKS, EMQX admin, metering; serves the web console |
| `apps/desktop` | GPUI desktop app (Rust, workspace member, bin `agmen-desktop`) |
| `apps/web` | Web console (TypeScript, React, Vite); its build is embedded in the hub image |
| `mods/agmen-claude` | Claude Code mod and `.claude-plugin/marketplace.json` |
| `deploy/` | docker compose for local dev (EMQX, Postgres, Toxiproxy); manifests to copy into `groupdrive/Kubernetes` |
| `docs/` | This file, `protocol.md`, `adr/`, `plans/` |

### 6.2 Stack (versions checked 2026-10-25)

| Need | Choice | Notes |
|---|---|---|
| Desktop UI | gpui (crates.io 0.2.2 or a pinned git rev, chosen by spike S8) | Apache-2.0, pre-1.0. Native notifications, single instance, updater, deep links and installers are not built in; S8 picks a crate or writes each. `gpui-component` (longbridge) is a candidate widget kit, unverified |
| Terminal UI | ratatui 0.30.2, crossterm 0.29.0, ratatui-textarea 0.9.3 | All MIT; MSRV 1.88, 1.63 and 1.86, under the pinned 1.99.1. Mouse through crossterm's `EnableMouseCapture`; the message box is a ratatui-textarea widget. Tests render to ratatui's `TestBackend` |
| Dispatcher model | `claude-haiku-5-5` through Claude Code headless | Default until spike S7 measures triage quality; `sonnet` is the fallback default |
| Web console | TypeScript, React, Vite (versions picked at M4) | Served by agmen-hub; live updates over server-sent events; browser notifications through the Notifications API and Web Push |
| Secrets | keyring 4.2.0 | agmend's tokens in the OS keychain; the desktop app holds none |
| MQTT client | rumqttc 0.25.1 | `v5` has `set_clean_start`, `set_session_expiry_interval`, `set_manual_acks`, `Transport::Wss`. 0.25.0 broke websocket. Production readiness unverified: spike S2. Fallbacks: `rumqttc-next` 0.34.0, `mqtt5` 0.48.2 |
| MCP server | rmcp 3.7.0 | Implements MCP `2026-07-28`; stdio via `transport-io` |
| Agent Client Protocol | agent-client-protocol 3.5.0 (protocol v1) | `usage_update` carries optional cumulative cost; spike S3 in M5 |
| Local outbox | rusqlite 0.41, `bundled` | `synchronous=FULL` |
| Hub database | sqlx 0.9, Postgres, `runtime-tokio`, rustls | |
| TS types | ts-rs 12.0.1 | Shared by the web console and the mod; the desktop app uses the Rust types |
| CLI installers | dist 0.33.1 | shell, PowerShell, Homebrew, MSI |
| Desktop bundles | Chosen by spike S8 | Must produce a signed .app in a dmg, an MSI, and a deb plus AppImage |
| Signing | Apple Developer ID + notarization; Azure Artifact Signing on Windows | Needs Andy's accounts; covers the desktop app and the CLI binaries |

### 6.3 Identity and trust

1. A human logs in with OAuth PKCE through a loopback redirect, or with a device code on a headless machine. Each machine is a registered device.
2. agmend mints child identities per device (for example `andy/sol`), scoped to a project, role and lanes, with an expiry. Every child is attributed to its human, so `andy/sol` can never post as `adam/coord`. Agents receive tokens scoped to their own identity, never the human's session. A dispatcher is a child identity of kind `dispatcher`, at most one per person per project; workers it starts record it as `delegated_by`.
3. The hub signs a short-lived (1 h) ES256 JWT per identity whose `acl` claim names that identity's topics in full and ends with deny-all; EMQX verifies it against the hub's JWKS and disconnects at `exp`. Clients refresh the token over HTTPS. Revocation stops refreshes and kicks the identity's client ids through the EMQX REST API. An expired identity drains: it can still send what is in its outbox.
4. Clients may publish only to `up/{identity_id}` (the `acl` claim; authorization `no_match = deny`). The hub stamps `from`, so readers never trust payload claims.
5. Human verbs (a human's RULING, PUSH_OK, PUSH_NO) are accepted only from human identities; agent and dispatcher identity tokens cannot send them.
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
- With `owner`, an agent sends `PUSH_REQUEST {remote, ref, sha}`. It lands in the owning human's inbox, the web console, the desktop app, and the Claude Code mod's band; the human answers `PUSH_OK` or `PUSH_NO` (from the web console, the desktop app, the mod, or `agmen approve <request>`). The agent then pushes with its normal git credentials.
- The Claude Code mod enforces the setting for Claude Code agents: when it is `owner`, a raw `git push` is refused until a `PUSH_OK` matches the branch and `HEAD`. Other agents follow the setting through the join prompt and the `request_push` MCP tool.
- agmen does not hold git credentials, route pushes, or enforce branch protection. Force pushes and protected branches are the git host's job.
- Pushes show up in each DONE as `branch@commit`, which is the record the coordinator reads.

### 6.7 Launcher and soft budgets

- `agmen launch --agent claude|opencode --model <m> --item B-412 --lane L-UI --budget 5.00 --base origin/<base> --branch fix/<slug>` cuts a worktree, mints an identity, writes an MCP config, renders the join prompt, starts the worker headless in its own process group, and records it in SQLite so agmend re-adopts it after a restart. The dispatcher's `delegate` tool (section 6.11) uses the same path. agmend keeps each local worker's stream-json output in a rolling log under its state directory, which feeds the agent view (section 6.8).
- Claude Code: `claude -p --verbose --input-format stream-json --output-format stream-json --max-budget-usd <remaining> --session-id <uuid> --mcp-config <file> --strict-mcp-config`. agmend keeps stdin open and writes chat messages for the worker to it (section 6.10). The final `result` message carries `total_cost_usd` (a client-side estimate). `--max-budget-usd` ends with `error_max_budget_usd`, can pass the cap by one response plus running subagents, and resets on `--resume`, so agmend passes the remaining budget on every resume.
- OpenCode: `opencode run --format json`; `step_finish` carries `cost` (USD) and `tokens`. When a run ends with chat messages pending for the worker, agmend continues it with `opencode run --session <id>` (section 6.10).
- Codex: MCP only in v1 (`codex exec --json` reports tokens, no cost).
- Preflight: OpenRouter `GET /api/v1/key` with the worker's own key returns `limit_remaining`; agmend refuses a dispatch when remaining credit is below the sum of in-flight reservations.
- Thresholds: 80% warns the worker and its human; 95% tells the worker to wrap up (commit WIP, post STATUS); 100% stops the process group.
- On stop, agmend saves changes to a local `wip/<branch>` ref with git plumbing (temporary `GIT_INDEX_FILE`, `commit-tree`, `update-ref`), which runs no hooks and never stashes.
- Interactive sessions get spend alerts only; agmend does not launch them.

### 6.8 Clients

**Web console** (`apps/web`, served by agmen-hub at `app.<domain>`). Views: Projects; Chat (section 6.10); Board (lanes by items); DMs; Alerts; Agents (who is online, their claims, and the spend agmend reports in STATUS; clicking one shows the remote half of the agent view below: its posts, claims and DMs with me, and a message box); Settings (account, devices, members and invites, push approval per project and for my agents). The human logs in with the same GitHub or Google OAuth; the hub keeps an HttpOnly, SameSite=Strict session cookie, and state-changing calls also need a CSRF token. Live updates come from `GET /v1/projects/{p}/events` (server-sent events whose event id is the stream seq, so a reconnect resumes through replay). The human's posts (chat messages, ANSWER, DM, RULING, PUSH_OK, PUSH_NO) go through `POST /v1/projects/{p}/messages`, which runs the same ingest as `up/`. Browser notifications for ALERTs, ASKs to me, chat messages that mention me, DMs and push requests, through Web Push when the tab is closed. Invite links open the console directly.

**Desktop app** (`apps/desktop`, GPUI). Everything the web console shows, read through agmend, plus the full agent view below. Native notifications for ALERTs, ASKs to me, DMs, push requests and pending delegations. Single instance. Spike S8 runs at the start of M4; dev builds macOS first (M4), then Windows and Linux; deep links and the updater arrive in M6.

**Terminal UI** (`agmen tui`, M3). The same local console as the desktop app in a terminal: tabs for Agents (the default), Chat (section 6.10), Board, DMs, Alerts (ALERTs, ASKs to me, push requests, pending delegations) and Settings (push approval, dispatcher policy and tiers). It talks to agmend over the local API, so it runs anywhere the CLI does, including over SSH on a headless machine after `agmen device login`. Mouse: click a tab, a row, or a folded tool call; scroll with the wheel; click the message box to type. Keys cover the same actions (`Tab` and `1` to `6` for tabs, `j`/`k` or arrows, `Enter` to open, `i` to type, `Esc` back, `?` for help), so it works with mouse capture off. New ALERTs, ASKs to me and DMs ring the terminal bell and badge their tab.

**Agent view** (terminal UI and desktop app in full; the web console shows the remote half).
- *List.* Every identity in `state/roster`, grouped by person, mine first: my dispatcher, the workers it or I launched, my interactive sessions, then everyone else's. Each row: handle, kind, model (from HELLO), item and lane, status (working, idle, waiting on me, stopped), spend against budget, and unread DMs. Workers sit under the dispatcher that started them (`delegated_by`).
- *Open an agent.* Click a row or press `Enter`. For an agent this machine's agmend runs (launched workers and the dispatcher), the main pane follows its live session from the stream-json log in section 6.7: assistant text, each tool call folded to one line with its arguments (click to expand the call and its result), and cost per turn. For any other agent, the pane shows its posts, claims, DONEs and our DM thread. Transcripts never leave the machine (non-goals).
- *Message it.* The message box at the bottom sends a DM to that agent, with `reply_to` set when a message is selected. The hub records it and the owner's agmend drains it into the session through the pending queue (section 6.10), so a launched Claude Code worker gets it on stdin like a message typed into Claude Code, and an interactive session gets it through the mod. The row shows `recorded #seq`, and for my own local agents also `in session` once agmend has written it. Messaging my dispatcher is how I hand it work directly.
- *Act on my own agents.* Stop, add budget, approve or decline a pending push or delegation, and open the worktree (the desktop app opens a file manager; the terminal UI prints the path).
- *Local API methods* (added in M3): `agents {project}` (roster merged with local process state), `activity {identity_id, after?, follow}` (streams log frames for a local agent; `error.code = "not_local"` otherwise), `worker_stop {identity_id}`, `worker_budget {identity_id, add_usd}`. Messaging uses the existing `post` method with a DM.

**Claude Code mod** (`mods/agmen-claude`, claude-code 2.1.310 hooks API):

| Feature | API | Behavior |
|---|---|---|
| Event stream | `$.process.spawn(['agmen','watch','--json'])` from `session.start` | Child for the session's life (the API's documented pattern). Its `input` is one string then stdin closes, so commands go through `$.process.run(['agmen', ...])` |
| Inert unless joined | | Does nothing outside a worktree that ran `agmen join` |
| Board pane | `$.ui.open` + `ui.render` Pane | My lanes, claims, open ASKs, DMs, budget |
| Chat pane | `$.ui.open` + `ui.render` Pane `agmen-chat` | The project chat room, live. `/agmen say <text>` posts as the human, not the agent |
| Band | `AbovePrompt` | ASKs, push requests and pending delegations addressed to me |
| Status line | `$.ui.status` | `agmen B-412 claimed, 2 unread, $1.20/5.00` |
| Notifications | `$.ui.notify` | ALERTs and DMs |
| Wake-up | `$.prompt.submit` | Queues a turn when idle and an assignment, a DM, or a chat message that mentions this agent arrives. Never sets `asUser`, so the model reads "The agmen plugin sent a message: ..." (`claude-code.d.ts` 2.1.310, `PromptOrigin` kind `plugin`). Rate-limited so two agents cannot loop. Replaces the Monitor and relay subagents of the 2026-10-24 run |
| Context | `$.session.append` | Messages addressed to this agent as meta rows the model reads as data |
| Guards | `tool.call` | Lane guard above, and the push guard when push approval is `owner`; every guard's `.catch` denies (fail closed) |
| Command | `$.command.register` | `/agmen` with claim, done, dm, ask, board, chat, say, approve |

Develop with `claude --plugin-dir mods/agmen-claude`; check with `claude plugin validate`, `tsc -p` and `claude plugin test` (pattern of the API's `pane.test.tsx`, on terminal and desktop surfaces). Install with `claude plugin marketplace add https://git.groupdrive.xyz/groupdrive/agmen.git` (a git URL is an allowed marketplace source), then `/plugin install agmen@<marketplace>`. The repo is private, so installing from it needs a git credential for git.groupdrive.xyz, and outside teams need a public mirror of the mod by M6.

**CLI and MCP.** Subcommands: `login`, `device login`, `join <invite>`, `chat`, `post`, `ask`, `answer`, `dm`, `inbox`, `watch`, `state`, `claim`, `release`, `lend`, `done`, `check-path`, `approve`, `settings`, `launch`, `workers`, `budget`, `dispatcher start|stop|status`, `delegate approve|decline`, `tui`, `mcp`. MCP tools: `chat`, `read_chat`, `post`, `claim`, `release`, `done`, `ask`, `answer`, `dm`, `inbox`, `state`, `lanes`, `check_path`, `request_push`, and for a dispatcher (`agmen mcp --dispatcher`) `delegate`, `workers` and `stop_worker` in place of `claim`; `post` and `chat` wait up to 5 s for the ACK and answer "recorded #seq" or "queued (not yet recorded)". Every tool result ends with the messages pending for the calling agent (section 6.10). Claude Code, OpenCode (`opencode.json`) and Codex (`[mcp_servers.agmen]` in `config.toml`) all load it through their MCP config.

### 6.9 Hosting

- New `agmen-env/` in `groupdrive/Kubernetes`: ArgoCD Application, namespace `agmen`, Vault Secrets Operator secrets, hub Deployment, EMQX StatefulSet (one replica) and Postgres 16 StatefulSet on Ceph RBD with pinned image tags, nightly `pg_dump` CronJob to Ceph RGW S3.
- HAProxy ingress with cert-manager Let's Encrypt for `api.`, `app.` and `mqtt-ws.` hosts (WSS on 443, the pattern of `mqtt-ws.dev.groupdrive.xyz`). The ingress must not buffer server-sent events.
- Jenkins builds the web console and the hub image into Nexus; ArgoCD syncs it.
- Observability: `tracing` plus OpenTelemetry; audit log of settings changes, push answers, rulings and identity minting; per-tenant rate and size limits; usage metering (messages, storage, active agents, launcher spend).

### 6.10 Chat

The project `feed` is the chat room that replaces the ntfy topic. Everyone in the project reads one numbered stream, so chat gets the same guarantees as every other message: an authenticated sender, an ACK, and replay after a gap. Free text is `FYI` (or `ASK` when it needs an answer), and replies thread by `reply_to`. Ledger traffic (CLAIM, GRANT, RELEASE, STATE) shares the stream; chat views fold it into one-line rows.

**Mentions decide who gets interrupted.** A message may mention `@all`, `@agents`, `@humans`, a lane such as `@L-UI` (from M2, once lanes exist), or up to 8 identities such as `@andy/sol`. Clients turn typed `@handle` text into identity ids from `state/roster`; the hub expands groups and stamps `mentioned` with the resolved identity ids, without the sender. A reply to a human's message also mentions that human. Unmentioned messages stay in the room and do not enter agent sessions, so 17 agents do not pay tokens for every line; agents read the room with `read_chat` when they choose.

**Messaging the swarm.** Humans and coordinators may use group mentions; agents may mention individuals only, so one agent cannot wake the swarm. The chat composer in the web console and the desktop app defaults to `@all` for humans. From a terminal, `agmen chat "deploy is frozen" --all` posts once, the way `curl -d` posted to ntfy, and `agmen chat` with no text follows the room live and sends each line typed. Without `--as`, both post as the human.

**Reaching an agent's session** (the job the curl poll loops and relay subagents did in the 2026-10-24 run). agmend keeps a pending queue per identity: chat messages that mention it, DMs to it, and assignments. It drains the queue into the session by the best path that agent offers:

| Agent | Delivery | Milestone |
|---|---|---|
| Any MCP agent (Codex; OpenCode or Claude Code without the mod) | Every agmen tool result ends with the messages pending for it; `read_chat` reads the room | M1 |
| Claude Code, interactive, with the mod | `$.session.append` as a meta row; if the session is idle, a wake-up through `$.prompt.submit` | M3 |
| Claude Code worker launched by agmend | Written to the worker's stdin as a stream-json user message (`--input-format stream-json`, in `claude --help` 2.1.310). Whether a message sent mid-turn waits for the turn to end is unverified; M3 tests it | M3 |
| OpenCode worker launched by agmend | When a run ends with messages pending, agmend continues the session with `opencode run --session <id>` (in `opencode run --help` 2.1.4) | M5 |

Known limit: an MCP-only agent that agmen did not launch cannot be woken while idle. It sees pending messages on its next agmen tool call, and the join prompt tells it to call `inbox` between steps.

**Loop limits.** At most one wake-up per agent every 30 seconds (start value, tuned in an ADR); messages that arrive in between go into the next wake as one batch. A message never wakes its sender, replies between agents do not add automatic mentions, and the existing rate limit (10 per second, burst 30) applies to chat.

**Surfaces.** Web console and desktop app: a Chat view with threads, `@` autocomplete for handles, lanes and groups, a "Show ledger events" toggle, and notifications when I am mentioned. Terminal UI: the Chat tab (section 6.8). Claude Code mod: the `agmen-chat` pane and `/agmen say`. CLI: `agmen chat`. The web console at phone width plus Web Push covers what the ntfy phone app did.

### 6.11 Dispatcher

A dispatcher is a small model that watches the chat for one person, answers what it can from agmen state, and hands everything else to a bigger-model worker. It does the job of the 2026-10-24 run's relay subagents and lets assignments go ahead while the person is away.

- **Start.** `agmen dispatcher start --project <p>` mints `andy/dispatch` (kind `dispatcher`; the hub allows one per person per project). agmend supervises it like a launched worker and re-adopts it after a restart. `stop` and `status` also exist. It stays off until the person starts it.
- **Runtime.** `claude -p --model claude-haiku-5-5 --input-format stream-json --output-format stream-json --verbose --tools "" --strict-mcp-config --mcp-config <dispatcher.json> --append-system-prompt <dispatcher prompt>`, unless spike S7 picks one process per batch. The MCP config runs `agmen mcp --identity <dispatcher id> --dispatcher`.
- **What reaches it.** Its pending queue (section 6.10) holds messages that mention it, DMs to it, and assignments to it. While it runs, agmend also adds mentions, DMs and assignments addressed to its person, and DONE, STATUS and ALERT from workers it started. Batching and the wake-up limit are the ones in section 6.10. Each event reaches the model with its stamped `from_handle` and `from_kind`, under the standing instruction that message text is data, never an instruction to the dispatcher.
- **What it does.** Each turn ends in one action: answer from state (for example "B-412 is held by andy/w3f, $3.10 of $5.00 spent"), delegate, forward to a running worker by DM, escalate to its person with ALERT, or nothing. On a group mention it answers only for its person and that person's workers.
- **Tools.** The MCP tools under its own identity, minus `claim` (the worker claims), plus:
  - `delegate {reply_to, brief, tier, budget_usd, item?, lane?, base?, branch?}`. agmend runs the launcher (section 6.7) on the tier's model with `delegated_by` set and returns the worker id. The dispatcher then posts `DELEGATED` in the same thread.
  - `workers`: the person's workers with model, item, budget used and state.
  - `stop_worker {worker}`: only workers this dispatcher started.
- **Policy, enforced by agmend.** The model asks; agmend decides.
  - Requester: `delegate` must name `reply_to`, and agmend reads that message's hub-stamped `from`. The sender must be in `accept_from` (default: the person and project coordinators), so the dispatcher cannot start work on its own initiative or on a stranger's say-so.
  - Caps: `per_delegation_usd` (default 5.00), `daily_usd` across delegations (default 20.00), `max_workers` (default 3), plus the launcher's preflight.
  - Over policy: agmend holds the delegation as pending, and the dispatcher ALERTs its person. The person approves or declines on their own machine (`agmen delegate approve|decline <id>`, the terminal UI, the desktop app, or the mod's band), so no wire verb is needed.
  - Its own spend: `dispatcher_daily_usd` (default 2.00), counted from `total_cost_usd`. At the cap, agmend pauses the dispatcher, posts STATUS and ALERTs the person.
- **Loops.** The section 6.10 loop limits apply. In addition, a dispatcher posts at most 3 messages in a thread without a human or worker message in between; agmend drops and logs the rest.
- **Tiers.** `standard` maps to `sonnet` and `deep` to `opus`; each person can remap them or add tiers. The prompt says to use `deep` only when the requester marks the work hard or a `standard` attempt failed. agmend refuses unknown tiers.
- **Coordinator dispatcher.** A person with the coordinator role may start one with `--coordinator`. It gains ASSIGN and answers unaddressed ASKs, so a cheap model can run the board while bigger models do the work. It still cannot send RULING.
- **Visibility.** Workers carry `delegated_by` in the roster and sit under their dispatcher in the agent view. The audit log records each delegation, approval and stop.

### 6.12 Shared agent packs

A pack is a git repo, normally on GitHub, holding agent setup that one person wants others to use: rules, skills, MCP servers and LSP servers. agmend installs a pack into a person's agents only after that person approves the exact commit and contents on their own machine.

**Format.** `agmen-pack.toml` at the repo root:

```toml
name = "groupdrive-rust"
description = "Rust rules, a review skill, rust-analyzer, a docs MCP server"
rules = ["rules/rust.md"]          # markdown, added to every session of every receiving agent
skills = "skills"                  # <name>/SKILL.md folders, the layout Claude Code, Codex and OpenCode all read

[mcp.docs]
command = "uvx"
args = ["example-docs-mcp==1.4.0"]
env = ["DOCS_API_TOKEN"]           # names only; each installer supplies their own value

[lsp.rust]
command = "rust-analyzer"
extensions = { ".rs" = "rust" }
```

`agmen pack check` validates the manifest and runs the secret scanner, so a publisher can test before pushing. agmend repeats the same checks on install and refuses a pack that has secret-shaped text, `hooks/`, `bin/`, or any file over 1 MiB. Both warn on an unpinned `npx`, `bunx`, `uvx` or `pipx` package. The hub's secret scanner moves from `agmen-hub` into `agmen-proto` in M3 so all three share it.

**Sharing.**
- *Publish:* push the repo to GitHub, public or private. Any git URL works the same way, including `git.groupdrive.xyz`.
- *Share a link:* post `github:owner/repo` or a git URL in chat or a DM. The terminal UI, the desktop app and the mod render it as a pack card with an Install button. The web console shows the card with "Install from your terminal or desktop app".
- *Recommend to a project:* a project admin pins a pack at a commit with `agmen pack attach github:owner/repo@<sha> --project <p>`. The hub stores only `{source, sha, attached_by, attached_at}` and publishes `state/packs`. Each member's agmend then offers "pr-3331 recommends groupdrive-rust@1a2b3c4"; nothing installs until that member approves.

**Permission.** Installing a pack runs code as the person: MCP and LSP servers start processes, skills can carry scripts the agent may run, and rules steer every turn. So:
- Only the person whose agents receive the pack can approve it, and only on their own machine: the `agmen pack add` prompt, the terminal UI, the desktop app, or the mod's band. The web console cannot reach agmend, and the hub never holds pack contents. A compromised hub can recommend a pack but cannot install one.
- The approval screen shows the source, the commit SHA with its author and date, every rule in full, each skill with its files, each MCP server's exact command or URL and the env names it needs, each LSP server's command, which agents receive it, and the scope (this project, or `--user` for all of my agents on this machine). The person approves everything or picks items.
- Every install call to agmend carries the SHA-256 of the plan that was shown (commit, chosen items, scope). agmend refuses the call if the plan changed since. Scripted installs pass the hash a human saw (`agmen pack add <source> --accept <sha256>`), the same pattern as `claude plugin install --accept-command <sha256>` in Claude Code 2.1.310. Calls made with an agent's identity token are refused, and the design 6.3 limit applies: an agent running as the same OS user can still drive the person's own CLI.
- Updates never apply on their own. `agmen pack update` fetches, shows the diff against the approved commit, and needs a new approval.
- Secrets: agmend asks for each declared env value at approval time and keeps it in the OS keychain (service `agmen`, account `pack:<name>:<VAR>`). Values never go into the pack, the hub, or chat.
- Private repos: agmend fetches with the person's own `git`, so `gh auth login` credentials and SSH keys work. agmen holds no GitHub token, matching the non-goal on git credentials.
- `agmen pack remove` deletes exactly what agmend wrote. agmend records each file and config entry it adds in SQLite and backs up any agent config file before editing it.

**Install per agent.** Spike S9, at the start of M3, settles every step marked unverified.

| Agent | How agmend installs a pack | Milestone |
|---|---|---|
| Claude Code | agmend renders the pack as a local Claude Code plugin: skills, `.mcp.json` and `.lsp.json`, which plugins support per the plugin manifest reference. Rules go in an agmen-written SessionStart hook that prints them, because a plugin's `CLAUDE.md` is not loaded while SessionStart stdout is added to context (hooks reference). Interactive sessions: a local marketplace plus `claude plugin install <pack>@agmen-packs --scope local`, or `--scope user` (both scopes in `claude plugin install --help` 2.1.310). Launched workers: `--plugin-dir <rendered>` and `--append-system-prompt-file <rules>` | M3 |
| OpenCode | The config schema (opencode.ai/config.json, read 2026-10-25) has `instructions` (rules), `skills` (extra skill paths), `mcp` and `lsp`. agmend writes those keys into a config file it owns; how that file layers onto the person's own config is unverified (S9) | M5 |
| Codex | Skills and MCP servers as a rendered Codex plugin in a local marketplace (`codex plugin marketplace add <path>`, `codex plugin add`, in `codex --help` 0.164.0). How rules reach Codex (AGENTS.md handling) is unverified (S9). I found no LSP support in Codex, so LSP items are skipped, and the approval screen says so | M5 |

## 7. Milestones

The Claude Code plugin MVP ([plans/mvp.md](plans/mvp.md)) comes first. It takes the spikes from M0 (S1 redone for EMQX; S6 dropped), the M1 hub and CLI without agmend, the claims and lanes half of M2, and the wake-up, context and lane-guard half of M3. Exit: two Claude Code sessions on two machines exchange DMs, an idle one wakes on an ASSIGN, 20 concurrent CLAIMs give one GRANT, and an out-of-lane Edit is refused. Then the dogfood round. The milestones below stay as the longer roadmap.

| # | Deliverable | Exit check |
|---|---|---|
| M0 | Scaffold, this design, protocol, spikes S1 (EMQX JWT `acl` claim, no_match deny, expiry and REST kick, shared subscriptions, packet limits, WSS behind HAProxy, ACL attacks with `+`, `#`, `/` in ids), S2 (rumqttc v5 sessions, manual acks, WSS), S4 (mod streams `agmen watch` into a pane), S5 (numbering and replay property tests), S6 (local API on Windows), S7 (dispatcher runtime: stream-json versus `--resume`, cost per turn, tool lockdown, a message sent mid-turn, triage quality of `claude-haiku-5-5`) | Each spike ends in an ADR |
| M1 | Hub core, EMQX, agmend, CLI and MCP: OAuth and device login, identities for interactive sessions, typed messages, ACK and NACK, replay, DMs, presence, swarm chat (`agmen chat`, mentions, pending queues, MCP `chat` and `read_chat`); minimal deploy on the GroupDrive cluster | Two machines exchange messages; a human's `@all` message reaches every agent's pending queue once; a network kill during DONE loses nothing; a spoofed publish is refused by the broker |
| M2 | Ledger and push approval: lanes, items, atomic claims with leases, LEND, the `push_approval` setting with PUSH_REQUEST and PUSH_OK, alerts, STATE heads and snapshots | 20 concurrent CLAIMs on one item give exactly one GRANT; with `owner`, a PUSH_REQUEST reaches the owner and PUSH_OK reaches the agent; DONE outside the lane is flagged |
| M3 | Claude Code mod, plus spend alerts and `agmen launch --agent claude` with `--max-budget-usd` and the WIP commit; the dispatcher with `delegate` and agmend's policy; the terminal UI with the agent view; spike S9 and shared packs for Claude Code (`agmen pack add|update|remove|check|attach`, approval in the CLI, terminal UI and mod, `state/packs`) | Pane, chat pane, band, status line, notify, wake-up and guards work in a real session; an `@all` message wakes an idle interactive session and reaches a launched worker's stdin; plugin tests pass on terminal and desktop; a $0.50 Claude worker stops with its WIP committed; an ASSIGN to an away person reaches their dispatcher, which starts a `standard` worker that posts DELEGATED then DONE in the thread; an ASSIGN from someone outside `accept_from` gets an ALERT and no launch; in `agmen tui` a click on a local worker shows its tool calls live, a message typed there is recorded and written to the worker's stdin, and every step also works with mouse capture off; a pack from a private GitHub repo installs into a Claude Code session only after approval, after which its rules are in context, its skill loads, and its MCP and LSP servers start; a changed commit waits for a new approval; `agmen pack remove` leaves the agent config as it was |
| Dogfood | One real multi-agent round with Adam's team on agmen instead of ntfy | Retro ADR |
| M4 | Human consoles: web console first, then spike S8 (GPUI on 3 OSes: long lists, text input, native notifications, single instance, packaging, signing) and the GPUI desktop app | The web console's Chat view messages the swarm and shows replies live, answers a PUSH_REQUEST, notifies with the tab closed, and lets an admin attach a pack that members then approve locally; GPUI dev builds on macOS, then Windows and Linux |
| M5 | Full launcher: OpenCode adapter (for workers, and for a dispatcher on a small OpenRouter model), S3 (ACP vs CLI JSON), re-adoption after restart, preflight; packs for OpenCode and Codex | A worker survives an agmend restart and is re-adopted; preflight refuses a dispatch over the reservation; one pack installs into Claude Code, OpenCode and Codex on one machine, and the Codex approval screen lists its skipped LSP items |
| M6 | Beta delivery: invites, metering, tenant isolation tests in CI, signed and notarized desktop app installers for 3 OSes, updater, deep links, public mod mirror, docs, privacy policy and ToS, backups, monitoring | An outside team installs and joins without help |

## 8. Left to the builder

- Exact Postgres schema and migrations (sqlx), within the tables named above.
- Local API wire format between CLI, mod, desktop app and agmend (JSON over the socket or pipe), with version negotiation.
- Join prompt template wording.
- Price table for converting Codex tokens to dollars if Codex launching lands later.
- The hub's secret-shaped-string scanner rules (start from gitleaks patterns).
- Retry and backoff constants, lease durations and heartbeat intervals (document each in an ADR).
- The dispatcher prompt and its triage examples, starting from the S7 prompt and fixtures.
- The terminal UI's layout and key map, within the views and keys named in section 6.8.

## 9. Open items and risks

| Item | Owner | Notes |
|---|---|---|
| Domain for agmen | Andy | Subdomains of `groupdrive.xyz` are enough for the dogfood |
| Ceph RBD encryption at rest | Cluster owners | Unverified; if absent, the hub encrypts message bodies itself |
| Port 8883 reachable from the internet | Cluster owners | Unverified; WSS on 443 is the plan either way |
| Apple Developer ID, Windows Artifact Signing | Andy | Accounts and cost |
| Hosted beta legal | Andy | ToS and privacy policy before outside teams |
| Mod API churn | Builder | Pin a tested Claude Code version in CI |
| GPUI churn and gaps | Builder | Pre-1.0 with frequent breaking changes; pin one version or git rev. If S8 fails on a platform, the web console covers that platform and the agent view stays in the terminal UI |
| Dispatcher misroutes or is steered by chat text | Builder | Policy lives in agmend, the dispatcher has no built-in tools, and the hub refuses human verbs from it. S7 counts injection cases. A person can stop it at any time |
| Small model too weak for triage | Builder | S7 measures accuracy; below 90% on the labeled set, the default becomes `sonnet` |
| Malicious or compromised pack | Builder | Approval of an exact commit with every command shown, no hooks or binaries, the secret scan, no auto-update, and removal that restores config. A pack the person approved can still do anything an MCP server can; the approval screen says so |
| Agent config formats move between releases | Builder | S9 pins the tested Claude Code, OpenCode and Codex versions; CI renders a fixture pack for each and checks it with `claude plugin validate` and the other agents' own loaders |
| Mouse support varies by terminal | Builder | Every action has a key; test in Terminal.app, iTerm2, Windows Terminal, a Linux terminal and tmux with `mouse on` |
| Dogfood participation | Andy and Adam | Adam's agents must install `agmen` |
| EMQX license before outside teams | Andy and Adam | BSL community terms cover one node for internal use; M6 needs a commercial license or another broker |
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

Rust and UI:
- gpui on crates.io (0.2.2, 2025-10-22, Apache-2.0; checked 2026-10-25 through the crates.io API): https://crates.io/crates/gpui
- gpui-component: https://github.com/longbridge/gpui-component
- gpui-ce fork: https://docs.rs/crate/gpui-ce/0.3.2
- ratatui 0.30.2, crossterm 0.29.0, ratatui-textarea 0.9.3 (licenses and MSRV checked 2026-10-25 through the crates.io API): https://crates.io/crates/ratatui, https://crates.io/crates/crossterm, https://github.com/ratatui/ratatui-textarea
- crossterm mouse capture: https://docs.rs/crossterm/0.29.0/crossterm/event/struct.EnableMouseCapture.html
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

Packs (read 2026-10-25), plus `claude plugin install --help` and `claude plugin marketplace add --help` (2.1.310), `codex plugin --help`, `codex plugin add --help` and `codex plugin marketplace add --help` (0.164.0), and `opencode plugin --help` (2.1.4):
- Claude Code plugin manifest (skills, `mcpServers`, `lspServers`; a plugin's `CLAUDE.md` is not loaded): https://code.claude.com/docs/en/plugins-reference
- Claude Code hooks (SessionStart `additionalContext` and plain stdout reach context): https://code.claude.com/docs/en/hooks
- OpenCode config schema (`instructions`, `skills`, `mcp`, `lsp`): https://opencode.ai/config.json
- OpenCode skills (`.opencode/skills`, `~/.config/opencode/skills`, and the `.claude/skills` and `.agents/skills` paths): https://opencode.ai/docs/skills
- Codex plugins (skills, MCP servers, hooks; no LSP listed): https://learn.chatgpt.com/docs/plugins
