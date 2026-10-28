# 0011: The desktop app's Backend trait

Status: accepted
Date: 2026-10-26

## Context

The desktop app is built before agmend's local API exists (design 6.7, the roles and listener plan). Its screens still need data, and the CLI and `agmen mcp` will later reach the same agmend. Task 6 of [the desktop plan](../plans/desktop.md) puts one seam between the views and wherever their data comes from.

## Decision

- `backend::Backend` is a small object-safe trait: `snapshot() -> Snapshot`, `send(Draft)`, `act(Action)`, and `subscribe()`, which returns a `futures` unbounded receiver of `Update`s (a new message, or a whole new snapshot). Views will hold it as `Rc<dyn Backend>`. Everything runs on GPUI's main thread, so there is no `Send` bound.
- Chat messages cross the seam as `agmen_proto::StampedEnvelope`, unchanged from the wire. The views render what the hub stamped, and the fixture can only build what the protocol accepts: every fixture message is written as wire JSON and parsed by agmen-proto's own deserializer.
- Everything else in `Snapshot` (roster rows, listener and agent details, status counts, change cards, the plan) is a desktop view type with only the fields some screen shows. agmend will compute these; their names follow the spec's words.
- `Draft` is the person's message: who it is `to` (the listener by default, spec 4), the body and an optional `reply_to`. `Action` lists what is not a message: Apply, Cancel and Undo a change card; take over, hand back and switch an agent; answer or snooze an alert; approve a spec or one's own share; kick off. The CLI's and `agmen mcp`'s commands for the same things should map onto these one to one.
- `FixtureBackend::for_screen(Screen)` serves the state of each of the 25 mockup renders (`docs/mockups/console/desktop-*.webp`), with the mockup fixes Andy chose on 2026-10-26. It keeps sends and change-card actions in memory and publishes them to subscribers. `agmen-desktop --screen <name>` opens the app in that state, and `crates/agmen-desktop/scripts/shot.sh <name>...` captures only the app's window into `target/shots/`.

## Consequences

- To replace the fixture, agmend must give the desktop app: the project chat as stamped envelopes, plus a push of new ones; the roster with each identity's role, harness, status and parent; listener and agent details (spec 4 and the agent view); worktree, branch and change data for the Project and Git panels; change cards with Apply, Cancel and Undo; plans and approvals; and alert state. Each of those is one field of `Snapshot` today.
- When the live backend fills `WorktreeInfo.texts` from disk, it must resolve each path a reference chip or the Project panel opens under its worktree root after following symlinks, and refuse one that leaves the root (review C297). `git ls-files` lists tracked symlinks, so an agent could commit `notes.md` pointing at `~/.ssh/id_ed25519`, and a chip for `notes.md` passes today's check that the worktree's `files` list holds the path. The fixture reads nothing from disk. Since the desktop-live plan's Task 8, agmend's `file_text` enforces this: it reads only in a worktree it lists for a checkout whose trusted identity file names the project, canonicalizes the path under that worktree and refuses one that leaves it or reaches into `.git`.
- Mockup items such as `C015` are not valid protocol item keys (`^[A-Z]{1,4}-[0-9]{1,6}$`). The fixture keeps the mockup's text in bodies and uses `C-015` in structured fields. The mockup and the spec examples should switch to dashed keys when someone owns them.
- The mockup's SETTINGS badge is not a protocol verb. The listener's message is an ANSWER, and the change card is local data attached to that message's id, because settings changes are local to agmend (spec 4).
- Since the desktop-live plan's Task 7, a plain `agmen-desktop` opens on `AgmendBackend`: the project named by the working folder's `.agmen/identity.json` (agmen-client's `find_identity`), else the last project in the `[desktop]` table of `~/.config/agmen/settings.toml`, at agmen-client's `agmend_socket()`. Without a project, or with agmend unreachable or refusing, the window says so and never shows the fixture; `--fixture` and `--screen` keep `FixtureBackend`. `Snapshot.listener` is optional, and every field agmend has no source for yet is empty, with a plain empty state in its panel.
- shot.sh needs Screen Recording permission for the terminal that runs it, and floats the window in AeroSpace when AeroSpace is running.
