# agmen desktop app plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A GPUI desktop app, `agmen-desktop`, that looks and works like Zed and shows every desktop screen in the console mockup, driven by fixture data behind one small trait that agmend, the CLI and `agmen mcp` can plug into later.

**Architecture:** One workspace member, `crates/agmen-desktop`, a binary with private modules. `appearance` reads the person's Zed theme, icon theme and fonts (read-only) and agmen's own `settings.toml`; `backend` holds the `Backend` trait and `FixtureBackend`, which serves the mockup's data as `agmen-proto` types; `refs` finds clickable references; `ui` holds the GPUI views (workspace, docks, panes, chat, panels, menus, settings, plan). Spike S8 (Tasks 1 to 5) proves the platform pieces on macOS first and records them in ADR-0010.

**Tech Stack:** Rust 1.99.1, edition 2024; gpui (pinned in Task 1, ADR-0010); agmen-proto (read-only, path dependency); Lucide SVGs (ISC); serde, serde_json, toml, regex; anyhow in the binary, thiserror in modules with their own error enum.

**Spec:** [../specs/2026-10-25-roles-listener-consoles-design.md](../specs/2026-10-25-roles-listener-consoles-design.md) sections 4, 5.4, 5.5, 7, 8, 9, 13, 14; [../design.md](../design.md) section 6.8 ("Desktop app", "Agent view", "Appearance") and the M4 row of section 7 (S8's checklist); [../mockups/README.md](../mockups/README.md), `../mockups/agmen-console.figcraft` (the source of truth for the look), `../mockups/console/desktop-*.webp` (one render per screen) and `../mockups/scripts/console.py` (exact sizes, colors, spacing and fixture text); [../protocol.md](../protocol.md) and `crates/agmen-proto` for message types.

**Out of this plan:** agmend's local API, the hub and MQTT; a real PTY in take-over (spike S10 picks the crate; Task 14 renders a fixture screen behind the same trait); Windows and Linux runs (marked unverified in ADR-0010); packaging, signing, notarization, updater, deep links (notes in ADR-0010 only); the GitHub App's PR cards beyond a fixture chip.

## Global Constraints

- Paths: only `crates/agmen-desktop/**`, the `members` list in the root `Cargo.toml`, `Cargo.lock`, `deny.toml` only if Task 1 records why (otherwise ask Andy), `docs/plans/desktop.md`, and `docs/adr/0010-*.md` to `docs/adr/0019-*.md`. `crates/agmen-proto`, `agmen-client`, `agmen-hub`, `agmen-cli`, `mods/` and `deploy/` are read-only; anything needed there goes in the "Notes for Andy" section below.
- Crate: `crates/agmen-desktop`, package and binary `agmen-desktop`, `version.workspace`, `edition.workspace` (2024), `rust-version.workspace` (1.99.1), `[lints] workspace = true`. design.md section 4.3 names `apps/desktop`; Andy's brief names `crates/agmen-desktop`, which wins.
- No `unsafe` in our code (`unsafe_code = "forbid"`). No `unwrap`, `expect`, `panic!`, `todo!` or untrusted indexing outside tests. `#[expect(lint, reason = "...")]`, never `#[allow]`. Tests move to `foo_tests.rs` once they are half the file.
- Errors: `anyhow` with `.context()` in `main.rs` and at app wiring; each module with its own failure modes (`appearance`, `backend`, `instance`) has one `thiserror` enum.
- Dependencies: `cargo add` only; `--locked` in every gate command; `cargo deny check` passes; git sources only for gpui and only at the exact `rev` ADR-0010 records.
- Icons: UI icons are Lucide SVGs, embedded with `include_bytes!`, with Lucide's ISC notice in `crates/agmen-desktop/assets/icons/LICENSE-lucide.txt` and shown in Settings → About. File and folder icons are read at runtime from the person's installed Zed icon theme, and from the OS when there is none. Zed's own UI icon SVGs are never committed (no license of their own in a GPL-3.0-or-later repo).
- Zed's files are read-only: agmen never writes under `~/.config/zed` or `~/Library/Application Support/Zed`.
- agmen's settings: `~/.config/agmen/settings.toml` (spec 9.4), on macOS too, because the terminal UI reads the same file.
- Look: 1440 by 900 window; colors from the theme, with the mockup's "Gruvbox Material Dark Hard" values as the built-in fallback (`console.py` lines 22 to 37); fonts and sizes as Zed shows them now (Andy, 2026-10-26): `ui_font_family` (unset means Zed's `.ZedSans`, IBM Plex Sans) at `ui_font_size` (15 for Andy), `buffer_font_family` (CaskaydiaCove Nerd Font Mono) at `buffer_font_size` (13); unset sizes use Zed's 16 and 15. IBM Plex Sans and Lilex ship in `assets/fonts/` under the SIL OFL 1.1. The window's rem size is `ui_font_size`, and text uses Zed's scale only (`rems_from_px` 16, 14, 12, 10 in `ui/tokens.rs`); code text uses the buffer font at its px size. The mockup's SF Pro Text and fixed 11 to 13 px sizes are superseded. Sizes from `console.py`: title bar 38 px, status bar 28 px, left dock 272 px, right dock 340 px, tab bar 34 px, breadcrumb bar 32 px, tree rows 24 px, monospace cell 7.62 by 18 px at 13 px.
- Copy: plain words, no em or en dashes, in code, UI text, docs and commits. Commit messages: one short line, no trailers.
- Gate, all green before every commit:
  ```sh
  cargo fmt --all --check
  cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
  cargo nextest run --workspace --locked
  cargo test --doc --workspace --locked
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
  cargo deny check
  ```
- Screens: each UI task ends by capturing its screens with `crates/agmen-desktop/scripts/shot.sh <screen>` (Task 6) and comparing them side by side with the named `desktop-*.webp`. Differences that remain go in the task's "Differences" line in this file.

## Review Focus

