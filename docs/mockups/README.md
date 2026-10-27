# Mockups

FigCraft prototypes for [the roles, listener and consoles spec](../specs/2026-10-25-roles-listener-consoles-design.md). Open a `.figcraft` file in FigCraft, press ⌥⌘⏎ to present, and pick a flow.

| File | What it shows | Flows |
|---|---|---|
| `agmen-console.figcraft` | The Zed-style desktop app and the vim-style terminal UI: chat with the listener, take-over in a terminal inside the chat, switching agents, the model and effort picker, Project and Git panels, urgent alerts, planning a greenfield project, clickable references, changing settings from chat, Settings → Appearance (theme, icon theme, fonts), drawn with Lucide icons | Desktop (Zed), Terminal (vim), Needs you (desktop), Needs you (terminal), Plan a greenfield project, Change settings from chat |
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
LUCIDE_ICONS=/path/to/lucide/icons uv run scripts/console.py console.json
figcraft-cli run console.json --out agmen-console.figcraft
figcraft-cli export agmen-console.figcraft -o console --format webp
```

`LUCIDE_ICONS` is the `icons/` folder of Lucide v1.61.0 (ISC), from the release archive https://github.com/lucide-icons/lucide/releases/download/1.61.0/lucide-icons-1.61.0.zip (SHA-256 `d4cfe5949e0cf202ec558c3fe2cc6c8373371c1cb8643fac2f7c68ce4153ff4b`, the same archive the desktop app embeds). Without it, UI icons become dots and file icons become squares. The layout code names UI icons by their Zed names, and `ZED_TO_LUCIDE` in `console.py` maps each to the Lucide icon drawn in its place. File and folder icons are Lucide's `file`, `file-code`, `file-text`, `folder` and `folder-open`, tinted per folder name or file suffix.

The script recolors copies of the icons into `icon-cache/` next to the output file; the SVG files are not committed. The mockups carry no icons from Zed or the Material Icon Theme: Zed's own icons fall under the Zed repository's GPL-3.0-or-later license (most are modified Lucide icons; see spec 9.4), and agmen is not GPL.

`export` names each file after its frame (`Desktop · chat.webp`); the committed previews use lowercase names with each run of other characters as one hyphen (`desktop-chat.webp`); this loop renames them:

```sh
for f in console/*.webp roles/*.webp; do
  n=$(basename "$f" .webp | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/-/g; s/-$//')
  [ "$f" = "$(dirname "$f")/$n.webp" ] || mv "$f" "$(dirname "$f")/$n.webp"
done
```

`scripts/roles.py` builds `agmen-roles.figcraft` the same way, without icons (`uv run scripts/roles.py roles.json`, then `run` and `export` into `roles/`). The mockups use the fonts SF Pro Text, Menlo and CaskaydiaCove Nerd Font Mono, and the colors of the Zed theme "Gruvbox Material Dark Hard"; FigCraft substitutes fonts that are not installed.
