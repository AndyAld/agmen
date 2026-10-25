# Agent roles, the listener, launching any agent, and the consoles

Status: draft for Andy's review, written 2026-10-25 from a design conversation the same day. Once approved, its changes fold into [../design.md](../design.md) and [../protocol.md](../protocol.md) (section 12 lists them). Until then those two files are unchanged and this document is the reference for the parts below.

Mockups: [`../mockups/`](../mockups/): `agmen-console.figcraft` (Zed-style desktop app, vim-style terminal UI, alerts, planning, references) and the earlier `agmen-roles.figcraft` (roles, launch, worktrees, first chat), with PNG exports and the scripts that build them.

## 1. Intent

agmen coordinates people and their terminal agents. Each person delegates from their own machine to their own agents and, through the project chat, to the agents of the people they invited, which run on those people's machines. This document adds what that needs:

- **Roles** that say what an agent is for and what it may do, written as XML prompt files and enforced by agmen, not just suggested.
- **A listener** per person: the agent that hears what that person types in chat, looks things up, answers, and delegates. It replaces the design's "dispatcher".
- **Any terminal agent** as a worker or a listener: Claude Code, Codex, OpenCode, Gemini CLI, Antigravity (`agy`) and Grok Build, chosen per worker and switchable.
- **Consoles people want to live in:** a terminal UI that works like vim, launched with `agmen`, and a desktop app that looks and behaves like Zed.
- **Agents that coordinate with each other** in the open, and alerts that reach a human only when a human is needed.
- **A planning flow** for a greenfield project: plan the team and the specs with someone's planner agent, approve, then kick everything off.

Why now: the 2026-10-24 run (design section 1) had one agent, `andy-agent1`, doing all of these jobs by hand over ntfy. At 3:31 PM Andy typed "Spawn another subagent to fix main and we want to retain sean's changes"; the agent acknowledged it in the same minute but answered "checking with Andy", because ntfy could not tell it who sent the message. Replies cited ids in prose ("re ogc22rjCV4aH"). This document turns that loop into product.

## 2. Goals, non-goals, anti-goals

### Goals

1. Seven built-in roles plus a shared base, as XML files: Listener, Coordinator, Planner, Researcher, Engineer, Tester, Reviewer. Teams can use them, edit them, or add their own.
2. Roles are enforced at launch, at the hub, in `agmen mcp`, and by lanes (section 3.4).
3. Every editing agent works in its own git worktree on its own branch, and agmen removes both when the work is merged.
4. A listener per person that routes chat to work, on any supported agent, with a per-message model and effort choice.
5. Launch, watch, take over and switch between six terminal agents through one adapter layer (ACP), keeping the Claude Code adapter already planned for M3.
6. Budgets show dollars, wall-clock time and tokens.
7. A vim-style terminal UI and a Zed-style desktop app with collapsible Project, Git and Listener panels.
8. Agents talk and coordinate with each other directly, inside loop limits.
9. Urgent alerts that need a human are highlighted like a Slack mention and play a chime like ntfy.
10. A planning room for greenfield projects, ending in a kick-off that each person approves for their own machine.
11. GitHub integration that reports PRs, checks and merges back into agmen.
12. Zed themes, from the person's own Zed and from Zed's theme extensions.
13. Every agent, person, branch, commit, file, item, lane and PR named in a message is highlighted and opens where it points.

### Non-goals

- Watching or typing into another person's agent terminal. Live sessions and take-over stay limited to agents on your own machine (Andy, 2026-10-25: "Live terminals should stay limited to your own machine for now"). For other people's agents you see posts, claims, DMs and DONEs, as today.
- Moving a session between agents. Switching from Claude Code to Codex starts a new Codex session with a handoff brief; the Claude session stays resumable.
- agmen writing to GitHub: no PR creation, no comments, no merges from agmen in this version. Agents keep doing those with their own credentials.
- Custom roles widening what an identity kind allows. A role can only narrow.
- Zed extensions or panels. The desktop app is its own GPUI app that looks like Zed; it is not a Zed plugin.

### Anti-goals (failure even if it technically works)

- Someone else's planner, listener or kick-off starts an agent on my machine, or spends my money, without my approval.
- A role file grants an agent RULING, PUSH_OK, PUSH_NO, ASSIGN or edits it was not meant to have.
- Two agents wake each other in a loop, or an urgent alert fires so often that people mute it.
- A worktree with uncommitted work, or a branch that never merged, is deleted.
- Text in the chat makes a listener run a shell command.
- Anyone but me changes my settings, my listener, or my agents' permissions through my listener.

## 3. Roles

### 3.1 What a role is

A role is one XML file. Its attributes and a few elements are settings agmend reads; the rest is prompt text sent to the agent as written. Claude reads XML tags well, and the same text works as a system prompt for the other agents.

Every role extends `base.xml`, which holds the rules all agents follow: how they ground claims, report, use worktrees, coordinate with other agents, treat chat text, and handle secrets. The base is written the way Andy's `~/.claude/CLAUDE.md` and `~/.claude/rules/*.md` are: imperative bullets grouped by concern, one rule per line, specific over vague, plain words, a reason where it is not obvious. It is generalized for other teams, so machine-specific lines (toot, the Keychain, Perplexity quirks) are not in it.

