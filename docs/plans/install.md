# Easy installation implementation plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task by task. Steps use checkbox (`- [ ]`) syntax. This document is planning only. Leave this work uncommitted for the reviewer.

**Goal:** A friend installs agmen and agmend with one command, joins a project with one invite command, and gets a precise repair command from `agmen doctor` when something fails.

**Architecture:** Extend onboarding's release, installer, login, invite and setup work. Keep agmend as the sole per-user service and let the desktop find its existing socket. Add only the missing signature verification, install lifecycle, repository selection, diagnostics and private-preview instructions.

**Tech Stack:** The existing Rust workspace, POSIX sh, launchd on macOS, systemd user services on Linux, dist 0.33.1 as planned, and Homebrew. Proposed addition: Cosign for release signatures, subject to the verifier bootstrap check in Task I1. No new Rust dependency is assumed.

**Spec:** `docs/specs/2026-10-26-onboarding-and-hosting-design.md:108` describes join and setup; `docs/specs/2026-10-26-onboarding-and-hosting-design.md:129` describes distribution. This plan extends `docs/plans/onboarding.md:764` Tasks 13 and 14. Completion plan D37 controls the release shape (`desktop/live:docs/plans/completion.md:73`). Automatic repository selection is a proposed spec amendment, not an approved behavior.

## Evidence and ownership

Local citations refer to plan/install at `d902c5276b0ed37cf28b0b2b4ba2ff84bfb2f9f0`. Branch-qualified citations mean `git show <branch>:<path>`, with line numbers in that file. Snapshots read on 2026-10-27:

| Branch | Snapshot |
|---|---|
| desktop/live | fd36a779cf1012b39cc7e47b3b3ef384a08d9232 |
| agmend/service | 7340c9528797d4b7d2c390dc7d3e938a16955f71 |
| dev/trial-hub | 70d3385f849ae0612d08f15acfdfbc76b9317751 |
| release/oss-prep | 2d3cd544d7810af2221ae1aac934a018a03e824f |
| mvp/impl | df9331a97f5d861139e21b2da2541fdde1ff7e59 |

The local worktree does not contain the desktop or daemon crates (`Cargo.toml:3`). They exist on the cited branches. Existing plan tasks describe future work unless code evidence below says otherwise.

