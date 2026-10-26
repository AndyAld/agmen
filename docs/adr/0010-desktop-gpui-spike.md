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
| 10,000-row virtualized list | `agmen-desktop --spike list --autoscroll`: GPUI `list` with 10,000 wrapped rows of 1 to 60 words, scrolled 120 px a frame for 1,500 frames down and 1,500 up, release build; test `list_measures_10k_rows` (`AGMEN_GUI=1`) | Works. Visible window on the 144 Hz main display: p50 6.94 ms, p99 10.93 ms, max 42.82 ms, every row reached. Launched hidden or tiled away by AeroSpace, every frame takes about 34.7 ms, the same with one-word rows, so that is macOS pacing an unseen window, not list cost. Andy scrolled it by hand: smooth |
| Text input with IME | `ui::TextInput`, built on GPUI's `EntityInputHandler` and `ElementInputHandler` like gpui's `examples/input.rs`: marked text underlined, UTF-16 ranges converted at the edge, grapheme-wise backspace; 8 buffer tests and 3 view tests, including composing "に" then committing "日本" through the handler | The handler path is tested; typing with a real input method (Kotoeri, the emoji picker) by hand is still to do. Single line only |
| Native notification | `notify::notify`: inside an `.app` bundle, GPUI's `App::show_system_notification` (`UNUserNotificationCenter`, which macOS delivers only for a bundle id); a bare binary posts through `notify-rust` 4.18.2 (MIT OR Apache-2.0, default features off and macOS only, so its Linux `zbus` stack and advisories stay out). `agmen-desktop --spike notify` | The bare-binary path works: Andy saw the banner on 2026-10-25. The bundled path is untested, because a throwaway bundle id would leave a notification permission entry; it gets tested with packaging. `user-notify` was ruled out (LGPL-3.0) |
| Single instance | `instance::acquire`, std only: `File::try_lock` on `~/Library/Application Support/agmen/desktop.lock`, and a Unix socket `desktop.sock` (mode 0600) where a second launch writes one JSON line of at most 64 KiB; 4 tests (forward, stale socket after a crash, oversize and bad lines dropped) | Works live: a second `agmen-desktop` exited 0 in 0.02 s and the first stayed the only process. macOS refuses `SO_RCVTIMEO` (EINVAL) once the peer has closed, so the reader ignores that error |
| Zed theme and fonts | `appearance::resolve` reads `~/.config/zed/settings.json` (JSONC with comments and trailing commas, through `jsonc-parser` 0.34, MIT, nesting capped at 512), finds the theme in `~/.config/zed/themes/` or an installed extension, fills missing keys from a built-in copy of the mockup's Gruvbox colors, and reads `~/.config/agmen/settings.toml`; 14 tests | Works: Andy's "Gruvbox Material Dark Hard" and fonts load; no Zed, a missing theme or a bad file falls back with a warning. Role colors come from the theme's `players` palette, which matches every mockup role color |
| UI and file icons, system icons without `unsafe` | Lucide v1.61.0 (2026-10-21, `lucide-icons-1.61.0.zip` SHA-256 `d4cfe5949e0cf202ec558c3fe2cc6c8373371c1cb8643fac2f7c68ce4153ff4b`): the 25 SVGs the views use, embedded, with Lucide's ISC license (which also carries Feather's MIT notice) in `assets/icons/LICENSE-lucide.txt`. File icons from the installed Zed icon theme through its own mapping (exact name, then longest suffix, then `default`; named folders open and closed); paths that leave the extension are refused. Without a Zed theme, Finder's icon through `NSWorkspace::sharedWorkspace().iconForFile()` at 32 px as TIFF, which GPUI decodes; 8 tests | Works: Andy's Material Icon Theme draws `src`, `main.rs`, `Cargo.toml` and `README.md` as in the mockup, and Finder returns TIFF icons for files and folders. All objc2-app-kit 0.3.2 and objc2-foundation 0.3.2 calls used are safe fns, so `unsafe_code = "forbid"` holds. Windows (shell icons) and Linux (freedesktop) unverified |

## Packaging and signing (notes, untested)

- The app needs a bundle (`agmen.app` with an `Info.plist` and a bundle id) for GPUI's own notifications, a Dock icon and deep links. `cargo-bundle` or a hand-written `Info.plist` both work for that; neither is installed or tried here.
- Signing: a Developer ID Application certificate, `codesign --options runtime`, then `xcrun notarytool submit --wait` and `xcrun stapler staple`, which need Andy's Apple account (design 4.3).
- Windows MSI and Linux deb and AppImage: not started.

## Consequences

- Moving the pin means re-running this spike's checks and `cargo deny check`; zed's API changes between commits.
- Build machines need Xcode. With `runtime_shaders` they do not need the Metal Toolchain, at the cost of compiling shaders at each start (not measured yet).
- Unverified: Windows and Linux (nothing has run there), packaging, signing and notarization.