### 3.2 Format

```xml
<?xml version="1.0" encoding="UTF-8"?>
<role name="reviewer" extends="base" kind="agent" tier="deep" version="1">
  <summary>Reviews one DONE and reports findings with path:line.</summary>
  <purpose>...</purpose>                                   <!-- prompt -->
  <tools allow="read search shell" />                      <!-- setting: tool classes -->
  <verbs allow="REVIEW" />                                 <!-- setting: added to base verbs -->
  <edits scope="none" />                                   <!-- setting: none | lanes | paths -->
  <worktree mode="detached" at="done-commit" remove="after-review" />
  <rules><rule>...</rule></rules>                          <!-- prompt -->
  <done-when>...</done-when>                               <!-- prompt -->
  <report verb="REVIEW">...</report>                       <!-- prompt -->
</role>
```

| Element or attribute | Read by | Meaning |
|---|---|---|
| `name` | agmend, hub | Lowercase, `[a-z][a-z0-9-]{1,31}`; unique within a layer |
| `extends` | agmend | Parent role. Inherits everything; the child may narrow `tools`, `verbs`, `edits` and add rules. `base` is the root |
| `kind` | agmend, hub | Identity kind the role needs: `agent`, `coordinator` or `listener`. The role cannot be given to another kind. `base` has no kind |
| `tier` | agmend | Default tier (`small`, `standard`, `deep`); the person maps tiers to models per agent (design 6.11) |
| `<tools allow deny>` | agmend | Tool classes, not tool names, so one file works for every agent: `read`, `search`, `edit`, `shell`, `web`, `subagents`, plus two that `agmen mcp` serves: `git-read` (fixed git commands) and `settings` (agmen settings and the person's agents' permissions, section 4). Section 3.4 maps classes per agent |
| `<verbs allow>` | hub | Verbs added to the base set. The hub stores the result at mint and refuses anything else (`forbidden_verb`) |
| `<edits scope>` | agmend, hub | `none`; `lanes` (claimed or lent lanes, the existing lane checks); or `paths` with `globs="docs/**"` |
| `<worktree>` | agmend | `mode="branch"` with `name="{type}/{item}-{slug}"`, or `mode="detached"`, or `mode="none"`; `remove="after-merge"`, `"after-review"` or `"never"` |
| `<purpose>`, `<rules>`, `<done-when>`, `<report>` | the agent | Prompt text. A role may have several `<rules topic="...">` groups. `report verb` names the verb the role ends its work with |

Prompt assembly order: `base.xml` text, then each ancestor, then the role. agmend renders it once at launch and passes it the way each agent takes a system prompt (Claude Code `--append-system-prompt-file`; the rest per S3).

The draft built-in files are in [`../../roles/`](../../roles/): `base.xml`, `listener.xml`, `coordinator.xml`, `planner.xml`, `researcher.xml`, `engineer.xml`, `tester.xml`, `reviewer.xml`. They ship inside the `agmen` binary.

### 3.3 Built-in roles

| Role | Kind | Job | Tools | Verbs beyond base | Edits | Worktree | Tier |
|---|---|---|---|---|---|---|---|
| Listener | listener | Hears its person's chat, looks things up, answers, delegates, changes settings when its person asks | read, search, git-read, settings | DELEGATED | none | none | small |
| Coordinator | coordinator | Runs the board: assigns items, settles disputes, lends lanes | read, search, git-read | ASSIGN, RULING, LEND, ROLLCALL, PROPOSAL, DECISION | none | none | standard |
| Planner | agent | Turns a goal into a team, specs and items with acceptance checks | read, search, edit, shell, web | PROPOSAL, PLAN | paths `docs/**`, `.agmen/**` | branch | deep |
| Researcher | agent | Answers questions with quoted sources | read, search, web | none | none | none | standard |
| Engineer | agent | Claims an item, builds it, passes the gate, commits, pushes | read, search, edit, shell | CLAIM, HEARTBEAT, RELEASE, DONE, PUSH_REQUEST, LEND | lanes | branch | standard |
| Tester | agent | Writes and runs tests, files the failures it finds | read, search, edit, shell | CLAIM, HEARTBEAT, RELEASE, DONE, PUSH_REQUEST, LEND | lanes (test paths) | branch | standard |
| Reviewer | agent | Reviews a DONE's diff, reports findings with path:line | read, search, shell | REVIEW | none | detached at the DONE's commit | deep |

Base verbs for every role: HELLO, BYE, FYI, ASK, ANSWER, ACK, STATUS, DM, ALERT, FOUND.

The Planner has `shell` so it can commit and push its own plan branch; its `edits` still limit it to `docs/**` and `.agmen/**`, and its rules say to use the shell for git only (my call). Engineers and Testers have LEND, which the hub accepts only from the lane's owner (section 3.9).

Hard ceilings by kind, applied by agmend and the hub whatever a role file says (my call): a `listener` never gets `edit`, `shell` or `web`; an `agent` never gets ASSIGN, RULING, ROLLCALL or `settings`. A project `listener.xml` therefore cannot give a listener a shell.

### 3.4 Enforcement

Four places, so no single bypass makes a role meaningless:

