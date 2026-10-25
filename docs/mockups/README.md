# Mockups

FigCraft prototypes for [the roles, listener and consoles spec](../specs/2026-10-25-roles-listener-consoles-design.md). Open a `.figcraft` file in FigCraft, press ⌥⌘⏎ to present, and pick a flow.

| File | What it shows | Flows |
|---|---|---|
| `agmen-console.figcraft` | The Zed-style desktop app and the vim-style terminal UI: chat with the listener, take-over in a terminal inside the chat, switching agents, the model and effort picker, Project and Git panels, urgent alerts, planning a greenfield project, clickable references, changing settings from chat | Desktop (Zed), Terminal (vim), Needs you (desktop), Needs you (terminal), Plan a greenfield project, Change settings from chat |
| `agmen-roles.figcraft` | The first pass: agents with role badges, launching a worker with a role, the roles library, worktree cleanup, the first chat | Agent roles, Chat |

Where the two differ, `agmen-console.figcraft` and the spec win. The first pass still says "dispatcher", has a separate Worktrees view, and suspends agmen for take-over.

Previews of every screen, exported at 1x, are in `console/` and `roles/`.

## Keys in the prototypes

- Terminal: `Enter` opens `andy/fx1`, `s` the spawn command, `f` the reference labels, `o` take-over, `q` back.
- Desktop: ⌘⇧P command palette, ⌥O open in terminal, ⌘R the listener panel.
- Escape ends a FigCraft presentation, so the prototypes use `q` for back.

## Rebuild

The scripts use only the Python standard library. Each writes a FigCraft command script, which `figcraft-cli` turns into the file:

```sh
uv run scripts/console.py console.json
figcraft-cli run console.json --out agmen-console.figcraft
figcraft-cli export agmen-console.figcraft -o console --format webp
```

`scripts/roles.py` builds `agmen-roles.figcraft` the same way. The mockups use the fonts SF Pro Text, Menlo and CaskaydiaCove Nerd Font Mono, and the colors of the Zed theme "Gruvbox Material Dark Hard"; FigCraft substitutes fonts that are not installed.