| Experience | Existing owner and evidence | Remaining gap |
|---|---|---|
| Install both binaries | Onboarding Task 13 at `docs/plans/onboarding.md:764`; completion L5 corrects its CLI-only opt-in to include agmend at `desktop/live:docs/plans/completion.md:214` | Confirm the actual paired artifact layout. Completion explicitly leaves dist's multi-package archive behavior unverified at `desktop/live:docs/plans/completion.md:686`. Do this inside Task 13, not a second release pipeline. |
| Shell install and checksum | Onboarding Task 14 at `docs/plans/onboarding.md:805`; version, directory and download-base controls at `docs/plans/onboarding.md:814` | Signature verification is I1. Upgrade and uninstall are I2. |
| Start the service | Completion L5 assigns the installer call to Task 14 at `desktop/live:docs/plans/completion.md:214`. Actual service commands exist at `agmend/service:crates/agmend/src/main.rs:14`. | Reuse service install, uninstall and status. I2 covers failure recovery and Homebrew parity. |
| Homebrew formula | Onboarding Task 13 specifies `agmen-dev/homebrew-tap` and formula `agmen` at `docs/plans/onboarding.md:773`; D27 owns the tap and publisher secret at `desktop/live:docs/plans/completion.md:82`. | Verify signatures before executing either binary, start the same user service, and keep upgrades pointed at a stable path. I1 and I2. |
| Stable install URL | Onboarding Task 14 publishes the script asset at `docs/plans/onboarding.md:814`; hosted Task 17 owns redirects and the post-release HTTP check at `docs/plans/hosted.md:808`. | No second site or installer endpoint. I5 documents the private-preview exception. |
| Browser login and invite | Onboarding Tasks 1, 1b, 2 and 3; L14 owns them at `desktop/live:docs/plans/completion.md:290`. Task 7 owns login, accept and identity creation at `docs/plans/onboarding.md:503`. | Repository selection is I3. Keep login and invite parsing in their existing tasks. |
| Agent wiring | Onboarding Tasks 4, 5 and 6; L15 owns them at `desktop/live:docs/plans/completion.md:296`. Task 7 invokes setup at `docs/plans/onboarding.md:508`; L22 owns Task 7 at `desktop/live:docs/plans/completion.md:348`. | I4 inspects configuration without rewriting it. No second setup implementation. |
| Claude Code plugin | Marketplace already names `./mods/agmen-claude` at `desktop/live:.claude-plugin/marketplace.json:10`. Task 4 specifies plugin installation and MCP fallback at `docs/plans/onboarding.md:347`. | Keep marketplace and plugin distinct from binary installation. Private source access must also cover the marketplace. I5. |
| End-to-end doctor | Existing command enum has login, whoami, join and MCP, but no doctor (`desktop/live:crates/agmen-cli/src/cmd/mod.rs:26`). | I4 adds the command. |
| Desktop download later | L5 Task 13b owns bundle, signing and notarization after the Apple account, independently of the first release (`desktop/live:docs/plans/completion.md:214`). | Add acceptance cases to Task 13b below. Do not create another packaging task. |
| Public release preparation | Onboarding Tasks 11, 12 and 15 at `docs/plans/onboarding.md:697`, `docs/plans/onboarding.md:731` and `docs/plans/onboarding.md:846`; L5 orders them at `desktop/live:docs/plans/completion.md:210`. | Consume oss-prep, then finish the existing tasks. I5 documents access before publication. |

`git log mvp/impl..release/oss-prep` returned three commits: `cc5c2e4`, `d4b1b1e` and `2d3cd54`. The three-dot diff contains 16 files, 216 insertions and 50 deletions. Its MIT metadata and public repository URL are already present at `release/oss-prep:Cargo.toml:17`; its notices promise bundled license texts at `release/oss-prep:THIRD_PARTY_NOTICES.md:5`. This is partial release preparation, not an implemented installer or dist workflow. D28 still selects generated notices and approves installing dist (`desktop/live:docs/plans/completion.md:83`).

## Global Constraints

- Target command: `curl -fsSL https://getagmen.ai/install.sh | sh`. Alternative: `brew install agmen-dev/tap/agmen`. These are target experiences, not verified live endpoints. Install agmen and agmend from the same version. Do not install the hub, a broker, Docker or an agent for a joining friend.
- Initial daemon support means macOS with a GUI user session and Linux with a working systemd user manager. Service backends are selected at `agmend/service:crates/agmend/src/service/mod.rs:91`. Other Linux environments get a precise unsupported-service result and the foreground `agmend` command. Do not promise universal Linux autostart. Preserve onboarding's Windows CLI work; do not claim Windows daemon support.
- Use the existing user service only. launchd writes `Library/LaunchAgents/dev.agmen.agmend.plist` (`agmend/service:crates/agmend/src/service/launchd.rs:109`); systemd writes `.config/systemd/user/agmend.service` (`agmend/service:crates/agmend/src/service/systemd.rs:138`). Do not also register a Homebrew-managed service.
- The bootstrap script is initially trusted through HTTPS and the chosen distribution host. A release checksum alone does not authenticate its author. I1 must verify a signature against a separately pinned signing identity before shell extraction or any installed binary execution. Do not call checksum verification signature verification.
- Preserve identity trust, invite fragment handling and per-machine tokens from onboarding's constraints (`docs/plans/onboarding.md:25` and `docs/plans/onboarding.md:28`). No credentials in URLs, command output or receipts. Tests use temporary homes and token directories, not the user's keychain (`crates/agmen-cli/tests/support/mod.rs:356`).
- Keep the existing noninteractive join behavior until Andy approves I3's spec amendment. The spec explicitly stops outside a clone (`docs/specs/2026-10-26-onboarding-and-hosting-design.md:112`), and Task 7 accepts the invite before that stop (`docs/plans/onboarding.md:510`). Changing this requires an explicit migration note and replacement of its documented test, not a silent rewrite.
- Every proposed operation has a bound: three downloads at most per asset, 10 s connection and 60 s total per download; 120 s signature verification; 120 s service transition; 300 s clone; 30 s total doctor run. Bound downloaded metadata to 1 MiB, archives to 256 MiB, diagnostic output to 64 KiB and a single diagnostic result to 512 characters. Validate these proposed ceilings against release artifacts before shipping.
- Reuse before adding: `git::toplevel` (`crates/agmen-cli/src/git.rs:61`), the HTTP client (`crates/agmen-cli/src/client.rs:64`), TokenStore (`crates/agmen-client/src/token_store.rs:119`) and the shared local frame codec (`desktop/live:crates/agmen-local/src/frame.rs:43`). New files below have no source lines yet and are explicitly proposed.
- The planning change creates only this file and `target/install/report.md`. Do not implement, install tools, stage, commit, stash, publish or change services in this task. Implementation workers run the cargo gate in `Jenkinsfile:49`, integration profiles in `.config/nextest.toml:13` and `.config/nextest.toml:28`, and Plugin checks in `Jenkinsfile:132` when applicable. Run integration profiles only against an isolated dev stack; never restart the trial hub or a personal service.