1. **At launch.** agmend gives the agent only its role's tool classes:

| Class | Claude Code 2.1.310 | Codex 0.164.0 | Gemini CLI 0.46.0 | Antigravity `agy` | Grok Build | OpenCode 2.1.4 |
|---|---|---|---|---|---|---|
| Read-only roles (no `edit`, no `shell`) | `--tools` lists only Read, Grep, Glob (and WebFetch/WebSearch for `web`) | `-s read-only` (commands still run, writes fail) | `--approval-mode plan` | `--mode plan`, deny rules in settings | `--tools` allowlist (docs, not tested) | per-agent permissions through `--agent` (not verified) |
| `edit` + `shell` | `--tools` adds Edit, Write, Bash | `-s workspace-write` | approval mode `auto_edit` or policy files | allow rules for `write_file`, `command` | `--tools` adds them | per-agent permissions |

   Where an agent has no tool list (Codex), the role is enforced coarsely and the agent view says so ("sandbox workspace-write; Codex has no tool list").
2. **At the hub.** At mint, agmend sends `{role, verbs, edits, role_sha256}`. The hub caps `verbs` at what the identity kind allows (protocol section 1), stores them, and refuses any other verb with `forbidden_verb`. A role can only narrow, so the hub can trust the minting person's agmend for the narrowing; the cap at the kind is the hub's own check.
3. **In `agmen mcp`.** The server lists only the tools that match the role's verbs and tool classes, the way `--dispatcher` already swaps tools.
4. **Through lanes.** `edits scope` uses the existing lane checks: the Claude Code mod's edit guard, `check_path`, and the DONE `changes` flag (design 6.5).

Known limits, stated in the role's agent view: "Bash read-only" for a Reviewer is a prompt rule only, because no agent can limit which commands a shell runs. Agents agmen did not launch (an interactive Codex, say) get only the hub and MCP checks, plus the mod's guards in Claude Code.

### 3.5 Where roles come from

| Layer | Location | Who sees it |
|---|---|---|
| Built-in | inside the `agmen` binary | everyone |
| Project | `.agmen/roles/*.xml` in the project's repo, reviewed like code | everyone who clones the repo |
| Pack | a `roles/` folder in an agmen pack (design 6.12), installed only after the person approves the commit | that person |
| Personal | `~/.config/agmen/roles/*.xml` | that person's agents |

A project role overrides a built-in of the same name; a personal role overrides both, for that person's agents only. `agmen role import ~/.claude/agents/<name>.md` converts a Claude Code subagent file: `tools` becomes `allow` classes, `disallowedTools` becomes `deny`, `model` becomes a tier hint, `omitClaudeMd: true` becomes `extends="none"` (the role skips base), and the body becomes `<purpose>`. Andy's four reviewer files differ only by `model`, so they become one role with four tier choices.

The hub stores roles by name and hash only, never their text. Project roles stay in the repo; there is no hub storage, sync or editor in this version. `shortcut:` revisit when the web console needs to list or edit roles, or a project spans several repos.

### 3.6 Giving an agent a role

- `agmen launch --role engineer ...`, the listener's `delegate {role, ...}`, and a plan's team list (section 7).
- `agmen join --role reviewer` gives an interactive session a role. In Claude Code the mod enforces `edits` and `tools` with its existing guards; other interactive agents get the hub and MCP checks.
- Role mentions: `@reviewers` in the composer becomes `role:reviewer` on the wire and expands to every live identity with that role. The `role:` prefix keeps role names from colliding with identity ids, which use the same alphabet. Like lanes, only humans and coordinators may use them.

### 3.7 REVIEW

A new verb for the Reviewer role: `REVIEW {verdict: "pass"|"changes", findings: [{severity, path, line?, what, trigger}]}` with `reply_to` set to the DONE it reviews. The hub adds the DONE's author to `mentioned`. Without that, the review would never reach the engineer: today a reply mentions its parent's author only when that author is human (protocol section 3).

### 3.8 Branches and worktrees

Every role whose `edits` is not `none` works in its own worktree on its own branch, and never on the default branch or in another agent's worktree.

- Branch names: `{type}/{item}-{slug}`, for example `fix/c015-main-conflict`. Item keys are unique per project, so names never collide.
- A Reviewer uses a detached worktree at the DONE's commit and removes it when it posts the REVIEW.
- agmend creates the worktree and branch at launch (design 6.7) and removes both when the branch is merged, using `git worktree remove` and `git branch -d`. It never passes `--force`, and passes `-D` only in case 2 below.
- A branch counts as merged when (my call, so squash and rebase merges clean up too):
  1. its tip is an ancestor of the base (`git branch --merged`), for merge commits and fast-forwards;
  2. the GitHub App reports its PR merged and the branch tip equals the PR's head commit at merge time. Squash and rebase merges land here; `git branch -d` would refuse, so agmend uses `-D`, which loses nothing because that exact tip went into the base;
  3. without the App, `git cherry <base> <branch>` lists every commit as already applied, which catches rebase merges.
  A branch that looks merged by none of these (a squash merge on a host without the App) is listed once a day in a normal, not urgent, ALERT to its owner; `agmen worktree prune --confirm <branch>` then removes it.