1. **No Zed installed, or Zed settings that do not parse.** `~/.config/zed/settings.json` missing, JSONC with comments and trailing commas (Andy's has both), `theme` as an object (`{"mode": "system", "light": ..., "dark": ...}`), or a theme name no file defines. Expected: the app starts with the built-in Gruvbox fallback and Settings says why. Tests: Task 2 `zed_settings_jsonc_trailing_commas`, `theme_object_follows_system_mode`, `unknown_theme_falls_back`.
2. **A Zed theme missing keys.** Third-party themes omit `terminal.ansi.*`, `players` or status colors. Expected: each missing key falls back to the built-in value, never a black or transparent fill. Test: Task 2 `partial_theme_fills_from_fallback`.
3. **Message text that only looks like a reference, or overlaps one.** `v1.2.3`, `abc1234` that is not a commit, `#` in prose, a file path inside a branch name, hub `refs` whose byte offsets split a UTF-8 character. Expected: plain text, never a panic or a chip on half a word. Tests: Task 8 `refs_ignore_non_commit_hex`, `refs_reject_offsets_off_char_boundary`, `refs_prefer_hub_ref_over_regex_overlap`.
4. **A long or wide chat.** 10,000 messages, a 32 KiB body, one unbroken 500-character token. Expected: smooth scroll (no frame over 16.7 ms in the S8 measurement), wrapping without horizontal overflow. Tests: Task 3 `list_measures_10k_rows` (spike measurement), Task 9 `message_wraps_long_token`.
5. **Two launches.** A second `agmen-desktop` while one runs, and a stale lock left by a crash. Expected: the second focuses the first and exits 0; a stale lock is taken over. Tests: Task 4 `second_instance_forwards_and_exits`, `stale_socket_is_replaced`.

---

## Phase S8: the spike

### Task 1: Crate, pinned gpui, and Zed's window layout

- [x] Done. gpui and gpui_macos at zed `08cbb88a` (ADR-0010); deny.toml changes and the agmen-proto drift test change approved by Andy.

**Files:**
- Create: `crates/agmen-desktop/Cargo.toml`, `src/main.rs`, `src/ui/mod.rs`, `src/ui/workspace.rs`, `src/ui/dock.rs`, `docs/adr/0010-desktop-gpui-spike.md`
- Modify: `Cargo.toml` (members), `Cargo.lock`

**Interfaces:**
- Produces: `ui::Workspace` (a GPUI view) with `left_dock: Dock`, `right_dock: Dock`, `center: Entity<Pane>` and `fn toggle_dock(&mut self, side: DockSide, cx)`; `pub(crate) enum DockSide { Left, Right }`; actions `ToggleLeftDock` (⌘B), `ToggleRightDock` (⌘R), `Quit` (⌘Q).

- [ ] **Step 1:** Pick the gpui source. Research on 2026-10-25 found: crates.io `gpui` is still 0.2.2 (2025-10-22), from before zed split the platform code into `gpui_platform`, `gpui_macos` and others; zed `main` is at `08cbb88a152c3751a43e853425a70e9f631a5918` (2026-10-25) and pins Rust 1.98.1, below our 1.99.1; `gpui-ce` 0.3.0 and longbridge's `gpui-pre` 0.3.10 are third-party snapshots. Build a 20-line window with crates.io 0.2.2 first. If it fails, or lacks `show_system_notification` or another S8 API, use `gpui` and `gpui_platform` from git at that exact rev (re-check main's HEAD on the day and take the newest green one). Record the version or commit, its date, and why in ADR-0010 "Decision". Add with `cargo add gpui --git https://github.com/zed-industries/zed --rev <sha>` (same for `gpui_platform`). Allowing that git source in `deny.toml` (`[sources] allow-git`) is Andy's call: if it is needed, stop and ask.
- [ ] **Step 1b:** If `xcrun metal` is missing on a build machine, Zed's docs say `xcodebuild -downloadComponent MetalToolchain`; test whether gpui's `runtime_shaders` feature avoids the need and record it.
- [ ] **Step 2:** Run `cargo deny check`. Every new license must already be in `deny.toml`'s allowlist; if one is not, stop and list the crates and licenses for Andy instead of widening the list.
- [ ] **Step 3: Write the failing test** `workspace_toggles_docks` with GPUI's `TestAppContext`: a new `Workspace` has both docks open; `ToggleRightDock` closes the right one; a second dispatch opens it.
- [ ] **Step 4:** Implement `main.rs` (`Application::new().run`, a 1440 by 900 window with a transparent title bar and traffic lights inset to match the mockup's 38 px bar), `Workspace` (title bar, left dock 272 px, center, right dock 340 px, status bar 28 px), and the actions. Placeholder text in each region.
- [ ] **Step 5:** Run the gate; launch with `cargo run -p agmen-desktop` and check the four regions and both toggles by hand.
- [ ] **Step 6: Commit** `Add agmen-desktop with gpui pinned and Zed's dock layout`

### Task 2: Zed theme, fonts and agmen's appearance settings

- [x] Done. Fonts are `FontPrefs` (all optional) in both settings sources, resolved to `Fonts`; role colors from the `players` palette (ledger ruling).

**Files:**
- Create: `src/appearance/mod.rs`, `src/appearance/zed.rs` (settings.json reader), `src/appearance/theme.rs` (theme JSON to `Theme`), `src/appearance/fallback.rs` (mockup colors), `src/appearance/settings.rs` (agmen `settings.toml`), `src/appearance/theme_tests.rs`, `tests/fixtures/zed/settings.json`, `tests/fixtures/zed/themes/partial.json`
- Modify: `src/ui/workspace.rs` (paint from `Theme`)

**Interfaces:**
- Produces:
  - `pub(crate) struct Theme` (Clone) with fields `background, surface, elevated_surface, element_selected, border, border_variant, text, text_muted, text_placeholder, text_accent, editor_background, error, warning, success, info, ansi: [Hsla; 16], players: Vec<Hsla>` and `fn role_color(&self, role: &RoleName) -> Hsla` (from the theme's `players` palette: planner 0, coordinator 1, reviewer 2, engineer 3, researcher 4, tester 6; listener mixes `text.placeholder` 30% toward `text`, which gives the mockup's #a89984 within a few steps, since no theme key holds that color).
  - `pub(crate) struct Fonts { ui_family: SharedString, ui_size: Pixels, code_family: SharedString, code_size: Pixels, code_ligatures: bool, terminal_family: Option<SharedString>, terminal_size: Pixels }`.
  - `pub(crate) struct ZedSettings { theme: ThemeChoice, icon_theme: Option<String>, fonts: Fonts }`; `pub(crate) enum ThemeChoice { Named(String), BySystem { mode: Mode, light: String, dark: String } }`; `fn read_zed_settings(dir: &Path) -> Result<ZedSettings, AppearanceError>` (`dir` is `~/.config/zed` in the app, a fixture in tests).
  - `fn load_theme(zed_dir: &Path, extensions_dir: &Path, name: &str) -> Result<Theme, AppearanceError>` searching `zed_dir/themes/*.json` then `extensions_dir/*/themes/*.json`, filling missing keys from `fallback::gruvbox_material_dark_hard()`.
  - `pub(crate) struct AgmenSettings { follow_zed: bool (default true), theme: Option<String>, mode: Mode, icon_theme: IconThemeChoice, fonts: Fonts overrides, density: Density }`, `fn load(path) / fn save(&self, path)`; `pub(crate) enum Density { Comfortable, Compact }`; `pub(crate) enum IconThemeChoice { Zed(String), Lucide, System }`.
  - `pub(crate) struct Appearance { theme: Theme, fonts: Fonts, icon_theme: IconThemeChoice, source: AppearanceSource, warnings: Vec<String> }` as a GPUI `Global`, built by `fn resolve(home: &Path) -> Appearance`, which never fails (warnings explain fallbacks).
- [ ] **Step 1: Write the failing tests:** `zed_settings_jsonc_trailing_commas` (fixture copied in shape from Andy's file: tabs, `//` comments, trailing commas; asserts theme "Gruvbox Material Dark Hard", icon theme "Material Icon Theme", code font "CaskaydiaCove Nerd Font Mono" 13, UI size 15), `theme_object_follows_system_mode`, `unknown_theme_falls_back` (warning names the theme), `partial_theme_fills_from_fallback` (`partial.json` lacks `terminal.ansi.red`; result equals the fallback's), `gruvbox_hard_matches_mockup` (loading the real theme file shape gives `background` #1d2021 and `text` #d8bf94 within 1/255), `agmen_settings_round_trip` (tempdir), `follow_zed_off_uses_agmen_theme`.
- [ ] **Step 2:** Pick the JSONC reader: check for a maintained crate under the allowlisted licenses that handles comments and trailing commas, else write a small comment and trailing-comma stripper before `serde_json` (string-aware, bounded by input length). Note the choice in ADR-0010.
- [ ] **Step 3:** Implement; wire `Appearance` as a global and paint `Workspace` from it. Fonts apply through GPUI's text style on the root.
- [ ] **Step 4:** Gate; compare colors by eye against `desktop-chat.webp`.
- [ ] **Step 5: Commit** `Load the Zed theme and fonts, with a Gruvbox fallback`

### Task 3: Virtualized 10,000-row list and text input with IME

- [x] Done, except the hand check below. The input is multi-line on request (Andy, 2026-10-26): `TextInput::new(cx).multiline(max_lines)` wraps, grows to `max_lines`, then scrolls; Shift-Enter adds a newline and Enter submits; one line scrolls sideways; both paint inside a content mask; `set_text`; text is capped at 64 KiB. The spike scrolls only after the first layout, until the last row renders and back, and times only frames shown while the window is active; its test is `#[ignore]` and runs with `--release --run-ignored only`.
- [ ] Hand IME check: type with Kotoeri (Japanese) and the emoji picker in the running app, and record what was typed in ADR-0010.
- [ ] Rerun `list_measures_10k_rows` on an idle machine and record it in ADR-0010; the only run so far (17.46 ms p99, under load) misses its own 16.7 ms assertion. If it still misses, take it to Andy (review C69).

**Files:**
- Create: `src/ui/text_input.rs` (single and multi line input, the GPUI input-handler pattern from gpui's `examples/input.rs`), `src/ui/text_input_tests.rs`, `src/spike.rs` (`--spike list` mode)
- Modify: `src/main.rs`, `src/ui/workspace.rs`

**Interfaces:**
- Produces: `pub(crate) struct TextInput` (GPUI view, implements `EntityInputHandler` and `Focusable`) with `fn text(&self) -> &str`, `fn set_text(&mut self, s, cx)`, `fn placeholder(self, s) -> Self`, event `TextInputEvent::Submit(String)` on Enter (Shift-Enter inserts a newline). Marked text (IME composition) is shown underlined and replaced on commit.
- [ ] **Step 1: Write the failing tests:** `input_inserts_and_submits` (simulated keystrokes "hi", Enter: one `Submit("hi")`, then empty), `input_ime_marked_text_commits` (call `replace_and_mark_text_in_range` with "に", then `replace_text_in_range` with "日本": text is "日本", no marked range), `input_backspace_by_grapheme` ("é👍🏽" then backspace twice: empty), `input_utf16_ranges_map_to_utf8` (IME ranges arrive in UTF-16).
- [ ] **Step 2:** Implement `TextInput`.
- [ ] **Step 3:** `agmen-desktop --spike list` opens a window with a 10,000-row virtualized list (GPUI's `list` with variable heights, since chat rows wrap) above a `TextInput`, and logs frame times while scrolling with `--spike list --autoscroll` (scrolls top to bottom and back at a fixed speed, prints the slowest frame and the 99th percentile, exits). Test `list_measures_10k_rows` runs only with `AGMEN_GUI=1` (it opens a window) and asserts p99 under 16.7 ms in a release build.
- [ ] **Step 4:** Measure: `AGMEN_GUI=1 cargo nextest run -p agmen-desktop --release list_measures_10k_rows`; type Japanese (Kotoeri) and an emoji picker entry into the input by hand. Record numbers and what was typed in ADR-0010 "Results".
- [ ] **Step 5:** Gate. **Commit** `Prove a 10k-row list and IME text input in GPUI`

### Task 4: Native notification and single instance

- [x] Done. `notify(tag, title, body, cx)`; instance forwarding polled every 250 ms (`shortcut:`); spikes skip the instance lock so they run beside the app.

**Files:**
- Create: `src/notify.rs`, `src/instance.rs`, `src/instance_tests.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces:
  - `pub(crate) fn notify(title: &str, body: &str, cx: &mut App) -> Result<(), NotifyError>`: GPUI's own `App::show_system_notification` (zed main has it; its example says macOS delivers only from an app bundle), and `notify-rust` 4.18 (MIT OR Apache-2.0; NSUserNotificationCenter, works from an unbundled binary, deprecated by Apple) for `cargo run` builds. `user-notify` is out (LGPL-3.0). Never blocks the UI thread.
  - `pub(crate) enum Instance { Primary(Listener), Secondary }`; `fn acquire(dir: &Path) -> Result<Instance, InstanceError>`: an exclusive `std::fs::File::try_lock` (1.89) on `dir/desktop.lock`, plus a local socket `dir/desktop.sock` (mode 0600) that the primary listens on; a secondary connects, writes `{"focus":true,"args":[...]}\n` (at most 64 KiB) and exits 0. `Listener::next() -> Option<Forwarded>` feeds the app, which activates its window.
  - `dir` is `~/Library/Application Support/agmen` on macOS (same parent as agmend's socket in roles-listener.md).
- [ ] **Step 1:** Compare notification crates (license, macOS 26 behavior for an unbundled binary versus a `.app` bundle id) and single-instance options; pick, record in ADR-0010.
- [ ] **Step 2: Write the failing tests** (tempdir, no GUI): `first_acquire_is_primary`, `second_instance_forwards_and_exits` (second `acquire` returns `Secondary` and the primary's `next()` yields its args), `stale_socket_is_replaced` (a leftover socket file with no lock holder: the next `acquire` is primary), `forward_over_limit_is_dropped` (a 65 KiB line is refused, the primary keeps running).
- [ ] **Step 3:** Implement; `main` acquires before opening a window. `--spike notify` sends one notification "agmen" / "adam/coord needs you". Run it from a plain binary and from a throwaway `.app` bundle built in the scratch dir; record which shows a banner.
- [ ] **Step 4:** Gate. **Commit** `Add single instance and native notifications`

### Task 5: Icons: Lucide, the Zed icon theme, and macOS file icons

- [x] Done. `FileIcons` global resolves Zed theme, then Finder, then Lucide; file icons draw with `img()` (own colors), UI icons with `svg()` (tinted). Lucide's license shows in About (Task 17). Interfaces as built: `UiIcon::render(color, size)` (the plan said `icon`); `IconTheme::load` returns `AppearanceError` (no separate `IconError`); `chevron_icons` is not read (Lucide chevrons are used); `FileIcons::icon(path, is_dir, open)` replaces `file_icon(name, is_dir, open, cx)` and looks up by file name and type, so the path need not exist. Finder icons are looked up by type (`iconForContentType`), cut to one 64 px representation (about 20 KB), on the background executor, with the Lucide icon until they arrive. Zed's lookup order: whole name, each suffix, then the first prefix, each in stems then suffixes, then `default`; a mapping to a missing SVG counts as no match. The ADR screenshot of Finder icons waits for Task 11's Project panel, the first view that draws file icons.

### Task 2b: Marked's styles and themes

- [x] Done. `Theme.syntax` is a map by Zed's syntax names (what Task 10 looks up), not a bare ramp; Marked's ramp fills it the way Marked's syntax JSON does. The theme is chosen by name, "Wallpaper Adaptive (Balanced)". `theme_overrides` (per theme) and `experimental.theme_overrides` (global) are read when following Zed. Shot with a throwaway HOME choosing the Marked theme: translucent and blurred as intended.

Andy's decision (2026-10-26): reuse what his own app Marked (`~/Projects/Marked`, MIT, Andy's project) does for color.

**Files:** Modify `src/appearance/theme.rs`, `src/appearance/fallback.rs`, `src/appearance/mod.rs`, `src/appearance/zed.rs`; create `src/appearance/marked.rs`, `src/appearance/marked_tests.rs`.

**Interfaces:**
- `Theme` gains Marked's tokens where it lacks them: `element`, `accents: Vec<Hsla>`, `syntax: Vec<Hsla>` (an 8-color ramp), `selection`. `destructive` and `informational` are aliases of `error` and `info`, not new fields.
- `marked::wallpaper_adaptive()` returns Marked's default palette "Wallpaper Adaptive (Balanced)" (Marked `src/theme.rs:32` to `80`: background `#101010b8`, accent `#68acd4`, the syntax ramp `#aea1db` to `#c5ab72`), offered as a built-in theme next to the Gruvbox fallback.
- `marked::load_wallpaper_palette(path)` reads the wallpaper utility's `state.json` (`active_palette.accent`, `accents`, `syntax`, as Marked `src/theme.rs:84` to `130` does) and overlays it on the built-in palette. The file is at `%LOCALAPPDATA%/ZedWallpaperThemeButton/state.json`, which exists on Windows only today; on macOS the function is wired but finds nothing (`shortcut:` until the utility runs here).
- Zed's `theme_overrides` (from `settings.json`, keyed by theme name, or `experimental.theme_overrides`) are applied over the active theme's style before it becomes a `Theme`.
- The syntax colors for `ui/syntax.rs` (Task 10) come from the theme's `syntax` map, with Marked's Zed-style syntax JSON (Marked `src/theme.rs` near line 240) as the shape for the built-in themes.
- Translucent backgrounds (alpha below 1) use GPUI's own window background appearance (`WindowBackgroundAppearance::Blurred` or `Transparent`). Do not copy Marked's `windows_blur.rs`, which uses `unsafe`.
- [ ] **Step 1: Tests:** `wallpaper_adaptive_matches_marked` (accent `#68acd4`, eight syntax colors, background alpha 0xb8), `state_json_overrides_accent_and_syntax` (tempdir file), `missing_state_json_keeps_defaults`, `theme_overrides_replace_keys` (an override of `background` wins over the theme file), `translucent_theme_requests_blur`.
- [ ] **Step 2:** Implement; gate; shot `chat` with the Marked theme chosen. **Commit** `Add Marked's palette and Zed theme overrides`

**Files:**
- Create: `crates/agmen-desktop/assets/icons/lucide/*.svg` (only the icons used: `users`, `folder-tree`, `git-branch`, `user`, `search`, `square-terminal`, `messages-square`, `bell`, `settings`, `chevron-down`, `chevron-right`, `chevron-left`, `sparkle`, `diff`, `pencil`, `x`, `check`, `plus`, `minus`, `corner-down-left`, `circle-alert`, `panel-right`, `file`, `folder`, `folder-open`), `assets/icons/LICENSE-lucide.txt`, `src/assets.rs` (GPUI `AssetSource`), `src/icons/mod.rs`, `src/icons/zed_theme.rs`, `src/icons/system.rs`, `src/icons/zed_theme_tests.rs`, `tests/fixtures/zed/icon_theme/icon_themes/fixture.json` plus three small SVGs
- Modify: `src/ui/workspace.rs` (left dock tab icons, status bar buttons)

**Interfaces:**
- Produces:
  - `pub(crate) enum UiIcon { Agents, Project, Git, Roles, Search, Terminal, Chat, Bell, Settings, ChevronDown, ChevronRight, ChevronLeft, Sparkle, Diff, Pencil, Close, Check, Plus, Minus, Enter, Alert, PanelRight, File, Folder, FolderOpen }` with `fn path(self) -> &'static str` and `fn icon(self, color: Hsla, size: Pixels) -> Svg`.
  - `pub(crate) struct IconTheme` from a Zed icon theme JSON (`file_stems`, `file_suffixes`, `file_icons`, `directory_icons`, `named_directory_icons`, `chevron_icons`), `fn load(ext_dir: &Path, name: &str) -> Result<IconTheme, IconError>`, `fn file(&self, name: &str) -> Option<PathBuf>`, `fn folder(&self, name: &str, open: bool) -> Option<PathBuf>`; lookup order is Zed's: exact stem, then longest suffix (`tar.gz` before `gz`), then default.
  - `pub(crate) enum FileIcon { Svg(PathBuf), Image(Arc<Image>), Ui(UiIcon) }` and `fn file_icon(name, is_dir, open, cx) -> FileIcon` choosing Zed theme, then system, then Lucide `File`/`Folder`.
- [ ] **Step 1:** Download Lucide v1.61.0 (2026-10-21; record version and SHA-256 of the archive in ADR-0010), copy only the listed SVGs and the license text.
- [ ] **Step 2: Write the failing tests:** `icon_theme_stem_beats_suffix` (`Cargo.toml` uses its stem entry), `icon_theme_longest_suffix_wins`, `icon_theme_named_folder_open_and_closed`, `icon_theme_missing_file_falls_back` (mapping points at a missing SVG: `None`), `material_theme_found_by_name` (fixture dir named `material-icon-theme`, theme `name` "Material Icon Theme"), `every_ui_icon_asset_exists`.
- [ ] **Step 3:** `icons::system`: get a file type's Finder icon as PNG bytes without our own `unsafe`. Try, in order, `objc2-app-kit` 0.3.2 (`NSWorkspace::sharedWorkspace`, `iconForFile`, `iconForContentType` and `NSImage::TIFFRepresentation` are safe fns there; `representationUsingType_properties` is unsafe, so keep TIFF and let GPUI decode it, or convert with a decoder already in the tree), then `file-icon-provider` 1.0.2 (MIT, safe API, returns pixels), then running `/usr/bin/sips` or `qlmanage -t` into a cache under `~/Library/Caches/agmen/icons/`. Record which worked, with a screenshot, in ADR-0010; Linux (freedesktop) and Windows (shell) stay unverified.
- [ ] **Step 4:** Implement, show icons in the docks and status bar, gate.
- [ ] **Step 5:** Finish ADR-0010: status accepted for macOS, a results table for all seven S8 checks (window and layout, 10k list, IME, notification, single instance, Zed theme, system icons), packaging and signing notes (`cargo-bundle` or a hand-written `Info.plist`, Developer ID, notarytool; untested), Linux and Windows unverified.
- [ ] **Step 6: Commit** `Show Lucide UI icons and Zed or Finder file icons; record S8 in ADR-0010`

## Phase screens

From here every task follows the same shape: write the view's tests (GPUI `TestAppContext` for state and actions, plain unit tests for formatting), build the view from fixture data, capture its screens, compare with the named renders, gate, commit, tick the box. Each UI task lists its screens.

### Task 6: Backend trait, fixture data, and the screenshot harness

- [x] Done. As built: `Draft { to: Vec<IdentityId>, body, reply_to }` (the model choice joins it in Task 15); `Update::Message(Box<StampedEnvelope>)`; roster rows and agent details carry `id: IdentityId`, and `Snapshot.agents` is keyed by it (review C62); `Snapshot` adds `me_id`, `listener_id`, `messages`, `changes` (cards keyed by message id), `plan`; worktrees, installed agents and models join it in Tasks 11, 12, 14 and 15. The fixture builds the mockup fixes for Task 6 (28, 29, 34, 35, 36, 39, 52, 53). `shot.sh` uses `scripts/window-id.swift` to find the window, since the app cannot read its own CGWindowID without `unsafe`. ADR-0011.

**Files:**
- Create: `src/backend/mod.rs` (trait and types), `src/backend/fixture.rs` (mockup data), `src/backend/fixture_tests.rs`, `scripts/shot.sh`, `docs/adr/0011-desktop-backend-trait.md`
- Modify: `src/main.rs` (`--screen <name>` opens the app in that screen's state at 1440 by 900)

**Interfaces:**
- Produces:
  - ```rust
    pub(crate) trait Backend: 'static {
        fn snapshot(&self) -> Snapshot;
        fn send(&self, draft: Draft) -> Result<(), BackendError>;
        fn act(&self, action: Action) -> Result<(), BackendError>;
        fn subscribe(&self) -> async_channel::Receiver<Update>; // or the GPUI-native channel Task 6 finds already in the tree
    }
    ```
    Implementations are held as `Rc<dyn Backend>` in a GPUI global; a later `AgmendBackend` and the CLI and MCP surfaces map onto the same `Draft` and `Action` types.
  - `pub(crate) struct Snapshot { me: Person, project: ProjectName, roster: Vec<PersonGroup>, messages: Vec<StampedEnvelope>, listener: ListenerInfo, agents: BTreeMap<IdentityId, AgentInfo>, worktrees: Vec<WorktreeInfo>, plan: Option<PlanInfo>, installed_agents: Vec<InstalledAgent>, models: Vec<ModelGroup>, alerts: AlertState, spend_today_usd: f64, online: u32 }`. `spend_today_usd` is the person's total today across all projects, so the Ridgeline screens show the same $14.20 (V69). Messages are `agmen_proto::StampedEnvelope`; everything else is a desktop view type in `backend/mod.rs`, each with only the fields some screen shows.
  - `pub(crate) struct Draft { to: Vec<Mention>, body: String, reply_to: Option<MessageId>, model: Option<ModelChoice> }`.
  - `pub(crate) enum Action { Apply(ChangeId), Cancel(ChangeId), Undo(ChangeId), TakeOver(IdentityId), HandBack(IdentityId), SwitchAgent { worker: IdentityId, agent: String }, AnswerAlert { alert: MessageId, choice: String }, SnoozeAlert(MessageId), ApproveSpec { path: String, sha: GitSha }, ApproveShare, KickOff }`.
  - `pub(crate) enum Update { Message(StampedEnvelope), Snapshot(Snapshot) }`.
  - `pub(crate) enum Screen` with one variant per render in `docs/mockups/console/desktop-*.webp` (25), `FromStr` from the file stem (`chat`, `project-tests-open`, ...); `FixtureBackend::for_screen(Screen)` returns the state that screen shows.
- [ ] **Step 1: Write the failing tests:** `fixture_chat_matches_mockup` (9 messages; verbs, handles and bodies equal `CHAT` in `console.py`; the GRANT is from `hub`), `fixture_roster_groups` (andy 5, sean 2, adam 2, with roles and harnesses from `TREE`), `fixture_send_appends_and_notifies` (`send` then the receiver yields one `Update::Message` whose `from_handle` is "andy"), `fixture_apply_changes_card_state`, `screen_names_cover_every_render` (reads `docs/mockups/console/` and checks every `desktop-*.webp` stem parses to a `Screen`).
- [ ] **Step 2:** Implement. Fixture text is copied from `console.py` verbatim. `StampedEnvelope` values are built through `serde_json` from literal JSON, so the fixture uses only the proto's public wire format.
- [ ] **Step 3:** `scripts/shot.sh <screen>`: builds debug (shots do not depend on speed), runs `agmen-desktop --screen <screen>`, waits for the window, captures it with `screencapture -x -o -l <window id>` (window id from the app's own `--print-window-id` line on stdout) to `target/shots/<screen>.png`, quits the app. `shellcheck` and `shfmt -d` clean. Record in ADR-0011 that it needs Screen Recording permission for the terminal.
- [ ] **Step 4:** ADR-0011: why a trait (CLI and MCP later), why `StampedEnvelope` crosses it unchanged, what agmend must provide to replace the fixture.
- [ ] **Step 5:** Gate. **Commit** `Add the desktop Backend trait, mockup fixtures and a screenshot script`

### Task 7: Title bar, status bar, left dock Agents tab, right dock Listener panel

- [x] Done. Deviations: the component layer below came first; the dock tab strip uses 8 px gaps so it fits 272 px; tree depth 2 indents one more 14 px step; text uses Zed's text scale and code text the code font size, so mono values read 13 px where the mockup draws 12. Landed in d2b2298 with the review fixes for Tasks 1 to 5, because both rewrote `main.rs` and `workspace.rs` in one shared tree; review fixes C61 to C64 and V61 to V63 in the commit after fe5f047. Evidence: `shot.sh chat listener-collapsed` (2026-10-26) compared with `desktop-chat.webp` and `desktop-listener-collapsed.webp`: title bar, Agents dock, Listener panel, status bar and the collapsed state match in layout, text and color; the named tests `agents_panel_groups_and_folds`, `agents_rows_indent_by_depth`, `status_bar_shows_toggle_only_when_dock_closed`, `selecting_agent_shows_agent_panel`, `spend_formats_cents`, `tokens_format_like_mockup` and `listener_label_has_colon` pass in the full gate. Differences: the window is 1280 wide on Andy's display, not 1440; the ● ○ ◐ glyphs draw smaller in `.ZedSans`; mono values are 13 px (Zed's buffer size) where the mockup draws 12; the center pane waits for Task 9; `buffer_font_weight` is not read yet (V64).

Screens: `desktop-chat` (frame only), `desktop-listener-collapsed`.

**Files:** Create `src/ui/title_bar.rs`, `src/ui/status_bar.rs`, `src/ui/agents_panel.rs`, `src/ui/listener_panel.rs`, `src/ui/agent_panel.rs` (right dock for a selected agent), `src/ui/format.rs`, `src/ui/tokens.rs`, `src/ui/components/`, tests alongside. Modify `src/ui/workspace.rs`, `src/ui/dock.rs`. The two-column label/value sections are `components::KvRow` and `KvSection`.

**Interfaces:**
- Consumes: `Snapshot`, `Theme`, `UiIcon`.
- Produces: `AgentsPanel` emits `AgentsPanelEvent::Selected(IdentityId)`; the workspace swaps the right dock between `ListenerPanel` and `AgentPanel`. Status bar `StatusBar` shows needs-you count (red pill when above 0), online, alerts, worktrees, a context line, today's spend, a "Listener" toggle that appears only while the right dock is closed, and the right-side icon buttons with ⌘⇧P.
- [x] **Step 0: Component layer.** `src/ui/tokens.rs` holds every size, spacing, radius and text size (Zed's text scale in rems: Large 16, Default 14, Small 12, XSmall 10 over 16; layout in px). `src/ui/components/` holds stateless `RenderOnce` components with builder methods that take colors only from `Theme`: `Label`, `Icon`, `IconButton`, `Button`, `Chip`, `Badge`, `Avatar`, `StatusDot`, `KeyHint`, `ListItem`, `Tab`, `KvRow`, `KvSection`, `ProgressBar`, and the `Color` names they resolve. Views compose them and add only layout. Nothing comes from Zed's `ui` crate (GPL-3.0-or-later); only gpui (Apache-2.0).
- [x] **Step 1: Tests:** `agents_panel_groups_and_folds` (folding "sean" hides 2 rows), `status_bar_shows_toggle_only_when_dock_closed`, `selecting_agent_shows_agent_panel`, `spend_formats_cents` (14.2 shows "$14.20 today"), `tokens_format_like_mockup` (96000 in, 8000 out, 6 h 40 min shows "96k in · 8k out · up 6 h 40 min").
- [x] **Step 2:** Implement with the mockup's text and colors: status dots ● ○ ◐ from `STATUS`, role labels from `ROLE`, harness names right-aligned, dimmed.
- [ ] **Step 3:** Shots: `chat`, `listener-collapsed`; compare. Gate. **Commit** `Add the title bar, status bar, Agents dock and Listener panel`

Differences: the ● ○ ◐ glyphs draw smaller in .ZedSans than in the mockup's SF Pro; section headings have no 6% letter spacing (gpui has none); ⟨ and ⟩ are Lucide chevrons; code values are 13 px, so some kv values wrap one more line.

### Task 8: Clickable references

- [ ] Done. `src/refs.rs` is in (as built: identities, items, lanes, roles and PRs come only from hub `refs`, which the fixture stamps as the hub would; branches, commits and files come from a client regex confirmed by the new `Snapshot.git` and DONE fields; `RefTarget::Agent` and `Person` carry handles; `Pr` from the hub's number). `ref_chip.rs` (RefChip and its hover card) is agmen-c7's, with Task 9.

**Files:** Create `src/refs.rs`, `src/refs_tests.rs`, `src/ui/ref_chip.rs` (chip and hover card, built on `ui/components/` `Chip`).

**Interfaces:**
- Produces: `pub(crate) enum RefTarget { Agent(String), Person(String), Role(String), Branch { name: String, at: Option<String> }, Commit(String), File { path: String, lines: Option<(u32, u32)> }, Item(String), Lane(String), Pr(u32) }`; `pub(crate) struct Span { range: Range<usize>, target: RefTarget }`; `fn find_refs(body: &str, hub_refs: &[agmen_proto::Ref], known: &Known) -> Vec<Span>` (sorted, non-overlapping; hub refs first, then the regex from `console.py` `REF_RE`, each regex match kept only when `Known` confirms it: handles in the roster, branches and commits named in any earlier message body, DONE, CLAIM or hub ref, files in fixture worktrees). `Known` is built from `Snapshot`.
- `RefChip` renders a colored chip (agent in its role color, others per `ref_color` in `console.py`); hover card per spec 9.3; click emits `OpenRef(RefTarget)`, ⌘-click `OpenRefInSplit(RefTarget)`.
- [ ] **Step 1: Tests:** `refs_find_every_kind_in_chat_fixture` (the 9 fixture bodies give the chips the mockup shows, e.g. message 2: `4d9fdc8` commit, `src/main.rs:2-8` file with lines (2, 8), `@sean/a1` agent), `refs_ignore_non_commit_hex` ("abc1234" unknown stays plain), `refs_ignore_version_numbers`, `refs_reject_offsets_off_char_boundary` (a hub ref ending inside "é" is dropped, no panic), `refs_prefer_hub_ref_over_regex_overlap`, `refs_pr_and_item_and_lane`.
- [ ] **Step 2:** Implement; regexes in `LazyLock`. Gate. **Commit** `Find and render clickable references`

### Task 9: Chat pane, messages and composer

- [ ] Done

Screens: `desktop-chat`.

**Files:** Create `src/ui/chat.rs` (virtualized list from Task 3), `src/ui/message.rs`, `src/ui/composer.rs`, `src/ui/pane.rs` (tab bar and breadcrumb), tests alongside. Modify `src/ui/workspace.rs`.

**Interfaces:**
- Consumes: `TextInput`, `find_refs`, `RefChip`, `Backend::send`.
- Produces: `Pane` with `tabs: Vec<Tab>`, `fn open(&mut self, item: TabItem, cx)`, `fn activate(...)`, `fn close(...)`; `pub(crate) enum TabItem { Chat, Agent(IdentityId), File { worktree, path }, Diff { worktree, path }, RoleFile(String), Plan, Spec(String), Settings(SettingsPage) }`. `ChatView` with `show_ledger: bool`. `Composer` shows the "to <target>" chip (default the listener), the model chip (Task 15 opens it), the input (`TextInput::multiline(6)`) with the mockup's placeholder, and the ⏎ hint.
- [ ] **Step 1: Tests:** `chat_hides_ledger_by_default` (the GRANT line shows, other ledger verbs such as HEARTBEAT do not, until "Show ledger"), `reply_indents_under_parent`, `mention_of_me_gets_yellow_bar` (sean's FYI to @andy), `verb_badge_colors` (from `VERB_COLOR`), `message_wraps_long_token` (a 500-character token wraps within the pane width), `composer_sends_to_listener_by_default` (submit "hi": `Draft.to` is `andy/listen`), `composer_at_mention_overrides_target`. `cmd_click_ref_opens_split` (the center splits into two panes side by side, Zed style; `Workspace::split(direction: SplitDirection)` with `SplitDirection { Right, Down }`).
- [ ] **Step 2:** Implement: avatar letter in role color, handle bold, role label, verb badge, time, body with chips; "Open in terminal ⌥O" chip on a local agent's DONE.
- [ ] **Step 3:** Shot `chat`; compare. Gate. **Commit** `Add the chat pane with messages, references and composer`

### Task 10: Center tabs: agent view and role file

- [x] Done. Syntax and the code view in 76699c6; the agent view, the Roles tab, the role file tab (Duplicate to .agmen/roles on built-ins only) and the Role panel after it. Checked alone with `--view agent` and `--view roles` until Task 9's tabs host them. Fix 55 is in (a role-colored dot ends each Roles row). Fix 50's deletion marks wait for the main.rs file view in Task 11, the file the +2 -5 edit touched. Open in terminal and Stop do nothing until Task 14. As built against node 11725 (ruling D12): the Role panel shows what the role file grants, not the mockup's sample values. Tools are the role's tool classes (`read search shell`); Verbs are the role's own plus base's; Refused is what the identity kind could send (the hub's kind cap, `may_send` in agmen-hub `ingest.rs`) minus the role's verbs; one line says these come from the role file. The code view has no active line or cursor (the mockup's line 11): it is read-only, and no task adds a cursor yet.

Screens: `desktop-agent`, `desktop-role-file`.

**Files:** Create `src/ui/agent_view.rs`, `src/ui/roles_panel.rs` (left dock Roles tab: built-in roles with their kind and tier; opening one shows its XML from `roles/*.xml`, read with `include_str!` from the repo root, which this crate only reads), `src/ui/code_view.rs` (read-only monospace text with line numbers and gutter marks, used again by Tasks 11 and 12), `src/ui/syntax.rs`, tests alongside.

**Interfaces:**
- Produces: `CodeView::new(text, language, marks: Vec<GutterMark>)`; `pub(crate) enum GutterMark { Added(Range<u32>), Modified(Range<u32>), Deleted(u32) }`; `syntax::highlight(text, language) -> Vec<(Range<usize>, HighlightKind)>` for Rust, TOML, XML and Markdown, colored from the theme's `syntax` map.
- [ ] **Step 1:** Decide syntax highlighting: check for `tree-sitter-highlight` plus grammar crates under the allowlist; if they pass `cargo deny`, use them; otherwise a keyword and string lexer for the four languages with a `shortcut:` comment naming tree-sitter as the revisit. Record in ADR-0012.
- [ ] **Step 2: Tests:** `agent_view_lists_tool_calls` (the fixture `SESSION` gives 12 rows with tool, out, say and cost kinds), `code_view_marks_gutter`, `highlight_rust_keywords`, `highlight_xml_tags`.
- [ ] **Step 3:** Implement; shots `agent`, `role-file`; compare. Gate. **Commit** `Add the agent view and the role file tab`

### Task 11: Project panel and file view

- [x] Done by a subagent (24c1acc, merged at 2c2db0b). As built: `ProjectPanel::new(worktrees, Vec<ScopeOption>, cx)` emits `OpenFile { worktree, path }`; `FileView::new(&WorktreeInfo, path, cx)` is the file tab (breadcrumb, CodeView, a Diff button that does nothing yet). `ScopeChip` (`ui/scope_chip.rs`) is shared with the Git panel; `scope_chip::branch_label` is the one reader of a worktree's head. Fix 50 is in (app.rs; main.rs's deletion marks after lines 1, 3 and 4). Differences kept: the tree sorts folders first and then names without case, as Zed does, not console.py's hand order; the chip text clips at 252 px with the 13 px code font; files other than main.rs show placeholder text until the backend reads files. The code view's line numbers now measure the code font's digits (they clipped at 15 px).

Screens: `desktop-project`, `desktop-project-tests-open`.

**Files:** Create `src/ui/project_panel.rs`, `src/ui/scope_chip.rs` (worktree chip built on `ui/components/` `Chip`, shared with Task 12), tests alongside.

**Interfaces:**
- Consumes: `Snapshot.worktrees` (`WorktreeInfo { owner: IdentityId, branch: String, files: Vec<FileNode>, changes: Vec<FileChange>, commits: Vec<CommitInfo>, state: WorktreeState }`), `file_icon`, `CodeView`.
- Produces: `ProjectPanel` emits `OpenFile { worktree, path }`; folders fold; `M` markers in the warning color.
- [ ] **Step 1: Tests:** `tree_matches_fixture_order` (the `PROJECT` list in `console.py`), `opening_tests_shows_two_files`, `scope_chip_switches_worktree`, `change_marker_on_modified_files`.
- [ ] **Step 2:** Implement; shots `project`, `project-tests-open`; compare. Gate. **Commit** `Add the Project panel and file view with change bars`

### Task 12: Git panel and diff view

- [ ] Done except Step 1b. A subagent built the panel and diff view (1a97324, merged at adb9081, with the scope chip from Task 11 switching which worktree Changes and Commits show). As built: `GitPanel::new(worktrees, branches, Vec<ScopeOption>, cx)` emits `OpenDiff { worktree, path }`; `DiffView::new(hunks, language)` (the plan had one argument); `WorktreeHead { Branch, Detached { at, item } }` (C183) and `CommitInfo.at: Timestamp`, shown in local time (C181); hunks live on `FileChange.hunks`; the 14 branches are one const that `git_facts()` also reads. Differences kept: count badges use `Badge`, not the mockup's surface fill; section titles have no letter spacing; long branch names clip. Step 1b (OpenRef routing) waits for agmen-c7's workspace.

Screens: `desktop-git`, `desktop-git-collapsed`.

**Files:** Create `src/ui/git_panel.rs`, `src/ui/diff_view.rs`, tests alongside.

**Interfaces:**
- Produces: `GitPanel` sections Changes, Commits, Worktrees (by person, state dot pushed, local only, reading), Branches, each with a count badge and fold state; `DiffView::new(hunks: Vec<Hunk>)` with removed lines on a red tint and added on a green tint from the theme's `version_control` colors.
- [ ] **Step 1: Tests:** `git_sections_counts` (Changes 2, Worktrees 5, Branches 14), `collapsed_state_hides_rows`, `diff_view_line_kinds`, `selecting_change_opens_diff_tab`.
- [ ] **Step 1b:** Route `OpenRef` now that every target has a home (spec 9.3): agent to its agent tab, person to the Agents panel filtered to them, role to the role file, branch to the Git panel scoped to it, commit to its diff, file and line to the file tab scrolled to the line, item, lane and PR to their hover card only (the board and the GitHub App are later plans; `shortcut:` comment). Test `open_ref_routes_each_kind`.
- [ ] **Step 2:** Implement; shots `git`, `git-collapsed`; compare. Gate. **Commit** `Add the Git panel and diff view`

### Task 13: Needs you: urgent alerts, banner, notification and chime

- [ ] Done

Notes from the alerts review (V70): the MVP has no rate-limit display (C102); the needs-me rule as built in `src/alerts.rs` is: an urgent ALERT needs me when I own its sender or the hub put me in `mentioned`; coordinators only keep the quiet-hours sound; remembered alert ids are capped at 1024, so a redelivery never reopens one (C106). The `ApproveSpec` sha dispatch test (C88) lands with Task 19's spec view.

Screens: `desktop-needs-you`, `desktop-needs-you-answered`.

**Files:** Create `src/alerts.rs` (state: which alerts need me, needs `anyone` versus `owner`, rate limit display), `src/alerts_tests.rs`, `src/chime.rs`, `assets/sounds/chime.wav` (generated in code at build time or synthesized at startup: no third-party sound file), `src/ui/alert_banner.rs`, `src/ui/toast.rs`.

**Interfaces:**
- Consumes: `Payload::Alert` with `Urgency` and `Needs` from agmen-proto; `notify`.
- Produces: `AlertState { needing_me: Vec<MessageId> }`, `fn on_message(&mut self, env: &StampedEnvelope, me: &IdentityId) -> AlertEffect`; `pub(crate) enum AlertEffect { None, Highlight, HighlightChimeNotify }`. Quiet hours from `AgmenSettings` (urgent alerts from my own agents and my coordinators still sound, spec 8).
- [ ] **Step 1: Tests:** `urgent_alert_needs_me` (the fixture ALERT from adam/coord mentioning andy), `normal_alert_is_silent`, `anyone_alert_clears_on_first_answer` (adam's ANSWER clears it), `owner_alert_stays_for_owner_after_coordinator_reply`, `quiet_hours_mute_others_but_not_my_agents`, `muted_agent_alert_arrives_normal`, `chat_tab_badge_counts`.
- [ ] **Step 2:** Implement the red-shaded message with NEEDS YOU badge and its action buttons (Reply, the sender's actions, Snooze 15 min), the banner with "Jump to it", the red counts in the chat tab and status bar, the in-app toast, the native notification and the chime (played with the crate or `afplay` path chosen here; record in ADR-0013).
- [ ] **Step 3:** Shots `needs-you`, `needs-you-answered`; compare. Gate. **Commit** `Highlight urgent alerts with a banner, notification and chime`

### Task 14: Take-over terminal in the chat, switching and adding agents

- [ ] Done

Screens: `desktop-chat-terminal`, `desktop-chat-codex`, `desktop-switch-agent`, `desktop-add-agent`.

**Files:** Create `src/ui/terminal_view.rs` (renders a cell grid: text, fg, bg, cursor), `src/ui/switch_menu.rs`, `src/ui/add_agent_dialog.rs`, tests alongside. Modify `src/backend/mod.rs` (`TerminalScreen { cols, rows, cells }` in `Snapshot` per taken-over agent).

**Interfaces:**
- Produces: `TerminalView` between the chat list and the composer with header (agent chip `claude ▾`, `--resume <id>`, handle, "live · stays on this machine", Open as tab, Hand back ×); typing sends nothing to the hub (spec 5.4); ⌥O dispatches `Action::TakeOver`. `SwitchMenu` lists `installed_agents` (current session check, version, Update… for outdated, Add… for missing, "Add a terminal agent…"); `AddAgentDialog` shows the official source, the copyable prompt (spec 5.2 text, including the look-alike warning) and Check again.
- `shortcut:` the grid comes from the fixture; spike S10 supplies the PTY and terminal emulator behind `Backend`.
- [ ] **Step 1: Tests:** `take_over_opens_terminal_between_chat_and_composer`, `hand_back_removes_it`, `switch_menu_marks_outdated_and_missing` (agy 1.1.7 "update to 1.1.15+ for JSON", grok "not installed"), `add_agent_prompt_names_official_source_and_warns` (grok prompt contains `@vibe-kit/grok-cli` warning and ends with `grok --version` and `agmen agents scan`), `copy_prompt_writes_clipboard` (GPUI test clipboard, not the system one).
- [ ] **Step 2:** Implement; shots the four screens; compare. Gate. **Commit** `Add take-over in the chat and the switch and add agent menus`

### Task 15: Model picker and ASK

- [ ] Done

The effort row uses `Segmented::code()` (mono text, 12 px padding, model picker node 2971; V71).

Screens: `desktop-model-picker`, `desktop-ask`.

**Files:** Create `src/ui/model_picker.rs`, `src/ui/ask_card.rs`, tests alongside.

**Interfaces:**
- Produces: `ModelPicker` (models grouped by installed agent, an effort row only for agents that take one: claude low to max, agy low to high; "this message" or "until I change it"), returning `ModelChoice { agent, model, effort: Option<Effort>, scope: ChoiceScope }` into the composer's chip. `AskCard` renders an ASK addressed to me with its answer buttons.
- [ ] **Step 1: Tests:** `effort_row_hidden_for_codex`, `choice_scope_this_message_resets_after_send`, `ask_card_answer_sends_reply_to`.
- [ ] **Step 2:** Implement; shots; compare. Gate. **Commit** `Add the model and effort picker and ASK cards`

### Task 16: Change cards for settings from chat

- [x] Done by agmen-c7 (merged at f658bc9, cleared by agmen-db). Undo (Andy's answer to C93, V72): a narrowing card goes to `ChangeState::Undone` with no buttons; a widening card returns to Pending with Apply and Cancel (mockup fix 44). The backend side is in the commit after f658bc9.
- [ ] Undone card rendering (V68): agmen-c7, on top of f658bc9; the merge has minimal Undone arms marked `shortcut:` until then.

Screens: `desktop-settings-from-chat`, `desktop-settings-applied`.

**Files:** Create `src/ui/change_card.rs`, tests alongside.

**Interfaces:**
- Produces: `ChangeCard { requested_by, widens: bool, rows: Vec<ChangeRow { target, setting, before, after }> }`: a widening card shows "Waiting for you" with Apply and Cancel; a narrowing or applied card shows the result with Undo. Buttons dispatch `Action::Apply`, `Cancel`, `Undo`.
- [ ] **Step 1: Tests:** `widening_card_waits_for_apply`, `apply_turns_card_into_applied_with_undo`, `before_value_struck_through`.
- [ ] **Step 2:** Implement; shots; compare. Gate. **Commit** `Add Apply, Cancel and Undo change cards in the chat`

### Task 17: Settings → Appearance, and themes from Zed

- [ ] Done. The controls (`Toggle`, `Segmented`, `Dropdown`, `Stepper` in `ui/components/`) are in, by agmen-c7 (merged at f658bc9); every planned stepper is 6 to 72, so `Stepper` has no range setter. The Settings view and the theme sources remain here.

Andy's decision (2026-10-26): the theme picker covers the themes Zed already has and Zed's theme marketplace. Research with sources: `.superpowers/sdd/mvp/zed-themes-research.md` in the main checkout.

- Installed: themes in `~/.config/zed/themes/`, `~/Library/Application Support/Zed/extensions/installed/*/themes/*.json` and the index `extensions/index.json`, read-only.
- Marketplace through `api.zed.dev`, GET only (HEAD returns 404): `GET /extensions?max_schema_version=1&provides=themes&filter=<query>`, `GET /extensions/{id}`, `GET /extensions/{id}/{version}/download`. The User-Agent names agmen and its version; the list is cached for 1 hour; errors back off (1, 2, 4 ... up to 60 minutes) and the picker says the marketplace is unreachable.
- Installing copies the theme JSON with the extension's LICENSE and author into agmen's own themes folder (`~/.config/agmen/themes/<id>/`), never into Zed's. A theme with no license file cannot be installed. The picker credits Zed with a link to zed.dev/extensions and shows each theme's author, license and repository.
- Tests add: `marketplace_list_parses_and_caches_an_hour` (a local HTTP fixture, not the real API), `install_refuses_theme_without_license`, `installed_theme_keeps_license_and_author`, `picker_lists_installed_before_marketplace`.

Screens: `desktop-settings`.

**Files:** Create `src/ui/settings_view.rs`, `src/ui/controls.rs` (toggle, segmented control, dropdown, stepper, built on `ui/components/`), tests alongside.

**Interfaces:**
- Consumes: `AgmenSettings`, `ZedSettings`, the list of installed Zed themes and icon themes.
- Produces: `SettingsView` with the left page list (General, Appearance, Listener, Agents, Roles, Notifications, Terminal, Keys; only Appearance and About are built here, the rest show "comes with agmend"), Follow Zed toggle, theme dropdown with Light, Dark, System, icon theme dropdown with a preview row, UI font and size, code font, size and ligatures, terminal font and size ("Same as code font"), chat density; every change saves `settings.toml` and re-resolves `Appearance` live. "Open settings.toml" opens the file in a tab. About shows the Lucide notice.
- [ ] **Step 1: Tests:** `toggling_follow_zed_off_keeps_current_values`, `size_stepper_bounds` (6 to 72), `change_saves_settings_toml` (tempdir home), `theme_change_repaints` (the global `Appearance` changes), `about_shows_lucide_notice`.
- [ ] **Step 2:** Implement; shot `settings`; compare. Gate. **Commit** `Add Settings Appearance with live theme, icon and font changes`

### Task 18: Command palette and key bindings

- [ ] Done. The fuzzy filter `ui/palette_filter.rs` is in, by agmen-c7 (merged at f658bc9); the palette view and keymap remain here.

Screens: `desktop-palette`.

**Files:** Create `src/ui/palette.rs`, `src/keymap.rs`, tests alongside.

**Interfaces:**
- Produces: `CommandPalette` (⌘⇧P) listing every registered action with its binding, fuzzy filtered as you type, including "Spawn worker with <agent>" per installed agent (dispatches through `Backend::act` in a later plan; here it opens the fixture's DELEGATED flow). Bindings are Zed's wherever agmen has the same action (V86, from Zed's `assets/keymaps/default-macos.json` at 08cbb88): ⌘B left dock; ⌘R and ⌘⌥B right dock (the listener panel); ⌘⇧E Project panel; ⌃⇧G Git panel; ⌘1 to ⌘9 activate pane N; ⌘W close tab; ⌘\\ split right; ⌘⇧P palette; ⌘, settings; ⌘⇧F search; ⌃` terminal (the take-over terminal). Agents and Roles have no Zed binding and open by click and the palette. agmen-only: ⌥O open in terminal. agmen-c7 binds the dock, panel, pane, split and close keys in Task 9's follow-up; `keymap.rs` lists the whole set and `bindings_do_not_collide` covers it. Vim mode is a setting stub that only records the choice (`shortcut:` vim keys come with the terminal UI's keymap).
- [ ] **Step 1: Tests:** `palette_filters_fuzzy` ("swc" finds "Spawn worker with codex"), `every_action_has_palette_entry`, `bindings_do_not_collide`.
- [ ] **Step 2:** Implement; shot `palette`; compare. Gate. **Commit** `Add the command palette and key bindings`

### Task 19: Planning room, spec review and kick-off

- [ ] Done

Screens: `desktop-plan`, `desktop-spec`, `desktop-plan-ready`, `desktop-kick-off`, `desktop-running`.

**Files:** Create `src/ui/plan_panel.rs`, `src/ui/spec_view.rs`, `src/ui/kickoff_dialog.rs`, `src/plan.rs` (checklist logic), `src/plan_tests.rs`.

**Interfaces:**
- Consumes: `Snapshot.plan` (`PlanInfo { name, version, goal, team: Vec<TeamRow { role, handle, agent, model, lane, owner, usd_per_day }>, specs: Vec<SpecState { path, sha, approvals: BTreeMap<Person, Approval> }>, lanes, checks }`).
- Produces: `fn checklist(plan, me) -> Vec<Check { label, ok }>` (repo exists, lanes do not overlap, specs approved, each owner approved own agents and spend); `fn kickoff_enabled(plan) -> bool` (every check ok and the plan branch merged); the left dock's Planning room roster; the plan panel in the right dock; the spec tab with Approve and Request changes; the kick-off confirm dialog; the running view.
- [ ] **Step 1: Tests:** `lanes_overlap_detected` (`core/**` and `core/x/**` overlap), `kickoff_disabled_until_all_approve` ("Kick off · waiting on sean"), `my_share_sums_my_agents` (andy 4 agents, $65/day), `spec_approve_dispatches_decision`.
- [ ] **Step 2:** Implement; shots of the five screens; compare. Gate. **Commit** `Add the planning room, spec review and kick-off`

### Task 20: Whole-app pass and merge readiness

- [ ] Done

- [ ] **Step 1:** Capture all 25 screens with `shot.sh` into `target/shots/`, build a side-by-side sheet (mockup left, app right) in the scratch dir, and list the remaining differences per screen in this file.
- [ ] **Step 2:** `rg 'shortcut:' crates/agmen-desktop` and list each in ADR-0010's Consequences.
- [ ] **Step 3:** Full gate; tell Andy it is ready for the review loop.

## Work claimed by agmen-c7 (Andy, 2026-10-26)

agmen-c7 builds these on branch `desktop/c7` and hands over SHAs to merge; skip them here: Task 16 (change cards); Task 17's controls (toggle, segmented, dropdown, stepper in `ui/components/`; the Settings view stays here); Task 13's `src/alerts.rs` logic and its tests (no banner, toast, chime or notification); Task 18's fuzzy filter `src/ui/palette_filter.rs` (the palette view and keymap stay here); Task 19's `src/plan.rs` logic and its tests. This file keeps one writer: tick those boxes on merge.

## Mockup fixes to build (Andy, 2026-10-26)

agmen-c7 found 31 mockup bugs (`.superpowers/sdd/desktop/findings-for-agent.md` items 27 to 57 in the main checkout, node ids in its `mockup-bugs.md`). Andy's decision: build each suggested fix, not the mockup as drawn, and leave `docs/mockups` alone. Each task notes the fixes in its Differences line:

- Task 6 fixture (built in fe5f047): rv2 runs on an installed harness (opencode), not grok (28); "5 worktrees" in the status bar (29); the alert count follows one rule (chat 1, needs you 2, answered 1) (34); spend adds up (workers $13.78 plus listener $0.42 is $14.20; the ask frame shows listener $0.73 and a 15:52 Recent row) (36); andy/fx1 has run about 14 min, 10 working (39); sean/plan keeps one harness across projects (52); "2 specs" (53).
- Deferred to the task that first holds the data (review C80): Task 10's session fixture shows the edit as +2 -5, and Task 12's diff hunk header is `@@ -1,15 +1,12 @@` (41); Task 11's PROJECT has app.rs, and Task 10's gutter marks deletions after lines 1, 3 and 4 (50); Task 12's worktree list shows rv2 detached @9e2a71c on C015, and Task 10's Used by lists andy/rv2 "opencode · reading C015" with no adam/sec, plus "security-reviewer extends it" in the placeholder color (51).
- Task 9: ledger bodies wrap (32); every tab reserves its close slot, one tab bar builder (40); REVIEW's badge color follows the verdict (42).
- Task 8: no gap between a chip and punctuation after it (54).
- Task 10: Roles rows show a role-colored dot (55); gutter deletion marks after lines 1, 3 and 4 (50).
- Task 13: Revert and Reply lead to the answered state, Snooze dims the alert (43); ⌘R keeps the alert, ASK or picker open (45).
- Task 14: the switch menu hangs from its `claude ▾` chip (27); agy's Update… and the generic row do not open the Grok dialog (38); command names are monospace chips, no backticks (48).
- Task 15: the picker opens with the listener's own "codex · default", effort row hidden, and a footer line when a model of another agent runs one turn (31).
- Task 16: Undo returns the card to its pending state, Cancel shows a cancelled card, Audit log is not a link (44).
- Task 17: the Follow Zed knob uses a theme color (49); segmented controls draw no divider after the last option (57).
- Task 18: the palette shows only what matches the query (30).
- Task 19: the spec view has an approved state and links back where it came from (33); per-project status bar counts (35); person rows 26 px (46); the planning room uses the Agents dock builder (47); "2 specs" (53).

## Notes for Andy

Things this plan needs from paths it may not edit, or decisions that are Andy's:

- design.md section 4.3 lists the desktop app at `apps/desktop`; this plan builds `crates/agmen-desktop` per the brief. design.md should follow when someone owns it.
- Task 1 (approved 2026-10-25): `deny.toml` allows git sources `zed-industries/zed` and `zed-industries/font-kit` and ignores RUSTSEC-2024-0436, RUSTSEC-2026-0206 and RUSTSEC-2026-0192; `crates/agmen-proto/tests/schema_drift.rs` compares schema JSON as values, because gpui turns on `serde_json/preserve_order` in workspace builds.
- AeroSpace tiles the app window. `scripts/shot.sh` floats only the agmen window for comparison shots; a permanent float rule would be a change to Andy's AeroSpace config, which is Andy's call.
