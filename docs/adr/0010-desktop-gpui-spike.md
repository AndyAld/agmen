# 0010: Spike S8, the GPUI desktop app on macOS

Status: accepted for macOS; Windows and Linux unverified
Date: 2026-10-25

## Context

The desktop app is a GPUI app with Zed's layout (design 4.2 and 6.8, spec section 9). Spike S8 has to show, on macOS first, a window with Zed's docks, a long virtualized list, text input with IME, a native notification, a single instance, the person's Zed theme and fonts, and file icons from the system without `unsafe` code, which the workspace forbids. GPUI is pre-1.0 and changes often, so the spike also picks one source and pins it.

## Decision

**gpui source.** `gpui` and `gpui_macos` from git, `https://github.com/zed-industries/zed` at `08cbb88a152c3751a43e853425a70e9f631a5918` (zed `main` on 2026-10-25, "git_ui: Fix git graph lagging when toggling commit details panel (#66132)"), chosen by Andy on 2026-10-25 over crates.io.

- crates.io `gpui` 0.2.2 (2025-10-22) also builds and opens a window on Rust 1.99.1, but it is a year old, predates zed's split of the platform code into `gpui_platform`, `gpui_macos` and the rest, and has no system notification API. Under our `deny.toml` it needs four license exceptions (Apache-2.0 WITH LLVM-exception, CC0-1.0 twice, NCSA) and 13 advisory ignores.
- zed main passes our license list as is and needs three advisory ignores on the macOS graph: `paste` (through `metal`), `rustybuzz` and `ttf-parser` (through `usvg`, which draws SVG icons). None has a known vulnerability; all three are "unmaintained" notices. zed pins Rust 1.98.1, below our 1.99.1.
- The app depends on `gpui_macos` directly under `cfg(target_os = "macos")`, not on `gpui_platform`. `gpui_platform` pulls `gpui_web` for wasm, which pins `unicode-properties = "=0.1.3"`, and that conflicts with the 0.1.4 that `sqlx-postgres` already locks for agmen-client. Windows and Linux will add `gpui_windows` and `gpui_linux` under their own `cfg` lines.
- Features: `runtime_shaders`, because this Mac has no Metal Toolchain (`xcrun metal` fails with "missing Metal Toolchain; use: xcodebuild -downloadComponent MetalToolchain"), so shaders compile when the app starts; and `font-kit`, without which gpui draws no text (its log: "gpui_macos was compiled without the `font-kit` feature, so no text will be rendered"). `font-kit` brings `zed-font-kit` 0.14.1-zed (MIT OR Apache-2.0) from `https://github.com/zed-industries/font-kit` at `94b0f28`, the commit zed's workspace pins.
- `deny.toml` (approved by Andy) allows those two git sources and ignores the three advisories, each with its reason.
- gpui turns on `serde_json/preserve_order`. In a `--workspace` build that reaches every crate, so agmen-proto's `schema_drift` test now compares `.schema.json` files as parsed JSON (approved by Andy); the committed `schema/` files are unchanged.

## Results

