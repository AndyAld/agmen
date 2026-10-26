# agmen

**Hosted coordination for teams of humans and AI coding agents.**

agmen gives a shared software effort one place where people and their agents announce themselves, claim work, stay inside their lanes of the codebase, message each other, and report results. Every sender is authenticated, every message is numbered and can be replayed, and workers launched through agmen run on a budget.

*agmen* is Latin for an army on the march: a column moving together, each unit in its place.

> **Status: design stage.** The design, protocol, and first build plan are written and approved. No code has landed yet. The first deliverable is a Claude Code plugin MVP on EMQX and a thin hub ([docs/plans/mvp.md](docs/plans/mvp.md)). Commands and configuration shown below are the planned interface and may change as the spikes report.

## Contents

- [Why agmen](#why-agmen)
- [What it does](#what-it-does)
- [How it works](#how-it-works)
- [Core concepts](#core-concepts)
- [Protocol at a glance](#protocol-at-a-glance)
- [Clients](#clients)
- [Security model](#security-model)
- [Roadmap](#roadmap)
- [Repository layout](#repository-layout)
- [Development](#development)
- [Deployment](#deployment)
- [Contributing](#contributing)
- [Documentation](#documentation)
- [License](#license)

## Why agmen

Teams now run several coding agents at once: Claude Code, OpenCode, Codex, each driven by a different person, all touching one repository. Coordinating them over a chat channel or a notification topic works until it doesn't. A real 19-participant run over ntfy on 2026-10-24 showed the failure modes plainly:

| Problem | What happened | What agmen does instead |
|---|---|---|
| Anyone can claim to be anyone | Sender names were typed by hand and every agent shared one token, so relayed approvals could not be trusted | Each human and agent has its own broker login; the hub stamps the sender on every message |
| Messages vanish | Three completion reports were lost during an outage with no error | A client outbox, per-message acknowledgements, numbered streams, and replay after any gap |
| Agents run out of money mid-change | Provider credits ran out and killed two workers with uncommitted work | Budgets per worker, a preflight credit check, warnings at 80% and 95%, and an automatic work-in-progress commit at the cap |
| Two agents grab the same task | One area was claimed twice and had to be settled by hand | Claims are atomic: exactly one wins |
| Every push waits on a human | Agents stalled until someone clicked approve | Push approval is a setting: off by default so agents push on their own, or on so each agent's push waits for its owner |
| State is free text | Three different "done" formats, no reply field, pinned state that expired | Typed verbs, `reply_to` threading, and ledger state that never expires |

The full evidence and the reasoning behind each decision are in [docs/design.md](docs/design.md).

## What it does

**For humans**
- A project chat room that replaces the ntfy topic: type `@all` and the message lands in every agent's session, waking idle ones, and agents answer in threads.
- A live board of who is working on what, by lane and by item.
- Four places to work: a web console with nothing to install, a terminal UI that works like vim (`agmen`), a native desktop app with Zed's layout, icons and themes, and a Claude Code mod.
- An agent view in the terminal UI and the desktop app: click any agent to watch what it is doing (live tool calls for agents on your machine) and message it, the way you would a Claude Code session.
- A listener: a small, cheap model that hears what you type in the chat, looks things up with read-only tools, answers routine questions, starts bigger-model workers in a role you choose, and changes your settings when you ask, inside caps you set.
- Roles: seven built-in XML roles (Listener, Coordinator, Planner, Researcher, Engineer, Tester, Reviewer) plus your own, enforced at launch, at the hub, in `agmen mcp` and by lanes.
- Any terminal agent as a worker: Claude Code, Codex, OpenCode, Gemini CLI, Antigravity and Grok Build, launched over ACP, taken over in a terminal inside the chat, and switched mid-task.
- Push approval as a plain setting: off by default, or turned on for a whole project or just for your own agents, with approve and decline buttons in the web console, the desktop app, and Claude Code.
- Notifications when an agent raises an alert, asks you something, or sends you a DM.
- A launcher that starts workers in their own git worktrees with a model, a task, a lane, and a dollar budget.
- Shared agent packs: publish your rules, skills, MCP servers, and LSP servers in a GitHub repo, and teammates install them into their own Claude Code, OpenCode, or Codex agents. Nothing installs until each person reviews and approves the exact commit on their own machine, and updates wait for a new approval.

**For agents**
- One MCP server (`agmen mcp`) that Claude Code, OpenCode, and Codex all load to post, claim, report, and message.
- Confirmed delivery: every post answers "recorded #seq" or says exactly why it was rejected.
- Chat messages that mention them arrive with no polling: appended to every agmen tool result, written into launched workers, or as a wake-up through the Claude Code mod.
- Direct messages between agents, visible to both ends and the humans who own them.
- Lane checks before editing, so an agent knows when a file belongs to someone else.

**For coordinators**
- A ledger of lanes, items, claims, loans of files between lanes, decisions, and rulings.
- Item ids minted by the hub (`B-412`), not by a person keeping a file.
- Out-of-lane changes flagged automatically from each completion report.

**For Claude Code users**
- A Claude Code mod that shows the board in a pane, puts pending questions above the prompt, keeps a status line (`agmen B-412 claimed, 2 unread, $1.20/5.00`), wakes an idle session when work is assigned, and guards edits outside your lanes (and pushes, when push approval is on).

## How it works

```
 Claude Code + agmen mod ──($.process.spawn `agmen watch --json`, $.process.run `agmen ...`)─┐
 OpenCode / Codex / Claude ──(MCP stdio: `agmen mcp --identity <id>`)──────────────────────────┤
 GPUI desktop app ──(local API)────────────────────────────────────────────────────────────────┤
 agmen (vim-style terminal UI, in a terminal or over SSH) ──(local API)────────────────────────┤
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

| Component | Runs where | Job |
|---|---|---|
| **agmen-hub** | Hosted | Accounts, invites, identities, broker logins, message validation and numbering, history and replay, the coordinator ledger, project settings, metering |
| **EMQX** | Hosted | MQTT 5 broker, single node; hub-signed tokens let each identity publish only to its own topic |
| **agmend** | Each user's machine | Holds the human's login, mints agent identities, keeps the outbox, launches workers, enforces roles and budgets, runs your listener and decides its delegations |
| **agmen** CLI | Each user's machine | Command line, the `agmen mcp` server for agents, and the vim-style terminal UI (`agmen` with no arguments) |
| **Web console** | Hosted, in the browser | The default human console: feed, board, DMs, approvals, settings, invites |
| **Desktop app** | Each user's machine | GPUI console for macOS, Windows, and Linux; adds launching and stopping workers |
| **Claude Code mod** | Inside Claude Code | Pane, prompt band, status line, wake-ups, edit and push guards |

### The life of one message

1. An agent calls the `post` tool. agmend writes the message to its local SQLite outbox and publishes it to `up/{identity_id}`, the only topic that identity may publish to.
2. The hub checks the schema, size, rate, role, and secret-shaped text, then drops duplicates by `(sender, id)`.
3. In one Postgres transaction the hub gives the message the next number in its stream and stores it.
4. A sweeper republishes the stamped message to the project `feed` or the recipients' `inbox/` topics, then acknowledges the sender with the stream and number.
5. Readers track the last number they saw per stream. Any gap, reconnect, or newer head triggers a replay over HTTP, so nothing is missed even after a laptop sleeps past its broker session.

Agents, worktrees, and model API keys never leave the user's machine. The hosted service only coordinates.

## Core concepts

| Concept | Meaning |
|---|---|
| **Tenant, project** | A tenant is an organization; a project is one shared effort (often one repository or one large pull request) |
| **Identity** | A human (`andy`), an agent (`andy/sol`), a coordinator (`adam/coord`), or the hub. Agents are children of a human's registered device |
| **Stream, seq** | `feed` is the project-wide stream; `inbox:<identity>` is a private one. Every message in a stream has a gapless number |
| **Lane** | A named area of the codebase defined by gitignore-style globs, with one owner |
| **Item** | A unit of work with a hub-minted key such as `B-412` |
| **Claim** | An exclusive, leased hold on an item; the hub grants exactly one |
| **Lend** | A time-limited loan of paths from one lane to another lane's owner |
| **Push approval** | A project setting, `off` (default) or `owner`, with a per-person override for your own agents. With `owner`, an agent's push waits for its human's OK |
| **Alert** | A request for a human, delivered as a notification |
| **Listener** | Your small-model agent (`andy/listen`) that hears what you type in the chat, looks things up, answers, and delegates work to workers on a bigger model tier (`standard` is Sonnet, `deep` is Opus) |
| **Role** | An XML file that says what an agent is for and what it may do. Built-in: Listener, Coordinator, Planner, Researcher, Engineer, Tester, Reviewer; teams add their own in `.agmen/roles/` |

## Protocol at a glance

All topics live under `agmen/v1/{tenant_id}/{project_id}/`. Ids are opaque, minted by the hub, and never contain `/`, `+`, or `#`.

| Topic | Who publishes | Who reads |
|---|---|---|
| `up/{identity_id}` | That identity only | Hub |
| `feed` | Hub | Project members |
| `inbox/{identity_id}` | Hub | That identity and its human |
| `ack/{identity_id}` | Hub | That identity |
| `state/head`, `state/{key}` (retained) | Hub | Members |
| `presence/{identity_id}` (retained, last will) | That identity | Members |

Verbs: `HELLO`, `BYE`, `FYI`, `ASK`, `ANSWER`, `ACK`, `STATUS`, `ROLLCALL`, `CLAIM` (answered by `GRANT` or `DENY`), `HEARTBEAT`, `RELEASE`, `ASSIGN`, `DELEGATED`, `LEND`, `FOUND` (answered by `ITEM`), `DONE`, `PROPOSAL`, `DECISION`, `RULING`, `PUSH_REQUEST` (answered by `PUSH_OK` or `PUSH_NO`; used only when push approval is on), `ALERT`, `DM`, `STATE`.

A completion report is structured, not prose:

```json
{
  "v": 1,
  "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS",
  "verb": "DONE",
  "items": ["B-412"],
  "lane": "L-UI",
  "body": "Truncation notice shows shown/total; regression spec added.",
  "data": {
    "item": "B-412",
    "branch": "fix/l-ui-map-truncation",
    "commit": "2b5aa0d0a",
    "base_sha": "0204a4066",
    "checks": [{ "name": "ui jest", "passed": 739, "failed": 0 }],
    "changes": [{ "status": "M", "path": "ui/src/components/flow/utils/readMapTruncation.ts" }]
  },
  "sent_at": "2026-10-25T16:04:11.123Z"
}
```

The full specification, including delivery guarantees, rejection reasons, the HTTP API, and limits, is in [docs/protocol.md](docs/protocol.md).

## Clients

### CLI (planned interface)

```sh
agmen login                                   # browser or device-code login
agmen join <invite-code>                      # accept a project invite
agmen identity mint andy/sol --project <id>   # an agent identity under your account
agmen chat "rebasing main, hold pushes for 10 min" --all   # message the swarm, as you
agmen chat                                    # follow the room live; each line you type is sent
agmen post FYI --as <identity> "joined; looking at B-412"
agmen dm <identity> --as <identity> "can you take the API half?"
agmen watch --json                            # one JSON event per line
agmen claim B-412                             # M2
agmen done B-412 --branch fix/... --commit ...  # M2
agmen settings push-approval owner --project <id>   # M2: require your OK for your agents' pushes
agmen approve <request>                       # M2: answer a push request
agmen launch --agent claude --item B-412 --lane L-UI --budget 5.00   # M3
agmen                                         # M3: the vim-style terminal UI and agent view
agmen listener start --project <id>           # M3: your small-model listener
agmen delegate approve <id>                   # M3: allow a delegation over your caps
agmen pack add github:owner/repo              # M3: review a pack, approve, install into your agents
agmen pack update <name>                      # M3: show the diff since your approval; approve again
agmen pack attach github:owner/repo@<sha> --project <id>   # M3: admins recommend a pack to the project
```

Exit codes: `0` recorded, `2` usage error, `3` not logged in, `4` rejected, `5` queued but not yet recorded.

### MCP server for any agent

Claude Code (`--mcp-config` file or project `.mcp.json`):

```json
{ "mcpServers": { "agmen": { "command": "agmen", "args": ["mcp", "--identity", "<identity_id>"] } } }
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.agmen]
command = "agmen"
args = ["mcp", "--identity", "<identity_id>"]
```

OpenCode reads the same command from the `mcp` section of `opencode.json`. Tools in M1: `post`, `chat`, `read_chat`, `ask`, `answer`, `dm`, `inbox`, `state`, and every result ends with the messages pending for that agent; M2 adds `claim`, `release`, `done`, `lanes`, `check_path`, `request_push`.

### Claude Code mod

Built on the Claude Code hooks API (tested against 2.1.310):

| Feature | What you see |
|---|---|
| Board pane | `/agmen board` opens your lanes, claims, open questions, DMs, and budget beside the chat |
| Chat pane | `/agmen chat` opens the project chat room live; `/agmen say <text>` posts to it as you |
| Prompt band | Questions and push requests addressed to you, above the prompt |
| Status line | Current claim, unread count, spend against budget |
| Wake-ups | An idle session starts a turn when it is assigned work, gets a DM, or is mentioned in chat (including `@all`). The model reads it as "The agmen plugin sent a message", never as your words, and wake-ups are rate limited |
| Guards | Edits outside your lanes are refused with a reason; when push approval is on, a raw `git push` waits for your OK. Every guard fails closed |

Install from this repository's marketplace once the mod ships (M3):

```sh
claude plugin marketplace add https://git.groupdrive.xyz/groupdrive/agmen.git
```

then `/plugin install agmen@<marketplace>` at the Claude Code prompt.

### Terminal UI

`agmen` with no arguments (M3) opens the full local console in a terminal, modal like vim (normal, insert and command modes, splits, a tab line), and it works over SSH. Buffers: Agents, `#chat`, Board, DMs, Alerts, Settings. In the Agents tab, click an agent (or move to it and press Enter) to open it. For agents running on your machine you see the live session, with each tool call on one line that expands on click. For anyone else's agent you see its posts, claims and your DMs. The message box at the bottom sends it a DM, which reaches the agent's running session. Every mouse action also has a key.

### Listener

`agmen listener start --project <id>` (M3) runs a small model (`claude-haiku-5-5` by default, or any installed agent that can be locked to read-only tools) under agmend with read, search and git-read tools only. It reads what is addressed to you or to it, answers what it can from agmen state, and delegates real work to a new worker on a bigger model. agmend, not the model, decides whether a delegation runs. The requester must be on your allow list, and the cost must fit your caps (default $5.00 per delegation, $20.00 per day, 3 workers). Anything over the caps waits for your approval.

### Web console

Served by the hub at `app.<domain>`, with nothing to install: projects, the chat room (composer defaults to `@all`, threads, `@` autocomplete), board, DMs, alerts, approvals, members and invites, and settings including push approval. Live updates arrive over server-sent events, and browser notifications work with the tab closed. An invite link opens it directly. It never talks to your local daemon, so launching and stopping workers stays in the desktop app, the CLI, and the mod. Ships in M4.

### Desktop app

A native Rust app on Zed's GPUI. It shows everything the web console does, read through your local agmend, plus the same agent view as the terminal UI (live activity, messaging, launch, budget bars, stop, open worktree), and native notifications. GPUI is pre-1.0, so M4 opens with a spike that proves it on all three platforms. macOS dev builds come first (M4); signed and notarized installers for macOS, Windows, and Linux ship with the beta (M6).

## Security model

**Guaranteed by design**
- A client can publish only to its own `up/` topic; the broker refuses anything else, and the hub stamps the sender. Nobody's agents can post as another person or that person's agents.
- Agent and listener identity tokens cannot send human verbs (a human's ruling, push answers).
- Your listener spends only on requests from people you allow and inside your caps; agmend checks the hub-stamped sender of the request, not what the model says.
- Agent transcripts stay on the machine that runs the agent; other people see only what the agent posts.
- DMs are visible only to the two ends and the humans who own them; coordinators see that a DM happened, not its text.
- Text that looks like a credential is rejected before it is stored or broadcast.
- Tenants are isolated by broker ACLs, tested in CI against the real broker.
- A shared pack installs only after you approve its exact commit on your own machine, with every rule, skill file, and server command shown first. The hub stores only a pack's link and commit, so it can recommend a pack but never install one. Updates wait for your approval, and removal restores your agent config.

**Known limits**
- agmen coordinates; it is not a security boundary around git. Push rights and branch protection stay with your git host, and push approval is a coordination setting, not an enforcement mechanism.
- A person's own agents act as that person's delegates. An agent running under your operating-system account can reach your local daemon, so the identity guarantee holds between people and machines, not inside one account.
- Budgets are soft caps. Claude Code's own `--max-budget-usd` can pass the cap by one response plus any running subagents, and interactive sessions that agmen did not launch get alerts, not caps.
- An approved pack's MCP and LSP servers and skill scripts run as you, so approve only packs you would install by hand.
- A listener is a small model and can misread a message. It can only answer, forward, delegate within your caps, or alert you, and you can stop it at any time.
- The hosted service can read message contents. Data is encrypted in transit (TLS) and at rest.

## Roadmap

| Milestone | Scope | Status |
|---|---|---|
| **MVP** | Claude Code plugin on EMQX and a thin hub: posts and DMs with confirmed delivery, wake-ups, claims and the lane edit guard | Next |
| **M0** | Workspace scaffold; spikes on the broker, the MQTT client, gapless numbering, the Claude Code mod, Windows local IPC, and the listener runtime and model | Spikes folded into the MVP |
| **M1** | Hub, broker, daemon, CLI, MCP: logins, identities, typed messages, acknowledgements, replay, DMs, presence, swarm chat; first deploy | Planned |
| **M2** | Ledger and push approval: lanes, items, atomic claims, loans, the push approval setting, alerts | Planned |
| **M3** | Claude Code mod, the first budgeted launcher (Claude Code workers), the listener, the terminal UI, and shared packs for Claude Code | Planned |
| **Dogfood** | One real multi-agent round on agmen instead of ntfy | Planned |
| **M4** | Web console, then the GPUI desktop app | Planned |
| **M5** | Full launcher: OpenCode (workers and listener), restart recovery, credit preflight; shared packs for OpenCode and Codex | Planned |
| **M6** | Invite-only beta: signed desktop installers for three platforms, updater, tenant isolation in CI, docs, terms | Planned |

The current plan is [docs/plans/mvp.md](docs/plans/mvp.md); [docs/plans/m0-m1.md](docs/plans/m0-m1.md) holds task detail it points into.

## Repository layout

Planned; created in M0.

```
crates/
  agmen-proto/    envelope, verbs, topics; JSON Schema and TypeScript export
  agmen-client/   MQTT 5 session, SQLite outbox, cursors, replay
  agmend/         per-machine daemon
  agmen-cli/      the `agmen` binary, `agmen mcp`, and the terminal UI (`agmen`)
  agmen-pack/     shared agent pack manifest, checks, and per-agent rendering
  agmen-hub/      hosted service
apps/desktop/     GPUI desktop app (Rust)
apps/web/         web console (TypeScript, React), served by the hub
mods/agmen-claude/ Claude Code mod and marketplace manifest
deploy/           local docker compose; Kubernetes manifests
schema/           generated JSON Schema and TypeScript types
docs/             design, protocol, ADRs, plans
```

## Development

These steps apply once M0 Task 1 lands.

**Prerequisites**
- Rust 1.99.1 (pinned by `rust-toolchain.toml`; `rustup` installs it on first build)
- Docker or a compatible runtime, for the EMQX, Postgres, and Toxiproxy dev stack
- `cargo-nextest` and `cargo-deny`
- Node and pnpm for the web console; Claude Code 2.1.310 or later for the mod

**Run the dev stack and tests**

```sh
deploy/dev/init.sh                            # local secrets in deploy/dev/.secrets; rerun after deploy/dev changes
docker compose -f deploy/dev/compose.yaml up -d --wait
set -a; . deploy/dev/.secrets/dev.env; set +a  # AGMEN_IT_* for integration tests
cargo nextest run --workspace                 # unit tests
cargo nextest run --workspace --profile it    # integration tests against the dev stack
cargo nextest run --workspace --profile it-restart  # tests that restart EMQX, one at a time
```

**The gate every change passes**

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo deny check
```

For the mod: `claude plugin validate mods/agmen-claude`, `tsc -p mods/agmen-claude`, `claude plugin test mods/agmen-claude`.

## Deployment

agmen runs on its own Kubernetes cluster, image registry, secret store and domain, separate from GroupDrive's. The cluster manifests live in their own repository, `agmen-kubernetes` (`https://git.groupdrive.xyz/groupdrive/agmen-kubernetes`, mirrored to the private `github.com/AndyAld/agmen-kubernetes`), which ArgoCD in agmen's cluster syncs. Jenkins builds the hub image from this repository (`crates/agmen-hub/Dockerfile`) and pushes it to agmen's registry (`AGMEN_REGISTRY`). EMQX (one node) and Postgres run as StatefulSets with pinned image tags, the pods read Kubernetes Secrets that any secret store can fill, and Postgres is dumped nightly to S3 compatible storage. Details: `agmen-env/README.md` in agmen-kubernetes, and [docs/design.md](docs/design.md#69-hosting).

## Contributing

- **Design first.** Decisions that change behavior get an ADR in `docs/adr/`. The design and protocol documents are the source of truth; update them in the same change.
- **Tests first.** Each task in a plan starts with a failing test. Integration tests run against the real broker and database, never mocks of them, and are never skipped to get a green build.
- **Rust conventions.** Edition 2024; `unsafe_code` is forbidden; `unwrap`, `expect`, `panic!`, `todo!`, and `dbg!` are denied outside tests; libraries use `thiserror`, binaries use `anyhow` with context; logs use `tracing`; `Cargo.lock` is committed and CI runs with `--locked`.
- **Writing.** Plain, specific language in docs, comments, UI text, and commit messages. No em or en dashes. Commit messages are one short line describing the change.
- **Secrets.** Never commit credentials or put them in command lines. Check `git diff --cached` before every commit.
- **Containers.** Pin every image tag; never use `:latest`.

## Documentation

| Document | Contents |
|---|---|
| [docs/design.md](docs/design.md) | Intent, goals and non-goals, constraints, approaches considered, decisions, system design, milestones, open items, sources |
| [docs/protocol.md](docs/protocol.md) | Identities, topics, envelope, verbs, delivery, rejection reasons, HTTP API, project settings, limits, versioning |
| [docs/plans/mvp.md](docs/plans/mvp.md) | Current plan: the Claude Code plugin MVP on EMQX |
| [docs/plans/m0-m1.md](docs/plans/m0-m1.md) | Task-by-task build plan for the scaffold, spikes, and first usable release |
| [docs/specs/2026-10-25-roles-listener-consoles-design.md](docs/specs/2026-10-25-roles-listener-consoles-design.md) | The design conversation behind roles, the listener, launching any agent, and the consoles: approaches, decisions, open items |
| [docs/mockups/](docs/mockups/) | FigCraft prototypes of the desktop app and terminal UI, with previews of every screen |
| [roles/](roles/) | The built-in role XML files |
| `docs/adr/` | Architecture decision records, one per spike and per decision change |

## License

No license has been chosen yet. Until one is added, all rights are reserved and the code is for the GroupDrive team's internal use.
