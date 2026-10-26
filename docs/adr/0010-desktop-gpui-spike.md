# 0010: Spike S8, the GPUI desktop app on macOS

Status: proposed (results are filled in as Tasks 1 to 5 of [the desktop plan](../plans/desktop.md) land)
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
| 10,000-row virtualized list | Task 3 | pending |
| Text input with IME | Task 3 | pending |
| Native notification | Task 4 | pending |
| Single instance | Task 4 | pending |
| Zed theme and fonts | `appearance::resolve` reads `~/.config/zed/settings.json` (JSONC with comments and trailing commas, through `jsonc-parser` 0.34, MIT, nesting capped at 512), finds the theme in `~/.config/zed/themes/` or an installed extension, fills missing keys from a built-in copy of the mockup's Gruvbox colors, and reads `~/.config/agmen/settings.toml`; 14 tests | Works: Andy's "Gruvbox Material Dark Hard" and fonts load; no Zed, a missing theme or a bad file falls back with a warning. Role colors come from the theme's `players` palette, which matches every mockup role color |
| System file icons without `unsafe` | Task 5 | pending |

## Consequences

- Moving the pin means re-running this spike's checks and `cargo deny check`; zed's API changes between commits.
- Build machines need Xcode. With `runtime_shaders` they do not need the Metal Toolchain, at the cost of compiling shaders at each start (not measured yet).
- Unverified: Windows and Linux (nothing has run there), packaging, signing and notarization.