| Check | How | Outcome |
|---|---|---|
| Window with Zed's layout | `cargo run -p agmen-desktop`: transparent title bar with inset traffic lights, left dock 272 px, center, right dock 340 px, status bar; ⌘B and ⌘R toggle the docks; test `workspace_toggles_docks` | Works on macOS 27.2. AeroSpace, the tiling window manager on Andy's Mac, tiles the window; floating it gives 1280 by 900, because `Bounds::centered` clamps to the primary display's visible width |
| 10,000-row virtualized list | `agmen-desktop --spike list --autoscroll` (release only): GPUI `list` with 10,000 wrapped rows of 1 to 60 words. After 3 warm-up frames it scrolls 120 px a frame until the top stops moving for 30 frames, then back to row 0, and times only frames shown while the window was active. Test `list_measures_10k_rows` (`#[ignore]`, run with `--release --run-ignored only`) asserts the last row rendered, at most 10 rows jumped in one frame, and p99 under 16.7 ms | The scroll is real: the last row (9,999) rendered, the top reached row 9,982, and the largest jump was 4 rows a frame. 2026-10-26 on the 144 Hz main display, with another agent's cargo builds running at the same time: 10,179 timed frames, p50 7.38 ms, p99 17.46 ms, max 26.30 ms, so the 16.7 ms target is missed by 0.8 ms under that load; to rerun on an idle machine. A window that is not in front is paced at about 35 ms a frame whatever the row cost, which is why inactive frames are skipped (1,386 that run). The earlier Task 3 numbers scrolled before the first layout and did not describe a scroll; they are withdrawn |
| Text input with IME | `ui::TextInput`, built on GPUI's `EntityInputHandler` and `ElementInputHandler` like gpui's `examples/input.rs`: marked text underlined, UTF-16 ranges converted at the edge, grapheme-wise backspace; one line scrolling sideways, or several wrapped lines (Shift-Enter) growing to a set maximum, painted inside a content mask; 8 buffer tests and 9 view tests, including composing "に" then committing "日本" through the handler and a 500-character token keeping the cursor in view | The handler path is tested. Typing with a real input method (Kotoeri, the emoji picker) by hand is still to do |
| Native notification | `notify::notify`: inside an `.app` bundle, GPUI's `App::show_system_notification` (`UNUserNotificationCenter`, which macOS delivers only for a bundle id); a bare binary posts through `notify-rust` 4.18.2 (MIT OR Apache-2.0, default features off and macOS only, so its Linux `zbus` stack and advisories stay out). `agmen-desktop --spike notify` | The bare-binary path works: Andy saw the banner on 2026-10-25. The bundled path is untested, because a throwaway bundle id would leave a notification permission entry; it gets tested with packaging. `user-notify` was ruled out (LGPL-3.0) |
| Single instance | `instance::acquire`, std only: `File::try_lock` on `~/Library/Application Support/agmen/desktop.lock`, and a Unix socket `desktop.sock` (mode 0600) where a second launch writes one JSON line of at most 64 KiB; 4 tests (forward, stale socket after a crash, oversize and bad lines dropped) | Works live: a second `agmen-desktop` exited 0 in 0.02 s and the first stayed the only process. macOS refuses `SO_RCVTIMEO` (EINVAL) once the peer has closed, so the reader ignores that error |
| Zed theme and fonts | `appearance::resolve` reads `~/.config/zed/settings.json` (JSONC with comments and trailing commas, through `jsonc-parser` 0.34, MIT, nesting capped at 512), finds the theme in `~/.config/zed/themes/` or an installed extension, fills missing keys from a built-in copy of the mockup's Gruvbox colors, and reads `~/.config/agmen/settings.toml`; 14 tests | Works: Andy's "Gruvbox Material Dark Hard" and fonts load; no Zed, a missing theme, a Zed file with no theme, or a bad file falls back with a warning that says why. Six role colors come from the theme's `players` palette and match the mockup; the listener's is `text.placeholder` mixed 30% toward `text`, within a few steps of the mockup's #a89984, because no theme key holds that color. Fonts and sizes follow Zed: unset keys use Zed's `.ZedSans` (IBM Plex Sans) at 16 and `.ZedMono` (Lilex) at 15, both bundled under the SIL OFL 1.1; the rem size is `ui_font_size` |
| UI and file icons, system icons without `unsafe` | Lucide v1.61.0 (2026-10-21, `lucide-icons-1.61.0.zip` SHA-256 `d4cfe5949e0cf202ec558c3fe2cc6c8373371c1cb8643fac2f7c68ce4153ff4b`): the 27 SVGs the views use (25, then `square` and `copy` for the window buttons), embedded, with Lucide's ISC license (which also carries Feather's MIT notice) in `assets/icons/LICENSE-lucide.txt`. File icons from the installed Zed icon theme through its own mapping (exact name, then longest suffix, then `default`; named folders open and closed); paths that leave the extension are refused. Without a Zed theme, Finder's icon for the file type through `NSWorkspace::iconForContentType` (UTType from the extension, `public.folder` for folders), cut to the one representation at least 64 px wide and returned as TIFF (about 20 KB, where the whole icon is a 36.9 MB TIFF of 32 representations), looked up on the background executor with the Lucide icon standing in; 13 tests | Works: Andy's Material Icon Theme draws `src`, `main.rs`, `Cargo.toml` and `README.md` as in the mockup, and Finder returns a different small TIFF for files, folders and plain files. All objc2-app-kit, objc2-foundation and objc2-uniform-type-identifiers 0.3.2 calls used are safe fns, so `unsafe_code = "forbid"` holds. The Project panel with the system icon theme shows Finder's folder and document icons: [img/0010-finder-icons.png](img/0010-finder-icons.png) (a throwaway HOME with `follow_zed = false` and `icon_theme = { kind = "system" }`). Threads: Apple documents none of the AppKit calls used as main-thread-only or as thread-safe; the Threading Programming Guide's Thread Safety Summary lists NSImage as generally not thread-safe but usable from any thread one thread at a time, and NSWorkspace is not mentioned (checked 2026-10-26). objc2-app-kit marks none of them `MainThreadOnly`. So every lookup runs in one background task at a time, and each NSImage is created and used on that task's thread only. Windows (shell icons) and Linux (freedesktop) unverified |

