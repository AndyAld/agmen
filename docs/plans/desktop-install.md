# Desktop install implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. This document is a draft for Andy's review. No task here runs before Andy rules on the decisions at the end.

**Goal:** Installing the agmen desktop app on macOS is one download and one drag. On first open the app sets up agmend with the person's consent, signs them in, and joins a project from an invite link. Updates arrive in the app, keep the app and agmend on the same version, and roll back when the new version fails. Uninstall removes the app, the user service and, if the person asks, local data. Linux and Windows follow later.

**Architecture:** One `agmen.app` bundle carries three executables built from one tag: `agmen-desktop`, `agmend` and `agmen`. The app registers the bundled agmend as the existing per-user launchd service (`agmend service install`, branch `agmend/service`) and checks agmend's version on every connect, so the desktop and agmend always ship and run together. A hand-written `Info.plist` and one shell script built on macOS's own tools (`lipo`, `codesign`, `xcrun notarytool`, `xcrun stapler`, `ditto`, `hdiutil`, `iconutil`, `plutil`) build, sign, notarize and staple the bundle and its DMG. Andy runs the script by hand first; the release workflow runs the same script later. Updates come from a small updater in the app that reads an ed25519-signed manifest, checks the download's code signature and Team ID, swaps the bundle with the old one kept for rollback, and restarts agmend from the new bundle.