## Review Focus

1. A valid checksum with an invalid signature, a wrong workflow identity, or a missing bundle. Expected: no extraction, replacement or service call. I1 tests both script and formula routes.
2. First install, repeat install, upgrade while agmend runs, interrupted replacement, and service start failure. Expected: both versions agree, a bounded health check confirms the new daemon, or rollback restores the prior pair and service. I2.
3. A single-use invite accepted before cloning fails. Expected: membership remains usable through `join --project`; no second acceptance is attempted. I3. Cover an absent repo URL, an existing destination and private-repo authentication failure.
4. Doctor with no daemon, missing sign-in, wrong local API version, HTTPS failure, broker failure and each unwired agent. Expected: independent failures remain visible; dependent checks say skipped with a reason. No automatic repair. I4.
5. A friend without source, tap, asset or marketplace access. Expected: the guide distinguishes all four permissions and never promises that a private tap alone grants release access. I5.

## Phase A: extend the existing release tasks

### Task I1: Authenticate release artifacts before installation

**Files:**
- Modify when created by onboarding Tasks 13 and 14: `dist-workspace.toml`, `.github/workflows/release.yml`, `install/install.sh`, `install/install.ps1`, `install/test-install.sh`, `install/test-install.ps1`, `.github/workflows/test-installers.yml`.
- Create: `install/test-signatures.sh`.
- Modify in the separately owned tap repository: `Formula/agmen.rb`, created by Task 13. It is not a file in this repository.