## Packaging and signing (notes, untested)

- The app needs a bundle (`agmen.app` with an `Info.plist` and a bundle id) for GPUI's own notifications, a Dock icon and deep links. `cargo-bundle` or a hand-written `Info.plist` both work for that; neither is installed or tried here.
- Signing: a Developer ID Application certificate, `codesign --options runtime`, then `xcrun notarytool submit --wait` and `xcrun stapler staple`, which need Andy's Apple account (design 4.3).
- Windows MSI and Linux deb and AppImage: not started.

## Consequences

- Moving the pin means re-running this spike's checks and `cargo deny check`; zed's API changes between commits.
- Build machines need Xcode. With `runtime_shaders` they do not need the Metal Toolchain, at the cost of compiling shaders at each start (not measured yet).
- Unverified: Windows and Linux (nothing has run there), packaging, signing and notarization.
- Window chrome: the title bar takes the platform's style (`PlatformStyle`). On macOS it leaves 96 px for the traffic lights. On Windows it draws minimize, maximize (restore when maximized) and close at the right edge, 36 px each as Zed's, and marks them and the bar with GPUI's `WindowControlArea` so Windows hit-tests them natively. Linux draws the same buttons only under client-side decorations, with click handlers, as Zed does, and only those `window_controls()` reports. No window asks for client decorations yet, so on X11 the server's decorations stay and agmen's buttons do not show (server decorations for now); Zed requests client decorations with resize edges itself (`zed.rs` near line 367), and agmen will when it adds `gpui_linux`. Zed's Windows buttons use Segoe glyphs chosen by an `unsafe` `RtlGetVersion` call; agmen draws Lucide `minus`, `square`, `copy` and `x` (from the same v1.61.0 archive) instead. Only the macOS look has run; the Windows and Linux layouts are checked by tests on macOS, not on those platforms.

### Deliberate shortcuts (Task 20, desktop/gpui at ff47dcd; refreshed on desktop/openref2 and desktop/routing3)

Each is a `shortcut:` comment in `crates/agmen-desktop/src`, naming its ceiling and what triggers a revisit. `rg 'shortcut:' crates/agmen-desktop` lists them; there are 49.

