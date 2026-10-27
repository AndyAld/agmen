# agmen design

Status: draft for M0, written 2026-10-25. Approved by Andy as a plan on 2026-10-25; revised the same day to add the listener (section 6.11, first called the dispatcher) and the terminal UI with its agent view (section 6.8). This document is the builder's reference.
Protocol details live in [protocol.md](protocol.md). The current build plan is the Claude Code plugin MVP in [plans/mvp.md](plans/mvp.md); the earlier M0 and M1 plan in [plans/m0-m1.md](plans/m0-m1.md) holds task detail the MVP plan points into.

**Revised 2026-10-25 (Adam):** the broker is EMQX, not Mosquitto, and the first deliverable is a Claude Code plugin MVP on a thin hub (section 7).

**Revised 2026-10-25 (Andy):** agent roles as enforced XML files (section 6.13), the listener in place of the dispatcher (6.11), any terminal agent as a worker over ACP with take-over and switching (6.7), a vim-style terminal UI and a Zed-style desktop app (6.8), agents coordinating with each other and urgent alerts (6.10), planning rooms (6.14), a read-only GitHub App (6.15). The conversation, the approaches and the mockups behind it: [specs/2026-10-25-roles-listener-consoles-design.md](specs/2026-10-25-roles-listener-consoles-design.md) and [mockups/](mockups/).

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
- A coordinator assigns an item to a person who is away; that person's listener starts a bigger-model worker inside its caps, and the worker posts DONE with no human click.
- A person types "spawn a worker to fix main and keep Sean's changes" in the chat; their listener checks git, starts an Engineer on their machine inside their caps, the worker's DONE gets a REVIEW, and nobody runs a relay subagent or answers "checking with Andy".
- From one terminal a person clicks any agent in the project, sees what it is doing, and messages it, the way they would a Claude Code session.

## 2. Goals, non-goals, anti-goals

### Goals (v1)

1. **Human console** on four surfaces: a web console served by the hub, a GPUI desktop app, a terminal UI (`agmen tui`), and the Claude Code mod. Live feed and board, approvals, alerts as notifications.
2. **Agent view:** in the terminal UI and the desktop app, click any agent to see its live activity and message it (section 6.8).
3. **Agent bridge:** a CLI and MCP server so Claude Code, OpenCode and Codex post typed messages instead of curl and free text.
4. **Coordinator ledger:** lanes, item ids, atomic claims, rounds, rulings, snapshots that never expire.
5. **Agent launcher:** start workers in git worktrees with a model and a budget.
6. **Listener:** a small, cheap model per person hears what that person types in the chat, looks things up with read-only tools, answers routine traffic, delegates real work to bigger-model workers through the launcher, and changes that person's settings when asked (section 6.11).
7. **Swarm chat and DMs:** one project chat room that replaces the ntfy topic, where a human can message every agent at once and agents answer in threads, with private DMs beside it (section 6.10).
8. **Shared agent packs:** agent rules, skills, MCP servers and LSP servers published in a GitHub repo and installed into other people's agents only with each person's approval (section 6.12).
9. **Roles:** seven built-in roles plus a shared base, as XML files (Listener, Coordinator, Planner, Researcher, Engineer, Tester, Reviewer), enforced at launch, at the hub, in `agmen mcp` and by lanes; teams edit them or add their own (section 6.13).
10. **Any terminal agent:** Claude Code, Codex, OpenCode, Gemini CLI, Antigravity and Grok Build as workers or listeners, launched over ACP, taken over in a terminal inside the chat, and switched mid-task with a handoff brief (section 6.7).
11. **Worktree lifecycle:** every editing agent works in its own worktree on its own branch, and agmen removes both once the branch is merged (section 6.7).
12. **Consoles people live in:** a terminal UI that works like vim, opened by `agmen` with no arguments, and a desktop app with Zed's layout, icons, themes and appearance settings (section 6.8).
13. **Agents coordinating with each other** in the open, within loop limits, and urgent alerts that highlight and chime only when a human must act (section 6.10).
14. **Planning rooms:** plan a greenfield project with a Planner agent, approve the specs, and kick off a team that each person approves for their own machine (section 6.14).
15. **GitHub App**, read-only: PR, check and merge state back into agmen (section 6.15).
16. **Clickable references:** every agent, person, role, branch, commit, file, item, lane and PR named in a message opens where it points (section 6.8).
17. **Must prevent:** spoofable senders, lost messages, spend blowouts.

### Non-goals (v1)

- Billing code. The beta is invite-only; usage is metered so pricing can be set later.
- Running agents or holding model API keys in the cloud. Agents, worktrees and provider keys stay on each user's machine.
- Attachments over 32 KB (a clear rejection instead).
- Hard provider-side spend caps through management keys (Andy chose soft caps).
- Automatic veto timers (veto windows are manual in v1).
- End-to-end encryption of message bodies.
- Sharing an agent's session transcript off the machine that runs it. Other people see an agent's posts, claims and DMs with them, not its tool calls.
- Listeners making human decisions. A listener is an agent: it cannot send RULING, PUSH_OK or PUSH_NO, it cannot approve its own over-cap delegations, and it cannot widen its person's settings without that person's click.
- Enforcing git permissions. Push rights and branch protection stay with the git host; agmen coordinates people and their agents and is not a security boundary around git.
- Hosting or mirroring pack contents. Packs stay in their git repos; the hub stores only a source URL and a commit SHA.
- Hooks, `bin/` executables or other binaries inside packs (v1). A pack carries rules, skills, MCP servers, LSP servers and roles only.
- Watching or typing into another person's agent terminal. Live sessions and take-over stay limited to agents on your own machine (Andy, 2026-10-25). For other people's agents you see posts, claims, DMs and DONEs.
- Moving a session between agents. Switching from Claude Code to Codex starts a new Codex session with a handoff brief; the old session stays resumable.
- agmen writing to GitHub: no PR creation, comments or merges from agmen. Agents do those with their own credentials.
- Custom roles widening what an identity kind allows. A role can only narrow.
- agmen installing other agents. It shows the official source and a prompt to paste into an agent that has a shell.
- Zed extensions or panels. The desktop app is its own GPUI app that looks like Zed; it is not a Zed plugin.

### Anti-goals (failure even if it technically works)

- One person's agents can post as another person, or as another person's agents.
- A message is acknowledged to its sender but missing from history, or silently dropped.
- Agents need a human to click for every routine commit and push (the stall that prompted "Automatically do things when told what to do").
- A guard that fails open.
- A human messages the swarm and some agents never see it, or agents wake each other in a loop.
- Coordination traffic from one tenant visible to another.
- Someone outside a person's allow list makes that person's listener spend their money.
- Someone else's planner, listener or kick-off starts an agent on my machine, or spends my money, without my approval.
- A role file grants an agent RULING, PUSH_OK, PUSH_NO, ASSIGN, or edits it was not meant to have.
- A worktree with uncommitted work, or a branch that never merged, is deleted.
- Text in the chat makes a listener run a shell command, or anyone but me changes my settings through my listener.
- An urgent alert fires so often that people mute it.
- A pack, or an update to one, starts running on someone's machine before that person approved that exact commit.

## 3. Constraints