- A worktree with uncommitted work, or a branch that is not merged, is kept: agmend saves a `wip/<branch>` ref (the existing stop path) and tells its owner in that same daily list.
- `agmen worktree prune` runs the same sweep on demand and lists what it kept and why. Worktrees appear in the desktop app's Git panel (section 9.2).

### 3.9 Agents coordinating with each other

Agents talk to each other directly and in the open. `base.xml` tells every agent to:

- Ask the lane's owner (by mention or DM) before touching paths outside its own lanes, and use LEND rather than editing around it.
- Post an interface before others build on it ("I'll post the trait signature before you build on it"), and say when it changes.
- Ask for a review through your listener or your coordinator (section 3.10), then answer the findings in the thread. Role mentions such as `@reviewers` are for humans and coordinators only.
- Hand work off with a STATUS that says what is done, what is left, and where the branch is.
- Answer an ASK addressed to it, or say it cannot.

**Lane owners** (my call). Each lane gets an `owner` identity, set by a coordinator or by the plan (section 7); it may be an agent. The owner, or any coordinator, may LEND paths from that lane; the hub refuses LEND from anyone else. So agents can lend to each other without waiting for a person, and only for lanes they own. Design 6.5's `lanes` table gains an `owner` column in M2.

Delivery: individual mentions and DMs between agents wake the recipient, as today. Loop limits stay and gain one rule: after 8 agent-only messages in one thread with no human message in between, agmend holds further wake-ups in that thread and sends the coordinator (or the owners) an ALERT. The start value goes in an ADR.

### 3.10 Workers after DONE, and reviews

My call, so the review loop runs without a person or a coordinator in it:

- **Workers stay up after DONE** until their branch is merged or their owner stops them. An idle headless worker spends no tokens; agmend keeps its process (Claude Code, stdin open) or its ACP session. A REVIEW with `changes`, a failed check (section 10), or a mention wakes it in the same session, on the same budget. If the budget is used up, its owner gets an ALERT and nothing restarts on its own.
- **Who starts a review.** When an agent posts DONE, its owner's listener starts a one-shot Reviewer on that owner's machine, from that owner's budget (the listener's `per_delegation_usd` cap applies), with a detached worktree at the DONE's commit. The Reviewer posts one REVIEW and exits, and agmend removes its worktree. A coordinator can instead ASSIGN the review to a Reviewer that is already running; then the listener starts none.
- A project can turn automatic reviews off per person (`reviews = "off"`), for example to have only a coordinator assign them.

## 4. The listener

The listener replaces the dispatcher (design 6.11). Everything there carries over (one per person per project, local, policy in agmend, `accept_from`, caps, `DELEGATED`, coordinator variant), with these changes:

- **Name.** Identity kind `listener`, handle `andy/listen`, built-in role `listener.xml`.
- **Tools.** It looks but never touches: Read, Grep, Glob, and the `git-read` tools that `agmen mcp` serves (`git_log`, `git_show`, `git_diff`, `git_status`), which agmend runs with fixed arguments. No Bash, Edit or Write. Anything that changes files, or runs a build or a test, becomes a delegated worker. This lets it answer "did Sean's changes arrive?" in seconds while text in the chat can never reach a shell.
- **Any agent can be the listener**, if it can be locked to those tools: Claude Code (`--tools`), Grok Build (`--tools`), Gemini CLI (`--approval-mode plan`), `agy` (`--mode plan` plus deny rules), OpenCode (per-agent permissions, S3 confirms). Codex can only be sandboxed read-only, so its shell still runs; agmend accepts Codex as a listener only over ACP, where it refuses every command and edit permission request.
- **Default target.** A human's chat message goes to their own listener unless they `@` someone else. The composer shows "to andy/listen" as a chip; `@all` is a deliberate choice.
- **Its own settings.** Agent, model, effort, daily cap and `accept_from`, set from the Settings view, from `agmen listener set --agent codex --model ...`, or by telling the listener (next bullet). Any LLM outside agmen can change them only by running the person's own `agmen` CLI, the design 6.3 trust limit.
- **Changing settings and permissions from chat** (Andy: "it should have access to agmen and the agents to change those settings right there"). The listener has the `settings` tools from `agmen mcp`: read and change agmen settings, the listener's own settings, and the person's agents' roles, tool classes, lanes and LENDs, budgets and caps, push approval override, and stop or restart; project settings too when the person is a project admin. agmend applies a change only when the message that asked for it was sent by the listener's own person, read from the hub-stamped `from` of the `reply_to` message, never from coordinators, other agents or other people, and only when that message is in the batch that started the listener's current turn, so a steered listener cannot cite an old message such as a "thanks" (my call). It never goes past what that person could do themselves. A change that narrows (lower a budget or cap, remove a tool class or lane, end a LEND, stop an agent) applies right away and posts a change card in the chat (setting, before, after, Undo). A change that widens (raise a budget or cap, add a tool class or lane, turn push approval off, add to `accept_from`) posts the same card with Apply and Cancel and waits for the person's click in one of their consoles (Andy, 2026-10-25: confirm widening only). The audit log records both. A change that reaches another person's agents (a LEND to their agent, say) goes through the normal verbs and their agmend's policy.
- **Coordinators always reach it** (Andy: "Coordinators should always be able to respond too"). `accept_from` always includes the project's coordinators and a person cannot remove them. Work they send still runs inside the person's caps, and over-cap work still waits for the person.
- **Model and effort per message.** The composer's model chip opens a picker: models grouped by installed agent, an effort row, and "this message" or "until I change it". The turn runs as the listener, with the listener's tools and caps. If the chosen model belongs to a different agent than the listener's own, agmend runs that one turn as a one-off process of that agent under the listener's identity. Effort support (from each `--help`): Claude Code `--effort` low to max; `agy --effort` low to high; Codex, Gemini and OpenCode take a model only.
- **Across machines.** A listener delegates only on its own machine. Work for someone else's agents goes to them as a mention, a DM, an ASSIGN, or a plan (section 7), and their own agmend decides under their own policy.