| Where | Shortcut |
|---|---|
| `alerts.rs:66` | quiet hours and muted agents are plain values the caller fills in; read them from AgmenSettings once its settings file has them (spec 9.4). |
| `appearance/catalog.rs:120` | reads the manifest once per theme file; cache by folder if an extension with many theme files makes the scan slow. |
| `appearance/marked.rs:125` | no macOS path until the wallpaper utility runs on macOS. |
| `appearance/settings.rs:42` | mode picks which half (light or dark) the theme picker lists; it does not switch the theme when the OS changes mode. Add light and dark theme keys if people ask for that. |
| `backend/fixture.rs:215` | the fixture stands in for agmend's Spawn; a later plan sends it to agmend, and the listener's answer comes from the hub. |
| `backend/fixture.rs:355` | the fixture records nothing for this; snoozing stays in the person's own view (Task 13). |
| `backend/fixture_data.rs:253` | a stand-in for the hub's own stamp_refs; once the fixture is replaced by agmend, the hub's refs arrive on the wire and this goes. |
| `backend/take_over.rs:6` | the screens are fixture grids; spike S10 picks the PTY and the terminal emulator that fill `TerminalScreen` behind `Backend`, and keys typed into the terminal reach nothing until then. |
| `icons/system.rs:9` | Windows (shell icons) and Linux (freedesktop theme) return None until S8 runs there. |
| `keymap_vim.rs:3` | the desktop app only records the choice; vim keys come with the terminal UI's keymap, and the desktop reads this setting when they do. |
| `main.rs:116` | macOS only until spike S8 runs on Windows and Linux (ADR-0010). |
| `main.rs:193` | polls every 250 ms, so focus lags up to 250 ms and the app wakes 4 times a second; switch to a channel the run loop wakes on when idle wakeups matter. |
| `main.rs:333` | the fixture stands in for agmend until its local API exists (ADR-0011). |
| `notify.rs:57` | Windows and Linux use GPUI's notifications until S8 runs there. |
| `roles.rs:124` | a copy of agmen-hub's private `may_send` (ingest.rs) for the three role kinds; read one shared table once agmen-proto exports it. |
| `roles.rs:202` | a child's verbs replace its parent's additions instead of being checked as a narrowing; the hub caps verbs at the identity kind anyway. |
| `ui/agent_view.rs:194` | Stop does nothing until the hub backend can stop an agent. |
| `ui/chat.rs:375` | each message clones the snapshot and rebuilds the context, O(messages); keep messages in a persistent list if long chats lag. |
| `ui/chat_context.rs:305` | PR cards come from the GitHub App (spec 10), a later plan; until then the card says so. |
| `ui/chat_context.rs:387` | ahead and behind counts need per-branch counts that WorktreeInfo does not hold; until the live backend (ADR-0011) reads them from git, the card shows the worktree's state line. |
| `ui/code_view.rs:293` | long lines are clipped; sideways scroll when the file and diff views need to show them whole. |
| `ui/dev_views.rs:253` | Open file does nothing here; the dev window hosts no file tabs. The workspace opens the file (tab_views.rs). |
| `ui/diff_view.rs:184` | long lines are clipped, as in the code view; sideways scroll comes with it. |
| `ui/ink_clip.rs:37` | the font's metrics, not the shaped line's, so a fallback font taller than `font` is still cut; shape the text if a label shows such glyphs. |
| `ui/pane/group.rs:224` | one direction per center, so the group is a flat list; nested splits need a tree of groups if people ask for them. |
| `ui/pane/item.rs:44` | a placeholder tab the status bar's search button and cmd-shift-f open; no task in the desktop plan builds search. Revisit when a plan names project search and its view. |
| `ui/pane/tabs.rs:289` | an item with no view shows its name: Search (no task builds it), Plan (no code opens one) and an agent with no session here (the Agents panel opens one; spec 9.3's remote agent view is a non-goal). Revisit when a plan builds search or that view. |
| `ui/plan_panel.rs:259` | the backend has no pause action yet (desktop plan, Task 19), so Pause all draws disabled; enable it once agmend has one. |
| `ui/plan_room.rs:182` | team rows take no click; the backend has no details to show for them. Emit `AgentsPanelEvent::Selected` with `TeamRow::id` once it does. |
| `ui/project_panel.rs:85` | the whole tree is built from one list of every path, as the fixture holds it. The live backend needs the worktree's root path and lazy loading per folder before large repos (100k files) reach it. |
| `ui/project_panel.rs:174` | derived from the head until the backend gives each worktree's root path (the live backend, ADR-0011). |
| `ui/project_panel.rs:385` | a file the backend has not read shows a placeholder line; the live backend reads files and their marks from `git diff` (ADR-0011). |
| `ui/project_panel.rs:427` | Diff does nothing: the file view sends the workspace no events, and no task in the desktop plan wires one. Revisit when a file view event opens TabItem::Diff, as GitPanelEvent::OpenDiff does from the Git panel. |
| `ui/roles_panel.rs:267` | Duplicate does nothing until agmend writes role files; no task in the desktop plan does. Revisit with the live backend (ADR-0011). |
| `ui/settings_view.rs:161` | a flat list capped at 40 rows; a virtual list (Task 3's) if people browse all ~700. |
| `ui/settings_view.rs:256` | scans on the UI thread when Settings opens or an install ends; move to the background executor if a person's extension folder makes opening Settings slow. |
| `ui/spec_view.rs:58` | only the fixture's design spec has text; agmend serves the others at their commit once its local API exists. |
| `ui/spec_view.rs:115` | the backend has no changes-requested action, so Request changes draws disabled; enable it once agmend records one. |
| `ui/syntax.rs:4` | Markdown uses the block grammar only (headings, lists, code fences); inline emphasis and links come with the inline grammar's injection when a screen needs them. |
| `ui/syntax.rs:14` | UI-thread highlighting caps files at 128 KiB; highlight on the background executor when larger files need colors. |
| `ui/tab_views.rs:181` | the text is read when the tab is first drawn and kept while it stays open, so a change made in Settings after that shows once the tab is reopened; reload it on Settings' saves when the file tab gets edited in place. |
| `ui/terminal_view.rs:519` | Open as tab draws but does nothing until the center has a terminal tab (spec 5.4); the pane's TabItem has no Terminal variant yet, and the pane belongs to agmen-c7. |
| `ui/title_bar.rs:76` | no window asks for client decorations yet, so X11 keeps server ones and these buttons stay hidden; request them with resize edges, as Zed does, when gpui_linux is added. |
| `ui/workspace.rs:185` | the mockup's tabs stand in for the tabs the person left open. No task in the desktop plan restores tabs yet; the change that restores them once agmend keeps them deletes this function and its call in Workspace::new, so production windows stop opening the mockup's tabs. |
| `ui/workspace_palette.rs:174` | the result goes to stderr only; show it in the window once the status bar has a place for transient notes. |
| `ui/workspace_refs.rs:96` | a sender with no worktree here that holds the path (a person, another machine's agent, or a local agent whose worktree lacks it) opens the reader's own copy, which may be at another version, and the chip's card names that copy (C315); spec 9.3 opens the file at the commit the message names, which needs the live backend (ADR-0011) to read a file at a commit. |
| `ui/workspace_refs.rs:114` | items, lanes and PRs show their hover card only; the board (items and lanes) and the GitHub App (PR cards, spec 10) are later plans, and each gives its kind a place to open. |
| `ui/workspace_refs.rs:139` | no per-commit diff; the worktree's change is not the commit's (9e2a71c and 4d9fdc8 would show the same diff), so a commit shows its row instead. Once the backend gives a commit its own hunks, a commit opens those. |
| `ui/workspace_take_over.rs:47` | the Git and Project panels, the center's TabViews and the plan room keep the snapshot they were built with, so after an update a new worktree's file chip opens a placeholder tab and its branch chip a Git panel on another worktree (C314). Latent while nothing sends such updates; they follow too once the live backend (ADR-0011) sends worktree changes. |