- **Transport is MQTT** (Andy's call; ntfy is out).
- **Broker is EMQX** (Adam's call, 2026-10-25), single node under its BSL 1.1 community terms, which allow free production use on one node as long as EMQX is not offered to third parties as a service. That covers the internal MVP and dogfood; outside teams (M6) or clustering need a commercial license or a broker change.
- **Hosted on agmen's own stack**: its own Kubernetes cluster, database, broker, secret store, image registry and domain, nothing shared with GroupDrive's runtime (Andy, 2026-10-25). The source repositories and Jenkins CI stay on GroupDrive's GitLab and Jenkins. The cluster manifests live in `groupdrive/agmen-kubernetes` (Andy, 2026-10-26), mirrored to private GitHub, and ArgoCD in agmen's cluster syncs them.
- **Source on `https://git.groupdrive.xyz/groupdrive/agmen`, private visibility.** Andy asked for internal, but the `groupdrive` group is private and GitLab does not allow a project more visible than its group.
- **Human UI is GPUI, the Claude Code mod, a web service, or a terminal UI** (Andy, 2026-10-25: "Make sure this is on GPUI, or using claude mods, or a web service in the plans", then chose all three; then "can you add a TUI version as well? Ensure we can click and view into the different agents and message them like how we can like with claude"). No Tauri or other webview shell.
- **The terminal UI works like vim and the desktop app looks like Zed** (Andy, 2026-10-25: "use this like vim (launch in the terminal) and look like Zed for the desktop app"). Zed's own icon SVGs are not shipped: they carry no license of their own in a GPL-3.0-or-later repo, so agmen ships Lucide and reads file icons from the person's installed Zed icon theme (section 6.8).
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
| **GPUI desktop app** | Pure Rust on Zed's GPUI, sharing `agmen-proto` types directly. The full local console, including the launcher, with Zed's layout: docks, panes with tabs, a status bar and a command palette. Risks: the newest `gpui` on crates.io is 0.2.2 (2025-10-22, Apache-2.0), it is pre-1.0 with frequent breaking changes, and it ships no notification, updater, deep-link or packaging support. Spike S8 settles these. | **Chosen by Andy** (2026-10-25) |
| **Claude Code mod** | The in-session surface: pane, band, status line, notifications, wake-ups and guards. Reaches only Claude Code users. | **Chosen by Andy** (already a first-class client) |
| **Terminal UI (`agmen` with no arguments)** | ratatui in the `agmen` binary, modal like vim (normal, insert and command modes, splits, a tab line), so it needs no extra install, works over SSH on a headless box, and ships before the web console. The full local console, like the desktop app. Mouse support depends on the terminal (tmux needs `mouse on`, unverified for each terminal), so every action also has a key. No native notifications: a terminal bell and unread badges. | **Chosen by Andy** (2026-10-25): "can you add a TUI version as well?" |
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
| Hub-hosted listener | One place for routing, but the hub would hold model keys, against the non-goal | Out |
| **Local listener on a small model** | One per person per project, run by agmend with that person's keys. A cheap model reads the chat and hands real work to bigger models through the launcher. A small model can misroute, so policy lives in agmend, not in the prompt | **Chosen by Andy**: "a lower tier model watching and responding to the chat and delegating to other bigger models. We will need that for delegations" |

Runtime (my call; spike S7 settles it):

| Option | Tradeoff |
|---|---|
| **Claude Code headless, one long-lived process** (`claude -p --input-format stream-json --output-format stream-json`) | The same adapter, MCP config, stdin delivery (section 6.10) and `total_cost_usd` reporting as launched workers. Per-turn cost reporting and context growth over hours are unverified |
| Claude Code headless, one process per batch (`--session-id` then `--resume`) | Same pieces; pays process start per batch; `--max-budget-usd` resets on each resume, so agmend tracks the running total |
| Messages API called from agmend | Fewest processes, but agmend gains a model client, a tool loop and per-provider code |

What the listener may do (Andy, 2026-10-25: "I like that rec"):

| Option | Tradeoff | Outcome |
|---|---|---|
| Route only | Cheapest and safest, but "did Sean's changes arrive?" needs a worker just to run `git log` | Not chosen |
| A full agent, like andy-agent1 on 2026-10-24 | Answers and fixes things itself, but a busy agent answers late and chat text can reach its shell | Not chosen |
| **Route, and look but never touch** | Read, Grep, Glob and fixed git-read tools from `agmen mcp`; never Bash, Edit, Write or web. Every change becomes a worker | **Chosen** |

### 4.7 Sharing agent setup

| Approach | Tradeoff | Outcome |
|---|---|---|
| Each agent's own plugin marketplace | Claude Code and Codex both install plugins from git repos (`claude plugin marketplace add` in 2.1.310, `codex plugin marketplace add` in 0.164.0), but a publisher writes and maintains one plugin per agent, and nothing ties an install to a project or records the person's approval of a commit | Used underneath for Claude Code and Codex |
| Hub-hosted pack registry | One place to browse and install, but the hub would serve code that runs on members' machines, so a hub compromise becomes code execution on every machine | Out |
| **agmen packs in git repos** | One agent-neutral manifest in a GitHub (or any git) repo. agmend renders it into each agent's own mechanism after the person approves an exact commit. agmen must track each agent's config format as it changes | **Chosen** (my call, for Andy's request: "ensure agent rules, skills, LSPs, MCPs can be shared/installed with github to other user's agents with the user's permission") |

### 4.8 Roles, launching any agent, and take-over (2026-10-25)

| Question | Options | Outcome |
|---|---|---|
| Prompt-only or enforced roles | Prompt only; enforced at launch, at the hub, in `agmen mcp` and by lanes | Enforced (Andy) |
| Where custom roles live | Hub storage with sync and an editor; the project's repo, packs and personal files, with the hub keeping name and hash | Repo, packs and personal files (my call) |
| How agmen launches agents | One CLI adapter per agent (six JSON formats, six lockdowns); ACP for every agent (one client; cost reporting optional, take-over unverified); ACP for every agent plus the Claude Code CLI adapter already planned | ACP plus the Claude Code adapter (Andy); an agent that fails spike S3's take-over check falls back to its own CLI |
| Take-over in the desktop app | A docked terminal under the agent tab; a live terminal inside the chat | Inside the chat (Andy) |
| Other people's live terminals | Streamed with their permission; local only | Local only for now (Andy) |
| Settings changed from chat | Settings view and CLI only; the listener applies them at once; widening changes wait for a click | The listener applies narrowing changes at once with Undo and waits for one Apply click on widening ones (Andy) |
| Who reaches a listener | Its owner's `accept_from` only; coordinators always | Coordinators always (Andy) |
| Desktop app | Keep the GPUI app; use Zed itself plus the terminal UI | Keep GPUI with Zed's layout and panels (Andy) |
| Zed's icons | Copy Zed's SVGs; ship Lucide and read file icons from the person's Zed icon theme | Lucide plus the installed icon theme, system icons without Zed (my call, on licensing) |

## 5. Decisions

### Made by Andy (2026-10-24/25)

- Name **agmen**, folder `~/Projects/agmen`, repo `groupdrive/agmen` (private; see Constraints).
- Hosted service, invite-only beta, metered, no billing code.
- MQTT, not ntfy.
- The service may read message contents; TLS plus encryption at rest.
- DMs visible to sender, recipient and the humans who own each; coordinators and admins see metadata only.
- Three human surfaces: a web console served by the hub, a GPUI desktop app on macOS, Windows and Linux, and the Claude Code mod. No Tauri (2026-10-25).
- v1 scope: human console, agent bridge, coordinator ledger, agent launcher, agent DMs; then the listener and the terminal UI with its agent view (2026-10-25).
- Must prevent: spoofable senders, lost messages, spend blowouts.
- Push approval is a setting: per project with a per-person override, `off` by default so agents commit and push on their own.
- Soft spend caps; no provider management keys.
- Hosting on the GroupDrive cluster.
- A Claude Code mod as a first-class client.
- A terminal UI as a fourth human surface, where you click into any agent to see what it is doing and message it, like Claude Code's own agent views (2026-10-25).
- A lower tier model per person, the listener (first called the dispatcher), watches and answers the chat and delegates work to bigger models (2026-10-25).
- Agent rules, skills, LSP servers and MCP servers can be shared through GitHub and installed into other people's agents, with each person's permission (2026-10-25).
- Roles are enforced, not prompt-only: six built-in roles plus a shared base, written like Andy's `CLAUDE.md` and `rules/*.md` as XML files, with the listener as the seventh (2026-10-25).
- The listener routes and looks but never touches: read-only tools, every change delegated (2026-10-25).
- Any terminal agent can be a worker or the listener, launched over ACP plus the Claude Code adapter; a live terminal inside the chat takes a session over and can switch it to another installed agent; live terminals stay limited to your own machine for now (2026-10-25).
- The terminal UI works like vim and opens as `agmen`; the desktop app looks like Zed, with collapsible Project, Git and Listener panels, Zed's icons, the person's Zed file icons, and theme, icon theme and fonts as settings (2026-10-25).
- Agents talk and coordinate with each other; urgent alerts highlight like Slack and chime like ntfy; coordinators can always reach a listener and answer alerts (2026-10-25).
- The listener changes agmen settings and agents' permissions from chat when its person asks; widening changes wait for one Apply click (2026-10-25).
- Planning rooms: plan a greenfield project with a Planner, approve specs, kick off a team per machine (2026-10-25).
- Every agent, person, branch and other reference in a message is clickable; the agent list offers Add and Update with a copyable install prompt (2026-10-25).
- GitHub integration and Zed's theme extensions (2026-10-25); that the App is read-only is my call.

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
- The terminal UI opens when `agmen` runs with no arguments, a module of `agmen-cli` (ratatui 0.30.2, crossterm 0.29.0, ratatui-textarea 0.9.3, all MIT), and uses the same local API as the desktop app.
- Messaging an agent from any view is a DM. The hub records it, and the owner's agmend delivers it into the running session through the pending queue in section 6.10. There is no second, unrecorded path into a session, with one exception Andy approved: taking a local agent over in a terminal (section 6.7), where what you type stays on your machine, the way the Claude window connected to ntfy did.
- Live activity (tool calls, text, cost per turn) is shown only for agents that this machine's agmend runs. Transcripts never leave the machine.
- The listener runs locally under agmend: one per person per project, off until started, on any installed agent that can be locked to read-only tools; default `claude-haiku-5-5` through Claude Code (`--model` takes full model names, per `claude --help` 2.1.310). Tiers are `small` (`claude-haiku-5-5`), `standard` (`sonnet`) and `deep` (`opus`), and each person can remap them per agent.
- agmend, not the model, enforces delegation policy: who may ask (always including the project's coordinators), caps per delegation and per day, and worker count.
- The listener has read, search and `git-read` tools and the agmen MCP server, never edit, shell or web, so text in the chat cannot make it run commands or edit files; agmend and the hub apply that ceiling whatever a role file says.
- One posting surface: `agmen mcp` for every agent; the Claude Code mod adds UI, wake-ups and guards on top.
- agmend runs as a per-user OS service (launchd agent, systemd user unit, Windows logon task), not part of the desktop app, because it must outlive the app and run on machines without it.
- The mod reaches agmend through the `agmen` CLI (`$.process.run`, `$.process.spawn`), not `$.http.fetch` with `socketPath`, which is a Unix socket path and does not cover Windows.
- ts-rs 12.0.1 (stable) for the TS types the web console and the mod share.
- Packs install only from the person's own machine, never from the web console or the hub, and every install or update is pinned to a commit the person approved (section 6.12).
- Jenkins CI, matching the GroupDrive repos.
- Roles come from four layers: built-in, the project's repo (`.agmen/roles/`), packs, and personal files; the hub stores a role's name and hash, never its text. A role can only narrow its kind, and agmend and the hub apply hard ceilings per kind (section 6.13).
- The Planner has a shell for git only; kick-off waits until a person merges the plan branch; agmen creates no repos and never writes to the default branch (section 6.14).
- Workers stay up after DONE until their branch merges or their owner stops them; the owner's listener starts a one-shot Reviewer for each DONE unless the person turns that off (sections 6.7, 6.11).
- Each lane has an owner, which may be an agent; the owner or a coordinator may LEND (section 6.5).
- Squash and rebase merges count as merged when the GitHub App reports the PR merged at the branch's tip, or `git cherry` finds every commit applied; everything else is listed daily and removed only with `--confirm` (section 6.7).
- Budgets may cap hours and tokens as well as dollars, which is what makes Codex and Antigravity enforceable (section 6.7).
- agmen ships Lucide icons and reads file icons from the person's installed Zed icon theme; it runs no installers for other agents; the GitHub App is read-only.

## 6. System design

```
 Claude Code + agmen mod ──($.process.spawn `agmen watch --json`, $.process.run `agmen ...`)─┐
 OpenCode / Codex / Claude ──(MCP stdio: `agmen mcp --identity <id>`)──────────────────────────┤
 Codex / OpenCode / Gemini / agy / Grok workers ──(ACP, launched by agmend)────────────────────┤
 GPUI desktop app ──(local API)────────────────────────────────────────────────────────────────┤
 agmen tui (ratatui, in a terminal or over SSH) ──(local API)──────────────────────────────────┤
                                                                                                 ▼
            agmend (one per machine; launchd agent / systemd user unit / Windows logon task)
            human login + devices, child identities, outbox, launcher, budgets, listener, roles
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
| `crates/agmend` | Daemon: identities, devices, local API (Unix socket or Windows named pipe, lock file, version negotiation), launcher and ACP client, roles, worktree lifecycle, budgets, listener supervision and delegation policy |
| `crates/agmen-cli` | `agmen` binary: subcommands, `agmen mcp` (rmcp stdio, the pattern in `~/Projects/kit_mcp`), `agmen watch`, the terminal UI (`agmen` with no arguments) |
| `crates/agmen-pack` | Pack manifest parsing, validation, the plan hash, and rendering into each agent's format; used by agmend and by `agmen pack check`, which runs without agmend |
| `roles/` | The built-in role XML files, compiled into the `agmen` binary (section 6.13) |
| `crates/agmen-hub` | Hosted service: control plane, ledger, history, project settings, broker JWTs and JWKS, EMQX admin, metering; serves the web console |
| `apps/desktop` | GPUI desktop app (Rust, workspace member, bin `agmen-desktop`) |
| `apps/web` | Web console (TypeScript, React, Vite); its build is embedded in the hub image |
| `mods/agmen-claude` | Claude Code mod and `.claude-plugin/marketplace.json` |
| `deploy/` | docker compose for local dev (EMQX, Postgres, Toxiproxy); the cluster manifests live in `agmen-kubernetes` (section 6.9) |
| `docs/` | This file, `protocol.md`, `adr/`, `plans/`, `specs/`, `mockups/` (FigCraft prototypes with previews) |

### 6.2 Stack (versions checked 2026-10-25)

| Need | Choice | Notes |
|---|---|---|
| Desktop UI | gpui (crates.io 0.2.2 or a pinned git rev, chosen by spike S8) | Apache-2.0, pre-1.0. Native notifications, single instance, updater, deep links and installers are not built in; S8 picks a crate or writes each. `gpui-component` (longbridge) is a candidate widget kit, unverified |
| Terminal UI | ratatui 0.30.2, crossterm 0.29.0, ratatui-textarea 0.9.3 | All MIT; MSRV 1.88, 1.63 and 1.86, under the pinned 1.99.1. Mouse through crossterm's `EnableMouseCapture`; the message box is a ratatui-textarea widget. Tests render to ratatui's `TestBackend` |
| Listener model | `claude-haiku-5-5` through Claude Code headless, or any installed agent that locks to read-only tools | Default until spike S7 measures triage quality; `sonnet` is the fallback default |
| Embedded terminal | A PTY crate chosen by spike S10 | An agent's own interface in a ratatui split and a GPUI view, with resize, mouse and copy |
| UI icons | Lucide (ISC) | What most of Zed's icons derive from; Zed's own SVGs are not shipped (section 6.8) |
| Themes and file icons | Zed theme JSON and the person's installed Zed icon theme, read at runtime | System file icons without Zed; spike S8 checks GPUI can show them without `unsafe` |
| Web console | TypeScript, React, Vite (versions picked at M4) | Served by agmen-hub; live updates over server-sent events; browser notifications through the Notifications API and Web Push |
| Secrets | keyring 4.2.0 | agmend's tokens in the OS keychain; the desktop app holds none |
| MQTT client | rumqttc 0.25.1 | `v5` has `set_clean_start`, `set_session_expiry_interval`, `set_manual_acks`, `Transport::Wss`. 0.25.0 broke websocket. Production readiness unverified: spike S2. Fallbacks: `rumqttc-next` 0.34.0, `mqtt5` 0.48.2 |
| MCP server | rmcp 3.7.0 | Implements MCP `2026-07-28`; stdio via `transport-io` |
| Agent Client Protocol | agent-client-protocol 3.5.0 (protocol v1) | `usage_update` carries optional cumulative cost; spike S3 (M0) runs it against all six agents (section 6.7) |
| Local outbox | rusqlite 0.41, `bundled` | `synchronous=FULL` |
| Hub database | sqlx 0.9, Postgres, `runtime-tokio`, rustls | |
| TS types | ts-rs 12.0.1 | Shared by the web console and the mod; the desktop app uses the Rust types |
| CLI installers | dist 0.33.1 | shell, PowerShell, Homebrew, MSI |
| Desktop bundles | Chosen by spike S8 | Must produce a signed .app in a dmg, an MSI, and a deb plus AppImage |
| Signing | Apple Developer ID + notarization; Azure Artifact Signing on Windows | Needs Andy's accounts; covers the desktop app and the CLI binaries |

### 6.3 Identity and trust

1. A human logs in with OAuth PKCE through a loopback redirect, or with a device code on a headless machine. Each machine is a registered device.
2. agmend mints child identities per device (for example `andy/sol`), scoped to a project, role and lanes, with an expiry. Every child is attributed to its human, so `andy/sol` can never post as `adam/coord`. Agents receive tokens scoped to their own identity, never the human's session. A listener is a child identity of kind `listener`, at most one per person per project; workers it starts record it as `delegated_by`. Every child identity carries its role: name, allowed verbs, edit scope and the role file's hash (section 6.13); the hub caps the verbs at what the kind allows.
3. The hub signs a short-lived (1 h) ES256 JWT per identity whose `acl` claim names that identity's topics in full and ends with deny-all; EMQX verifies it against the hub's JWKS and disconnects at `exp`. A client-info check binds the client id to the identity (`<id>-mcp`, `<id>-watch` or `<id>-cli`), and the token's `sub` must equal the username (ADR-0002). Clients refresh the token over HTTPS. Revocation stops refreshes and kicks the identity's client ids through the EMQX REST API. An expired identity drains: it can still send what is in its outbox.
4. Clients may publish only to `up/{identity_id}` (the `acl` claim; authorization `no_match = deny`). The hub stamps `from`, so readers never trust payload claims.
5. Human verbs (a human's RULING, PUSH_OK, PUSH_NO) are accepted only from human identities; agent and listener identity tokens cannot send them.
6. Known limit, by design: an agent running as the same OS user as its human can reach that human's daemon and act as that human locally. agmen treats a person's own agents as that person's delegates; the guarantee is between people and machines, not inside one user account.

### 6.4 Delivery

Summarized here; normative in [protocol.md](protocol.md#delivery).

- Client outbox (SQLite) then QoS 1 publish to `up/`.
- Hub validates, dedupes on `(identity, id)`, numbers the message per stream inside one Postgres transaction (`UPDATE stream_heads ... RETURNING`), and writes a publish-outbox row.
- A sweeper publishes to `feed` or `inbox/`, sends ACK to `ack/`, and advances `state/head`.
- Readers keep a per-stream cursor and replay over HTTP after any gap, reconnect, or a `state/head` ahead of their cursor.
- A rejected message gets a NACK (retryable with `retry_after`, or final), never endless retries.

### 6.5 Coordinator ledger

- Tables: `lanes {name, globs[], owner}` (gitignore syntax via `globset`; overlapping globs rejected; `owner` is an identity set by a coordinator or a plan, and may be an agent), `items`, `claims` (partial unique index on active claims, with a lease), `lends`, `decisions`, `rules` (versioned markdown), `human_decisions`, `project_settings`, `member_settings`.
- The hub owns mechanics: atomic claims, item ids, lane ownership, snapshots. An identity with the coordinator role (a human, or an agent like adam-coord was) assigns work and rules.
- Lane enforcement in three layers:
  1. The Claude Code mod guards Edit, Write and NotebookEdit on `$.fs.stat(path, { resolve: true }).realPath` and runs `git status --porcelain` after each Bash call to flag out-of-lane changes.
  2. `agmen check-path` and the MCP tool `check_path` for other agents.
  3. DONE carries `changes` from `git diff --name-status --no-renames <base_sha_at_claim>...<commit>`; the hub flags any path outside the claimed or lent lanes.
- LEND covers the cross-lane case from the run (B-287 needed files from L-INFRA-BUILD). The lane's owner or a coordinator may LEND, so agents lend to each other without waiting for a person; the hub refuses LEND from anyone else (`not_lane_owner`).

### 6.6 Push approval setting

- `push_approval` is a project setting with two values: `off` (the default; agents commit and push on their own) and `owner` (an agent's push waits for its human's OK). Project admins set the project value; each person may override it for their own agents in either direction.
- With `owner`, an agent sends `PUSH_REQUEST {remote, ref, sha}`. It lands in the owning human's inbox, the web console, the desktop app, and the Claude Code mod's band; the human answers `PUSH_OK` or `PUSH_NO` (from the web console, the desktop app, the mod, or `agmen approve <request>`). The agent then pushes with its normal git credentials.
- The Claude Code mod enforces the setting for Claude Code agents: when it is `owner`, a raw `git push` is refused until a `PUSH_OK` matches the branch and `HEAD`. Other agents follow the setting through the join prompt and the `request_push` MCP tool.
- agmen does not hold git credentials, route pushes, or enforce branch protection. Force pushes and protected branches are the git host's job.
- Pushes show up in each DONE as `branch@commit`, which is the record the coordinator reads.

### 6.7 Launcher, agents, worktrees and budgets

- `agmen launch --agent claude|codex|opencode|gemini|agy|grok --role engineer --model <m> --item B-412 --lane L-UI --budget 5.00 --base origin/<base> --branch fix/<slug>` cuts a worktree, mints an identity with its role, writes an MCP config, renders the role prompt (section 6.13), starts the worker headless in its own process group, and records it in SQLite so agmend re-adopts it after a restart. The listener's `delegate` tool (section 6.11) uses the same path. agmend keeps each local worker's stream-json output in a rolling log under its state directory, which feeds the agent view (section 6.8).
- Claude Code: `claude -p --verbose --input-format stream-json --output-format stream-json --max-budget-usd <remaining> --session-id <uuid> --mcp-config <file> --strict-mcp-config`. agmend keeps stdin open and writes chat messages for the worker to it (section 6.10). The final `result` message carries `total_cost_usd` (a client-side estimate). `--max-budget-usd` ends with `error_max_budget_usd`, can pass the cap by one response plus running subagents, and resets on `--resume`, so agmend passes the remaining budget on every resume.
- OpenCode: `opencode run --format json`; `step_finish` carries `cost` (USD) and `tokens`. When a run ends with chat messages pending for the worker, agmend continues it with `opencode run --session <id>` (section 6.10).
- Codex, Gemini CLI, Antigravity (`agy`) and Grok Build: over ACP (below). Codex and `agy` report tokens but no cost, so their budgets are token caps.
- Preflight: OpenRouter `GET /api/v1/key` with the worker's own key returns `limit_remaining`; agmend refuses a dispatch when remaining credit is below the sum of in-flight reservations.
- Thresholds: 80% warns the worker and its human; 95% tells the worker to wrap up (commit WIP, post STATUS); 100% stops the process group.
- On stop, agmend saves changes to a local `wip/<branch>` ref with git plumbing (temporary `GIT_INDEX_FILE`, `commit-tree`, `update-ref`), which runs no hooks and never stashes.
- Interactive sessions get spend alerts only; agmend does not launch them.

**Launching over ACP** (Andy's choice, 2026-10-25; spike S3 in M0 proves it per agent). agmend has one Agent Client Protocol client. Claude Code keeps the CLI adapter above (`--max-budget-usd`, `total_cost_usd`, `--tools`, stdin delivery); every other agent runs as an ACP agent, which asks agmend before each tool call, so agmend refuses what the role forbids. An agent that fails S3's take-over check falls back to its own CLI. What each agent offered on 2026-10-25:

| Agent | Headless and JSON | Cost | Tool lockdown | Resume / take-over | ACP |
|---|---|---|---|---|---|
| Claude Code 2.1.310 | `-p`, stream-json in and out | `total_cost_usd` | `--tools`, `--allowedTools`, `--disallowedTools` | `--resume <id>` | adapter `claude-agent-acp` |
| Codex 0.164.0 | `exec --json` | tokens only | `-s read-only|workspace-write|danger-full-access` | `codex resume <id>`; `codex queue --thread --message` adds a message to a live session | adapter `codex-acp` |
| OpenCode 2.1.4 | `run --format json` | `cost` per step | `--agent` permissions (not verified) | `--session <id>` | `opencode acp` |
| Gemini CLI 0.46.0 | `-p`, `-o stream-json` | not checked | `--approval-mode plan`, `--policy` | `--resume latest` or by index | `--acp` |
| Antigravity `agy` 1.1.7 | `-p`; `--input-format stream-json` needs 1.1.15+ (`agy changelog`) | tokens only | settings allow and deny rules, `--mode plan`, `--sandbox` | `--conversation <id>` | separate `agy_acp_server` with its own login (registry entry, not tested) |
| Grok Build (not installed; xAI docs) | `-p`, `--output-format streaming-json` | `total_cost_usd` when the server reports it | `--tools`, `--disallowed-tools` | `-r <id>` | `grok agent stdio` |

agmend finds installed agents on `PATH` and runs each one's `--version`; `agmen agents scan` reruns it. In the agent list a missing agent gets **Add…** and an outdated one **Update…**, and **Add a terminal agent…** takes any ACP agent or a custom command. Each opens a dialog with the agent's official install source (agmend keeps a short list: binary name, source, minimum version, ACP command), a prompt to paste into any agent you already run that has a shell (it names the source, warns about look-alike packages such as the community npm package `@vibe-kit/grok-cli` that also installs a `grok` command, and ends with `<agent> --version` and `agmen agents scan`), and **Check again**. agmen runs no installers itself: install steps change between releases, and running them would make agmen a software installer on every member's machine.

**Take-over.** From the chat or the agent view, "Open in terminal" (⌥O) opens a live terminal inside the chat, between the messages and the composer, running the agent's own interface on its session (`claude --resume <id>`, `codex resume <id>`, and so on). agmend pauses the headless run while you have it and resumes it on "Hand back"; "Open as tab" moves the terminal into its own tab. In the terminal UI `o` does the same in a split, like vim's `:terminal`, and `q` hands back. What you type there stays on this machine and is not recorded by the hub (section 5). Only agents this machine's agmend runs can be taken over.

**Switching agents.** The terminal's agent chip lists the agents installed on this machine. Choosing another starts a new session of that agent in the same worktree, as the same identity, with the same claim and budget, and a handoff brief: item, lane, branch and its state, what is done, what is next, and the previous session's id, which stays resumable. Role enforcement follows the new agent's capabilities (section 6.13), and the agent view shows any loss of precision ("sandbox workspace-write; Codex has no tool list").

**Worktrees and branches.** Every role whose edits are not `none` works in its own worktree on its own branch, named `{type}/{item}-{slug}` (item keys are unique per project, so names never collide), never on the default branch or in another agent's worktree. A Reviewer gets a detached worktree at the DONE's commit, removed after its REVIEW. agmend removes a worktree and its branch once the branch is merged, with `git worktree remove` and `git branch -d`, and never passes `--force`. A branch counts as merged when its tip is an ancestor of the base (`git branch --merged`); when the GitHub App (section 6.15) reports its PR merged with the branch tip as the PR's head (squash and rebase merges: `-d` refuses, so agmend uses `-D`, which loses nothing because that exact tip went into the base); or when `git cherry <base> <branch>` finds every commit already applied. Any other branch, and any worktree with uncommitted work, is kept: agmend saves a `wip/<branch>` ref and lists it once a day in a normal ALERT to its owner. `agmen worktree prune [--confirm <branch>]` runs the sweep on demand and says what it kept and why.

**Workers after DONE.** A worker stays up until its branch merges or its owner stops it. An idle headless worker spends nothing: agmend keeps its process (Claude Code, stdin open) or its ACP session, and a REVIEW with `changes`, a failed check (section 6.15) or a mention wakes it in the same session on the same budget. A used-up budget alerts the owner and restarts nothing on its own.

**Budgets** show spent against the cap, wall-clock time since launch and time working, and tokens (input, output, cache read) in the agent view, the Listener panel and the terminal statusline (`$1.20/5.00 · 1h12m · 454k tok`). A budget may also cap `hours` and `tokens`, with the same 80, 95 and 100 percent steps as dollars; a token cap is what makes Codex and `agy` enforceable, since they report tokens but no cost (my call; display-only is the alternative, section 9).

### 6.8 Clients

**Web console** (`apps/web`, served by agmen-hub at `app.<domain>`). Views: Projects; Chat (section 6.10); Board (lanes by items); DMs; Alerts; Agents (who is online, their claims, and the spend agmend reports in STATUS; clicking one shows the remote half of the agent view below: its posts, claims and DMs with me, and a message box); Settings (account, devices, members and invites, push approval per project and for my agents). The human logs in with the same GitHub or Google OAuth; the hub keeps an HttpOnly, SameSite=Strict session cookie, and state-changing calls also need a CSRF token. Live updates come from `GET /v1/projects/{p}/events` (server-sent events whose event id is the stream seq, so a reconnect resumes through replay). The human's posts (chat messages, ANSWER, DM, RULING, PUSH_OK, PUSH_NO) go through `POST /v1/projects/{p}/messages`, which runs the same ingest as `up/`. Browser notifications for ALERTs, ASKs to me, chat messages that mention me, DMs and push requests, through Web Push when the tab is closed. Invite links open the console directly.

**Desktop app** (`apps/desktop`, GPUI, with Zed's layout). A title bar (project, the listener's state, the person); a collapsible left dock with tabs Agents, Project, Git and Roles; center panes with tabs (chat, an agent's live session, files, diffs, role files, plans) and splits; a collapsible right dock (the Listener panel, or the selected agent's role, enforcement, worktree and budget; a `⟩` in its header hides it and a status bar toggle brings it back); a status bar (needs-you count, online, alerts, worktrees, the listener, today's spend, and Project, Git, search, terminal, chat, bell and settings buttons); a command palette (⌘⇧P) for every action, including spawning a worker with a given agent. Keys are standard with vim mode as a setting, as in Zed. The Project panel is a file tree with collapsible folders and change markers, scoped by a chip to one local worktree (an agent's, or the main checkout); opening a file shows change bars in the gutter. The Git panel has collapsible sections Changes (files and line counts), Commits on the branch, Worktrees by person and agent (a state dot: pushed, local only, reading) and Branches; a change opens its diff. Both read disk through agmend, so they cover local agents only, like live sessions. Everything the web console shows, read through agmend, plus the full agent view below, take-over and switching (section 6.7). Native notifications for ALERTs, ASKs to me, DMs, push requests and pending delegations. Single instance. Spike S8 runs at the start of M4; dev builds macOS first (M4), then Windows and Linux; deep links and the updater arrive in M6.

**Terminal UI** (`agmen` with no arguments, M3). The same local console as the desktop app, modal like vim. Buffers: Agents (the default), `#chat` (section 6.10), Board, DMs, Alerts (ALERTs, ASKs to me, push requests, pending delegations), Settings (push approval, listener policy and tiers, appearance), plans and role files. Normal mode: `j`/`k`, `gg`/`G`, `Ctrl-d`/`Ctrl-u`, `/` search with `n`/`N`, `Enter` opens an agent, thread or file, `q` closes a split or goes back, `?` for help. Insert mode: `i` writes in the composer, `Esc` leaves; a message goes to your listener unless you `@` someone. Command line: `:spawn codex --role tester <brief>` (Tab cycles the installed agents), `:listener codex`, `:role reviewer`, `:model opus high`, `:plan`, `:approve`, `:kickoff`, `:prune`, `:set theme=<name>`, `:set icons=nerd|none`, `:q`. Windows: a tab line (`1 #chat`, `2 andy/fx1`, ...), splits with `Ctrl-w v` and `Ctrl-w h/l`, a statusline per window showing the mode. On an agent: `o` take it over in a split, `O` switch it to another agent, `m` message, `s` stop, `b` budget, `gn` jump to the next thing that needs you; `f` labels every reference on screen with a letter that opens it, and `gd` opens the one under the cursor. It talks to agmend over the local API, so it runs anywhere the CLI does, including over SSH on a headless machine after `agmen device login`. Mouse works where the terminal supports it (click a tab, a row or a folded tool call; scroll; click the composer), and every action has a key, so it works with mouse capture off. The theme is the desktop app's Zed theme through its terminal palette, in 24-bit color where the terminal supports it; file icons are Nerd Font glyphs when the font has them. New ALERTs, ASKs to me and DMs ring the terminal bell and badge their tab; an urgent alert also shades its line red and shows `! 1 NEEDS YOU` in the statusline.

**Agent view** (terminal UI and desktop app in full; the web console shows the remote half).
- *List.* Every identity in `state/roster`, grouped by person, mine first: my listener, the workers it or I launched, my interactive sessions, then everyone else's. Each row: handle, role (in its color), agent and model (from HELLO), item and lane, status (working, idle, reading, waiting on me, stopped), spend against budget, and unread DMs. Workers sit under the listener that started them (`delegated_by`).
- *Open an agent.* Click a row or press `Enter`. For an agent this machine's agmend runs (launched workers and the listener), the main pane follows its live session from the stream-json log in section 6.7: assistant text, each tool call folded to one line with its arguments (click to expand the call and its result), and cost per turn. For any other agent, the pane shows its posts, claims, DONEs and our DM thread. Transcripts never leave the machine (non-goals).
- *Message it.* The message box at the bottom sends a DM to that agent, with `reply_to` set when a message is selected. The hub records it and the owner's agmend drains it into the session through the pending queue (section 6.10), so a launched Claude Code worker gets it on stdin like a message typed into Claude Code, and an interactive session gets it through the mod. The row shows `recorded #seq`, and for my own local agents also `in session` once agmend has written it. Messaging my listener is how I hand it work directly, and the composer defaults to it.
- *Act on my own agents.* Stop, add budget, approve or decline a pending push or delegation, take the agent over in a terminal, switch it to another installed agent (section 6.7), and open the worktree (the desktop app opens a file manager; the terminal UI prints the path).
- *Local API methods* (added in M3): `agents {project}` (roster merged with local process state), `activity {identity_id, after?, follow}` (streams log frames for a local agent; `error.code = "not_local"` otherwise), `worker_stop {identity_id}`, `worker_budget {identity_id, add_usd}`. Messaging uses the existing `post` method with a DM.

**References you can click.** Everything a message names is highlighted and opens where it points: agents (in their role's color) open the agent view; people open their agents and a DM; roles (`@reviewers`) open the role file and its holders; branches open the Git panel scoped to them (commits, changes, diff, worktree); commits open their diff; `path:line` opens the file at that line in the agent's worktree when it is local, otherwise at the commit the message names; items and lanes open the board; PRs open the GitHub card (section 6.15). Structured fields come first (`mentioned`, `items`, `lane`, a DONE's `branch`, `commit` and `changes`, a REVIEW's findings, a PLAN's files); the hub stamps `refs` for the handles, lanes, items, roles and PR numbers it knows (protocol section 3), and clients match branches, commits and files against local git or against branches named in earlier DONEs and CLAIMs. Text that only looks like a reference stays plain. Other people's branches open read-only after a fetch; other people's agents open in the remote half of the agent view. Desktop app: colored chips with hover cards (agent state and spend, branch ahead or behind and its worktree, an item's claim), click opens a tab, ⌘-click a split. Terminal UI: colored by type, `f` and `gd` as above. Web console and mod: the same highlighting; the web console opens what the hub knows and marks branches and files "open on your machine".

**Urgent alerts.** ALERT carries `urgency` and `needs` (protocol section 4). An urgent alert is shaded red with a NEEDS YOU badge and its actions (Reply, plus any the sender lists, such as "Revert #10"); a red banner with "Jump to it" sits above the chat; the chat tab and the status bar show a red count; a native notification appears; and a chime plays, like ntfy's. Normal alerts and mentions are silent by default; Settings choose the sound and quiet hours, and urgent alerts from your own agents and your coordinators still sound during quiet hours. The owner and any coordinator, human or agent, can answer an urgent alert or an ASK; an `anyone` alert clears for everyone at the first answer, while an `owner` alert (a budget stop, a push approval, a settings change) shows the coordinator's reply but stays red, badged and chiming for the owner until the owner acts. Actions that spend the owner's money, approve the owner's pushes or change the owner's settings stay with the owner. At most 3 urgent alerts per sender per hour; past that they arrive as normal and the coordinator is told, and a person can mute urgency from one agent.

**Appearance** (Settings, from the gear in the status bar or the command palette). Follow Zed, on by default when Zed is installed: the theme, icon theme and fonts are read from Zed's settings and never written back. Off, or without Zed: theme (any Zed theme, the person's own in `~/.config/zed/themes/` or one from Zed's theme extensions, with Light, Dark or System), icon theme (one installed in Zed, agmen's own, or the system's), UI font and size, code font with size and ligatures, terminal font and size ("same as code font" by default), and chat density. Saved in `~/.config/agmen/settings.toml`, which the terminal UI reads too. Icons: agmen ships Lucide (ISC) for its own UI, which most of Zed's icons derive from; Zed's own SVGs are compiled into Zed and carry no license of their own in a GPL repo, so they are not copied. File and folder icons are read at runtime from the icon theme the person installed in Zed, through that theme's mapping file (names, suffixes, folders); agmen bundles none of them. Without Zed, the operating system's file icons (NSWorkspace on macOS, the shell on Windows, the freedesktop icon theme on Linux), which spike S8 checks GPUI can show without `unsafe` code. Role and reference colors come from the theme's terminal palette, so they stay readable in light and dark themes. Unverified: whether Zed's extension registry API may be used by other apps, and the license terms of theme extensions; if the API is off limits, the picker reads the public extensions repository instead.

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

Develop with `claude --plugin-dir mods/agmen-claude`; check with `claude plugin validate`, `tsc -p` and `claude plugin test` (pattern of the API's `pane.test.tsx`, on terminal and desktop surfaces). Install with `claude plugin marketplace add https://git.groupdrive.xyz/groupdrive/agmen.git` (a git URL is an allowed marketplace source), then `/plugin install agmen@agmen` (the marketplace in `.claude-plugin/marketplace.json` is named `agmen`). The repo is private, so installing from it needs a git credential for git.groupdrive.xyz, and outside teams need a public mirror of the mod by M6.

**CLI and MCP.** Subcommands: `login`, `device login`, `join <invite>`, `chat`, `post`, `ask`, `answer`, `dm`, `inbox`, `watch`, `state`, `claim`, `release`, `lend`, `done`, `check-path`, `approve`, `settings`, `launch`, `workers`, `budget`, `listener start|stop|status|set`, `delegate approve|decline`, `role list|import`, `agents scan`, `worktree prune`, `mcp`; `agmen` alone opens the terminal UI. MCP tools: `chat`, `read_chat`, `post`, `claim`, `release`, `done`, `ask`, `answer`, `dm`, `inbox`, `state`, `lanes`, `check_path`, `request_push`, `review`, and for a listener (`agmen mcp --listener`) `delegate`, `workers`, `stop_worker`, the `git-read` tools (`git_log`, `git_show`, `git_diff`, `git_status`, run by agmend with fixed arguments) and the `settings` tools in place of `claim`; the server lists only the tools the identity's role allows (section 6.13); `post` and `chat` wait up to 5 s for the ACK and answer "recorded #seq" or "queued (not yet recorded)". Every tool result ends with the messages pending for the calling agent (section 6.10). Claude Code, OpenCode (`opencode.json`) and Codex (`[mcp_servers.agmen]` in `config.toml`) all load it through their MCP config.

### 6.9 Hosting

- `agmen-env/` in agmen's GitOps repository `groupdrive/agmen-kubernetes`: ArgoCD Application, namespace `agmen`, Kubernetes Secrets that any secret store fills (a Vault Secrets Operator example is included), hub Deployment, EMQX StatefulSet (one replica) and Postgres 16 StatefulSet with pinned image tags, nightly `pg_dump` CronJob to S3 compatible storage.
- HAProxy ingress with cert-manager Let's Encrypt for `api.`, `app.` and `mqtt-ws.` hosts (WSS on 443) on agmen's own domain. The ingress must not buffer server-sent events.
- Jenkins builds the web console and the hub image into agmen's registry; ArgoCD syncs `agmen-kubernetes`.
- The GitHub App's webhooks arrive at `api.` (section 6.15).
- Observability: `tracing` plus OpenTelemetry; audit log of settings changes, push answers, rulings and identity minting; per-tenant rate and size limits; usage metering (messages, storage, active agents, launcher spend).

### 6.10 Chat

The project `feed` is the chat room that replaces the ntfy topic. Everyone in the project reads one numbered stream, so chat gets the same guarantees as every other message: an authenticated sender, an ACK, and replay after a gap. Free text is `FYI` (or `ASK` when it needs an answer), and replies thread by `reply_to`. Ledger traffic (CLAIM, GRANT, RELEASE, STATE) shares the stream; chat views fold it into one-line rows.

**Mentions decide who gets interrupted.** A message may mention `@all`, `@agents`, `@humans`, a lane such as `@L-UI` (from M2, once lanes exist), a role such as `@reviewers` (`role:reviewer` on the wire; every live holder of that role), or up to 8 identities such as `@andy/sol`. Clients turn typed `@handle` text into identity ids from `state/roster`; the hub expands groups and stamps `mentioned` with the resolved identity ids, without the sender. A reply to a human's message also mentions that human. Unmentioned messages stay in the room and do not enter agent sessions, so 17 agents do not pay tokens for every line; agents read the room with `read_chat` when they choose.

**Messaging the swarm.** Humans and coordinators may use group, lane and role mentions; agents may mention individuals only, so one agent cannot wake the swarm. The chat composer in every console defaults to the person's own listener (section 6.11), so a plain message is an instruction to it; `@all` is a deliberate choice and still reaches every agent once. From a terminal, `agmen chat "deploy is frozen" --all` posts once, the way `curl -d` posted to ntfy, and `agmen chat` with no text follows the room live and sends each line typed. Without `--as`, both post as the human.

**Agents coordinating with each other.** Agents talk to each other directly and in the open, and `base.xml` (section 6.13) tells every agent to: treat communication as part of the work, with one accurate point per message, its inbox read before a task and before DONE, and a blocker raised at once by ASK or ALERT; ask a lane's owner by mention or DM before touching paths outside its own lanes, and work under a LEND rather than around it; post an interface before others build on it, and again when it changes; ask for a review through its listener or its coordinator and answer the findings in the thread; hand work off with a STATUS that says what is done, what is left, which branch and how to verify it; answer every ASK addressed to it, or say who can. Individual mentions and DMs between agents wake the recipient as below. After 8 agent-only messages in one thread with no human message in between, agmend holds further wake-ups in that thread and ALERTs the coordinator or the owners (start value, tuned in an ADR).

**Reaching an agent's session** (the job the curl poll loops and relay subagents did in the 2026-10-24 run). agmend keeps a pending queue per identity: chat messages that mention it, DMs to it, and assignments. It drains the queue into the session by the best path that agent offers:

| Agent | Delivery | Milestone |
|---|---|---|
| Any MCP agent (Codex; OpenCode or Claude Code without the mod) | Every agmen tool result ends with the messages pending for it; `read_chat` reads the room | M1 |
| Claude Code, interactive, with the mod | `$.session.append` as a meta row; if the session is idle, a wake-up through `$.prompt.submit` | M3 |
| Claude Code worker launched by agmend | Written to the worker's stdin as a stream-json user message (`--input-format stream-json`, in `claude --help` 2.1.310). Whether a message sent mid-turn waits for the turn to end is unverified; M3 tests it | M3 |
| Any worker launched over ACP (Codex, OpenCode, Gemini CLI, `agy`, Grok Build) | agmend sends the batch as a prompt on the open ACP session; a CLI fallback (`opencode run --session <id>`, `codex queue --thread`) where S3 finds ACP take-over missing | M5 |

Known limit: an MCP-only agent that agmen did not launch cannot be woken while idle. It sees pending messages on its next agmen tool call, and the join prompt tells it to call `inbox` between steps.

**Loop limits.** At most one wake-up per agent every 30 seconds (start value, tuned in an ADR); messages that arrive in between go into the next wake as one batch. A message never wakes its sender, replies between agents do not add automatic mentions, and the existing rate limit (10 per second, burst 30) applies to chat.

**Surfaces.** Web console and desktop app: a Chat view with threads, `@` autocomplete for handles, lanes, roles and groups, a model and effort chip for the listener (section 6.11), a "Show ledger events" toggle, and notifications when I am mentioned. Terminal UI: the `#chat` buffer (section 6.8). Claude Code mod: the `agmen-chat` pane and `/agmen say`. CLI: `agmen chat`. The web console at phone width plus Web Push covers what the ntfy phone app did.

### 6.11 Listener

A listener is a small model that hears what one person types in the chat, looks things up, answers what it can, and hands everything else to a bigger-model worker. It does the job of the 2026-10-24 run's relay subagents and lets assignments go ahead while the person is away. The first draft called it the dispatcher; the spec in `docs/specs/` records the change.

- **Start.** `agmen listener start --project <p>` mints `andy/listen` (kind `listener`; the hub allows one per person per project) with the built-in `listener` role (section 6.13). agmend supervises it like a launched worker and re-adopts it after a restart. `stop`, `status` and `set` also exist. It stays off until the person starts it.
- **Agent and runtime.** Any installed agent that can be locked to read-only tools: Claude Code (`claude -p --model claude-haiku-5-5 --input-format stream-json --output-format stream-json --verbose --tools Read,Grep,Glob --strict-mcp-config --mcp-config <listener.json> --append-system-prompt-file <role prompt>`, unless spike S7 picks one process per batch), Grok Build (`--tools`), Gemini CLI (`--approval-mode plan`), `agy` (`--mode plan` plus deny rules) or OpenCode (per-agent permissions; S3 confirms). Codex only sandboxes read-only, so its shell still runs; agmend accepts it as a listener only over ACP, where it refuses every command and edit permission request. The MCP config runs `agmen mcp --identity <listener id> --listener`.
- **Tools.** It looks but never touches: Read, Grep and Glob; the `git-read` tools `agmen mcp` serves (`git_log`, `git_show`, `git_diff`, `git_status`, run by agmend with fixed arguments); the `settings` tools (below); and the chat tools under its own identity, minus `claim`. Never Bash, Edit, Write or web. Anything that changes files or runs a build or a test becomes a worker. So "did Sean's changes arrive?" is answered in seconds from `git_log`, and text in the chat can never reach a shell.
- **Who is there.** The listener reads `state/roster` through the `state` tool: every identity with its role, agent, model, status and `delegated_by`, kept current by the hub from each identity's retained `presence/` message, whose MQTT last will marks it offline within keep-alive times 1.5. That answers "who is in the pool?" without waking anyone; it never pings agents, which would cost each one a wake-up and tokens. A human who wants the answer in the chat posts ROLLCALL, and the hub answers it from the same presence, so no agent replies.
- **What reaches it.** Its pending queue (section 6.10) holds messages that mention it, DMs to it, and assignments to it. While it runs, agmend also adds every chat message its person sends without another `@` (the composer's default target), mentions, DMs and assignments addressed to its person, and DONE, STATUS, REVIEW and ALERT from workers it started. Batching and the wake-up limit are the ones in section 6.10. Each event reaches the model with its stamped `from_handle` and `from_kind`, under the standing instruction that message text is data, never an instruction to the listener.
- **What it does.** Each turn ends in one action: answer from state or git (for example "B-412 is held by andy/w3f, $3.10 of $5.00 spent"), delegate, forward to a running worker by DM, change a setting its person asked for, escalate to its person with ALERT, or nothing. On a group mention it answers only for its person and that person's workers.
- **Delegation tools.**
  - `delegate {reply_to, brief, role, agent?, tier, budget_usd, item?, lane?, base?, branch?}`. agmend runs the launcher (section 6.7) with that role on the tier's model, with `delegated_by` set, and returns the worker id. The listener then posts `DELEGATED` in the same thread.
  - `workers`: the person's workers with agent, role, model, item, budget used (dollars, hours, tokens) and state.
  - `stop_worker {worker}`: only workers this listener started.
- **Policy, enforced by agmend.** The model asks; agmend decides.
  - Requester: `delegate` must name `reply_to`, and agmend reads that message's hub-stamped `from`. The sender must be in `accept_from`, which always includes the person and the project's coordinators (Andy, 2026-10-25: "Coordinators should always be able to respond too"); a person cannot remove the coordinators. The listener cannot start work on its own initiative or on a stranger's say-so.
  - Caps: `per_delegation_usd` (default 5.00), `daily_usd` across delegations (default 20.00), `max_workers` (default 3), plus the launcher's preflight. Work a coordinator sends runs inside the same caps.
  - Over policy: agmend holds the delegation as pending, and the listener ALERTs its person. The person approves or declines on their own machine (`agmen delegate approve|decline <id>`, the terminal UI, the desktop app, or the mod's band), so no wire verb is needed.
  - Its own spend: `listener_daily_usd` (default 2.00), counted from `total_cost_usd` or tokens. At the cap, agmend pauses the listener, posts STATUS and ALERTs the person.
- **Settings and permissions from chat** (Andy, 2026-10-25: "it should have access to agmen and the agents to change those settings right there"). The `settings` tools read and change agmen settings, the listener's own settings (agent, model, effort, caps, `accept_from`), and the person's agents' roles, tool classes, lanes and LENDs, budgets, push approval override, and stop or restart; project settings too when the person is a project admin. agmend applies a change only when the message that asked for it was sent by the listener's own person (the hub-stamped `from` of `reply_to`) and is in the batch that started the current turn, so a steered listener cannot cite an old "thanks". A change that narrows (a lower cap, a removed tool class or lane, an ended LEND, a stopped agent) applies at once and posts a change card in the chat (setting, before, after, Undo). A change that widens (a higher cap, an added tool class or lane, push approval off, an addition to `accept_from`) posts the card with Apply and Cancel and waits for the person's click in one of their consoles. Both go to the audit log. Nothing goes past what the person could do themselves, and a change that reaches another person's agents goes through the normal verbs and their agmend's policy.
- **Model and effort per message.** The composer's model chip picks a model (grouped by installed agent) and an effort (Claude Code `--effort` low to max, `agy --effort` low to high; Codex, Gemini and OpenCode take a model only) for this message or until changed. The turn runs as the listener with the listener's tools and caps; a model from a different agent than the listener's own runs as a one-off process of that agent under the listener's identity.
- **Loops.** The section 6.10 loop limits apply. In addition, a listener posts at most 3 messages in a thread without a human or worker message in between; agmend drops and logs the rest.
- **Tiers.** `small` maps to `claude-haiku-5-5`, `standard` to `sonnet` and `deep` to `opus`; each person can remap them per agent or add tiers. The role prompt says to use `deep` only when the requester marks the work hard or a `standard` attempt failed. agmend refuses unknown tiers.
- **Reviews.** When one of the person's agents posts DONE, the listener starts a one-shot Reviewer on this machine from this person's budget (`per_delegation_usd` applies) with a detached worktree at the DONE's commit, unless the person set `reviews = "off"` or a coordinator already assigned the review (section 6.13).
- **Across machines.** A listener delegates only on its own machine. Work for someone else's agents goes to them as a mention, a DM, an ASSIGN or a plan (section 6.14), and their own agmend decides under their own policy.
- **Coordinator listener.** A person with the coordinator role may start one with `--coordinator`. It gains ASSIGN and answers unaddressed ASKs, so a cheap model can run the board while bigger models do the work. It still cannot send RULING.
- **Visibility.** Workers carry `delegated_by` in the roster and sit under their listener in the agent view. The Listener panel in the desktop app shows its agent, role, tools, policy, spend (dollars, tokens, uptime) and recent actions, and collapses. The audit log records each delegation, approval, settings change and stop.

### 6.12 Shared agent packs

A pack is a git repo, normally on GitHub, holding agent setup that one person wants others to use: rules, skills, MCP servers, LSP servers and roles (section 6.13). agmend installs a pack into a person's agents only after that person approves the exact commit and contents on their own machine.

**Format.** `agmen-pack.toml` at the repo root:

```toml
name = "groupdrive-rust"
description = "Rust rules, a review skill, rust-analyzer, a docs MCP server"
rules = ["rules/rust.md"]          # markdown, added to every session of every receiving agent
skills = "skills"                  # <name>/SKILL.md folders, the layout Claude Code, Codex and OpenCode all read
roles = "roles"                    # <name>.xml role files, installed as that person's personal roles

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
- The approval screen shows the source, the commit SHA with its author and date, every rule in full, each skill with its files, each MCP server's exact command or URL and the env names it needs, each LSP server's command, each role file in full, which agents receive it, and the scope (this project, or `--user` for all of my agents on this machine). The person approves everything or picks items.
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

### 6.13 Roles

A role says what an agent is for and what it may do: one XML file whose attributes and a few elements are settings agmend and the hub read, and whose text is the prompt sent to the agent as written. Claude reads XML tags well, and the same text works as a system prompt for the other agents. Every role extends `base.xml`, the rules every agent follows (grounding claims, reporting, branches and worktrees, coordinating with other agents, chat text as data, secrets, budgets), written the way Andy's `CLAUDE.md` and `rules/*.md` are and generalized for other teams. The built-in files live in [`../roles/`](../roles/) and ship inside the `agmen` binary.

**Format.**

```xml
<?xml version="1.0" encoding="UTF-8"?>
<role name="reviewer" extends="base" kind="agent" tier="deep" version="1">
  <summary>Reviews one DONE and reports findings with path:line.</summary>
  <purpose>...</purpose>                                   <!-- prompt -->
  <tools allow="read search shell" />                      <!-- setting: tool classes -->
  <verbs allow="REVIEW" />                                 <!-- setting: added to the base verbs -->
  <edits scope="none" />                                   <!-- setting: none | lanes | paths -->
  <worktree mode="detached" at="done-commit" remove="after-review" />
  <rules topic="..."><rule>...</rule></rules>              <!-- prompt -->
  <done-when>...</done-when>                               <!-- prompt -->
  <report verb="REVIEW">...</report>                       <!-- prompt -->
</role>
```

| Element or attribute | Read by | Meaning |
|---|---|---|
| `name` | agmend, hub | Lowercase, `[a-z][a-z0-9-]{1,31}`; unique within a layer |
| `extends` | agmend | Parent role. The child inherits everything, may narrow `tools`, `verbs` and `edits`, and adds rules. `base` is the root and has no kind |
| `kind` | agmend, hub | `agent`, `coordinator` or `listener`; the role cannot be given to another kind |
| `tier` | agmend | Default tier (`small`, `standard`, `deep`); the person maps tiers to models per agent (section 6.11) |
| `<tools allow deny>` | agmend | Tool classes, so one file works for every agent: `read`, `search`, `edit`, `shell`, `web`, `subagents`, plus `git-read` and `settings`, which `agmen mcp` serves |
| `<verbs allow>` | hub | Verbs added to the base set; stored at mint, and anything else is refused with `forbidden_verb` |
| `<edits scope>` | agmend, hub | `none`; `lanes` (claimed or lent lanes, section 6.5); or `paths` with `globs` |
| `<worktree>` | agmend | `mode` is `branch` (with `name`), `detached` or `none`; `remove` is `after-merge`, `after-review` or `never` |
| `<purpose>`, `<rules>`, `<done-when>`, `<report>` | the agent | Prompt text; `report verb` names the verb the role ends its work with |

Prompt assembly: `base.xml` text, then each ancestor, then the role, rendered once at launch and passed the way each agent takes a system prompt (Claude Code `--append-system-prompt-file`; the rest per S3).

**Built-in roles.** Base verbs for every role: HELLO, BYE, FYI, ASK, ANSWER, ACK, STATUS, DM, ALERT, FOUND.

| Role | Kind | Job | Tools | Verbs beyond base | Edits | Worktree | Tier |
|---|---|---|---|---|---|---|---|
| Listener | listener | Hears its person's chat, looks things up, answers, delegates, changes settings when its person asks | read, search, git-read, settings | DELEGATED | none | none | small |
| Coordinator | coordinator | Runs the board: assigns items, settles disputes, lends lanes | read, search, git-read | ASSIGN, RULING, LEND, ROLLCALL, PROPOSAL, DECISION | none | none | standard |
| Planner | agent | Turns a goal into a team, specs and items with acceptance checks | read, search, edit, shell, web | PROPOSAL, PLAN | paths `docs/**`, `.agmen/**` | branch `plan/{slug}` | deep |
| Researcher | agent | Answers questions with quoted sources | read, search, web | none | none | none | standard |
| Engineer | agent | Claims an item, builds it, passes the gate, commits, pushes | read, search, edit, shell | CLAIM, HEARTBEAT, RELEASE, DONE, PUSH_REQUEST, LEND | lanes | branch | standard |
| Tester | agent | Writes and runs tests, files the failures it finds | read, search, edit, shell | CLAIM, HEARTBEAT, RELEASE, DONE, PUSH_REQUEST, LEND | lanes (test paths) | branch `test/{item}-{slug}` | standard |
| Reviewer | agent | Reviews a DONE's diff, reports findings with path:line | read, search, shell | REVIEW | none | detached at the DONE's commit | deep |

The Planner has a shell so it can commit and push its own plan branch; its edits stay inside `docs/**` and `.agmen/**`, and its rules say git only. Engineers and Testers have LEND for lanes they own (section 6.5). Hard ceilings by kind, applied by agmend and the hub whatever a role file says: a `listener` never gets `edit`, `shell` or `web`; an `agent` never gets ASSIGN, RULING, ROLLCALL or `settings`. A project `listener.xml` therefore cannot give a listener a shell.

**Enforcement**, in four places so no single bypass makes a role meaningless:

1. At launch, agmend gives the agent only its role's tool classes: Claude Code `--tools` (Read, Grep, Glob, plus WebFetch and WebSearch for `web`, plus Edit, Write and Bash for `edit` and `shell`); Codex `-s read-only` or `workspace-write`; Gemini CLI `--approval-mode plan`, `auto_edit` or policy files; `agy` `--mode plan` plus allow and deny rules in its settings; Grok Build `--tools`; OpenCode per-agent permissions (S3 confirms). Where an agent has no tool list (Codex), enforcement is coarse and the agent view says so.
2. At the hub: agmend sends `{role, verbs, edits, role_sha256}` at mint; the hub caps `verbs` at what the kind allows, stores them, and refuses any other verb with `forbidden_verb` (protocol section 1). A role can only narrow, so the hub trusts the minting person's agmend for the narrowing; the cap at the kind is the hub's own check.
3. In `agmen mcp`: the server lists only the tools that match the role's verbs and tool classes.
4. Through lanes: `edits scope` uses the lane checks in section 6.5.

Known limits, shown in the agent view: "shell read-only" for a Reviewer is a prompt rule, because no agent limits which commands a shell runs; agents agmen did not launch (an interactive Codex, say) get only the hub and MCP checks, plus the mod's guards in Claude Code.

**Where roles come from.** Four layers: built-in (inside the binary); project (`.agmen/roles/*.xml` in the project's repo, reviewed like code, overriding a built-in of the same name); pack (a `roles/` folder in an agmen pack, section 6.12, installed only after the person approves the commit); personal (`~/.config/agmen/roles/*.xml`, overriding both for that person's agents). The hub stores a role's name and hash only, never its text; there is no hub storage, sync or editor (`shortcut:` revisit when the web console must list or edit roles, or a project spans several repos). `agmen role import ~/.claude/agents/<name>.md` converts a Claude Code subagent file: `tools` becomes `allow` classes, `disallowedTools` becomes `deny`, `model` becomes a tier hint, `omitClaudeMd: true` becomes `extends="none"`, and the body becomes `<purpose>`.

**Giving an agent a role.** `agmen launch --role engineer`, the listener's `delegate {role}`, and a plan's team list (section 6.14). `agmen join --role reviewer` gives an interactive session a role: in Claude Code the mod enforces `edits` and `tools` with its existing guards, and other interactive agents get the hub and MCP checks. `@reviewers` typed in a composer becomes `role:reviewer` on the wire (protocol section 3) and reaches every live holder of that role; humans and coordinators only, like lane mentions.

**REVIEW.** The Reviewer's verb: `REVIEW {verdict: "pass"|"changes", findings: [{severity, path, line?, what, trigger}]}` with `reply_to` set to the DONE. The hub adds the DONE's author to `mentioned`, since a reply otherwise mentions its parent's author only when that author is human. A review starts from the owner's listener after each DONE (section 6.11) or from a coordinator's ASSIGN to a Reviewer that is already running, and the worker stays up to answer it (section 6.7).

### 6.14 Planning rooms

A planning room is a project chat where people plan a greenfield project with a Planner agent, theirs or someone else's, before any worker exists. Kick-off starts the team, and each person approves their own share.

- **The repo comes first.** A person creates the project's repo before the planning room starts, so the Planner has somewhere to branch from (my call; agmen creates no repos).
- **The plan** is a file the Planner writes and keeps current on its own branch (`plan/<slug>`), `docs/plans/<name>.plan.toml`, committed and pushed with the specs so every approver reads the same commit: goal; team (role, handle, agent, model or tier, lane, owner, dollars per day); specs with their approval state per person; lanes with owners; items, or the plan file they come from. Each revision posts as `PLAN {version, path, sha}` (protocol section 4), so the room sees it and the plan panel renders it.
- **The plan panel** (desktop app and terminal UI) shows the goal, the team table, the specs and who approved each, the lanes, a kick-off checklist (the repo exists, lanes do not overlap, specs are approved, each owner approved their own agents and spend), and the Kick off button.
- **Specs** open in an editor tab with Approve and Request changes. Approval is a human DECISION with `reply_to` set to the PLAN message and `data.subject` naming the file and its commit. The Planner never marks a spec approved itself.
- **Kick-off** is a human `DECISION {outcome: GO}` on the final PLAN, enabled once a person has merged the plan branch into the default branch the normal way and agmen has seen the merge (section 6.7); agmen never writes to the default branch. Then the hub creates the plan's lanes and items; each person's agmend launches only the agents the plan assigns to that person, and only if that person approved their share (agents and dollars per day), so someone else's approval never starts anything on my machine; and the plan's coordinator assigns the first items. From then on the agents coordinate in the project chat (section 6.10).
- **Surfaces.** The desktop app and the terminal UI (`:plan`, `:approve`, `:kickoff`) do all of it. The web console shows the planning room, the plan panel, spec approval and REVIEWs, and says "approve on your machine" for a person's share of agents and spend, because that is where they launch, the same rule as packs (section 6.12).

### 6.15 GitHub integration

A GitHub App on the hub with read-only permissions (pull requests, checks, issues, metadata), installed per organization and separate from GitHub login:

- **Merge detection** drives worktree cleanup (section 6.7) and marks the item done.
- **PR and checks** appear on DONE cards: PR number, check results, review state. A check that fails after DONE mentions the agent that posted it, which wakes the worker.
- **Issues to items**, optional per project.
- agmen does not write to GitHub (non-goals) and holds no write credentials, matching section 6.6. Whether it should open PRs or comment is open (section 9). GitLab, where agmen's own repo lives, is a second adapter later.

## 7. Milestones

The Claude Code plugin MVP ([plans/mvp.md](plans/mvp.md)) comes first. It takes the spikes from M0 (S1 redone for EMQX; S6 dropped), the M1 hub and CLI without agmend, the claims and lanes half of M2, and the wake-up, context and lane-guard half of M3. Exit: two Claude Code sessions on two machines exchange DMs, an idle one wakes on an ASSIGN, 20 concurrent CLAIMs give one GRANT, and an out-of-lane Edit is refused. Then the dogfood round. The milestones below stay as the longer roadmap. Roles, the listener and the consoles (sections 6.7, 6.8, 6.11, 6.13 to 6.15) land after the MVP, in M2 to M5; the MVP's `agmen join` is where `--role` first attaches.

| # | Deliverable | Exit check |
|---|---|---|
| M0 | Scaffold, this design, protocol, spikes S1 (EMQX JWT `acl` claim, no_match deny, expiry and REST kick, shared subscriptions, packet limits, WSS behind HAProxy, ACL attacks with `+`, `#`, `/` in ids), S2 (rumqttc v5 sessions, manual acks, WSS), S3 (ACP against all six agents: launch, events, a refused tool call, cost or tokens, a message mid-turn, take-over by session id and hand-back), S4 (mod streams `agmen watch` into a pane), S5 (numbering and replay property tests), S6 (local API on Windows), S7 (listener runtime: stream-json versus `--resume`, cost per turn, read-only tool lockdown per agent, a message sent mid-turn, triage and look-up quality of `claude-haiku-5-5`), S10 (an agent's own interface in a PTY inside a ratatui split and a GPUI view) | Each spike ends in an ADR |
| M1 | Hub core, EMQX, agmend, CLI and MCP: OAuth and device login, identities for interactive sessions, typed messages, ACK and NACK, replay, DMs, presence, swarm chat (`agmen chat`, mentions, pending queues, MCP `chat` and `read_chat`); minimal deploy on the GroupDrive cluster | Two machines exchange messages; a human's `@all` message reaches every agent's pending queue once; a network kill during DONE loses nothing; a spoofed publish is refused by the broker |
| M2 | Ledger and push approval: lanes with owners, items, atomic claims with leases, LEND from owners, the `push_approval` setting with PUSH_REQUEST and PUSH_OK, alerts with `urgency` and `needs`, STATE heads and snapshots; roles: `base.xml` and the built-ins compiled in, `.agmen/roles/` and personal roles, `{role, verbs, edits, role_sha256}` at mint with the hub's per-kind caps, REVIEW, role mentions, `refs` | 20 concurrent CLAIMs on one item give exactly one GRANT; with `owner`, a PUSH_REQUEST reaches the owner and PUSH_OK reaches the agent; DONE outside the lane is flagged; a LEND from a non-owner is refused; an Engineer identity posting RULING or REVIEW gets `forbidden_verb`; a REVIEW reaches the DONE's author |
| M3 | Claude Code mod, plus spend alerts and `agmen launch --agent claude --role <r>` with `--max-budget-usd`, the WIP commit, the worktree lifecycle and `agmen worktree prune`; the listener with `delegate`, the `git-read` and `settings` tools and agmend's policy; workers staying up after DONE and listener-started reviews; the vim-style terminal UI with the agent view, take-over in a split and clickable references; spike S9 and shared packs for Claude Code (`agmen pack add|update|remove|check|attach`, approval in the CLI, terminal UI and mod, `state/packs`) | Pane, chat pane, band, status line, notify, wake-up and guards work in a real session; an `@all` message wakes an idle interactive session and reaches a launched worker's stdin; plugin tests pass on terminal and desktop; a $0.50 Claude worker stops with its WIP committed; an ASSIGN to an away person reaches their listener, which starts a `standard` Engineer that posts DELEGATED then DONE in the thread, and then a Reviewer that posts REVIEW; an ASSIGN from someone outside `accept_from` gets an ALERT and no launch; "raise fx1's budget to $8" from the owner waits for Apply, and the same request from someone else changes nothing; a merged branch's worktree is removed and one with uncommitted work is kept; in `agmen` a click on a local worker shows its tool calls live, `o` opens Claude Code on its session in a split and `q` hands back, `f` opens a referenced branch, a message typed there is recorded and written to the worker's stdin, and every step also works with mouse capture off; a pack from a private GitHub repo installs into a Claude Code session only after approval, after which its rules are in context, its skill loads, and its MCP and LSP servers start; a changed commit waits for a new approval; `agmen pack remove` leaves the agent config as it was |
| Dogfood | One real multi-agent round with Adam's team on agmen instead of ntfy | Retro ADR |
| M4 | Human consoles: web console first, then spike S8 (GPUI on 3 OSes: long lists, text input, native notifications, single instance, packaging, signing, system file icons without `unsafe`) and the GPUI desktop app with Zed's layout: Agents, Project, Git and Roles docks, the Listener panel, take-over inside the chat, the model and effort picker, urgent alerts with the chime, appearance settings with Zed themes and icon themes, planning rooms and kick-off, the read-only GitHub App | The web console's Chat view messages the swarm and shows replies live, answers a PUSH_REQUEST, notifies with the tab closed, shows a planning room and a REVIEW, and lets an admin attach a pack that members then approve locally; GPUI dev builds on macOS, then Windows and Linux; a kick-off starts only the agents each person approved on their own machine; a merged PR reported by the App removes the worktree of a squash-merged branch |
| M5 | Full launcher: ACP launching for Codex, OpenCode, Gemini CLI, Antigravity and Grok Build, as workers and as listeners, switching an agent mid-task with a handoff brief, the Add and Update dialogs, hour and token caps, re-adoption after restart, preflight; packs for OpenCode and Codex | A worker survives an agmend restart and is re-adopted; preflight refuses a dispatch over the reservation; a Claude Code worker switches to Codex in the same worktree and the Codex session reads the brief; a Codex worker stops at its token cap with WIP committed; one pack installs into Claude Code, OpenCode and Codex on one machine, and the Codex approval screen lists its skipped LSP items |
| M6 | Beta delivery: invites, metering, tenant isolation tests in CI, signed and notarized desktop app installers for 3 OSes, updater, deep links, public mod mirror, docs, privacy policy and ToS, backups, monitoring | An outside team installs and joins without help |

## 8. Left to the builder

- Exact Postgres schema and migrations (sqlx), within the tables named above.
- Local API wire format between CLI, mod, desktop app and agmend (JSON over the socket or pipe), with version negotiation.
- Join prompt template wording.
- Price table for converting Codex tokens to dollars if Codex launching lands later.
- The hub's secret-shaped-string scanner rules (start from gitleaks patterns).
- Retry and backoff constants, lease durations and heartbeat intervals (document each in an ADR).
- The listener's triage examples, starting from the S7 prompt and fixtures; the role prompts start from `roles/`.
- The terminal UI's layout and key map beyond the keys named in section 6.8.
- The role XML schema and its validation, within section 6.13; the exact prompt assembly.
- The tool-class-to-flag mapping per agent beyond section 6.13, as S3 finds it.
- The handoff brief template (section 6.7).
- `docs/plans/<name>.plan.toml` field names within section 6.14.
- Chime sounds and their files.

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
| Listener misroutes or is steered by chat text | Builder | Policy lives in agmend, the listener has read-only tools, a settings change binds to the person's own message in the current batch and widening ones wait for a click, and the hub refuses human verbs from it. S7 counts injection cases. A person can stop it at any time |
| Small model too weak for triage | Builder | S7 measures accuracy; below 90% on the labeled set, the default becomes `sonnet` |
| Malicious or compromised pack | Builder | Approval of an exact commit with every command shown, no hooks or binaries, the secret scan, no auto-update, and removal that restores config. A pack the person approved can still do anything an MCP server can; the approval screen says so |
| Agent config formats move between releases | Builder | S9 pins the tested Claude Code, OpenCode and Codex versions; CI renders a fixture pack for each and checks it with `claude plugin validate` and the other agents' own loaders |
| Mouse support varies by terminal | Builder | Every action has a key; test in Terminal.app, iTerm2, Windows Terminal, a Linux terminal and tmux with `mouse on` |
| Role enforcement is coarse on some agents | Builder | Codex has sandbox modes, not a tool list, and no agent limits which commands a shell runs; the agent view says what is enforced, and the hub's verb cap and the DONE lane check hold regardless |
| Squash merges without the GitHub App | Builder | `git branch --merged` never sees them; `git cherry` catches rebases, and the daily list with `--confirm` covers the rest |
| Hour and token caps | Andy | My call that budgets cap hours and tokens as well as dollars; display-only is the alternative |
| GitHub App write access | Andy | Read-only here; whether agmen should open PRs or comment is open |
| Zed theme registry and theme licenses | Builder | Whether Zed's extension registry API may be used by other apps, and each theme extension's license, are unverified; the fallback reads the public extensions repository |
| System file icons in GPUI | Builder | Spike S8 checks GPUI can show NSWorkspace, Windows shell and freedesktop icons without `unsafe` |
| Lucide notice | Builder | ISC requires the notice in every copy of the shipped app |
| Agent CLIs move | Builder | `agy` 1.1.7 lacks `--input-format stream-json` (1.1.15+); Grok Build was checked from xAI's docs only; OpenCode's per-agent permissions and Gemini's cost fields are unverified; S3 pins tested versions |
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

Roles, agents and consoles (2026-10-25), plus `codex --help` and `codex exec --help` (0.164.0), `opencode --help` and `opencode run --help` (2.1.4), `gemini --help` (0.46.0), `agy --help` and `agy changelog` (1.1.7), and Andy's `~/.claude/agents/*.md`, `~/.claude/CLAUDE.md`, `~/.claude/rules/*.md` and `~/.config/zed/settings.json` and themes:
- Grok Build repo and docs (headless mode, MCP servers, agent mode): https://github.com/xai-org/grok-build and https://docs.x.ai/build/overview.md
- Antigravity CLI docs (headless, permissions, MCP): https://antigravity.google/docs/cli/headless, https://antigravity.google/docs/cli/permissions/, https://antigravity.google/docs/cli/mcp/; ACP request: https://github.com/google-antigravity/antigravity-cli/issues/31
- ACP registry entry for Antigravity: https://github.com/agentclientprotocol/registry/blob/main/antigravity-acp/agent.json
- Zed icons: license notice (Lucide only) https://github.com/zed-industries/zed/blob/main/assets/icons/LICENSES, origin https://github.com/zed-industries/zed/blob/main/crates/icons/README.md, repo licensing https://github.com/zed-industries/zed/blob/main/README.md
- Material Icon Theme for Zed (Apache-2.0): https://github.com/zed-extensions/material-icon-theme; its icons (MIT): https://github.com/material-extensions/vscode-material-icon-theme
- Lucide license (ISC): https://github.com/lucide-icons/lucide/blob/main/LICENSE