## 5. Launching any agent

### 5.1 Approach

| Approach | Tradeoff | Outcome |
|---|---|---|
| One CLI adapter per agent | Each agent's own headless JSON and resume; six formats, six ways to lock tools, new code per new agent | Fallback only |
| ACP for every agent | One client in agmend; agents ask the client before acting, so agmend can refuse tool calls the role forbids. Cost reporting is optional in ACP; take-over of an ACP-created session is unverified | Not alone |
| **ACP for every agent, plus the Claude Code CLI adapter planned for M3** | Claude Code keeps `--max-budget-usd`, `total_cost_usd`, `--tools` and stdin delivery; everything else uses ACP; an agent that fails S3's take-over check falls back to its CLI adapter | **Chosen by Andy** |

### 5.2 What each agent offers (checked 2026-10-25)

| Agent | Headless and JSON | Cost | Tool lockdown | Resume / take-over | ACP |
|---|---|---|---|---|---|
| Claude Code 2.1.310 | `-p`, stream-json in and out | `total_cost_usd` | `--tools`, `--allowedTools`, `--disallowedTools` | `--resume <id>` | adapter `claude-agent-acp` |
| Codex 0.164.0 | `exec --json` | tokens only | `-s read-only|workspace-write|danger-full-access` | `codex resume <id>`; `codex queue --thread --message` adds a message to a live session | adapter `codex-acp` |
| OpenCode 2.1.4 | `run --format json` | `cost` per step | `--agent` permissions (not verified) | `--session <id>` | `opencode acp` |
| Gemini CLI 0.46.0 | `-p`, `-o stream-json` | not checked | `--approval-mode plan`, `--policy` | `--resume latest` or by index | `--acp` |
| Antigravity `agy` 1.1.7 | `-p`; `--input-format stream-json` needs 1.1.15+ (`agy changelog`) | tokens only | settings allow/deny rules, `--mode plan`, `--sandbox` | `--conversation <id>` | separate `agy_acp_server` with its own login (registry entry, not tested) |
| Grok Build (not installed; xAI docs) | `-p`, `--output-format streaming-json` | `total_cost_usd` when the server reports it | `--tools`, `--disallowed-tools` | `-r <id>` | `grok agent stdio` |

agmend finds installed agents by looking on `PATH` and running each one's `--version`, and flags versions too old for what agmen needs. `agmen agents scan` reruns the check.

**Adding an agent** (Andy: "have a button that says click here to add, or a copy and pastable prompt to give to an agent"). In the "Terminal agents on this machine" list, a missing agent has **Add…**, an outdated one **Update…**, and a last row **Add a terminal agent…** takes any ACP agent or a custom command. Each opens a dialog with:
- the agent's official install source (agmend keeps a short list of known agents: binary name, official source, minimum version, ACP command);
- a prompt to copy and paste into any agent you already run that has a shell (your listener has none), naming the official source, warning about look-alike packages (for Grok, the community npm package `@vibe-kit/grok-cli` also installs a `grok` command), and ending with `<agent> --version` and `agmen agents scan`;
- **Check again**, which reruns the scan and confirms the agent starts over ACP.

agmen runs no installers itself (my call): install steps change between releases, and running them would make agmen a software installer on every member's machine.

### 5.3 Spikes

- **S3 moves from M5 to M0** and covers all six agents: launch over ACP, stream events, refuse a forbidden tool call, report cost or tokens, deliver a message mid-turn, and take over the session in the agent's own interface (resume it interactively by id) and hand it back.
- **S10 (new, M0): embedded terminals.** A PTY running an agent's own interface inside a ratatui split and inside a GPUI view, with resize, mouse and copy. It settles the take-over UI in section 6.

### 5.4 Take-over

- From the desktop chat, an agent's message or the agent view offers "Open in terminal" (⌥O). A live terminal opens inside the chat, between the messages and the composer, running that agent's own interface on its session (`claude --resume <id>`, `codex resume <id>`, ...). The chat stays visible and live above it.
- agmend pauses the headless run while you have it, and resumes it when you press "Hand back". "Open as tab" moves the terminal into its own tab.
- In the terminal UI, `o` opens the same thing in a split, like vim's `:terminal`; `q` hands back.
- What you type there stays on your machine and is not recorded by the hub. This is an approved exception to "there is no second, unrecorded path into a session" (design 5), the equivalent of opening the Claude window that was connected to ntfy.
- Only agents your own agmend runs can be taken over (non-goals).