**Tech Stack:** Rust 1.99.1, edition 2024; the existing gpui pin (ADR-0010); `aws-lc-rs` for ed25519 verification (already in agmen-desktop's graph through reqwest and rustls: `cargo tree -p agmen-desktop -i aws-lc-rs` on desktop/live, run 2026-10-27); reqwest blocking (already a direct dependency, `desktop/live:crates/agmen-desktop/Cargo.toml`); POSIX sh with `shellcheck` and `shfmt`; Xcode command line tools; GitHub Actions `macos-14` for release builds (onboarding.md:35 keeps releases there and the per-commit gate on Jenkins); Homebrew cask after the repository is public.

**Spec:** [../specs/2026-10-26-onboarding-and-hosting-design.md](../specs/2026-10-26-onboarding-and-hosting-design.md) sections 3 (join, lines 104 to 116) and 6 (install, lines 129 to 134); `desktop/live:docs/design.md:331` (signing), `:408` (desktop app: updater and deep links in M6), `:650` (M6: "signed and notarized desktop app installers for 3 OSes, updater, deep links"), `:675` (Apple Developer ID and Windows signing are Andy's); `desktop/live:docs/adr/0010-desktop-gpui-spike.md:33` to 37 (packaging and signing notes, untested); [onboarding.md](onboarding.md) Tasks 2, 7, 13 and 14; `desktop/live:docs/plans/completion.md:73` (D37), `:82` (D27), `:83` (D28), `:214` and `:646` (Task 13b); [install.md](install.md) (the CLI and agmend install plan, written in parallel and not yet reviewed).

## What exists, and what this plan adds

Checked on 2026-10-27 by reading the branches named.

| Piece | Where it stands | This plan |
|---|---|---|
| App bundle, icon, Info.plist | None. `crates/agmen-desktop/assets` holds fonts, Lucide icons and licenses only (`git ls-tree desktop/live crates/agmen-desktop/assets`). ADR-0010:35 says `cargo-bundle` or a hand-written `Info.plist` would do, "neither is installed or tried here" | Tasks 2 and 3 |
| Bundle detection | `notify::in_app_bundle` (`desktop/live:crates/agmen-desktop/src/notify.rs:13`); the bundled notification path is untested (ADR-0010:28) | Task 1 reuses it; Task 15 tests the bundled notification |
| Signing and notarization | Notes only (ADR-0010:36); completion.md:646 asks for an onboarding "Task 13b" with `codesign --options runtime`, `notarytool submit --wait` and `stapler staple`. onboarding.md has no Task 13b yet (its tasks end at 15, line 846) | Tasks 3 and 4 are Task 13b's content |
| CI for releases | dist's GitHub Actions workflow on tags (onboarding.md:35, 764 to 803). Jenkins runs Kubernetes Linux pods only (`desktop/live:Jenkinsfile:2` to 7), so it cannot run `codesign`, `notarytool` or `hdiutil` | Task 4, and a decision |
| agmend as a user service | `agmend service install|uninstall|status` (`agmend/service:crates/agmend/src/main.rs:14` to 37). The launchd plist runs `current_exe()` (`service/mod.rs:85`), label `dev.agmen.agmend` (`service/launchd.rs:29`, `:86`), `RunAtLoad` true and `KeepAlive` false (`launchd.rs:94` to 97), log at `~/Library/Logs/agmen/agmend.log` (`launchd.rs:98`). Install boots out a loaded service and bootstraps again (`launchd.rs:52` to 65) | Reused as is. Task 6 runs it from the bundle |
| The desktop's agmend-down screen | Shows the command `agmend service install` and Retry (`agmend/service:crates/agmen-desktop/src/ui/live_root.rs:107` to 115, commit 7340c95). It cannot run it, and a person with only the app has no `agmend` on `PATH` | Task 6 adds a button that runs the bundled one |
| Version check | `hello` answers only `{api: 1}` (`agmend/service:crates/agmend/src/api.rs:85` to 95). The desktop ignores the answer's body (`agmend/service:crates/agmen-desktop/src/backend/agmend.rs:976`, `:1022`) | Task 5 adds `version` and `exe` |
| Sign in from the app | Not built. `login_start` and `login_status` are planned for agmend in lane L17, landing in M5 (`desktop/live:docs/plans/completion.md:314`). agmend's methods today: `whoami`, `console_snapshot`, `chat_history`, `post`, `file_text` (`agmend/service:crates/agmend/src/api.rs:99` to 103) | Task 7 consumes L17's methods |
| Joining from the app | Not built. The no-project screen says to run `agmen join <invite>` in the project's folder and start the app from there (`desktop/live:crates/agmen-desktop/src/ui/live_root.rs:84` to 95). An app opened from Finder starts in `/`, so `choose_project` finds no identity (`desktop/live:crates/agmen-desktop/src/project.rs:29` to 51) and falls back to `last_project`, which is written only after a project opens (`live_root.rs:262`) | Task 7 |
| `agmen` on `PATH` | `agmen setup` writes the bare command `agmen` into agent configs (onboarding.md:344, 410), so the CLI must be on `PATH` for agents to reach `agmen mcp` | Task 8 |
| Updater | None. design.md:315 (desktop/live) says gpui ships no updater | Tasks 10 and 11 |
| Uninstall | `agmend service uninstall` removes the plist (`launchd.rs:68` to 71). Nothing removes local data. install.md Task I2 covers the shell and Homebrew channels | Task 14 for the app |
| Third-party notices in the app | `crates/agmen-desktop/THIRD_PARTY_NOTICES.md` (desktop/live) says shipping crates.io license texts "is the release pipeline's job (not built yet)". release/oss-prep's root `THIRD_PARTY_NOTICES.md` says the same for binary releases | Task 2 copies both notices into `Contents/Resources` |
| Linux and Windows packaging | "not started" (ADR-0010:37); the desktop exits on non-macOS (`desktop/live:crates/agmen-desktop/src/main.rs:141` to 146) | Outline only, at the end |

## Preconditions and ownership

- D37 (`desktop/live:docs/plans/completion.md:73`) ships `agmen` and `agmend` first and the desktop app once the Apple account exists. Phase A Task 1 and Phase B can be built and tested before that account exists; Tasks 3, 4, 11 (its signature checks against a real Team ID), 12 and 13 cannot finish without it.
- Lane L5 owns `.github/workflows/*`, `install/*`, `site/**` and `dist-workspace.toml` (`desktop/live:docs/plans/completion.md:208`). Tasks 4, 12 and 13 edit those files, so they run inside L5 or after Andy hands those paths over.
- Base: `mvp/impl` after desktop/live and agmend/service merge into it. The desktop files cited here are on desktop/live; the service files are on agmend/service.
- install.md (written in parallel, unreviewed) plans the CLI channel: signed release archives, the shell and Homebrew installers, upgrade, uninstall, `agmen doctor`. It also says the app should "never start a second bundled daemon" and should only discover an installed agmend (install.md, "Existing Task 13b"). This plan bundles agmend instead and keeps the one-daemon rule another way (Global Constraints). Andy must pick one before either plan runs (Decision 3).

## Global Constraints

- Everything in [mvp.md Global constraints](mvp.md#global-constraints), [m0-m1.md Global Constraints](m0-m1.md#global-constraints), desktop.md's and desktop-live.md's Global Constraints (both on desktop/live), and onboarding.md's gate (onboarding.md:40).
- One version: `agmen-desktop`, `agmend` and `agmen` in one bundle come from one tag and one `[workspace.package] version` (`desktop/live:Cargo.toml:19`; all three crates use `version.workspace`). `CFBundleShortVersionString` is that version. `CFBundleVersion` is a build number that only grows (the release's run number), because macOS and the updater compare it.
- One daemon: at most one agmend serves a person. The launchd label `dev.agmen.agmend` (`launchd.rs:29`) is the only service, and agmend's lock file beside its socket refuses a second process (desktop-live.md Task 5). The app never starts agmend outside launchd.
- Consent: the app never installs, restarts or removes the service, writes a link on `PATH`, or deletes data without a button press on a screen that names exactly what will change.
- Never stuck: every first-run and update screen offers the Terminal command that does the same step (with Copy), the agmend log path, and Retry. No screen has only a spinner: each wait has a bound and then shows the fallback.
- Secrets by name only, in scripts and CI: `APPLE_TEAM_ID` (not secret), `APPLE_DEVELOPER_ID_P12_BASE64`, `APPLE_DEVELOPER_ID_P12_PASSWORD`, `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID`, `APPLE_NOTARY_KEY_P8_BASE64`, `AGMEN_UPDATE_ED25519_PKCS8_BASE64`, `HOMEBREW_TAP_TOKEN` (D27, `completion.md:82`). Locally, Andy's certificate stays in his login keychain and notarytool uses a stored keychain profile named `agmen-notary`. No value is ever echoed, written to a file outside a temporary keychain, or passed on a command line where `ps` shows it; the `.p8` key is written to a mode 0600 temp file that a `trap` deletes.
- No new Rust dependency (ed25519 verification uses `aws-lc-rs`, already in the graph). No new build tool: no cargo-bundle, no create-dmg, no Sparkle (Sparkle is an Objective-C framework, and calling it needs `unsafe` FFI, which `unsafe_code = "forbid"` at `desktop/live:Cargo.toml:27` refuses).
- Bounds: update check at launch and then every 24 h; manifest at most 64 KiB; download at most 512 MiB with a 10 min timeout; health check after an update 60 s; at most one kept previous version; at most 3 tries of a download, 1, 4 and 16 s apart.
- Minimum macOS: `LSMinimumSystemVersion` and `MACOSX_DEPLOYMENT_TARGET` are the value Andy picks (Decision 6). Only macOS 27.2 has run the app so far (ADR-0010:25).

## Review Focus

1. **First run with no agmend.** The app shows "Set up agmend", runs nothing until the person presses it, then installs the service from the bundle and connects. If launchd refuses, the screen shows launchctl's error, the Terminal command and the log path. Tests: Task 6 `first_run_installs_service_only_after_consent`, `service_install_failure_shows_command_and_log`.
2. **The app and agmend disagree on version, or the service points elsewhere.** An older agmend, a Homebrew or shell-installed agmend of another version, or a plist whose program path is not this bundle's agmend: the app says which, offers to switch the service to its own agmend, and never runs a second daemon. Tests: Task 5 `hello_reports_version_and_exe`; Task 6 `skewed_agmend_offers_switch_and_keeps_one_service`, `moved_app_reregisters_service`.
3. **A tampered update.** A manifest with a bad signature, a download whose SHA-256 differs, or an app signed by another Team ID is refused and nothing on disk changes. Tests: Task 10 `manifest_refuses_bad_signature`; Task 11 `update_refuses_wrong_sha256`, `update_refuses_wrong_team`.
4. **An update that fails after the swap.** The new app does not report healthy within 60 s: the previous bundle comes back, the service points at its agmend again, and the person sees why. Test: Task 11 `failed_update_rolls_back_app_and_service`.
5. **Uninstall removes only what it lists.** It stops and removes the service, removes the `agmen` link only if it points into this bundle, deletes local data only when ticked, and refuses symlinks and paths outside its list. Tests: Task 14 `uninstall_removes_only_listed_paths`, `uninstall_keeps_data_unless_ticked`, `uninstall_refuses_foreign_link`.

---

## Phase A: bundle, sign and notarize (macOS)

### Task 1: Desktop: `--version` and the bundled helper paths

**Files:**
- Create: `crates/agmen-desktop/src/bundle.rs`, `crates/agmen-desktop/src/bundle_tests.rs`
- Modify: `crates/agmen-desktop/src/main.rs` (`Launch::Version`, `USAGE` at line 35), `crates/agmen-desktop/src/notify.rs` (call `bundle::in_app_bundle`, drop its copy at line 13)

**Interfaces:**
- Produces:
  - `bundle::in_app_bundle(exe: &Path) -> bool`, moved from `notify.rs:13` unchanged.
  - `bundle::Bundle { app: PathBuf, agmend: PathBuf, agmen: PathBuf }` and `bundle::locate(exe: &Path) -> Option<Bundle>`: `Some` only when `exe` is `X.app/Contents/MacOS/agmen-desktop` and both helpers exist beside it as regular files (not symlinks).
  - `agmen-desktop --version` prints `agmen-desktop <version> (<build>)` and exits 0. `<build>` comes from `AGMEN_BUILD` at compile time through `option_env!`, else `dev`.
- [ ] **Step 1: Write the failing tests:** `locate_finds_helpers_beside_the_exe` (temp dir laid out as a bundle), `locate_refuses_a_bare_binary`, `locate_refuses_symlinked_helpers`, `version_flag_parses` (`Launch::parse(["--version"])`), and the existing `bundle_detection` test moved from `notify.rs`.
- [ ] **Step 2:** Run `cargo nextest run -p agmen-desktop bundle`. Expected: FAIL (module missing).
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run the tests and the gate. Expected: PASS.
- [ ] **Step 5:** Commit: `Find the bundled agmend and agmen beside the desktop app`

### Task 2: The bundle: Info.plist, icon, notices and an unsigned build script

**Files:**
- Create: `crates/agmen-desktop/macos/Info.plist.in`, `crates/agmen-desktop/macos/agmen.entitlements`, `crates/agmen-desktop/macos/AppIcon.icns` (from Andy's icon, Decision 8), `scripts/macos-app.sh`, `scripts/test-macos-app.sh`
- Modify: `crates/agmen-desktop/THIRD_PARTY_NOTICES.md` (the sentence about the release pipeline, once the bundle ships the texts)

**Interfaces:**
- `Info.plist.in` keys: `CFBundleIdentifier` (Decision 2), `CFBundleName` and `CFBundleDisplayName` `agmen`, `CFBundleExecutable` `agmen-desktop`, `CFBundlePackageType` `APPL`, `CFBundleShortVersionString` `@VERSION@`, `CFBundleVersion` `@BUILD@`, `LSMinimumSystemVersion` `@MIN_MACOS@`, `CFBundleIconFile` `AppIcon`, `NSHighResolutionCapable` true, `LSApplicationCategoryType` `public.app-category.developer-tools`, `NSHumanReadableCopyright`. No URL scheme yet (Task 9 adds `agmen`).
- `agmen.entitlements`: an empty dict. Hardened runtime starts with no exceptions. A key is added only when Task 15's checks show a failure that names it. Zed's own entitlements (JIT, unsigned executable memory, library validation off; `zed/crates/zed/resources/zed.entitlements` at rev 08cbb88) serve features agmen does not have, so they are not copied.
- `scripts/macos-app.sh build --version V --build N --out DIR [--arch universal|arm64]`:
  - builds `agmen-desktop`, `agmend` and `agmen` with `cargo build --release --locked` for `aarch64-apple-darwin` and `x86_64-apple-darwin` and joins each pair with `lipo -create` (or arm64 only, Decision 5);
  - lays out `agmen.app/Contents/{Info.plist,MacOS/{agmen-desktop,agmend,agmen},Resources/{AppIcon.icns,LICENSE,THIRD_PARTY_NOTICES.md,THIRD_PARTY_NOTICES-desktop.md}}`;
  - renders the plist with `sed` from the three values and checks it with `plutil -lint`;
  - writes nothing outside `DIR` and `target/`.
- `scripts/test-macos-app.sh` builds into a temp dir and asserts: `plutil -lint` passes; `plutil -extract CFBundleShortVersionString raw` equals the workspace version; `lipo -archs` on each executable prints `x86_64 arm64` (or `arm64`); each of the three runs `--version` (or `--help` for agmend) and exits 0; `LICENSE` and both notices are present.
- [ ] **Step 1:** Write `scripts/test-macos-app.sh`. Run `sh scripts/test-macos-app.sh`. Expected: FAIL (`scripts/macos-app.sh` missing).
- [ ] **Step 2:** Write the template, the empty entitlements and the script. `agmen --version` must exist; if the CLI has no such flag, the test uses `agmen --help` and the task report says so.
- [ ] **Step 3:** Run the test, `shellcheck` and `shfmt -d` on both scripts. Expected: PASS and no findings.
- [ ] **Step 4:** By hand: open the unsigned `agmen.app` from Finder (right click, Open). The window opens, About lists the licenses, and a notification posts through the bundled path (`notify.rs:26` to 36), which ADR-0010:28 left untested. Record the result in ADR-0010 Results. This leaves a notification permission entry for the bundle id; Andy approves using the real bundle id here (Decision 2) so no throwaway id is left behind.
- [ ] **Step 5:** Commit: `Build an unsigned universal agmen.app with agmend and agmen inside`

### Task 3: Sign, notarize, staple and package the DMG

**Files:**
- Modify: `scripts/macos-app.sh` (`sign` and `package` subcommands), `scripts/test-macos-app.sh`

**Interfaces:**
- `scripts/macos-app.sh sign --app PATH --identity "Developer ID Application: <name> (<team>)"` signs inside out, each with `--options runtime --timestamp`: `MacOS/agmend`, `MacOS/agmen`, then the bundle with `--entitlements agmen.entitlements`. No `--deep` (Apple's guidance is to sign nested code one by one; unverified on this app until Step 3).
- `scripts/macos-app.sh package --app PATH --out DIR [--notary-profile agmen-notary | --notary-key-env]`:
  - `ditto -c -k --keepParent agmen.app agmen-V.zip`, `xcrun notarytool submit agmen-V.zip --wait`, `xcrun stapler staple agmen.app`;
  - then `hdiutil create -volname agmen -srcfolder <dir with agmen.app and an /Applications link> -format UDZO agmen-V.dmg`, sign the DMG, notarize it, `xcrun stapler staple agmen-V.dmg`;
  - re-zips the stapled app as `agmen-V-update.zip` for the updater (Task 10);
  - writes `SHA256SUMS` for the DMG and the zip;
  - on a notarization failure, runs `xcrun notarytool log <id>` and exits 1 with the log path.
- `--notary-key-env` reads `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID` and `APPLE_NOTARY_KEY_P8_BASE64` (CI); `--notary-profile` uses the keychain profile (Andy's Mac).
- [ ] **Step 1:** Add to the test (skipped with a message when no `Developer ID Application` identity is in the keychain): `codesign --verify --strict --verbose=2 agmen.app`; `codesign -dv agmen.app` shows `flags=0x10000(runtime)` and the Team ID; `codesign --verify` on each helper; `spctl --assess --type execute -vv agmen.app` says `source=Notarized Developer ID`; `xcrun stapler validate` on the app and the DMG; `spctl --assess --type open --context context:primary-signature -vv agmen-V.dmg` accepts.
- [ ] **Step 2:** Andy runs it once on his Mac (Andy action: it uses his certificate and notary profile). Expected before the code: FAIL.
- [ ] **Step 3:** Implement. Andy reruns. Expected: PASS. If launch under hardened runtime fails (Metal shader compile under `runtime_shaders`, ADR-0010:42, or the agmend child), record the exact error and add only the entitlement it names, with a comment that says why.
- [ ] **Step 4:** `shellcheck`, `shfmt -d`, the gate.
- [ ] **Step 5:** Commit: `Sign, notarize and staple agmen.app and its DMG`

### Task 4: Run the bundle build in CI

**Files:**
- Modify: `.github/workflows/release.yml` (generated by dist in onboarding Task 13; add a custom job), or `dist-workspace.toml` `publish-jobs` with `./publish-macos-app` (whichever dist 0.33.1 supports for a custom job; onboarding.md:773 already uses `./publish-hub-image` that way)
- Create: `.github/workflows/publish-macos-app.yml`
- Modify: `Jenkinsfile` Gate stage (`desktop/live:Jenkinsfile:44` to 58): nothing new to install; the portable tests from Tasks 1, 5, 6, 10, 11 and 14 already run in `cargo nextest run --workspace`

**Interfaces:**
- `publish-macos-app.yml` (`on: workflow_call`, inputs `tag`): on `macos-14`, installs the pinned Rust toolchain, creates a temporary keychain, imports `APPLE_DEVELOPER_ID_P12_BASE64` with `APPLE_DEVELOPER_ID_P12_PASSWORD`, runs `scripts/macos-app.sh build`, `sign` and `package --notary-key-env`, runs `scripts/test-macos-app.sh`, signs the update manifest (Task 10) with `AGMEN_UPDATE_ED25519_PKCS8_BASE64`, uploads the DMG, the update zip, `SHA256SUMS` and `update.json` to the release, and deletes the keychain in an `always()` step.
- Jenkins stays the per-commit gate (R49a, onboarding.md:35). Jenkins pods are Linux (`Jenkinsfile:2` to 7), so no signing runs there. If Andy prefers Jenkins for signing too (Decision 4), the same script runs on a Jenkins macOS agent with label `macos` and Jenkins credentials of the same names.
- [ ] **Step 1:** Push a test tag on a fork or a branch with `workflow_dispatch` (Andy action: pushing is outward-facing). Expected: the job fails at the missing workflow.
- [ ] **Step 2:** Write the workflow.
- [ ] **Step 3:** Rerun. Expected: the release holds `agmen-V.dmg`, `agmen-V-update.zip`, `SHA256SUMS` and `update.json`; the job log shows no secret value (GitHub masks registered secrets; check the log by eye once).
- [ ] **Step 4:** Commit: `Build, sign and notarize agmen.app in the release workflow`

## Phase B: first run

### Task 5: agmend: `hello` reports its version and executable

**Files:**
- Modify: `crates/agmend/src/api.rs` (`hello`, lines 85 to 95), `crates/agmen-local/src/message.rs` (a `HelloAnswer` type), `crates/agmend/tests/api.rs`

**Interfaces:**
- Produces: `hello {api: 1}` answers `{ "api": 1, "version": "<workspace version>", "build": "<AGMEN_BUILD or dev>", "exe": "<current_exe()>" }`. The fields are added, nothing is removed, and the desktop's existing call ignores extra fields (`agmend.rs:976`, `:1022`), so older desktops keep working.
- `agmen_local::HelloAnswer { api: u32, version: Option<String>, build: Option<String>, exe: Option<PathBuf> }`: all optional, because an agmend before this task answers only `api`.
- [ ] **Step 1: Write the failing tests:** `hello_reports_version_and_exe`, `hello_answer_without_version_parses` (the old shape).
- [ ] **Steps 2 to 4:** Red, implement, green and the gate.
- [ ] **Step 5:** Commit: `Report agmend's version and path in hello`

### Task 6: Desktop: set up, check and switch the agmend service

**Files:**
- Create: `crates/agmen-desktop/src/service.rs`, `crates/agmen-desktop/src/service_tests.rs`
- Modify: `crates/agmen-desktop/src/ui/live_root.rs` (`LiveState`, `Wording::of` at lines 80 to 138, the Retry path at line 272), `crates/agmen-desktop/src/ui/live_root_tests.rs`, `crates/agmen-desktop/src/backend/agmend.rs` (keep the `hello` answer)

**Interfaces:**
- Consumes: Task 1 `bundle::locate`, Task 5 `HelloAnswer`, `agmend service install|status` (`agmend/service:crates/agmend/src/main.rs:29` to 33).
- Produces:
  - `service::Check` from the socket and the bundle: `Ready`, `Missing` (no socket answer and `agmend service status` says `not installed`), `Stopped` (installed, not running), `Skewed { running: Option<String>, ours: String, exe: Option<PathBuf> }`, `Elsewhere { exe: PathBuf }` (same version, another path), `NoBundle` (a bare `cargo run` binary: today's behavior).
  - `service::install(bundle: &Bundle, run: &mut Runner) -> Result<(), ServiceError>`: runs `<bundle.agmend> service install` with a 30 s bound, then waits up to 10 s for `hello`. `ServiceError` carries launchctl's stderr excerpt (agmend already prints it, `service/mod.rs:186` to 197) and the log path `~/Library/Logs/agmen/agmend.log`.
  - New `LiveState::NeedsService(Check)`. Its screen names what will change ("agmen will add a login item that starts agmend, the agmen background service, from /Applications/agmen.app"), has one primary button (Set up agmend, or Switch to this app's agmend), shows the Terminal command with Copy (`"<bundle path>/Contents/MacOS/agmend" service install`), the log path, and Retry.
  - On every connect, when the running agmend's `exe` is not this bundle's `agmend`: same version is `Elsewhere`, shown as a one-line notice that the app uses the agmend at that path, with no prompt; another version is `Skewed`, shown before the workspace opens.
  - When the app's own path changed since the service was installed (the person moved the app), `Check` is `Elsewhere` with a missing file, and the app offers to reinstall.
- macOS 13 and later show a "Background Items Added" notice when a LaunchAgent appears, naming the signer (unverified for this plist until Task 15). The setup screen says so ahead of time.
- [ ] **Step 1: Write the failing tests** with a fake runner and a fake agmend on a temp socket:
  - `first_run_installs_service_only_after_consent` (no runner call until the button's action)
  - `service_install_failure_shows_command_and_log`
  - `skewed_agmend_offers_switch_and_keeps_one_service` (the switch calls `service install` once with the bundled path, and never spawns agmend directly)
  - `same_version_elsewhere_connects_with_a_notice`
  - `moved_app_reregisters_service`
  - `bare_binary_keeps_todays_agmend_down_screen` (no bundle: the command stays `agmend service install`)
- [ ] **Step 2:** Run `cargo nextest run -p agmen-desktop service live_root`. Expected: FAIL.
- [ ] **Step 3:** Implement. The runner runs on a background thread; the UI thread never waits on it.
- [ ] **Step 4:** Run the tests and the gate. Expected: PASS.
- [ ] **Step 5:** Shoot the new screens with `scripts/shot.sh` beside the existing `live:agmend-down` shot.
- [ ] **Step 6:** Commit: `Set up and switch the agmend service from the desktop app`

### Task 7: Desktop: sign in, join from an invite link, or open a joined folder

**Files:**
- Create: `crates/agmen-desktop/src/ui/welcome.rs`, `crates/agmen-desktop/src/ui/welcome_tests.rs`
- Modify: `crates/agmen-desktop/src/ui/live_root.rs` (the `NoProject` state at lines 84 to 95), `crates/agmen-desktop/src/project.rs`

**Interfaces:**
- Consumes: agmend `login_start` and `login_status` (L17, `desktop/live:docs/plans/completion.md:314`; not built, interface as L17 defines it); `agmen join <link>` (onboarding Task 7, onboarding.md:496) run from the bundle; install.md Task I3 if Andy approves it (repository selection); `agmen_client::find_identity` (`project.rs:33`); gpui `App::prompt_for_paths` (gpui `app.rs:2015` at rev 08cbb88).
- Produces: the welcome screen replaces `NoProject` and offers three ways in:
  1. **Join with an invite link.** A field takes `https://<hub>/i#<code>`. The app checks the shape (the /i page's rule: 64 hex after `#`, onboarding.md:237) before sending it anywhere. If agmend has no login for that hub, the app calls `login_start`, shows the GitHub code in large text with Copy, opens the verification page with `App::open_url`, and polls `login_status` for at most 15 min. Then it asks for the clone's folder (`prompt_for_paths`, folders only), runs `<bundle.agmen> join <link>` with that folder as its working directory and a 120 s bound, and judges success by `find_identity(folder)`, not by parsing text. On failure it shows the CLI's stderr as written, since join already prints the next step (spec lines 112 to 113, such as the `git clone` line), plus the Terminal command with Copy.
  2. **Open a folder you already joined.** `prompt_for_paths`, then `choose_project(folder, settings)`. This covers a person who joined in a terminal and opened the app from Finder.
  3. **Start a new project.** Shows `agmen init` (hosted plan) and `agmen host up` (self-host plan) as Terminal commands. The hosted plan's first-run choice screens (hosted.md:53, Task 13 Step 6) replace this later.
  - After any success the app writes `last_project` (as `live_root.rs:262` does) and opens the workspace.
  - The invite code never goes into a log line or the settings file.
- [ ] **Step 1: Write the failing tests:** `invite_field_refuses_a_bad_code`, `login_code_is_shown_and_polled_until_done` (fake agmend), `login_wait_is_bounded`, `join_success_is_read_from_the_identity_file`, `join_failure_shows_stderr_and_the_command`, `open_joined_folder_opens_its_project`, `invite_code_is_not_logged`.
- [ ] **Steps 2 to 4:** Red, implement, green and the gate. Until L17's methods land, the sign-in step shows the Terminal command `agmen join <link>` and the folder step, and the test for polling is `#[ignore]` with a `shortcut:` comment naming L17.
- [ ] **Step 5:** Commit: `Join a project from an invite link in the desktop app`

### Task 8: Desktop: put `agmen` on `PATH`

**Files:**
- Modify: `crates/agmen-desktop/src/service.rs` (or a sibling `cli_link.rs` if it grows past one screen), the settings view, the welcome screen

**Interfaces:**
- Agent configs call the bare command `agmen` (onboarding.md:344, 410). If `agmen` is not on the login shell's `PATH` (checked with `/bin/zsh -lc 'command -v agmen'`, 5 s bound), the welcome screen and Settings offer "Install the agmen command", which links `~/.local/bin/agmen` to the bundle's `agmen`. It never overwrites a file that is not a link into an `agmen.app`. If `~/.local/bin` is not on `PATH`, the screen shows the line to add to `~/.zprofile` with Copy, and does not edit shell files.
- A Homebrew or shell-installed `agmen` already on `PATH` is left alone; the screen says which one agents will use and its version.
- [ ] **Step 1: Write the failing tests:** `link_points_into_the_bundle`, `link_refuses_a_foreign_file`, `existing_cli_is_left_alone`, `path_hint_when_local_bin_missing`.
- [ ] **Steps 2 to 4:** Red, implement, green and the gate.
- [ ] **Step 5:** Commit: `Offer to link the agmen command from the desktop app`

### Task 9: Open invite links in the app (after Task 7)

**Files:**
- Modify: `crates/agmen-desktop/macos/Info.plist.in` (`CFBundleURLTypes` with scheme `agmen`), `crates/agmen-desktop/src/main.rs` (`App::on_open_urls`, gpui `app.rs:325`), `crates/agmen-desktop/src/instance.rs` (forward the URL to the running app), the /i page from onboarding Task 2 (an "Open in agmen" link for macOS)

**Interfaces:**
- `agmen://join?hub=<origin>#<code>`: the page builds it from its own origin and fragment, client side, as it builds the join command today (onboarding.md:237). The app parses it with the same rule as Task 7 and fills the invite field; it never joins without the person pressing Join.
- [ ] **Step 1: Write the failing tests:** `join_url_fills_the_invite_field`, `join_url_with_a_bad_code_is_ignored`, `second_launch_forwards_the_url` (through `instance.rs`'s existing line protocol), and the page test `page_offers_open_in_agmen_on_macos`.
- [ ] **Steps 2 to 4:** Red, implement, green and the gate.
- [ ] **Step 5:** Commit: `Open agmen invite links in the desktop app`

## Phase C: updates

### Task 10: Signed update manifest

**Files:**
- Create: `crates/agmen-desktop/src/update/{mod.rs,manifest.rs,manifest_tests.rs}`, `scripts/sign-update-manifest.sh` (or a small `--sign-manifest` dev subcommand if openssl's ed25519 support is missing on the runner; check `openssl version` there first)

**Interfaces:**
- `update.json`: `{ "channel": "stable", "version": "V", "build": N, "min_macos": "13.0", "url": "https://.../agmen-V-update.zip", "sha256": "...", "size": n, "team_id": "XXXXXXXXXX", "notes_url": "..." }`, at most 64 KiB, with a detached `update.json.sig` (ed25519 over the exact bytes).
- `manifest::verify(bytes: &[u8], sig: &[u8], keys: &[PublicKey]) -> Result<Manifest, UpdateError>`: tries each compiled-in public key (at most 2, so a key can rotate through one release signed by the old key that ships the new one).
- `manifest::newer(current_build: u64, m: &Manifest, os: Version) -> bool`: only a larger `build` and only when `os >= min_macos`.
- [ ] **Step 1: Write the failing tests:** `manifest_verifies_with_the_release_key`, `manifest_refuses_bad_signature`, `manifest_refuses_oversize`, `older_or_equal_build_is_not_newer`, `min_macos_blocks_the_update`, `second_key_verifies_during_rotation`.
- [ ] **Steps 2 to 4:** Red, implement with `aws_lc_rs::signature::ED25519`, green and the gate. Run `cargo deny check`; aws-lc-rs moves from transitive to direct, with no new crate.
- [ ] **Step 5:** Commit: `Verify the desktop update manifest with ed25519`

### Task 11: In-app updater with rollback

**Files:**
- Create: `crates/agmen-desktop/src/update/{check.rs,install.rs,install_tests.rs}`, an update banner and a Settings row
- Modify: `crates/agmen-desktop/src/main.rs` (`--finish-update <state file>`, run only from the kept previous bundle)

**Interfaces:**
- Check: at launch and every 24 h, fetch `update.json` and `.sig` from the manifest URL (Decision 7), with a 10 s timeout. A newer build shows a banner: "agmen V is ready. Restart to update." Nothing downloads before the person presses Update, unless Andy picks automatic download in Decision 7.
- Install, on Update:
  1. Download the zip to `~/Library/Application Support/agmen/updates/V/` (at most 512 MiB, 3 tries) and check `sha256` and `size`.
  2. `ditto -x -k` into the same folder. Check `codesign --verify --strict` and that `codesign -dv` names the manifest's `team_id`, which must equal the Team ID compiled into the running app.
  3. Write `state.json` {from, to, old_path, started_at}. Copy the running bundle to `updates/previous/agmen.app` (one kept, older ones deleted).
  4. Start `updates/previous/agmen.app/Contents/MacOS/agmen-desktop --finish-update state.json` and quit. That helper waits for the old process to exit (30 s bound), swaps the new bundle into the app's path with `rename` (same volume) or `ditto` (other volume), runs the new bundle's `agmend service install` (which boots out and restarts, `launchd.rs:52` to 65), launches the new app, and waits up to 60 s for it to write `healthy` (written after `hello` reports the new version and the window opens).
  5. If `healthy` does not appear: restore the previous bundle, run its `agmend service install`, launch it, and show "The update to V failed and agmen went back to U", with the log path.
- If the app's folder is not writable (a standard account and `/Applications` owned by an admin), the banner says so and offers the DMG download instead. No `sudo`, no privileged helper.
- agmend data: rollback replaces binaries only. agmend's local SQLite schema must stay readable by the previous release, or the newer agmend refuses to migrate until the person confirms there is no way back. That rule goes into agmend.md (lane L2), not here; this task tests only that rollback re-points the service.
- [ ] **Step 1: Write the failing tests**, with injected download, codesign, launchctl and clock:
  - `update_refuses_wrong_sha256`
  - `update_refuses_wrong_team`
  - `update_keeps_one_previous_version`
  - `failed_update_rolls_back_app_and_service`
  - `read_only_app_folder_offers_the_dmg`
  - `no_download_before_consent`
- [ ] **Steps 2 to 4:** Red, implement, green and the gate.
- [ ] **Step 5:** By hand (Andy, Task 15's checklist): update from V to V+1 on a clean account; then publish a V+2 that exits at launch and see the rollback.
- [ ] **Step 6:** Commit: `Update the desktop app and agmend together, with rollback`

## Phase D: distribution

### Task 12: Before agmen is public: one download link

**Files:**
- Modify: `site/_redirects` (`/download/mac` and `/desktop/update.json`), `site/index.html`, the /i page (a "Download agmen for Mac" link beside the install line, onboarding.md:237), `scripts/macos-app.sh` (an `upload` subcommand for Decision 1's host)

**Interfaces:**
- One stable link for people, `https://getagmen.ai/download/mac`, and one for the updater, `https://getagmen.ai/desktop/update.json`. Both are redirects in `site/_redirects`, as the installers already are (hosted.md:800), so the host behind them can change without an app update.
- Behind them before the repository is public: the host Andy picks in Decision 1 (recommended: a Cloudflare R2 bucket on a `downloads.getagmen.ai` subdomain; cost and limits unverified). The notarized DMG and the update zip contain no secrets, but publishing a binary before the code is public is Andy's call.
- Friends get the link from the /i page. Andy's own first installs can use the DMG built in Task 3 by hand.
- [ ] **Step 1:** Extend `site/check.sh`: `_redirects` has both lines; `index.html` and the /i page link `/download/mac`. Run `sh site/check.sh`. Expected: FAIL.
- [ ] **Step 2:** Implement. Andy creates the bucket and its upload token, named `AGMEN_DOWNLOADS_TOKEN` (Andy action).
- [ ] **Step 3:** Run `sh site/check.sh`. Expected: PASS.
- [ ] **Step 4:** Commit: `Link the macOS download and update manifest from getagmen.ai`

### Task 13: After agmen is public: GitHub releases and a Homebrew cask

**Files:**
- Modify: `site/_redirects` (point both links at `https://github.com/agmen-dev/agmen/releases/latest/download/...`)
- Create in `agmen-dev/homebrew-tap`: `Casks/agmen.rb`, written by the release job with `HOMEBREW_TAP_TOKEN`

**Interfaces:**
- The cask installs `agmen.app`, declares `auto_updates true` (the app updates itself, so `brew upgrade` skips it unless asked; check the flag in Homebrew's Cask Cookbook before relying on it), `depends_on macos: ">= :ventura"` or Decision 6's floor, and `binary` stanzas for `agmen` and `agmend` from inside the app. Its `uninstall` stanza runs the app's `agmend service uninstall`, and its `zap` stanza lists Task 14's local data paths.
- The CLI formula (onboarding Task 13) and this cask both provide `agmen`. The cask notes that conflict; whether `conflicts_with formula:` works for a tap formula is unverified, so the release notes say "install one".
- [ ] **Step 1:** Write a `brew style` and `brew audit --cask --new` check in the release job (Andy action to run once against the tap). Expected before the cask: FAIL.
- [ ] **Steps 2 to 3:** Write the cask template, render it in the job, rerun the audit. Expected: PASS.
- [ ] **Step 4:** Commit: `Publish agmen.app as a Homebrew cask`

## Phase E: uninstall

### Task 14: Uninstall from the app, with consent

**Files:**
- Create: `crates/agmen-desktop/src/uninstall.rs`, `crates/agmen-desktop/src/uninstall_tests.rs`, a Settings section
- Modify: `crates/agmen-client/src/paths.rs` (one function that lists agmen's local data), `crates/agmen-desktop/src/main.rs` (`--list-local-data` for the cask's `zap` list)

**Interfaces:**
- `agmen_client::local_data() -> Vec<LocalData { path, what, holds_secrets }>`, the one home of this list:
  - the config folder (`login.json`), `~/Library/Application Support/agmen` on macOS (`paths.rs:4` to 6, 34);
  - the state folder (identities' SQLite, agmend's socket and lock), the same folder on macOS (`paths.rs:7` to 10, 43);
  - the desktop's lock and socket in the same folder (ADR-0010:29);
  - agmen's settings at `~/.config/agmen/settings.toml` (`desktop/live:crates/agmen-desktop/src/main.rs:338`, desktop.md:24), which is not the config folder on macOS;
  - logs at `~/Library/Logs/agmen` (`launchd.rs:98`);
  - updates at `~/Library/Application Support/agmen/updates`;
  - keychain items under service `agmen` (desktop-live.md:102), removed through agmend or the CLI's sign-out, never by deleting the keychain file.
- Settings, "Uninstall agmen…", lists each step with a tick box: stop and remove the agmend service (on, runs the bundle's `agmend service uninstall`); remove the `agmen` link (on, only if it points into this bundle); sign out of every hub and remove the stored tokens (off); delete local data, listing each folder and its size (off). Then the app moves itself to `~/.Trash` and quits. If that move fails, it says "Drag agmen from Applications to the Trash".
- Never touched: each project's `.agmen/identity.json`, the repositories, and agent configs that `agmen setup` wrote. The screen says so and names `agmen setup`'s own way to undo, if it has one (unverified in onboarding Task 4).
- [ ] **Step 1: Write the failing tests** in a temp HOME: `uninstall_removes_only_listed_paths`, `uninstall_keeps_data_unless_ticked`, `uninstall_refuses_foreign_link`, `uninstall_refuses_a_symlinked_data_folder`, `list_local_data_matches_paths` (`--list-local-data` prints exactly `local_data()`).
- [ ] **Steps 2 to 4:** Red, implement, green and the gate.
- [ ] **Step 5:** Commit: `Uninstall agmen from the desktop app with consent`

## Phase F: verification

### Task 15: CI checks, the clean-account checklist and the ADR

**Files:**
- Create: `docs/adr/00NN-desktop-distribution.md` (next free number when this runs; completion.md assigns several already)
- Modify: `docs/adr/0010-desktop-gpui-spike.md` (Packaging and signing becomes "see ADR-00NN"; Results gains the bundled notification and hardened-runtime rows)

**In CI** (the release job, Task 4, on every tag; the Linux-portable unit tests in the Jenkins Gate on every commit):
- `scripts/test-macos-app.sh`: plist lint and values, `lipo -archs`, each executable runs, notices present.
- `codesign --verify --strict` on the app and each helper; hardened runtime flag; Team ID.
- `spctl --assess` on the app and the DMG; `xcrun stapler validate` on both.
- The notarization log has no issues (`notarytool log` parsed for an empty `issues` list).
- `update.json` verifies with the public key compiled into the built app (`agmen-desktop --verify-manifest` dev flag, or the Task 10 unit test run against the release's file).
- Unit tests from Tasks 1, 5, 6, 7, 8, 10, 11 and 14.

**By Andy, by hand, on a new macOS user account** (System Settings, Users and Groups, a standard account; delete it afterwards). Each line passes or the release waits:
- [ ] Download from `getagmen.ai/download/mac` in Safari. The DMG opens with no Gatekeeper warning beyond the usual "downloaded from the internet" prompt.
- [ ] Drag to Applications, open. The welcome screen shows. No agmend runs yet (`launchctl print gui/$(id -u)/dev.agmen.agmend` exits 113).
- [ ] Set up agmend. macOS shows the Background Items notice naming the signer. The app connects.
- [ ] Join with a fresh invite link from Andy's main account. The GitHub code shows, the browser opens, the join finishes in a clone, and the project's chat opens.
- [ ] Post a message, and see it on the main account.
- [ ] An ALERT on the main account posts a native notification on the test account (the bundled path, ADR-0010:28).
- [ ] Install the agmen command. A new Terminal's `agmen --version` matches the app.
- [ ] Quit and reopen from the Dock. The last project opens.
- [ ] Log out and back in. agmend is running (`RunAtLoad`, `launchd.rs:94`) and the app connects with no prompt.
- [ ] Move the app to another folder and open it. The app offers to reinstall the service.
- [ ] Update from V to V+1. Both versions match afterwards in Settings, About and `hello`.
- [ ] Update to a deliberately broken V+2 from a test channel. The app rolls back to V+1 and says so.
- [ ] Uninstall with every box ticked. No agmend, no LaunchAgent plist, no `~/Library/Application Support/agmen`, no `~/Library/Logs/agmen`, no keychain items under `agmen`; the repository clone and its `.agmen/identity.json` remain.
- [ ] Repeat the first five lines on an Intel Mac or under Rosetta if Decision 5 keeps x86_64 (unverified whether a friend has one).

- [ ] **Step 1:** Write the ADR: bundled agmend and the one-daemon rule, the script over cargo-bundle, no Sparkle (the `unsafe` rule), the manifest format and key rotation, where signing runs, the minimum macOS.
- [ ] **Step 2:** Commit: `Record the desktop distribution decisions and the release checklist`

---

## Linux and Windows later (outline, no tasks)

- **Linux:** the same bundle idea as an AppImage or a tarball holding `agmen-desktop`, `agmend` and `agmen`, plus a `.desktop` file and icon. agmend's systemd user unit exists (`agmend/service:crates/agmend/src/service/systemd.rs:108` to 138), so Task 6's flow carries over. Updates: the same signed manifest; the AppImage replaces itself. Blocked on the desktop running on Linux at all (`main.rs:141` to 146, ADR-0010:43).
- **Windows:** MSIX or MSI, signed with Azure Artifact Signing (`desktop/live:docs/design.md:331`, `:675`). agmend has no Windows service (`service/mod.rs:7`) and the local API has no Windows transport (S6, desktop-live.md:22). Both come first.

## Out of this plan

- The CLI and agmend install channels, their release signatures, `agmen doctor`, and repository selection during join: install.md and onboarding Tasks 13 and 14.
- agmend's `login_start` and `login_status`: lane L17.
- The hosted first-run choice and billing screens: hosted.md Task 13 Step 6.
- An App Store build. The app needs no sandbox exceptions today, but the App Store forbids installing a LaunchAgent this way (unverified), and nothing asks for it.

## Decisions Andy must make

1. **Distribution before agmen is public.** Options: a private GitHub release (friends need repository access, and the updater cannot read private assets without a token in the app); a private Homebrew tap with a token (Homebrew's private-download support left core, unverified, and every friend would hold a token); a direct signed and notarized download. **Recommendation:** the direct download, through `getagmen.ai/download/mac` and `getagmen.ai/desktop/update.json` redirecting to a Cloudflare R2 bucket. Notarization makes it safe to open, nobody holds a token, and the same two links later point at GitHub releases without an app change.
2. **Bundle id and the Apple team.** The bundle id is hard to change later (notification permissions, Background Items and the updater's Team ID check all key on it). agmend's launchd label is `dev.agmen.agmend` (`launchd.rs:29`), which implies `agmen.dev`, while the decided domain is `getagmen.ai` (onboarding.md:36). **Recommendation:** `ai.getagmen.desktop` for the app; keep `dev.agmen.agmend` unless you own `agmen.dev` and want it, because renaming a launchd label needs a migration that boots out the old one. For the team: enroll the Apple Developer Program as an organization if agmen has, or will soon have, a legal entity (it needs a D-U-N-S number), so Gatekeeper and Background Items show "agmen" rather than a personal name; otherwise enroll as yourself now. Changing the team later changes the Team ID the updater pins, which costs one migration release signed by the old team. The yearly fee is $99 (Apple's published price as I know it; unverified for 2026).
3. **agmend inside the app, or separate.** install.md says the app should only discover an installed agmend. **Recommendation:** ship agmend (and `agmen`) inside the app and let the app register it with consent. A friend then needs no Terminal at all, and one tag builds all three, so versions match by construction. The one-daemon rule still holds through the single launchd label and agmend's lock, and the app switches the service only with consent (Task 6). CLI-only people keep the shell and Homebrew channels from install.md.
4. **Where signing runs.** Jenkins runs only Linux pods (`Jenkinsfile:2` to 7). **Recommendation:** Andy's Mac by hand for the first releases (Task 3), then the GitHub Actions release workflow onboarding Task 13 already creates, on `macos-14`, with the secrets named in Global Constraints. Jenkins stays the per-commit gate. A Jenkins macOS agent is the other option if you want signing keys off GitHub.
5. **Universal or Apple Silicon only.** **Recommendation:** universal for the first releases, so a friend never picks the wrong download; it costs build time and roughly doubles the binaries' size (not measured). Revisit once no tester has an Intel Mac. Apple has said macOS 26 is the last release for Intel Macs (my knowledge, unverified here).
6. **Minimum macOS.** Zed at the pinned rev builds for 10.15.7 (`.cargo/config.toml:24` in the zed checkout), but agmen has only run on 27.2. **Recommendation:** 13.0 (Ventura), the oldest that shows Background Items, so the setup screen's wording is true everywhere; lower only after someone tests it.
7. **Update channel and behavior.** **Recommendation:** one `stable` channel, checked at launch and daily, downloaded and installed only when the person presses Update; a `beta` channel later if testers want it. After the repository is public, the Homebrew cask with `auto_updates true` for people who prefer brew. Sparkle is ruled out by `unsafe_code = "forbid"`.
8. **The app icon.** No icon exists. **Recommendation:** a simple mark from the mockups' palette, made as a 1024 px PNG and turned into `AppIcon.icns` with `iconutil`; it is needed before Task 2 Step 4.

## Gaps found in the existing plans

- onboarding.md has no Task 13b. completion.md:214 and :646 describe it, and L4's erratum is to write it; this plan's Tasks 2 to 4 are its content.
- onboarding.md's release runs on GitHub Actions only; nothing says where `codesign` and `notarytool` run, and Jenkins cannot (Linux pods).
- No app icon, `Info.plist`, entitlements or bundle id anywhere. ADR-0010:35 left the tool choice open.
- The desktop's agmend-down screen tells the person to run `agmend service install`, but a person who installed only the app has no `agmend` on `PATH`, and the app cannot run it.
- `hello` carries no agmend version, so the app cannot see skew between itself and agmend, although D37 and desktop-live's progress notes say the two must ship together.
- The service plist points at `current_exe()`. A bundled agmend ties the service to the app's path, so moving or deleting the app breaks the service silently. Nothing checks it.
- No in-app sign-in or join. `login_start` is planned (L17, M5); nothing plans a join from the app, and an app opened from Finder never finds a project joined in a terminal until a project has been opened once.
- `agmen setup` writes the bare command `agmen`, so agents need `agmen` on `PATH`; nothing plans that for app-only installs.
- No updater anywhere, though design.md puts it in M6. The spec puts signed installers, updater and deep links out of scope for onboarding (spec line 33), and no plan picks them up.
- No uninstall for local data, and the data lives in two places on macOS: `~/Library/Application Support/agmen` (`paths.rs`) and `~/.config/agmen/settings.toml` (desktop.md:24).
- Crates.io license texts in binary releases are still "the release pipeline's job (not built yet)" in both notice files.
- The /i page shows only the CLI install line (onboarding.md:237); it has no desktop download.
- install.md and this plan disagree on bundling agmend (Decision 3).
- The bundled notification path is untested (ADR-0010:28), and hardened runtime with `runtime_shaders` is untested (ADR-0010:42 says shaders compile at each start).