**Interfaces:**
- Existing Tasks 13 and 14 remain the owners of building and installing both binaries. Add a signature bundle for a versioned checksum manifest that names every target artifact and the release version. Resolve latest once. Bind verification to that exact release tag and target filename, then verify the archive digest from the authenticated manifest.
- Recommendation: Cosign keyless signing by the release workflow. `verify-blob`, `--bundle`, `--certificate-identity` and `--certificate-oidc-issuer` are documented by [Sigstore](https://docs.sigstore.dev/cosign/verifying/verify/). Pin the exact repository, workflow and tag identity. The private-preview signing identity must be separately configured and approved, never accepted by a wildcard.
- Proposed bootstrap: include reviewed SHA-256 pins for a fixed Cosign version's four macOS/Linux verifier binaries in the installer. Download the verifier to its temporary directory, verify that pin, then execute it. Never download a checksum next to an untrusted verifier and treat that as the pin. Select and verify actual version, targets, pins and trust-root behavior during implementation; none were installed or tested in planning. PowerShell needs its own pinned verifier target.
- Check stdlib, platform tools and existing dependencies first. The inspected checksum-only installer contract has no signature verifier (`docs/plans/onboarding.md:815`). Prefer a maintained verifier to implementing signature and certificate validation in shell. Carry its notices through Task 11. Homebrew must verify the signed manifest before installing executable bytes, not run an unverified agmen to verify itself.

- [ ] **Step 1: Write failing tests.** Extend Task 14's local artifact fixtures with `refuses_missing_bundle`, `refuses_wrong_signer`, `refuses_wrong_release`, `refuses_tampered_manifest`, `refuses_wrong_verifier_pin` and `signature_failure_never_calls_service`. Assert the install directory and prior pair stay unchanged. Cover formula consumption of the same signed artifact.
- [ ] **Step 2: Run** `sh install/test-install.sh` and `sh install/test-signatures.sh`. Expected before implementation: failure for missing signature enforcement. A fake verifier tests orchestration only; include a real signed fixture plus a mutated fixture in release CI to test the verification claim.
- [ ] **Step 3: Extend Task 13's signing job and Task 14's installers.** Confirm dist's paired artifact layout from its actual plan. If two packages produce separate archives, authenticate and stage both before activating either. Keep generated workflow changes reproducible. Pin verifier downloads and workflow action revisions after reading their current interfaces.
- [ ] **Step 4: Run** both shell tests, `shellcheck install/*.sh`, `shfmt -d install` and the cargo gate. Run `install/test-install.ps1` on Windows CI as Task 14 already requires (`docs/plans/onboarding.md:836`). Record the real verifier version and signer identity in the task report. Do not mark the signature requirement complete from stub tests alone.

### Task I2: Complete install, upgrade and uninstall for both channels

**Files:**
- Modify when created: `install/install.sh`, `install/test-install.sh`, `install/install.ps1`, `install/test-install.ps1`, `.github/workflows/test-installers.yml`.
- Create: `install/uninstall.sh`, `install/test-lifecycle.sh`.
- Modify: `README.md`, `docs/mcp-setup.md`; tap repository `Formula/agmen.rb`.
- Reuse without duplicating: `agmend/service:crates/agmend/src/service/mod.rs:82` and its platform backends.

**Interfaces:**
- Fresh shell install stages the verified pair, records version, channel and owned paths in a nonsecret receipt, then invokes the installed absolute `agmend service install`. Repeat install is safe. Honor existing `AGMEN_VERSION` and `AGMEN_INSTALL_DIR` from Task 14. Print the PATH fix if needed. Do not report complete until the new daemon answers the local API handshake.
- Shell upgrade reruns the install line, or pins `AGMEN_VERSION`. Stage a versioned pair and switch a single active directory pointer. Keep the previous version until service and handshake checks succeed. Refuse to overwrite another installation channel. Service failure restores the old pointer, re-registers the old absolute daemon and reports both failures if rollback fails. First-install failure reports the exact retry command and leaves a consistent pair.
- Homebrew upgrade uses `brew upgrade agmen-dev/tap/agmen`. Its service executable must use a stable opt path rather than a versioned Cellar path. Confirm Homebrew's current supported install-step and sandbox APIs before promising automatic service registration; its [Formula Cookbook](https://docs.brew.sh/Formula-Cookbook#running-commands-after-installation) documents restrictions. If the supported mechanism cannot run the user service, report that target as blocked and propose `agmen setup` registering it. Do not silently claim `brew install` meets the one-command service target.
- Shell uninstall uses the installed `uninstall.sh` through its recorded path, calls `agmend service uninstall` before removing owned binaries, and preserves tokens, identities, project clones and user agent settings. Refuse an unrelated binary or symlink. Homebrew removal is `agmend service uninstall` followed by `brew uninstall agmen-dev/tap/agmen`; document these two steps unless a supported formula removal hook is verified. No automatic data purge.
- No sudo, system services, second daemon, unbounded retries or hidden agent installs. Linux without a user manager gets an actionable foreground fallback. Existing service install writes its unit before running the manager (`agmend/service:crates/agmend/src/service/mod.rs:112`), so rollback must cover the unit as well as the executable.

- [ ] **Step 1: Write failing tests.** `first_install_starts_verified_pair`, `repeat_install_is_safe`, `upgrade_switches_both`, `service_failure_restores_previous_pair`, `interruption_leaves_one_complete_pair`, `missing_user_manager_explains_fallback`, `uninstall_preserves_user_data`, `uninstall_refuses_foreign_paths`, `brew_uses_stable_service_path`, `mixed_channels_are_refused`. Inject manager processes and paths as the service implementation already does (`agmend/service:crates/agmend/src/service/mod.rs:72`).
- [ ] **Step 2: Run** `sh install/test-lifecycle.sh`. Expected: failure before lifecycle support exists. Use only fake service managers and temporary homes locally. Test launchd and systemd registration in disposable CI users or VMs, not the developer's session.
- [ ] **Step 3: Extend the existing installers and formula.** Add receipt handling and transaction recovery. Ship the uninstall script as part of the installed shell distribution. Do not add an `agmen upgrade` command with a second downloader.
- [ ] **Step 4: Run** installer, signature and lifecycle tests, shellcheck, shfmt and the cargo gate. Test real first install, repeat, upgrade and removal for each macOS/Linux architecture in isolated release CI. Verify that both binary and local API versions match the release contract. Windows keeps the CLI-only behavior until daemon support is approved.

## Phase B: join and diagnose

### Task I3: Select or clone the repository without consuming an invite twice

**Files:**
- Modify only after Andy approves the behavior change: `docs/specs/2026-10-26-onboarding-and-hosting-design.md`, `docs/plans/onboarding.md`, `crates/agmen-cli/src/cmd/join.rs`, `crates/agmen-cli/src/git.rs`, `crates/agmen-cli/tests/cli.rs`, `crates/agmen-cli/tests/it_cli.rs`.
- Extend the existing Task 7 flow; do not create a second join command or login client. The current implementation discovers the worktree before acceptance (`desktop/live:crates/agmen-cli/src/cmd/join.rs:80`); Task 7 proposes the later stop described above.

**Interfaces:**
- Proposed amendment: in a terminal, `agmen join <link>` uses the current worktree if one exists. Otherwise, after acceptance, offer an existing local path or cloning the project's recorded repository URL to a selected destination. Show the source and destination before cloning. A missing URL offers only an existing path and manual clone guidance. Never guess a private repository URL.
- Proposed `--repo <path>` selects an existing worktree and `--clone <destination>` requests a clone. They are mutually exclusive. Without a terminal and without either flag, preserve Task 7's documented exit and recovery text. These flags do not exist in the inspected CLI.
- Reuse Task 3's validated repository URL (`docs/plans/onboarding.md:279`), Task 7's `join_here` and Task 4's `setup::run` (`docs/plans/onboarding.md:508` and `docs/plans/onboarding.md:342`). Pass Git arguments separately, disable interactive credential prompts for unattended clones, cap the process at 300 s, and refuse nonempty destinations. Never embed tokens. Existing clone credentials remain Git's responsibility.
- After acceptance, a clone, path or credential failure prints `agmen join --project <id>` with the chosen path guidance. Membership is already accepted. A retry does not accept the single-use invite again. Mint and write identity only after validating the selected worktree. Keep existing identity overwrite refusal.

- [ ] **Step 1: Obtain Andy's ruling on the amendment.** If declined, retain onboarding Task 7 and the three-step install, manual clone, join flow. Do not build a conflicting default.
- [ ] **Step 2: Write failing tests.** `join_selects_existing_repo`, `join_clones_recorded_repo`, `join_clone_denied_recovers_by_project`, `join_missing_repo_url_prompts_for_path`, `join_refuses_nonempty_destination`, `join_noninteractive_keeps_old_exit`, `join_setup_runs_in_selected_root`. Replace `it_join_outside_a_worktree_points_to_the_clone` only after explicitly documenting its changed interactive assertion (`docs/plans/onboarding.md:48`). Keep its noninteractive assertion.
- [ ] **Step 3: Run** `cargo nextest run -p agmen-cli --locked -E 'test(join)'`. Expected: new cases fail before implementation. Use temporary local repositories, fake login/opening and fake agent programs. No real browser, clipboard or credentials.
- [ ] **Step 4: Extend Task 7, then run** the same tests, the CLI integration profile against the isolated stack, and the full cargo gate. Check identity and setup outputs in the selected clone, not only successful Git exit.

### Task I4: Add a bounded, read-only `agmen doctor`

**Files:**
- Create: `crates/agmen-cli/src/cmd/doctor.rs`, `crates/agmen-cli/tests/doctor.rs`.
- Modify: `crates/agmen-cli/src/cmd/mod.rs`, `crates/agmen-cli/src/client.rs`, `crates/agmen-cli/tests/support/mod.rs`, `docs/mcp-setup.md`.
- Extend when created by Tasks 4 to 6: `crates/agmen-cli/src/setup/mod.rs` and its agent writers, exposing inspection without applying changes.
- Add an agmen-local dependency only after checking the merged CLI manifest for an existing edge. Reuse its frame codec. No new diagnostic crate is needed because the command has one caller.

**Interfaces:**
- Proposed command: `agmen doctor`, text output with one bounded line per check and a repair line for each failure. Exit 0 when all required checks pass, 1 for an unhealthy or incomplete install, 2 for usage. Keep existing commands' exit codes unchanged (`desktop/live:crates/agmen-cli/src/lib.rs:10`).
- Check in order: binary resolution and pair compatibility; `agmend service status` plus owned socket and `hello {api: 1}`; saved sign-in validated with the hub; hub HTTPS health; selected worktree identity and broker transport; detected agents' plugin or MCP configuration. Read daemon ownership checks and reuse the codec, rather than copying the desktop client (`desktop/live:crates/agmen-desktop/src/backend/agmend.rs:1020`).
- Health reuses `/healthz` (`crates/agmen-hub/src/health.rs:22`) through the HTTP client. Preserve the hub URL's base path. Require HTTPS outside loopback and never downgrade TLS. Broker diagnosis first tests the configured transport with TLS/WSS where applicable, without replacing a live identity connection. A transport pass does not prove MQTT authorization. If a live session is present, report its status only if the merged daemon exposes it; otherwise label authorization untested and give the agent/watch check. Do not steal a client id or create a durable session to make doctor look complete.
- Print checked, failed and skipped distinctly. No identity means skip broker with `run agmen join <invite link> in your clone`. Missing daemon says `agmend service install`. Missing sign-in says `agmen login --hub <origin>`. Unwired agents say `agmen setup`. A hub failure names the endpoint and asks the host to restore it. A broker failure names its transport and asks the host to check the advertised endpoint. Never invent a local fix for a remote outage.
- Agent inspection uses Task 4's detection and merge logic without writing files or running plugin installation. Check all installed supported agents, with no agents reported as incomplete and `install an agent, then run agmen setup`. Inspect Codex's worktree cwd and plugin-vs-MCP choice, not just an agmen key's presence. Cursor and Gemini formats remain Task 6's responsibility (`docs/plans/onboarding.md:458`).
- Use at most 5 s per network check and 30 s for the command. Continue independent checks after failure; skip checks whose prerequisites failed. Use only safe origins and sanitized error categories in output. No token values, invite codes, automatic login, repair or configuration writes.

- [ ] **Step 1: Write failing tests.** `doctor_healthy_install`, `doctor_missing_daemon_prints_fix`, `doctor_rejects_foreign_socket`, `doctor_api_mismatch`, `doctor_expired_login`, `doctor_https_failure`, `doctor_broker_failure`, `doctor_does_not_claim_transport_proves_auth`, `doctor_checks_each_agent`, `doctor_no_identity_skips_broker`, `doctor_finishes_within_deadline`, `doctor_changes_no_files`. Use fake managers, temporary configs, a fake hub and a fake socket. Snapshot bytes before and after.
- [ ] **Step 2: Run** `cargo nextest run -p agmen-cli --locked -E 'test(doctor)'`. Expected: unknown subcommand or failed assertions before implementation.
- [ ] **Step 3: Register the command and implement checks with existing readers.** Do not invoke setup's writer. Where the merged daemon has no read-only broker status, expose the limitation in output and report the missing API as follow-up work rather than claiming authentication passed.
- [ ] **Step 4: Run** doctor tests, CLI tests, isolated integration profiles and the full gate. Verify combinations of independent failures and nonzero exit status, not only one fault at a time.

## Phase C: preview access and later desktop

### Task I5: Document the private preview and release handoff

**Files:**
- Create: `docs/install.md`.
- Modify: `README.md`, `docs/mcp-setup.md`; site install text under hosted Task 17's existing ownership.
- Do not edit the trial-hub script as part of this task. Its inspected help is evidence of the current source-install path, not a production installer.

**Interfaces:**
- One install guide owns prerequisites, install, upgrade, uninstall and doctor usage. README and the invite page link to it. Distinguish installed behavior from planned commands until each owning task lands.
- Before public artifacts exist, friends need access to the private source repository and Rust 1.99.1 for the current CLI source install. The actual trial help states this at `dev/trial-hub:deploy/dev/hub.sh:82`. That help installs only agmen and documents bare-code join; it does not satisfy paired install, device-flow onboarding or doctor. Keep the existing trial running; do not run its Funnel commands or installer during this documentation work.
- A private binary preview requires access to its release assets. A private tap also requires access to the tap repository; private Claude plugin installation requires marketplace source access. Browser GitHub sign-in to the hub grants none of these distribution permissions. Keep publisher `HOMEBREW_TAP_TOKEN` separate from each friend's read credential (D27, `desktop/live:docs/plans/completion.md:82`).
- Recommendation: keep preview source access for the small group until the approved public tree ships. Do not make friends put tokens into the public curl line. If Andy wants a private tap sooner, first validate tap Git authentication, private asset download authentication and plugin access with a dedicated test account and short-lived least-privilege credentials. Do not prescribe an untested environment variable as a complete private-tap solution.
- [Homebrew's tap docs](https://docs.brew.sh/Taps) establish that `agmen-dev/tap` maps to `agmen-dev/homebrew-tap` and permit explicit Git URLs. This mapping does not prove private release-asset authentication. Record the tested access mechanism in the guide before distributing a private tap.

- [ ] **Step 1: Write acceptance cases** for source-only preview, private binaries plus tap, private marketplace, public release, missing repository access, missing asset access and expired credentials. Each case states exactly which command is available today and which permission is missing.
- [ ] **Step 2: Check those cases against the current guide and release plan.** Expected before this task: no single guide explains all distribution permissions and lifecycle paths. This is a documentation check, not a reason to add a new test framework.
- [ ] **Step 3: Write the guide and links.** Reuse Tasks 13 and 14's artifact and URL names. Include I2's two-step Homebrew uninstall and preserve-data policy. State that Git and an installed supported agent are joining prerequisites, while Docker and broker setup belong to the host.
- [ ] **Step 4: Validate links and cited commands against the merged implementation.** Run `sh site/check.sh` once hosted Task 17's site is present (`docs/plans/hosted.md:806`), plus the existing cargo gate. Private-access live checks require Andy's distribution choice and a dedicated test account; record them as unverified until performed. They do not belong in this planning run.

### Existing Task 13b: desktop acceptance additions, no new packaging task

Keep the app behind D37's Apple account gate. Task 13b produces the signed and notarized macOS download, then verifies that launching it discovers the already installed daemon, performs the local API handshake and reports a missing or incompatible daemon with the install or upgrade command. Test on a clean macOS user account at packaging time. Never start a second bundled daemon or ask the app to hold hub tokens. The existing backend already opens the socket and calls `whoami` (`desktop/live:crates/agmen-desktop/src/backend/agmend.rs:409`).

Use the bundle, Developer ID Application certificate, hardened runtime, notarization and stapling procedure recorded at `desktop/live:docs/adr/0010-desktop-gpui-spike.md:35` and `desktop/live:docs/adr/0010-desktop-gpui-spike.md:36`. The Apple signing procedure was read, not run in this planning task. Linux desktop packages and automatic desktop updates remain outside this install plan; the ADR lists Linux packaging as not started at `desktop/live:docs/adr/0010-desktop-gpui-spike.md:37`.

## Decisions and external actions for Andy

| Item | Recorded decision or open action | Recommendation |
|---|---|---|
| Domain | `getagmen.ai` is already decided at `docs/specs/2026-10-26-onboarding-and-hosting-design.md:344`. Registration and serving are unverified here. | Keep it. Register it and complete the existing hosted Task 17 site and redirects. Do not reopen naming. |
| Public or private distribution | Public MIT and a fresh squashed `agmen-dev/agmen` repository are already approved at `docs/specs/2026-10-26-onboarding-and-hosting-design.md:343`. Publishing is Andy's action. | Finish L5's scans, notices and public-tree task. Use source access for friends until publication; approve a private tap only if that wait is unacceptable. |
| Homebrew tap and publisher credential | D27 names `agmen-dev/homebrew-tap` and `HOMEBREW_TAP_TOKEN`; provision status is unverified (`desktop/live:docs/plans/completion.md:82`). | Create the tap before the first tag. Prefer a public tap and public assets after the public-tree gate. Keep write credentials out of client installers. |
| Signing accounts and release trust | D37 defers the desktop until the Apple account exists (`desktop/live:docs/plans/completion.md:73`). No artifact signer is specified by onboarding Task 14. | Use workflow-bound Cosign signatures for CLI/daemon release artifacts. Andy authorizes the publisher identity and verifier bootstrap. Obtain Developer ID Application and notarization credentials for Task 13b. Apple signing and release-archive signatures are separate checks. |
| dist and notices tooling | D28 selects cargo-about and asks approval for dist (`desktop/live:docs/plans/completion.md:83`). | Keep those existing tool approvals in L5. Confirm paired artifact output during Task 13 before documenting its layout. No tool installation in this planning task. |
| Clone selection | The brief's desired one-command clone conflicts with the current explicit outside-clone stop (`docs/specs/2026-10-26-onboarding-and-hosting-design.md:112`). | Approve I3's interactive selection and explicit flags. Preserve the noninteractive contract and document the migration. If declined, ship the already planned manual clone flow. |
| Homebrew automatic service start | Current service code exists, but formula service-start support and sandbox behavior were not tested. | Require I2's disposable integration check before claiming parity. If blocked, approve service registration on first setup and state the extra step plainly. |

## Order and completion evidence

Keep L14, L15 and L22's ownership and merge order. I3 follows Task 7; I4 follows setup and the daemon/local API merges. I1 and I2 extend L5 Tasks 13 and 14 together, after D27 and D28 prerequisites. I5 follows verified release behavior. Task 13b remains independent of the first CLI/daemon tag, as L5 specifies (`desktop/live:docs/plans/completion.md:214`).

Completion requires actual artifact inspection, signature rejection tests, lifecycle tests, join recovery tests, doctor failure cases and the existing cargo gate. A green documentation check cannot prove that install URLs, accounts, release artifacts, private access or service installation work. Record exactly which architectures and release channels were exercised, with command outputs, before calling the target experience shipped.