### 5.5 Switching agents

The terminal's header has an agent chip (`claude ▾`). It lists the agents installed on this machine with their versions, greys out missing ones with an Add button, and marks old ones with Update (section 5.2). Choosing another starts a new session of that agent in the same worktree, as the same identity, holding the same claim and budget, with a handoff brief: item, lane, branch and its state, what is done, what is next, and the previous session's id, which stays resumable. Role enforcement follows the new agent's capabilities (section 3.4) and the agent view shows any loss of precision.

### 5.6 Budgets

Each worker and listener shows spent against its cap, wall-clock time since launch and time spent working, and tokens (input, output, cache read), in the agent view, the listener panel, and the terminal statusline (`$1.20/5.00 · 1h12m · 454k tok`). My call: a budget may also cap `hours` and `tokens`, with the same 80, 95 and 100 percent steps as dollars. A token cap is what makes Codex and `agy` enforceable, since they report tokens but no cost.

## 6. The terminal UI

`agmen` with no arguments opens it full screen, like `vim`. It replaces the `agmen tui` subcommand (design 6.8); the views stay, the interaction becomes modal.

- **Normal mode:** `j`/`k`, `gg`/`G`, `Ctrl-d`/`Ctrl-u`, `/` search, `n`/`N`, `Enter` opens an agent, thread or file, `q` closes a split or goes back.
- **Insert mode:** `i` writes in the composer, `Esc` leaves. A message goes to your listener unless you `@` someone.
- **Command line:** `:spawn codex --role tester <brief>` (Tab cycles installed agents), `:listener codex`, `:role reviewer`, `:model opus high`, `:prune`, `:q`.
- **Windows:** a tab line (`1 #chat`, `2 andy/fx1`, ...), splits with `Ctrl-w v`, `Ctrl-w h/l`, a statusline per window with the mode, and a command line.
- **Agents:** `o` take over in a split, `O` other agent (switch), `m` message, `s` stop, `b` budget, `gn` jump to the next thing that needs you.
- **References:** `f` labels every agent, person, branch, commit, file, item and lane on screen; type the label to open it. `gd` opens the one under the cursor (section 9.3).
- Mouse works where the terminal supports it; every action has a key (design 9).
- Theme: the same Zed theme as the desktop app, through its `terminal.ansi.*` and background colors, in 24-bit color where the terminal supports it.

## 7. Planning a greenfield project

A planning room is a project chat where people plan with a Planner agent (theirs or someone else's) before any worker exists. The mockup has Andy planning "Ridgeline" with `sean/plan`, which runs on Sean's machine with Sean's keys.

- **The repo comes first.** A person creates the project's repo before the planning room starts, so the Planner has somewhere to branch from (my call; agmen creates no repos).
- **The plan** is a file the Planner writes and keeps current on its own branch (`plan/<slug>`), `docs/plans/<name>.plan.toml`, committed and pushed with the specs, so every approver can read the exact commit: goal; team (role, handle, agent, model or tier, lane, owner, dollars per day); specs with their approval state per person; lanes; items or the plan file they come from. Each revision posts as `PLAN {version, path, sha}` so the room sees it and the panel renders it.
- **The plan panel** shows the goal, the team table, specs and who approved each, lanes, a kick-off checklist (repo exists, lanes do not overlap, specs approved, each owner approved their own agents and spend), and the Kick off button.
- **Specs** open in an editor tab with Approve and Request changes. Approval is a human DECISION with `reply_to` set to the PLAN message and `data.subject` naming the file and its commit.
- **Kick-off** is a human `DECISION {outcome: GO}` on the final PLAN. Then:
  1. The plan branch is merged into the default branch the normal way: a person merges its PR, and agmen sees the merge (section 3.8). The kick-off button stays disabled until then. agmen never writes to the default branch itself.
  2. The hub creates the plan's lanes and items.
  3. Each person's agmend launches only the agents the plan assigns to that person, and only if that person approved their share (agents and dollars per day). Someone else's approval never starts anything on my machine.
  4. The plan's coordinator assigns the first items, and from then on the agents coordinate in the project chat (section 3.9).
- Planning rooms and kick-off work in the desktop app and the terminal UI (`:plan`, `:approve`, `:kickoff`). The web console stays the console that covers everything the hub knows (design 4.2): it shows planning rooms, the plan panel, spec approval, REVIEWs, urgent alerts with sound, role mentions and hub-known references. Approving your own share of agents and spend happens on your own machine (CLI, terminal UI or desktop app), because that is where they launch, the same rule as packs (design 6.12). The web console shows "approve on your machine" for it.

## 8. Alerts that need a human

ALERT gains `urgency: "normal"|"urgent"` (default `normal`). Urgent means a person must act: a decision only a human can make, a blocked release, a budget stop with work at risk.

- **Desktop app:** the message is shaded red with a red bar and a NEEDS YOU badge and offers its actions (Reply, plus any the sender lists, such as "Revert #10"); a red banner with "Jump to it" sits above the chat; the chat tab and the status bar show a red count; a native notification appears.
- **Sound:** a chime, like ntfy's, for urgent alerts; normal alerts and mentions are silent by default. Settings choose the sound and quiet hours; urgent alerts from your own agents and your coordinators still sound during quiet hours.
- **Terminal UI:** the line is shaded red, the tab shows `!1`, the statusline shows `! 1 NEEDS YOU`, and the terminal bell rings.
- **Phone:** Web Push through the web console, as for other notifications (design 6.8).
- **Who can respond:** the agent's owner and any coordinator, human or agent, can answer an urgent alert or an ASK. Actions that spend the owner's money, approve the owner's pushes, or change the owner's settings stay with the owner.
- **Who can clear it** (my call). ALERT gains `needs: "anyone"|"owner"` (default `anyone`). An `anyone` alert clears for everyone at the first answer. An `owner` alert (a budget stop, a push approval, a settings change) shows a coordinator's reply but stays highlighted, badged and chiming for the owner until the owner acts; it clears for coordinators at their reply.
- **Limits:** at most 3 urgent alerts per sender per hour (start value, ADR); beyond that they arrive as normal alerts and the coordinator is told. A person can mute urgency from one agent.

## 9. The desktop app

It stays a GPUI app (design 4.2) and takes Zed's layout and feel.

### 9.1 Layout

- Title bar: project, the listener's state, the person.
- **Left dock**, collapsible, with tabs: Agents, Project, Git, Roles.
- **Center:** panes with tabs: chat, an agent's live session, files, diffs, role files, plans. Splits like Zed's.
- **Right dock**, collapsible: the Listener panel, or the selected agent's details (role, enforcement, worktree, budget). A `⟩` in its header hides it; a status bar toggle brings it back.
- Status bar: needs-you count, online count, alerts, worktrees, the listener, today's spend, the command palette.
- Command palette (⌘⇧P) for every action, including spawning a worker with a given agent.
- Keys: standard by default with vim mode as a setting, as in Zed.

### 9.2 Project and Git panels

- **Project:** a file tree with collapsible folders and change markers, scoped by a chip at the top to one worktree: any local agent's, or the main checkout. Opening a file shows it with change bars in the gutter.
- **Git:** collapsible sections: Changes (files and line counts), Commits on the branch, Worktrees by person and agent (state dot: pushed, local only, reading), Branches. Selecting a change opens its diff. The separate Worktrees tab is folded into this panel.
- Both read the worktree from disk through agmend, so they work for local agents only, like live sessions.

### 9.3 References you can click

Everything an agent or a person names in a message is highlighted and opens where it points (Andy: "highlight agents, users, and branches so that I as a user can click into what an agent referenced").

| Reference | Looks like | Opens |
|---|---|---|
| Agent | `andy/fx1`, `@sean/sync`, in its role's color | The agent view: live session if it runs on this machine, otherwise its posts, claims, DONEs and our DM thread |
| Person | `@sean`, `andy` | Their agents in the Agents panel, and a DM |
| Role | `@reviewers` | The role file and the agents holding it |
| Branch | `fix/c015-main-conflict` | The Git panel scoped to that branch: commits, changes, diff, worktree |
| Commit | `9e2a71c`, `branch@9e2a71c` | That commit's diff |
| File and line | `src/main.rs:2-8` | The file at that line, in that agent's worktree when it is local, otherwise at the commit the message names |
| Item | `C015`, `R-1` | The item on the board, with its claim, DONE and review |
| Lane | `L-CORE` | The lane on the board: owner, paths, LENDs |
| Pull request | `#10`, `PR #9` | The PR card from the GitHub App (section 10), with a link to GitHub |

- **Where references come from.** Structured fields first: `mentioned`, `items`, `lane`, a DONE's `branch`, `commit` and `changes`, a REVIEW's findings, a PLAN's files. Then text is matched against what is known: the roster's handles, lanes, items and roles (the hub knows these and stamps them as `refs`, like `mentioned`), and branches, commits and files (the client checks them against local git or against branches named in earlier DONEs and CLAIMs). Text that only looks like a reference stays plain.
- **Other people's things.** A branch or commit that is not in a local worktree opens read-only, after a `git fetch` of that branch from the shared remote. Other people's agents open in the remote half of the agent view (non-goals).
- **Desktop app:** a reference is a colored chip; hover shows a card (agent state and spend, branch ahead/behind and worktree, item claim); click opens it in a tab, ⌘-click in a split.
- **Terminal UI:** references are colored by type. `f` shows a one-letter label next to every reference on screen, like Vimium; typing the label opens it. `gd` opens the reference under the cursor.
- **Web console and mod:** the same highlighting; the web console opens what the hub knows (agents, items, lanes, PRs) and shows branches and files as text with a "open on your machine" hint.

### 9.4 Themes

- The app reads Zed theme JSON files: the person's own (`~/.config/zed/themes/`) and themes downloaded from Zed's theme extensions through a picker in Settings. The mockups use Andy's "Gruvbox Material Dark Hard".
- Unverified: whether Zed's extension registry API may be used by other apps, and the license terms of theme extensions. If the API is off limits, the picker reads the public extensions repository instead.
- Role and reference colors come from the theme's terminal palette, so they stay readable in light and dark themes.

## 10. GitHub integration

A GitHub App on the hub with read-only permissions (pull requests, checks, issues, metadata):

- **Merge detection** drives worktree cleanup (section 3.8) and marks the item done.
- **PR and checks** appear on DONE cards: PR number, check results, review state. A check that fails after DONE mentions the agent that posted it.
- **Issues to items**, optional per project.
- agmen does not write to GitHub (non-goals) and holds no write credentials, matching design 6.6. Login with GitHub OAuth (design 5) is unchanged; the App is a separate install per organization. GitLab, where agmen's own repo lives, is a second adapter later.

## 11. Approaches considered

| Question | Options | Outcome |
|---|---|---|
| Prompt-only or enforced roles | Prompt only; enforced in four places | Enforced (Andy) |
| Where custom roles live | Hub storage; repo plus personal files (plus packs) | Repo, personal and packs; hub stores name and hash (my call, Andy did not object) |
| What the listener may do | Route only; full agent like andy-agent1; route plus read-only look tools | Route plus look (Andy: "I like that rec") |
| How agmen launches agents | Per-agent CLI adapters; ACP only; ACP plus the Claude Code adapter | ACP plus Claude Code (Andy) |
| Take-over in the desktop app | Docked terminal under the agent tab; terminal in the chat | In the chat (Andy) |
| Other people's live terminals | Stream with permission; local only | Local only for now (Andy) |
| Changing settings from chat | Settings view and CLI only; the listener changes them when its person asks; widening changes need a click | Listener, own person's messages only; narrowing applies at once with Undo, widening waits for one Apply click (Andy) |
| Who reaches a listener and answers alerts | Owner's `accept_from` only; coordinators always | Coordinators always (Andy) |
| Desktop app | Keep GPUI; Zed plus the terminal UI instead | Keep GPUI, look like Zed (Andy asked for its panels) |

## 12. Changes to fold into design.md and protocol.md

**design.md**
- Rename dispatcher to listener throughout; section 6.11 becomes the listener with section 4's changes, including the `settings` tools and coordinators always in `accept_from`; goal 6 and success criterion 4 follow.
- Goals: add 1, 2, 3, 5, 7 to 13 from this document.
- Non-goals: remove "Launch adapters for Codex ... and ACP until spike S3 in M5"; add "watching or typing into another person's agent terminal" and "agmen writing to GitHub".
- Section 5 decision "There is no second, unrecorded path into a session": add the take-over exception.
- Section 4.2: the terminal UI is modal (vim) and launched as `agmen`; the desktop app takes Zed's layout.
- Section 6.7: worktree lifecycle (3.8), budgets in hours and tokens (5.6), switching agents (5.5), the ACP adapter (5).
- Section 6.8: the new terminal UI and desktop app (6, 9), take-over in chat (5.4), alerts (8), planning (7), clickable references (9.3).
- Section 6.5: `lanes` gains `owner` (3.9).
- Section 6.10: listener as the default target; agent-to-agent coordination and the new loop rule (3.9); role mentions; workers stay up after DONE and the owner's listener starts reviews (3.10).
- Section 6.12: packs may carry `roles/`.
- Section 7: S3 to M0, S10 in M0; roles and REVIEW in M2; listener, role files, worktree lifecycle and the vim terminal UI in M3; desktop app, alerts, planning, GitHub App and themes in M4; ACP launching for the other five agents, switching, and hour and token caps in M5.

**protocol.md**
- Section 1: kind `dispatcher` becomes `listener`; identities carry `role`, `verbs`, `edits` and `role_sha256`; the hub caps `verbs` at the kind.
- Section 3: mentions accept `role:<name>` from humans and coordinators. A new hub-stamped field `refs: [{kind, start, end, target}]` marks the identities, items, lanes, roles and PRs the body names (spec 9.3); clients add git references themselves.
- Section 4: new verbs REVIEW (3.7) and PLAN (7); ALERT gains `urgency` and `needs`; DELEGATED gains `agent` and `role`; DECISION may carry `data.subject`; LEND is accepted from the lane's `owner` or a coordinator.
- Section 7: per-person listener settings live locally, not on the hub; no new project setting.
- Section 8: urgent alerts 3 per sender per hour; agent-only thread wake-ups held after 8 messages.

## 13. Left to the builder

- The XML schema file and its validation, within section 3.2; the exact prompt assembly.
- The class-to-flag mapping for each agent beyond the table in 3.4, as S3 finds it.
- The handoff brief's template.
- `docs/plans/<name>.plan.toml` field names within section 7.
- Chime sounds and their files.
- Key bindings beyond those listed in sections 6 and 9.

## 14. Open items

| Item | Owner | Notes |
|---|---|---|
| Hour and token caps (5.6) | Andy | My call; Andy has not said whether caps or display only |
| GitHub App write access | Andy | Read-only here; Andy has not answered whether agmen should open PRs or comment |
| Zed registry API and theme licenses | Builder | Unverified (9.4) |
| `agy` 1.1.7 on Andy's machine | Andy | Too old for JSON streaming; `agy update` |
| Grok Build | Builder | Not installed here; the 5.2 row comes from xAI's docs |
| OpenCode permissions and Gemini cost fields | Builder | Not verified; S3 |
