# agmen completion program plan

> **For agentic workers:** This is a program plan over lanes, not a task plan. Each lane runs another plan file's tasks with superpowers:subagent-driven-development. Read the lane's entry here for its branch, owned files, order and deltas, then read the plan file and task numbers it names. Where this file and a task plan disagree on ownership, numbering or order, this file wins until the plan file is fixed (section 10).

**Goal:** Finish all remaining agmen work in nine steps: (1) land desktop/live; (2) close the MVP checks, deploy and dogfood; (3) roles; (4) the rest of agmend; (5) the launcher and desktop agents; (6) the listener and the desktop listener panel; (7) the roles and listener exit tests; (8) onboarding, self-host, hosted and the public release; (9) plans and decisions for the later items. Parallel subagent lanes do the work, and no two open lanes edit the same file.

**Architecture:** 30 lanes, each on its own branch and worktree under `/Users/andyaldrich/Projects/agmen/.worktrees/`. One coordinator session dispatches lanes, keeps the ADR and migration registries, and hands each hotspot file from one lane to the next at a merge point. A lane that only adds new files runs beside any other lane. A lane that edits a hotspot holds it alone until its merge point. A stacked lane starts from the reviewed tip of the lane before it, so it does not wait for Andy's merge. Pure agmend modules are built early on feeder branches but merge into mvp/impl only inside the lane that wires them, because agmend declares every module private (`crates/agmend/src/lib.rs:17` to 25) and unused private code fails clippy with `-D warnings`.

**Tech Stack:** Rust 1.99.1, edition 2024, with the versions already locked: rusqlite 0.39.0 and keyring-core 1.0.1 (`crates/agmen-client/Cargo.toml:18,21`), roxmltree 0.21.2 (`crates/agmen-local/Cargo.toml:18`), toml_edit 0.25.17 (`crates/agmen-desktop/Cargo.toml:37`), rmcp 3.7.0. The plans' rusqlite 0.41 and keyring 4.2.0 are stale. Claude Code 2.1.311 is installed; the ADRs and Jenkinsfile:25 pin 2.1.310. Dev stack: `deploy/dev/compose.yaml` (EMQX, Postgres, HAProxy, Toxiproxy, static JWKS).

**Spec:** [desktop.md](desktop.md) Task 20; [mvp.md](mvp.md) Tasks 5, 14, 16, 17; [m0-m1.md](m0-m1.md) Tasks 6, 7, 13, 17, and the swarm chat parts of Tasks 14 to 16 (D38); [roles-listener.md](roles-listener.md) Tasks 1 to 7; [desktop-live.md](desktop-live.md) Tasks 9 and 10; on branch `design/onboarding` (d902c52): `docs/plans/onboarding.md` (16 tasks), `self-host.md` (11), `hosted.md` (17), `broker-spike.md` (8, done) and `docs/specs/2026-10-26-onboarding-and-hosting-design.md`; [../design.md](../design.md) for step 9.

## Global Constraints

- Facts here were read on 2026-10-27 at desktop/live fbe988d and mvp/impl df9331a. Both remotes' mvp/impl are at df9331a. Nothing was built or run for this plan.
- Every implementer finds code by symbol, not by the plan's line numbers. The onboarding, self-host and hosted plans cite lines at 313a4aa or 149c67d, and the cited files have changed by +489/-278 since.
- Only the coordinator assigns ADR and migration numbers (sections 8.2 and 8.3). No lane computes "the next free one".
- Hotspot files have one owner at a time (section 8.6). Other lanes only read them.
- A test that asserts behavior a task replaces changes only with Andy's ruling (section 1). Without one, the lane controller stops and reports the test and the assertion.
- No live model run (`AGMEN_LIVE=1`, S7, `live_*` tests) without Andy's yes. No push, merge or branch delete without Andy's yes. No subagent handles secret values.
- Rulings R1 and up and the plan contracts live only in the main checkout's gitignored `/Users/andyaldrich/Projects/agmen/.superpowers/sdd/*/progress.md` and `.superpowers/sdd/onboarding/plan-contracts.md`. Every brief gives those absolute paths.
- This plan's only copy is the untracked `docs/plans/completion.md` in the desktop-live worktree, which D31 and 11.7 remove after M1. Before the first dispatch, the coordinator copies it to `/Users/andyaldrich/Projects/agmen/.superpowers/sdd/program/completion.md` and edits only that copy from then on. Every brief gives that absolute path.
- Plain, specific language in all docs, commits and comments. No em or en dashes. Ranges use "to".

## Review Focus

1. **Two lanes edit one file.** Every lane's owned-file list must be disjoint from every other open lane's, except the additive-only files in section 8.6. The coordinator checks this before each dispatch.
2. **Migration numbers collide or land out of order.** roles-listener.md:108 names `0005_roles.sql`, which collides with `0005_presence`. Hub lanes merge through one queue in number order (section 8.3).
3. **A plan step cannot pass as written.** Examples: the nextest filters `test(roles_proto)`, `test(it_roles)` and `test(role_cli)` match no test names; m0-m1 Task 17 Step 5 uses `AGMEN_IT_HUB`, which no code reads; self-host Task 5 edits a spike file absent on mvp/impl. Section 10 lists the fixes, and a lane does not start until its plan is fixed.
4. **The merge carries no CI evidence.** Neither forge runs CI (MR head_pipeline is null; no `.github/workflows` or `.gitlab-ci.yml`). Every PR body records the gate sha and counts from a local run.
5. **Local-only work is lost.** design/onboarding, spike/rmqtt, site/getagmen, release/oss-prep and release/mockups exist on no remote. They are pushed in Wave 0 (D31).

---

## 1. Decisions Andy must make

Each row names what it blocks. "Recommendation" is the default the lanes assume if Andy agrees.

### Needed in Wave 0

| ID | Decision | Recommendation | Blocks |
|---|---|---|---|
| D1 | Which PRE-1 minors (a) to (e) block the merge. (c) changes a hub route | Fix (a) to (c). Fix (d) and (e) or mark them `shortcut:` | L1 |
| D2 | Merge mvp/impl's 2e698c7 and df9331a into desktop/live before the gate, or merge as is | Merge first, so the gate runs on what lands (merge-tree is clean) | L1 gate |
| D3 | Merge method for PR #1 and MR !2 (353 commits, including the desktop/gpui merge), and which forge merges first | A merge commit, which keeps the reviewed shas. Merge on one forge, push mvp/impl to the other, close the twin | every merge point |
| D4 | Be at the unlocked Mac for shot.sh and the `list_measures_10k_rows` rerun (L1 T20-0). Accept a 1280 px window (main.rs:238 asks 1440x900; desktop.md:201 records 1280). Add an AeroSpace float rule or not | Accept 1280 as a recorded difference | L1 shots |
| D5 | Merge PR #1 and MR !2 yourself, after the review loop (the merge is held for you) | Yes | M1 |
| D11 | Push agmen-kubernetes c509129 (R71, durable sessions off) | Yes | L29, L3 deploy |
| D20 | S7: refresh it to the listener shape, and approve spend for the haiku and sonnet runs | Refresh it; approve a fixed budget | L2 P1 (the refresh), L11 live runs, L21 |
| D26a | Read onboarding.md, self-host.md and hosted.md and agree to their substance, so L4 can write the errata (section 10) | Yes | L4 |
| D7 | Local sign-in for the checks: a localhost GitHub OAuth App, or the existing fake GitHub | A real OAuth App, which Andy creates (hands-on list) | L3 task 2 (`deploy/dev/hub.sh`), L3 task 4's check |
| D25 | The ADR registry in section 8.2 | Approve | L3 task 1, L11, L25 |
| D30 | Send the EMQ license question (draft at `.superpowers/sdd/onboarding/emq-email-draft.md`, not read for this plan). Choose the hosted broker: EMQX Cloud Dedicated Flex ($234/month) or a commercial license. Is upstream `emqx/emqx:6.3.2` allowed in the self-host compose file | Send now, because the answer takes time | L25 Task 7, L29 hosted overlay, the site's subprocessor list |
| D31 | Push the five local-only branches as backup. Delete mvp/t14 (needs `-D`). Prune the 27 merged worktrees after copying their `.superpowers`. Commit spec-coord's untracked 268-line coordinator-inbox spec | Push all five now; prune after M1 | data safety, L5 |
| D35 | Compare the Task 20 shots against release/mockups' Lucide mockups, and land release/mockups before L20 and L23 take shots | Yes | L1 sheet, L20, L23 |
| D36 | Is spike S6 (Windows local transport, m0-m1 Task 6) in this batch, and is there a Windows 11 machine or VM | Out for now. L7 and L10 mark the Unix-only seam with `shortcut:` | L30, any Windows work |
| D10 | Start the outside infrastructure now, because it has lead time: agmen's cluster, registry, domain, secret store, S3 bucket, Jenkins `AGMEN_REGISTRY`, the agmen-dev org and its GitHub OAuth apps, and for hosted Task 16's uptime worker a Cloudflare account, a Workers KV namespace (`UPTIME`) and an ntfy alert topic. Ask the cluster owners whether Ceph RBD encrypts at rest and whether port 8883 is reachable from the internet (design.md:673 to 674, both unverified) | Start now. If the cluster has no encryption at rest, hosted takes no customer data until L27 adds hub-side body encryption (design.md:673) as an extra task | L3 deploy, L29, dogfood, real use of L14, the hosted launch |

### Needed before Wave 1

| ID | Decision | Recommendation | Blocks |
|---|---|---|---|
| D6 | Run the MVP checks on mvp/impl now, or after M1 | After M1, because desktop/live changes login and hub auth | L3 interactive check |
| D8 | ADR-0005: run the spike check on `.worktrees/t5`, or close it through ADR-0006 (ADR-0005:52 points that way) | Close it by reference | L3 |
| D9 | Where ruling R24 lives (it is not in the repo), and whether a subagent may drive the two sessions in tmux | You run the check yourself | L3 interactive check |
| D13 | Where role code lives, and which XML library | Move agmen-local::roles into a sync `agmen-roles` crate on roxmltree 0.21.2, and have agmen-local re-export it | L12, L2 |
| D14 | StaleRequest: a wire NackReason or an agmend local error. EditScope's home | An agmend ErrorCode, since the hub never sends it. EditScope in agmen-proto | L6, L10 |
| D17 | agmend scope now: which parts of m0-m1 Task 13; devices in or out; where the pending queue lives; how a request carries the local token; the new rusqlite 0.39.0 (bundled) edge into agmend for `state.rs` (8.5) | Identities, caller binding, inbox, watch, state, and an agmend pending queue for identities agmend runs. Onboarding Task 10's pending for interactive sessions. Devices deferred. Approve the rusqlite edge | L2 agmend.md, L10 |
| D18 | Who owns `agmend service install`, its layout, and replacing the tests at `crates/agmend/tests/cli.rs:37` to 46 and `crates/agmen-desktop/src/ui/live_root_tests.rs:74` to 81 | L7 owns it, as `service/` with one file per OS, declared from main.rs | L7 |
| D19 | `login_start` before or after onboarding Task 1, which drops `verification_uri_complete` and the approve routes | After | L4's onboarding Task 1 erratum, L2 P7, L17's login task |
| D29 | Hosted Task 10: reuse `GET state?key=roster`, or add `/roster` | Reuse it, add the head, and update agmend's reader in the same PR | L4's hosted Task 10 erratum, L26 |
| D33 | Where desktop Tasks H1 and H2 (hosted.md:636 to 642) go | desktop.md on mvp/impl | L4's hosted Task 13 erratum, L28 |
| D37 | What the first public release ships, and the signing accounts. Onboarding Task 13 builds only `agmen` (`dist = false` workspace-wide, only agmen-cli opts in, onboarding.md:774 on design/onboarding), yet steps 4 to 7 need agmend, a separate binary crate (`crates/agmend/Cargo.toml:9` to 11). The desktop app has no bundle, signing or notarization yet (ADR-0010:34 to 38) and needs the Apple Developer ID (design.md:675) | Ship `agmen` and `agmend` together in the first release, because the new desktop cannot talk to a pre-Task-8 agmend (desktop-live worktree `.superpowers/sdd/desktop-live/progress.md:107`, "ship together"). Ship the desktop app once the Apple account exists | L4's Task 13 erratum, L5 Task 13b, the release tag |

### Needed before Wave 2

| ID | Decision | Recommendation | Blocks |
|---|---|---|---|
| D15 | Hub roles: approve changing agmen-hub; replace the tests at `crates/agmen-hub/src/ingest_tests.rs:94` to 106 and 148 to 160 and `crates/agmen-local/src/roles_tests.rs:91` to 92; the fallback for identities with NULL verbs; the delegated_by column; whether the minted agent or HELLO wins in the roster; whether "coordinator" for LEND means the kind or the membership role | Approve. NULL verbs fall back to the kind table. Add delegated_by. The minted agent wins | L13, L12 |
| D24 | The migration registry in section 8.3, and whether any shared or deployed Postgres has applied hub migrations | Approve | L13, L14, L18, L27 |
| D26b | Approve the three plans with L4's errata applied, and merge design/onboarding (M-plans) | Your review of L4's diff | L5 Tasks 11, 13, 14 and 15, L14, L15, L18 to L28 |
| D27 | The agmen-dev org, two GitHub OAuth apps with Device Flow, and the `agmen-dev/homebrew-tap` repository with its `HOMEBREW_TAP_TOKEN` repository secret (onboarding.md:923 on design/onboarding; Task 13's `publish-jobs` includes `homebrew`, onboarding.md:773) | Create the org and apps before real use, and the tap before the first tag; the code lands with the placeholder client id | real use of L14, the release tag |
| D28 | Notices: cargo-about with a Jenkins `--check`, or oss-prep's hand-written file. The copyright line. Installing dist 0.33.1 | cargo-about, and the plan's copyright line | L5 |

### Needed before Wave 3 or later

| ID | Decision | Recommendation | Blocks |
|---|---|---|---|
| D12 | Merge MR !1 (mvp/impl into main). origin/main is 9866443 and github/main is 5c62032 | Merge after M-checks, and bring both mains to one sha | the Task 16 image, the Task 17 install |
| D16 | What an unjoined `agmen mcp` lists (`crates/agmen-cli/tests/mcp.rs:73` to 78 and `:237`); a `lend` tool; what `--kind` does once `--role` exists; a fake or real hub for the join test | Keep 12 tools when unjoined. No lend tool yet. The role decides the kind. A real hub `it_` test | L2 P3 Task 4, L16 |
| D21 | Live test spend for `live_launch.rs` and `live_listener.rs` (2.00 per process, roles-listener.md:25) | Approve when the lanes reach them | L17's `live_launch.rs`, L21's `live_listener.rs` |
| D22 | Whether `[listener.<project>]` shares `~/.config/agmen/settings.toml` with the desktop's appearance settings, and who writes it | Share it; both writers use toml_edit; agmend owns the listener section | L2 P4's settings part, L21's `[listener.<project>]` settings part |
| D23 | Where desktop SpawnWorker gets role, budget, base and brief (`crates/agmen-desktop/src/backend/mod.rs:194` to 196 carries only `agent`) | Role defaults plus the add-agent dialog | L2 P5, L20 |
| D32 | Site placeholders (legal name and place, retention periods, server location), the getagmen.ai domain, mailboxes, Paddle | Yours | site deploy, hosted |
| D34 | Schedule the dogfood round with Adam's team, and name the coordinator | After M-main and the deploy | L3 dogfood |
| D38 | Swarm chat from M1 (design.md:644): the `agmen chat` command, the MCP `chat` and `read_chat` tools (m0-m1.md:412) and the exit test `it_human_messages_swarm` (m0-m1.md:436). None exists on desktop/live (`crates/agmen-cli/src/cmd/` has no chat, `crates/agmen-cli/src/mcp/tools.rs` has no chat tool, `tests/e2e/tests/it_m1.rs` has no such test), and mvp.md never mentions chat | Build them: the command and tools in L22, the exit test in L24. If you rule them dropped, L5 amends design.md M1 instead | L2 P6, L22, L24 |
| D39 | The coordinator-inbox spec's nine open decisions (spec-coord worktree, untracked `docs/specs/2026-10-25-coordinator-inbox-design.md:180` to 191, Status proposed at :3), including a DM metadata endpoint, an ANSWER mention rule, replacing R31's interrupt filter and per-stream paging cursors in place of R45's single `after` | Rule after the dogfood round. Until then lanes keep the MVP rulings, and the spec waits in section 12 | L13 (mention stamping) and L22 (inbox paging), only if adopted earlier |

Andy's hands-on work, whatever the rulings: run shot.sh; be at the unlocked Mac with the window in front for the `list_measures_10k_rows` rerun (`cargo nextest run -p agmen-desktop --release --run-ignored only list_measures_10k_rows`, `crates/agmen-desktop/tests/spike_list.rs:3`; L1 T20-0); for D7, create the localhost GitHub OAuth App and store its client id and secret in the Keychain under the service `agmen-local-github-oauth` (accounts `client-id` and `client-secret`), which L3 task 2's script reads; the IME check for desktop.md Task 3 (L1 T20-0); run the ADR-0006 two-session check; every push and merge; the OAuth apps and the Homebrew tap repository with its token (D27); the site values; the Tailscale and ngrok runs (self-host Task 11); hosted Task 16 Steps 5 to 7 (applying the restore Job to the production namespace, deploying the uptime worker and setting its `ALERT_URL` secret with wrangler, and the production scale-to-zero test; hosted.md:762 to 763 on design/onboarding); and the publish after L5 Task 15 (onboarding.md:927): create the public `agmen-dev/agmen`, make one commit from main's tree authored with his GitHub no-reply address, push only that commit, then push the first version tag.

## 2. Risks settled in Wave 0

These unknowns can each invalidate weeks of lane work, so Wave 0 settles them before the lanes they affect are dispatched.

| Risk | What it can break | Wave 0 action |
|---|---|---|
| S7 triage quality and runtime mode (m0-m1.md:157 to 188; design.md:483, :680) | roles-listener Task 6's runtime (roles-listener.md:209), its default model, and budget tracking (design.md:175) | L11 builds fixtures and the harness now; the live runs follow D20 |
| S6 Windows transport (m0-m1.md:143 to 155). agmend and the desktop client are Unix-only (`crates/agmend/src/api.rs:13` to 14, `socket.rs:13` to 35, `crates/agmen-desktop/src/backend/agmend.rs:26` to 27) | every socket path in agmend and the desktop | D36. If in, L30 lands its bind and connect seam as a task on L10's branch before M4 |
| ADR-0006 interactive check (ADR-0006:108 to 119, Status proposed at :3) | the MVP exit, dogfood, onboarding Task 4's plugin install | L3 writes the hub runner now; Andy runs the check after M1 |
| EMQX license (ADR-0007:377; onboarding.md:925) | self-host Task 7's compose image, hosted Task 15, the site's subprocessors | D30: send the email now |
| design/onboarding unapproved and local only | L5's onboarding tasks, L14, L15, L18 to L28 | D26a, D26b and D31 |
| sqlx behavior for a migration version lower than one already applied (unverified) | every hub lane | Before L13 starts, a read-only check of sqlx's `migrate` source in `~/.cargo/registry`. Until it is done, out-of-order merges are forbidden |
| Outside infrastructure has lead time | the deploy, dogfood, hosted | D10 starts now |

## 3. Lanes at a glance

Remotes: `origin` is GitLab (`https://git.groupdrive.xyz/groupdrive/agmen.git`), `github` is GitHub (`https://github.com/AndyAld/agmen.git`). "Crit" marks the critical path (section 7).

| Lane | Branch | Worktree | Base | Step | Wave | Crit |
|---|---|---|---|---|---|---|
| L1 desktop-land | `desktop/live` (exists) | `desktop-live` (exists) | itself | 1 | 0 | yes |
| L2 plan-refresh | `plan/refresh` | `plan-refresh` | desktop/live fbe988d | 3 to 7 docs | 0 | |
| L3 mvp-checks | `mvp/checks` | `mvp-checks` | mvp/impl df9331a, then M1 | 2 | 0 to 6 | |
| L4 onboarding-plans | `design/onboarding` (exists) | `spec-onboard` (exists) | d902c52, merge M1 in | 8 docs | 1 | yes |
| L5 release-prep | `release/oss-prep` (exists) | `oss-prep` (exists) | 2d3cd54, merge M1 in | 8 | 0 to 6 | |
| L6 roles-proto | `roles/proto` | `roles-proto` | M1 | 3 | 1 | yes |
| L7 agmend-service | `agmend/service` | `agmend-service` | M1 | 4 | 1 | |
| L8 launcher-parts (feeder) | `agmend/launcher-parts` | `launcher-parts` | M1 | 5 | 1 | |
| L9 listener-parts (feeder) | `agmend/listener-parts` | `listener-parts` | M1 | 6 | 1 | |
| L10 agmend-core | `agmend/core` | `agmend-core` | M1 | 4 | 1 to 3 | yes |
| L11 spike-s7 | `spike/s7-listener` | `spike-s7` | mvp/impl df9331a | 6 | 0 to 3 | yes |
| L12 roles-crate | `roles/crate` | `roles-crate` | `roles/proto` tip | 3 | 2 | yes |
| L13 roles-hub | `roles/hub` | `roles-hub` | `roles/proto` tip | 3 | 2 | yes |
| L14 onb-hub | `onb/hub` | `onb-hub` | M1 plus M-plans | 8 | 2 | yes |
| L15 onb-setup | `onb/setup` | `onb-setup` | M1 plus M-plans | 8 | 2 | yes |
| L16 roles-cli | `roles/cli` | `roles-cli` | M3a plus M7c | 3 | 3 | yes |
| L17 agmend-launch | `agmend/launch` | `agmend-launch` | M4, then merges L8 | 5 | 3 | yes |
| L18 selfhost-hub | `selfhost/hub` | `selfhost-hub` | M7a | 8 | 3 | |
| L19 selfhost-infra | `selfhost/infra` | `selfhost-infra` | L18's reviewed tip plus M-notices | 8 | 3 | |
| L20 desktop-agents | `desktop/agents` | `desktop-agents` | M5 | 5 | 4 | |
| L21 listener-runtime | `listener/runtime` | `listener-runtime` | M5 plus M-S7, then merges L9 | 6 | 4 | yes |
| L22 onb-join | `onb/join` | `onb-join` | M3 plus M5 plus M7a plus M7c | 8 | 4 | yes |
| L23 desktop-listener | `desktop/listener` | `desktop-listener` | M6 plus M5b | 6 | 5 | yes |
| L24 roles-e2e | `roles/e2e` | `roles-e2e` | M6 | 7 | 5 | yes |
| L25 selfhost-cli | `selfhost/cli` | `selfhost-cli` | M7b plus M8a | 8 | 5 | |
| L26 hosted-early | `hosted/early` | `hosted-early` | M8a; its Task 10 half after M6 | 8 | 4 to 5 | |
| L27 hosted-hub | `hosted/hub` | `hosted-hub` | M8a plus M-H0; Task 9 after M9a; Task 11 after M-H1 | 8 | 5 | |
| L28 hosted-cli | `hosted/cli` | `hosted-cli` | M8b plus M9a | 8 | 6 | |
| L29 kube | `agmen-hosted` in agmen-kubernetes | `/Users/andyaldrich/Projects/agmen-kubernetes/.worktrees/agmen-hosted` | that repo's main after c509129 is pushed | 2 and 8 | 3 to 6 | |
| L30 spike-s6 (only if D36 says in) | `spike/s6-transport` | `spike-s6` plus a Windows 11 machine | M1 | 9 | 1 to 2 | |

## 4. Lane details

Every code lane uses the gates in section 8.1 unless its entry says otherwise. "Owns" means no other open lane edits those files.

### L1 desktop-land (step 1)

- Executes: desktop.md Task 20 Steps 1 and 3 (desktop.md:402 to 406; Step 2 is ticked at :405), desktop.md Task 3's two open boxes, then PR #1 and MR !2.
- Owns: `crates/agmen-desktop/**`, `docs/plans/desktop.md` (single writer, desktop.md:410), `docs/adr/0010-desktop-gpui-spike.md`. For PRE-1 only: `crates/agmen-hub/src/http.rs` (item c), `crates/agmend/src/console.rs` and `crates/agmen-desktop/src/backend/agmend.rs` (items d and e).
- Order:
  1. PRE-1 per D1. (a) A `shortcut:` comment near `crates/agmen-desktop/src/ui/chat.rs:610` saying the in-chat banner and the open-alert count wait for "Show the newest" (4c37b10 removed the earlier one). (b) Stop `LiveState::Open` drawing the `live:refused` selector (`live_root.rs:274`). (c) `DefaultBodyLimit` at 64 KiB on POST `/v1/projects/{project}/messages` (`http.rs:142`). (d) The race where an older snapshot answer overwrites a newer Local event. (e) The double post when a timed-out post actually landed. Each of (d) and (e) is fixed with a test that fails first, or marked `shortcut:`.
  2. SYNC per D2: merge `origin/mvp/impl` into desktop/live.
  3. T20-0: close desktop.md Task 3's two open boxes (desktop.md:94 to 95). The controller reruns `list_measures_10k_rows` with Andy at the Mac (D4), because the test opens a real release-build window that must stay in front (`crates/agmen-desktop/tests/spike_list.rs:1` to 3, :36 to 38). Before the rerun and before T20-1a, the coordinator pauses every other lane's builds (L11, L5 Task 12's gate, any L2 build), and resumes them after; the one run so far, 17.46 ms p99 under load, misses its own 16.7 ms assertion, and a second miss goes to Andy (review C69). Andy does the hand IME check with Kotoeri and the emoji picker at the Mac (ADR-0010:27). Both results go into ADR-0010 before T20-3.
  4. T20-2r: refresh ADR-0010's shortcut table (`docs/adr/0010-desktop-gpui-spike.md:46` to 48) and the counts in desktop.md Task 20 Step 2. This follows step 1, because item (a) adds a shortcut.
  5. T20-1a with Andy (D4): the controller, not a subagent, runs `crates/agmen-desktop/scripts/shot.sh all`. It needs the Mac unlocked and Screen Recording (shot.sh:9 to 10). Check all 25 PNGs exist and record their widths. Nothing else builds in this worktree during the run.
  6. T20-1b: pair each mockup with `target/shots/<screen>.png` using `magick ... +append` into the scratch dir (desktop.md:404). Per D35, take mockups from `release/mockups` with `git show release/mockups:docs/mockups/console/desktop-<screen>.webp`.
  7. T20-1c: five read-only comparison subagents, about 5 screens each, in the research groups (chat, agent, git and needs-you, ask and settings, plan). They return text and cite differences already accepted in desktop.md and in V112 (main checkout `.superpowers/sdd/desktop/findings-for-agent.md:760` to 767). Only the controller writes the Differences into desktop.md. The palette has no earlier comparison (desktop.md:374).
  8. T20-3: the full gate once, then tick Step 1, Step 3 and Done.
  9. PUSH: ask Andy, then push desktop/live to `origin` and `github` (both are at fbe988d today). Check that `git ls-remote origin desktop/live` and `git ls-remote github desktop/live` both print the local tip. Only then put the gate sha and counts in the PR #1 and MR !2 bodies.
  10. LAND (M1): Andy merges (D3, D5).
- Gate: the cargo gate, plus `--profile it` because item (c) changes hub code, plus the Plugin stage checks (Jenkinsfile:124 to 142) because SYNC brings changes to `mods/agmen-claude/hooks/register.tsx` and `register.test.tsx`.

### L2 plan-refresh (docs for steps 3 to 7)

- Executes: the fixes in section 10 for `docs/plans/m0-m1.md`, `roles-listener.md` and `desktop-live.md`, and the new `docs/plans/agmend.md`.
- Owns: those four files until M-docs. After M-docs, each executing lane ticks only its own boxes. L2 also ticks the desktop-live Tasks 1 to 8 step boxes (all `[ ]` at desktop-live.md:53 to 213).
- Order: P1 the S7 brief in m0-m1 Task 7 (unblocks L11); P2 `agmend.md` with L10's T1 to T5 and L7's service part; P3 roles-listener Tasks 1 to 4; P4 roles-listener Tasks 5 to 7; P5 desktop-live Tasks 9 and 10; P6 m0-m1 Task 13 pointer, Task 17 Step 5, Task 6 ADR number, and the swarm chat parts of Tasks 14 to 16 per D38; P7 the `login_start` and `login_status` task in agmend.md, for L17.
- Each piece starts once the decisions it writes in are made, so L2 runs from Wave 0 until its last piece: P1 needs D20's first half (the refresh); P2 needs D14, D17 and D18; P3 writes Task 2 first (D14) for L6, then Tasks 1 and 3 (D13, D15) for L12 and L13, then Task 4 (D16) for L16; P4 writes Task 5 and Task 6's policy and queue parts (D17, D18) for L8, L9 and L17, then Task 6's settings part (D22) for L21, then Task 7 for L24; P5 needs D23; P6's chat part needs D38; P7 needs D19.
- Where lanes read the fixed text: each lane's base (M1 or later) holds the unfixed plans until M-docs. So the coordinator records, in the program ledger, the plan/refresh sha at which each piece passed the plan reviewer (and, for P2 and P3, Andy's yes). The lane controller copies its plan text from that sha with `git show <sha>:docs/plans/<file>` into its briefs, and the lane does not start before that sha exists (section 5).
- Gate: the docs gate (section 8.1). A plan reviewer subagent rereads every cited line before Andy reads the plan.
- Merge point M-docs, after M1 and P7.

### L3 mvp-checks (step 2)

- Executes: mvp.md Task 14 Step 3 (mvp.md:252), Task 5 (mvp.md:132 to 134), Tasks 16 and 17; the ADR-0005 and ADR-0006 checks; the first-deploy ADR; dogfood prep and the retro.
- Owns: `docs/plans/mvp.md`, `docs/adr/0005-mod-streaming.md`, `docs/adr/0006-claude-code-plugin.md`, the ADR-0002 note below, the README Development subsection, and the new `deploy/dev/hub.sh`, `scripts/cluster-check.sh`, `scripts/smoke-deployed.sh`, `docs/adr/0008-first-deploy.md`, `docs/dogfood/round-1.md`, `scripts/dogfood-collect.sh`, and the retro ADR.
- Order:
  1. Plan fixes in mvp.md: Task 16 for agmen's own cluster (mvp.md:268, 272); the six anchors (mvp.md:172, 206, 213, 221, 261, 270); ADR-0006:127 for the Jenkins Plugin stage (Jenkinsfile:124 to 142); the registries from section 8.2; box ticks. Start from the four mvp.md hunks that release/oss-prep's cc5c2e4 already makes (mvp.md:268, 270, 272, 278: the "hosted cluster" heading, the m0-m1 Task 17 link, the persistent-volume bullet and the github.com install URL; `git show cc5c2e4 -- docs/plans/mvp.md`), so L5 can drop them (L5's entry). The m0-m1 Task 17 Step 5 rewrite is L2's P6.
  2. `deploy/dev/hub.sh` per D7. It reads the OAuth client id and secret from the Keychain service `agmen-local-github-oauth` (Andy's hands-on list) and never prints the secret.
  3. Write `scripts/cluster-check.sh` and `scripts/smoke-deployed.sh` (running them needs a kubeconfig).
  4. After M1, merge mvp/impl in. Andy runs the 8 steps of the ADR-0006 check (ADR-0006:108 to 119, D9). A subagent writes the results, sets Status accepted, ticks mvp.md:252, and records the Claude Code version actually used. mvp.md Task 5's interactive check (mvp.md:133) is recorded the same way.
  5. Close ADR-0005 per D8. Add the EMQX 6.3.2 resumed-session ACL finding from the RMQTT spike to ADR-0002.
  6. M-checks, then M-main (D12). Tag that main sha (for example `mvp-dogfood-1`), so later merges into mvp/impl do not change what dogfood installs.
  7. The first deploy through L29 (D10, D11), then run the two step 3 scripts against it and write ADR-0008.
  8. Dogfood prep: `docs/dogfood/round-1.md` (the round's goal, the team, the install steps from mvp.md Task 17 and what to record) and `scripts/dogfood-collect.sh` (gathers each machine's notes after the round). Then the round (D34), then the retro ADR (0020).
- Gate: shellcheck and shfmt on scripts, and the docs gate. The cargo gate only if the branch changes code, which it should not.

### L4 onboarding-plans (step 8 docs)

- Executes: the errata in section 10 for onboarding.md, self-host.md, hosted.md, broker-spike.md and the spec.
- Owns: those five files until M-plans.
- Starts after D26a and M1. First merge mvp/impl into design/onboarding (merge-tree is clean).
- Gate: the docs gate. Merge point M-plans, after Andy approves the errata (D26b).

### L5 release-prep (step 8 release)

- Executes: onboarding.md Tasks 11 to 15 and the new Task 13b (section 10), and the merges of `site/getagmen` (hosted Task 17, done) and `release/mockups`. Tasks 11, 13, 14 and 15 execute an unapproved plan until D26b, so they wait for M-plans.
- Owns: `LICENSE`, `THIRD_PARTY_NOTICES.md`, `crates/agmen-desktop/THIRD_PARTY_NOTICES.md` (after M1), `about.toml`, `about.hbs`, `scripts/third-party-notices.sh`, `scripts/public-tree-check.sh`, `.gitleaksignore`, `dist-workspace.toml`, `.github/workflows/*`, `install/*`, `site/**`, `docs/mockups/**`, `[workspace.package]` in Cargo.toml, comments in `deny.toml`, `docs/design.md` (sole writer), and the README License and Install sections. In turn (section 8.6): the `[package.metadata.dist]` tables in `crates/agmen-cli/Cargo.toml` and `crates/agmend/Cargo.toml`, `docs/mcp-setup.md:5` to 8, `Jenkinsfile:44` to 56 and `mods/agmen-claude/hooks/register.tsx` (Task 14, onboarding.md:809).
- L5 does not edit `docs/plans/mvp.md` or `docs/plans/m0-m1.md` before Task 15. Its cc5c2e4 already changes mvp.md:268, 270, 272 and 278 and m0-m1.md:5, 411, 442, 446, 450, 452, 455 to 456 and 477, lines L3 and L2 rewrite in Wave 0. So the first commit after merging M1 in reverses those hunks (`git diff 2536d68 cc5c2e4 -- docs/plans/mvp.md docs/plans/m0-m1.md | git apply -R`; 2536d68 is cc5c2e4's parent, pinned because after the M1 merge `git merge-base` returns the M1 tip), and L3 and L2 carry the wording they keep.
- Order:
  1. Task 12 (gitleaks over every ref) in Wave 0. It only reads history.
  2. After M1, merge mvp/impl in, then the reversal above.
  3. Task 11 per D28, after M-plans. Notices must cover agmen-desktop's gpui git dependencies, fonts and icons. The Material Icon Theme SVGs under `crates/agmen-desktop/tests/fixtures/zed-ext/material-icon-theme/` need a license check, and so do Zed's extension registry terms and the theme licenses (design.md:688, unverified). Merge point M-notices (LICENSE, the notices files, about.toml, about.hbs and the script), before Wave 3.
  4. Task 13 (dist) after M3, with L4's erratum: agmend opts into dist beside agmen-cli (D37). Task 13b (macOS app bundle, codesign, notarytool) only after D37 gives the Apple account. Task 14 (installers, which run `agmend service install` after install) after M7b, because it edits `docs/mcp-setup.md`, which L22 holds until then; it edits `mods/agmen-claude/hooks/register.tsx`, so run the Plugin stage checks. Merge point M-dist covers Tasks 13 and 14. Task 13b has its own merge point, M-app, which gates neither Task 15 nor the first release tag, because D37 ships the desktop app later.
  5. Task 15 (public tree) last, after every other docs lane merges. Its result is the tree Andy publishes (section 1, hands-on list), not a merge into mvp/impl.
  6. Bump the release version everywhere it appears.
- `site/getagmen` and `release/mockups` each get their own PR and MR as soon as D31 and D35 allow. release/mockups lands before L20 and L23 take shots.
- A conflict in `THIRD_PARTY_NOTICES.md` (L19 and L27 regenerate it after M-notices) is fixed by rerunning `scripts/third-party-notices.sh` on the merged tree, never by a hand merge.
- Gate: the cargo gate, `scripts/third-party-notices.sh --check` once it exists, `sh site/check.sh` exit 0, gitleaks for Tasks 12 and 15.

### L6 roles-proto (step 3)

- Executes: the rest of roles-listener.md Task 2 as L2 P3 fixes it, plus the shared types that let L12 and L13 run in parallel.
- Owns: `crates/agmen-proto/**`, `schema/**`.
- Order: T1 `EditScope` (serde, JsonSchema, TS). T2 the mint-body role spec under a name that does not clash with agmen-local's per-file `RoleSpec` (`crates/agmen-local/src/roles.rs:133`), for example `MintRole`. T3 export the per-kind verb table and the kind ceilings, so the hub (`ingest.rs:750` to 775) and agmen-local (`roles.rs:165` to 202) read one table. T4 PLAN `version >= 1` in validate. T5 `crates/agmen-proto/tests/roles_proto.rs` with the missing tests (REVIEW without reply_to, "dispatcher" refused, DELEGATED without agent or role). T6 regenerate `schema/` with the export-schema example.
- StaleRequest is not here (D14).
- Merge point M2. L12 and L13 branch from the `roles/proto` tip once T6 passes review.

### L7 agmend-service (step 4)

- Executes: the service part of m0-m1 Task 13 (m0-m1.md:368 to 369) per D18. roles-listener Task 5 drops its `service.rs`.
- Owns: `crates/agmend/src/main.rs`, the new `crates/agmend/src/service/{mod.rs,launchd.rs,systemd.rs}` declared from main.rs (so `lib.rs` is not touched), `crates/agmend/tests/cli.rs`, and the offline view in `crates/agmen-desktop/src/ui/live_root.rs` and `live_root_tests.rs` (shortcut at `live_root.rs:99` to 103).
- Order: T1 subcommand parsing (main.rs:1 to 24 refuses everything but `--help`). T2 unit-file rendering behind a test seam, so no test writes a real unit file (m0-m1.md:385). T3 install, uninstall, status. T4 the offline view offers `agmend service install`.
- If D36 keeps S6 out, mark the Unix-only service paths with `shortcut:`. Any manual check that loads a real LaunchAgent waits for Andy.
- Merge point M4, with L10.

### L8 launcher-parts (feeder for L17, step 5)

- Executes: roles-listener.md Task 5 parts worktree.rs and budget.rs.
- Owns: `crates/agmend/src/git.rs` and `git_tests.rs`, and the new `worktree.rs`, `worktree_tests.rs`, `budget.rs`, `budget_tests.rs`.
- Order: T1 a git runner with a longer timeout, captured stderr and a settable `GIT_INDEX_FILE` (`git.rs:23` fixes 5 s, `:86` drops stderr, `:80` removes GIT_INDEX_FILE). T2 worktree create, `merged_state` (Ancestor, Cherry, No), remove (never `--force` or `-D`), `save_wip` to `refs/wip/<branch>`, slugs up to 40 chars, and a detached create for L21's reviewer. Port the 7 tests at roles-listener.md:178 to 186. T3 budget steps at 80, 95 and 100 percent from `total_cost_usd`, behind a callback, with the unit tests L2 P4 names (`budget_stops_at_cap` itself needs launch, roles-listener.md:189, so it stays with L17).
- No PR of its own. Modules stay private `mod` in `lib.rs`, with tests in sibling `*_tests.rs` files as `git_tests.rs` does. Each new module carries `#![cfg_attr(not(test), expect(dead_code, reason = "wired by L17"))]` with a `shortcut:` comment naming L17 as the lane that removes it, and each new item in `git.rs` carries the same item-level form. A bare `#![expect(dead_code)]` fails clippy's `--all-targets` build, where the sibling tests use the items and the expectation goes unfulfilled; the repo already uses this form (`crates/agmen-desktop/src/backend/mod.rs:187` to 190). When L17 wires the module, the unfulfilled expectation fails the gate, which forces the removal.
- Gate on the branch: the cargo gate.

### L9 listener-parts (feeder for L21, step 6)

- Executes: roles-listener.md Task 6 parts policy (check_delegate) and queue, and the pure Narrow or Widen classify.
- Owns: the new `crates/agmend/src/listener/{mod.rs,policy.rs,queue.rs,classify.rs}` and their sibling test files.
- Order: T1 `check_delegate` with the defaults at roles-listener.md:23. T2 queue filtering, batch_id, one batch per 30 s, the 3-post thread cap, fed from a test source. T3 classify. Each task's tests are the sibling `*_tests.rs` unit tests that L2 P4 names. roles-listener Task 6's integration tests (from roles-listener.md:220) use a fake `claude` and a fake hub, and some run CLI commands L21 builds (`delegate_over_cap_is_pending` runs `agmen delegate approve`, `coordinator_cannot_be_removed` runs `set --accept-from`), so those stay with L21.
- Same rules as L8: no PR of its own, private modules, `#![cfg_attr(not(test), expect(dead_code, reason = "wired by L21"))]` with a `shortcut:` naming L21. The `feed.rs` hook waits for L21.

### L10 agmend-core (step 4)

- Executes: `docs/plans/agmend.md` from L2 P2, which replaces m0-m1.md Task 13 (m0-m1.md:357 to 388) for execution, in the scope D17 sets.
- Owns: `crates/agmend/src/api.rs`, `lib.rs` (Config and Agmend), `Cargo.toml`, `hub.rs`, `login.rs`, the new `identities.rs` and `state.rs`, `crates/agmend/tests/api.rs` and `tests/support/mod.rs`, and `crates/agmen-local/src/message.rs`.
- Order:
  1. T1 ErrorCode `Forbidden` and `StaleRequest`, additive behind `#[serde(other)] Unknown` (`message.rs:42` to 57). Check the desktop matches at `crates/agmen-desktop/src/backend/agmend_tests.rs:208,231`.
  2. T4 `state.rs` on rusqlite 0.39.0, a new edge into agmend that needs Andy's yes (D17, 8.5).
  3. T2 caller binding for `as` with a per-identity local token, replacing `api.rs:77` to 81 and its test at `tests/api.rs:79` to 88. Test with a seeded identity.
  4. T5 `inbox`, `watch {as}`, `state`, `pending {as}`, with `pending_survives_restart`.
  5. T3 `identity_mint` and `identity_list`, started only after M3a merges into mvp/impl (merge it in first). It posts the roles mint body `{handle, project, kind, role, verbs, edits, role_sha256, agent?, delegated_by?}` once, so it needs no second pass. Tokens go through agmen-client's `TokenStore`, keyed `<origin> <id>` (`token_store.rs:81` to 86).
  6. If D36 brings S6 in: L30's bind and connect seam lands here as an L10 task, because it changes `api.rs` (`serve(stream: UnixStream, ...)` at :25) and `lib.rs` (`socket::Bound` at :169, the bind and `api::serve` call at :181 to 186 and :233), which L10 holds.
- `login_start` and `login_status` (formerly T6) moved to L17, which holds `api.rs` after M4.
- Merge point M4 covers T1 to T5, so M4 waits on M3a.

### L11 spike-s7 (step 6)

- Executes: m0-m1.md Task 7 as L2 P1 refreshes it: `--tools Read,Grep,Glob`, `--append-system-prompt-file`, DelegateArgs with role and agent (roles-listener.md:209, 211), "listener" in names.
- Owns: the new `spikes/s7-listener/**`, one appended line in the root Cargo.toml members, `.config/nextest.toml` (section 8.6), and the new `docs/adr/0009-listener-runtime.md`.
- Order: T0 (before T3) the live gating in `.config/nextest.toml`: the default filter also excludes `live_` test names (`not test(/(^|::)(it|live)_/)`), and a new `[profile.live]` runs only them. Today the file has no `live` profile and excludes only `it_` (`.config/nextest.toml:6` to 7), so `tests/live_s7_*.rs` would join every lane's cargo gate and fail without `AGMEN_LIVE=1` (m0-m1.md:21). Live test functions are named `live_*`. L2 P1's Task 7 refresh names this. T1 harness and a fake agmen mcp. T2 40 triage fixtures with at least 6 injection cases, checked with gitleaks (m0-m1.md:169). T3 the 7 live tests (m0-m1.md:174 to 182), only after D20. T4 ADR-0009: runtime mode, mid-turn delivery, cost per turn, default model by the rule at m0-m1.md:186. The fixtures stay at `spikes/s7-listener/fixtures/triage.jsonl`; L2 P4 points roles-listener.md:202 there (it names `spikes/s7-dispatcher/`), so nothing is copied.
- Branches from mvp/impl because it touches no crate desktop/live changes. Merges after M1 (merge mvp/impl in first). Merge point M-S7.

### L12 roles-crate (step 3)

- Executes: roles-listener.md Task 1 as L2 P3 fixes it, in the home D13 picks.
- Owns: `crates/agmen-local/src/roles.rs` and `roles_tests.rs`, the new crate dir and its members line if D13 picks one, the re-exports in `crates/agmen-desktop/src/roles.rs`, and the role call sites in `crates/agmend/src/local.rs:26,150,227`.
- Lane deltas beyond the plan: schema errors for unknown elements; `version`, `worktree at`, `tools allow deny`; `extends` checked as a narrowing (replacing the shortcut at `roles.rs:257` to 258) and `extends="none"`; ceilings from L6's table; `Layers::load` keeps agmend's protections (`crates/agmend/src/local.rs:177` to 232); `claude_tools` maps git-read and settings to no Claude tool. Migrate the callers, and change `roles_tests.rs:91` to 92 per D15.
- Merge point M3a, with L13.

### L13 roles-hub (step 3)

- Executes: roles-listener.md Task 3 as L2 P3 fixes it.
- Owns: migration `0006_roles.{up,down}.sql`, `identities.rs`, `ingest.rs`, `ingest_tests.rs`, `lanes.rs`, `presence.rs`, the new `roles.rs`, `refs.rs`, `tests/it_roles.rs`, and protocol.md sections 1, 2 (roster row), 3, 4 and 5.4. For `RosterEntry.role` (8.6) only: the `RosterEntry` hunk in `crates/agmen-local/src/console.rs` (first in turn, before L20) and the `RosterEntry` literals in `crates/agmen-desktop/src/backend/fake_agmend.rs` (:28, :43, :48). Additive hunks only in `http.rs`, `lib.rs`, `test_hooks.rs`, `tests/support/mod.rs`.
- First task: the read-only sqlx ordering check (section 2). Record the answer in the program ledger.
- Lane deltas: the kind CHECK adds listener (`0002_core.up.sql:72`); columns role, verbs, edits, role_sha256, agent, delegated_by; `cap_verbs` intersects L6's table, including the listener's ban on CLAIM, DONE and LEND (protocol.md:21); the 409 for a second listener; REVIEW and PLAN opened by role (`ingest.rs:773`); the LEND owner check on `lanes.owner_identity_id`; a REVIEW's parent must be a DONE; role mentions (`ingest.rs:874`); refs (`ingest.rs:1130`); roster role per D15.
- Needs D15 before it starts. Merge point M3a.

### L14 onb-hub (step 8)

- Executes: onboarding.md Tasks 1, 1b, 2 and 3 as L4 refreshes them.
- Owns: `crates/agmen-hub/src/auth/**`, `config.rs`, `invites.rs`, `bootstrap.rs`, `main.rs`, `crates/agmen-hub/Cargo.toml`, `tests/it_auth.rs`, migrations `0007_github_device` and `0008_project_repo_url`, the new `invite_page.rs`, `invite_page.html`, `test_github.rs`; `crates/agmen-cli/src/cmd/account.rs` and the new `open.rs`, the `mod open` line in `crates/agmen-cli/src/lib.rs` (additive), and the `Ctx` and `run` part of `crates/agmen-cli/tests/support/mod.rs` (Task 1's FakeGithub, section 8.6); in turn (section 8.6), the non-route parts of hub `http.rs` and the `Config` and `TestHub` parts of hub `tests/support/mod.rs`; the new `crates/agmen-proto/src/repo_url.rs` and one export line in proto `lib.rs` after M2.
- Runs beside L13 through additive hub hunks. Merges after L13 in the hub queue (section 8.3). Merge point M7a.

### L15 onb-setup (step 8)

- Executes: onboarding.md Tasks 4, 5, 6 and 8.
- Owns: the new `crates/agmen-cli/src/setup/**`, `crates/agmen-cli/src/git.rs`, `docs/mcp-setup.md`, `crates/agmen-client/src/{session.rs,link.rs}`, the first turn on `crates/agmen-cli/Cargo.toml` (Task 5's toml_edit edge, section 8.6), the `mod setup` line in `crates/agmen-cli/src/lib.rs` (additive), and the `Home` and `apply_env` part of `crates/agmen-cli/tests/support/mod.rs` (Task 4's PATH rule, section 8.6). For Task 4 only: its `join.rs` hunk and a `Setup` arm in `cmd/mod.rs`.
- Order: Task 8 (independent), Task 4, Task 5, Task 6. Task 5 adds a toml_edit edge to agmen-cli. toml_edit 0.25.17 is already locked, so only the new edge needs Andy's yes.
- Merge point M7c, before L16 edits `join.rs`.

### L16 roles-cli (step 3)

- Executes: roles-listener.md Task 4 as L2 P3 fixes it, per D16.
- Owns: `crates/agmen-cli/src/cmd/{role.rs,join.rs,mod.rs}`, `crates/agmen-cli/src/mcp/{mod.rs,tools.rs}`, `crates/agmen-client/src/identity.rs` (new fields with `serde(default)`), `crates/agmen-cli/Cargo.toml`, `tests/{mcp.rs,support/mcp.rs}`, the new `tests/role_cli.rs`.
- Lane deltas: tool filtering at startup with rmcp 3.7.0 `remove_route` (`rmcp-3.7.0/src/handler/server/router/tool.rs:456`) or a `tools/list_changed` refresh; the `review` tool.
- Merge point M3, which completes the roles foundation.

### L17 agmend-launch (step 5)

- Executes: roles-listener.md Task 5 launch.rs, the local methods, the worker registry and the CLI commands, as L2 P4 fixes it.
- Owns from M4: `crates/agmend/src/api.rs`, `lib.rs`, `Cargo.toml`, `state.rs`, `tests/support/mod.rs`, plus L8's modules, and the new `launch.rs`, `tests/it_launch.rs`, `tests/live_launch.rs`, `crates/agmen-cli/src/cmd/{launch.rs,worktree.rs,workers.rs}`. CLI arms in `cmd/mod.rs` after M3. For the login task, from M7a: `crates/agmen-cli/src/cmd/account.rs` and a new agmen-client login module.
- First commit merges `agmend/launcher-parts` (L8). Wiring the modules removes their `expect(dead_code)` attributes.
- Before T1, the implementer runs `claude --help` and checks that 2.1.311 accepts `--max-budget-usd`, `--tools` and `--append-system-prompt-file` as roles-listener.md:172 writes them.
- Login task (formerly L10 T6), after M7a merges in: `login_start` and `login_status` in agmend's `call` match (`api.rs:97` to 109), from agmend.md's P7 task (D19), on a poll loop moved from `crates/agmen-cli/src/cmd/account.rs:17` to 108 into agmen-client. It lands in M5 with the rest of L17.
- Lane deltas: mint the worker through L10's `identity_mint` and write `.agmen/identity.json` into the worktree before claude starts; own process group; `activity {follow}` next to `console_watch` in `serve()` (`api.rs:47` to 54). `live_launch.rs` after D21.
- Merge point M5.

### L18 selfhost-hub (step 8)

- Executes: self-host.md Tasks 1 to 4.
- Owns: migrations `0009_tenant_roles` and `0010_hub_group`, `db.rs`, `me.rs`, the new `tenants.rs`, `projects.rs`, `group.rs`, and from M3a `identities.rs`. From M7a, in turn (section 8.6): hub `config.rs`, `main.rs`, the non-route parts of `http.rs` (`HubError`, `Inner`, startup) and the `Config` fields in `tests/support/mod.rs` (self-host.md:214 on design/onboarding).
- Merge point M8a, first in the hub queue of the pair.

### L19 selfhost-infra (step 8)

- Executes: self-host.md Tasks 5 and 6, without Task 5's edit of `spikes/s11-rmqtt/tests/it_s11_broker.rs` (absent on mvp/impl).
- Owns: `emqx.rs`, `crates/agmen-hub/Dockerfile`, the new `self_config.rs`, `mqtt_proxy.rs`, `tests/it_mqtt_proxy.rs`. After L18 in turn: hub `config.rs`, `main.rs`, the non-route parts of `http.rs` and the `Config` fields in `tests/support/mod.rs`. Tasks 5 and 6 rewrite ranges L18's Task 4 also rewrites (self-host.md:214, :276, :334 on design/onboarding: `config.rs:186` to 358, `http.rs:27` to 46, `main.rs`, `tests/support/mod.rs:259` to 290), so these are not additive hunks.
- Stacked on L18: L19 starts from L18's reviewed tip after M-notices has merged in, and opens its request only after L18 lands, so it shows only its own commits.
- New dependencies (aws-lc-rs, base64) need Andy's yes. Task 6 regenerates `THIRD_PARTY_NOTICES.md` with `scripts/third-party-notices.sh` from M-notices (L5's rule on conflicts). Merge point M8a, second in the hub queue of the pair.

### L20 desktop-agents (step 5 tail)

- Executes: desktop-live.md Task 9 as L2 P5 fixes it, per D23.
- Owns: `crates/agmen-local/src/console.rs`, `crates/agmend/src/console.rs`, `crates/agmen-desktop/src/backend/{agmend.rs,agmend_tests.rs,mod.rs}`.
- Ends with shots of the Agents and Agent screens against the mockups (Andy at the Mac). Runs beside L21: L20 does not touch `api.rs`, and L21 does not touch `console.rs`.
- Merge point M5b. It hands `crates/agmen-local/src/console.rs`, `crates/agmend/src/console.rs` and `crates/agmen-desktop/src/backend/agmend.rs` to L23.

### L21 listener-runtime (step 6)

- Executes: roles-listener.md Task 6 git-read tools, full settings, runtime, delegate and reviews, and the live tests, as L2 P4 fixes it.
- Owns from M5: `crates/agmend/src/api.rs`, `lib.rs`, `Cargo.toml`, `tests/support/mod.rs`, L9's `listener/**`, the `feed.rs` hook, one registration hunk in `crates/agmen-cli/src/mcp/mod.rs` (its last task, after M7b), `tests/live_listener.rs`, the new `crates/agmen-cli/src/mcp/listener_tools.rs` and `crates/agmen-cli/src/cmd/{listener.rs,delegate.rs,settings.rs}`.
- First commit merges `agmend/listener-parts` (L9).
- Lane deltas: git-read tools reuse L8's runner with fixed argv and a 32 KiB cap; `settings_apply` accepts a request only if it cites a message in the current batch whose hub-stamped sender is the listener's person; the runtime follows ADR-0009; the `[listener.<project>]` section per D22; live tests after D21, with cost and the triage table written into ADR-0009.
- Merge point M6.

### L22 onb-join (step 8)

- Executes: onboarding.md Tasks 7, 9 and 10. Task 10 must agree with D17's split of the pending queue. Per D38, also the swarm chat pieces of m0-m1 Tasks 14 and 15 as L2 P6 fixes them: the `agmen chat` command and the MCP `chat` and `read_chat` tools (m0-m1.md:412), on L10's pending (M4) and Task 10's pending for interactive sessions.
- Owns from M3: `crates/agmen-cli/src/cmd/{join.rs,inbox.rs,watch.rs}`, `crates/agmen-cli/src/{client.rs,watch_lock.rs}`, `crates/agmen-cli/src/mcp/{mod.rs,tools.rs}`, the new `crates/agmen-cli/src/cmd/chat.rs`; from M5, `crates/agmen-cli/src/cmd/account.rs` (onboarding.md:499 on design/onboarding); from M7c, `docs/mcp-setup.md` (onboarding.md:600, :648). L21 later rebases its one hunk on top.
- Merge point M7b.

### L23 desktop-listener (step 6 tail)

- Executes: desktop-live.md Task 10 as L2 P5 fixes it.
- Starts after M6 and M5b (L20 merged).
- Owns from M6 and M5b: `crates/agmen-local/src/console.rs` (from L20), `crates/agmend/src/console.rs` (from L20), `crates/agmend/src/listener/settings.rs` (from L21), `api.rs` for `settings_undo`, and agmend `lib.rs`, `Cargo.toml` and `tests/support/mod.rs` (from L21, section 8.6), `crates/agmen-desktop/src/backend/agmend.rs` (from L20).
- The settings change type is `SettingsChangeEntry`, because `ChangeEntry` is already a changed git file (`crates/agmen-local/src/console.rs:174` to 189). Ends with shots of the listener screens.

### L24 roles-e2e (step 7)

- Executes: roles-listener.md Task 7.
- Also, per D38, the swarm exit test `it_human_messages_swarm` from m0-m1 Task 16 (m0-m1.md:436) in `it_m1.rs`, after M7b brings L22's `agmen chat`.
- Owns: `tests/e2e/tests/it_roles_listener.rs` and `it_m1.rs`, and the harness: `tests/e2e/Cargo.toml`, `tests/e2e/src/**` and `tests/e2e/tests/machines/**`. No other lane touches `tests/e2e`. Task 7 needs two agmend instances (roles-listener.md:245), but the e2e crate has no agmend dependency or binary, and cargo shows a package's tests only that package's own binaries (`tests/e2e/Cargo.toml:10` to 12). L2 P4 says how the harness gets agmend; the new dependency edge needs Andy's yes (8.5).
- Runs beside L23. Merge point M-RL. Afterwards, rerun the shot pass for the screens L20 and L23 changed.

### L25 selfhost-cli (step 8)

- Executes: self-host.md Tasks 7 to 11.
- Owns: `crates/agmen-cli/src/paths.rs`, the new `cmd/host/**` and `cmd/group.rs`, the README "Host it yourself" section, ADR-0021. In turn (section 8.6): `crates/agmen-cli/src/client.rs` (self-host Task 9 adds `client::Group`, self-host.md:589 and :664), `cmd/account.rs` (Task 10, self-host.md:653) and `cmd/join.rs` (Task 9). New test functions in `crates/agmen-cli/tests/it_cli.rs` are additive.
- Task 7 needs D30. Task 11 needs Andy's machines (Tailscale with Funnel, a second machine, an ngrok token). Merge point M9a.

### L26 hosted-early (step 8)

- Executes, in two halves:
  - First half, from M8a: the `/statusz` part of Task 16 (hosted.md:736 on design/onboarding), and the client part of Task 9 (hosted.md:431 to 443): `skip_to` on the cursor store, the client `Page.pruned_through` field with `#[serde(default)]`, and `replay` calling `skip_to`, tested with `replay_skips_pruned_seqs` and `skip_to_never_lowers`. Merge point M-H0. It carries no part of Task 10, because Task 10's `flush` replaces the retained `state/roster` array with a head object (hosted.md:479, :485 to 486 on design/onboarding), and today's readers parse that topic as an array: the CLI's `resolve` (`crates/agmen-cli/src/joined.rs:169` to 177, then `resolve_handle` at `crates/agmen-client/src/state.rs:394` to 399, which returns `BadState` on an object) and agmend's `roster` (`crates/agmend/src/hub.rs:219` to 222). `it_handles_resolve_through_the_roster` (`crates/agmen-cli/tests/it_cli.rs:366`) and `it_mcp.rs:277` would fail.
  - Task 10 half, after M6 (its turn on `mcp/tools.rs`; M3a and M4 are in by then): all of hosted.md Task 10 in one PR. Move `Hub::roster` to the new `roster.rs`, publish the head in `presence.rs` `flush`, the route line, `roster` in `crates/agmen-client/src/hub.rs`, the agmend roster reader, `crates/agmen-cli/src/joined.rs` (`resolve`, `mentions`), its callers in `crates/agmen-cli/src/cmd/post.rs` and `crates/agmen-cli/src/mcp/tools.rs` (the `state` tool and the `resolve` call), its tests in `crates/agmen-cli/tests/it_cli.rs` and `it_mcp.rs`, and `mods/agmen-claude/skills/agmen/SKILL.md` (hosted.md:479, :493 to 494), so it runs the Plugin stage checks. Merge point M-H1.
- Owns, first half: `crates/agmen-hub/src/health.rs` (existing; adds `/statusz`, changes :19 to 23), `crates/agmen-hub/src/mqtt.rs` (`run_connection` sets the broker flag) and `Inner.broker_up` in `http.rs:54` to 65 in turn (section 8.6), `crates/agmen-hub/tests/it_health.rs`, `crates/agmen-client/src/{hub.rs,replay.rs,replay_tests.rs,cursor.rs}`. Task 10 half: `crates/agmen-hub/src/{presence.rs,state.rs}` after M3a, the new `roster.rs`, `crates/agmen-client/src/state.rs`, the roster reader at `crates/agmend/src/hub.rs:219` to 222 after M4, the protocol.md section 2 roster row after L13, and the files listed under the half.
- L27 owns the rest of Task 9: migration 0023, `retention.rs`, the hub `replay.rs` `Page`, `mqtt.rs:149` to 193 and the `it_` retention tests.
- Task 10 keeps agmend and the desktop roster working in the same PR (D29).

### L27 hosted-hub and L28 hosted-cli (step 8)

- L27 executes hosted.md Tasks 1 to 8, the hub part of Task 9 (L26's entry says which part is whose), and Task 11. Task 14 is skipped (ADR-0007:63, "Verdict: fail"). Its base is M8a plus M-H0: Task 11 consumes every hub route through Task 10 (hosted.md:520), and Task 9's hub half needs L26's client `skip_to`. Task 9 starts after M9a merges in, because it consumes self-host Task 7's `AGMEN_RETENTION_DAYS` compose line and `host up --retention-days` (hosted.md:434). It owns `crates/agmen-hub/src/billing/**`, `groups.rs`, `usage.rs`, `retention.rs`, migrations 0020 to 0023, hub `ingest.rs` in turn after L13 (Task 4's `accept`, hosted.md:219), and the NackReason additions in `crates/agmen-proto/src/reply.rs` and `schema/`; in turn after L19 and L26, hub `config.rs`, `main.rs`, `mqtt.rs`, the non-route parts of `http.rs` (`Inner.paddle`, hosted.md:268) and `tests/support/mod.rs` (`TestHub::start_configured`). New dependency hmac needs Andy's yes; Task 6 regenerates `THIRD_PARTY_NOTICES.md` by L5's rule. The hub `mqtts://` change for Task 15 is here. Task 2's `crates/agmen-cli/src/client.rs` hunk (`client::Group` gains `state` and `trial_ends_at`, hosted.md:138, :150) goes to L28, because `client::Group` comes from self-host Task 9 in L25. Task 4's hint hunks in `crates/agmen-cli/src/output.rs:76` and `crates/agmen-cli/src/joined.rs:333` to 345 (hosted.md:219) also go to L28, because L26's Task 10 half holds `joined.rs` until M-H1. Task 11 starts after M-H1 merges in, because its ROUTES lists Task 10's `GET /v1/projects/{p}/roster` (hosted.md:525).
- L28 executes hosted.md Tasks 12 and 13, plus Task 2's `client.rs` hunk and Task 4's hint hunks. It owns `crates/agmen-cli/src/cmd/init.rs`, `crates/agmen-cli/src/output.rs` and the group subcommands, and in turn `crates/agmen-cli/src/client.rs`, `cmd/account.rs` (hosted.md:602) and `joined.rs` (after M-H1). Task 13 Step 6 writes desktop Tasks H1 and H2 per D33; no lane builds them yet (section 12).
- Both wait on D26b. Real use waits on D30 and D32.

### L29 kube (steps 2 and 8, agmen-kubernetes repo)

- Executes: the first-deploy placeholder fill (`agmen-env/placeholders.yaml:14` to 24). Every value but `hub-image` can be filled after D10 and D11. `hub-image` needs a `build-<N>` tag from the Jenkins Image stage, which builds only on main (`Jenkinsfile:144` to 151), so it is the first main build after M-main. Then hosted.md Task 15 (branch B, EMQX Cloud, after M8b brings L27's `mqtts://` hub change, hosted.md:706, and after the first release tag, because its overlay sets `hub-image` to the newest released semver tag, hosted.md:710, which only onboarding Task 13's `release.yml` publishes, on version tags, onboarding.md:777) and the cluster half of Task 16 (after M-H0 brings `/statusz`). For Task 16 the subagent writes `agmen-env/ops/restore-test-job.yaml` and `ops/uptime` and stops: Steps 5 to 7 (apply the restore Job to the production namespace, `wrangler deploy` and `wrangler secret put ALERT_URL`, the scale-to-zero test; hosted.md:762 to 763) are Andy's.
- Worktree: `/Users/andyaldrich/Projects/agmen-kubernetes/.worktrees/agmen-hosted` on branch `agmen-hosted`.
- Owns the agmen-kubernetes repo. Starts after D10 and D11. A subagent never handles secret values.
- Gate: shellcheck and shfmt on `agmen-hosted/check.sh`, then `check.sh` itself, `kubectl kustomize` on each overlay, kubeconform after Andy's yes to install it (if he says no, the PR body says kubeconform did not run), and `node --test ops/uptime` (hosted.md:715, :717, :759 and :761 on design/onboarding).

### L30 spike-s6 (step 9, only if D36 says in)

- Executes: m0-m1.md Task 6 (m0-m1.md:143 to 155), with ADR-0022.
- Owns: the new `crates/agmend/src/local_transport.rs` and its test, then `socket.rs`, and the desktop's `backend/agmend.rs` connect seam in turn with L20. Its `api.rs` and `lib.rs` hunks land as a task on L10's branch (L10's entry), because L10 holds those files until M4.
- It must land its bind and connect seam before M4. If it cannot, L7 and L10 build the Unix-only shape and mark it with `shortcut:`, and L30 adapts after M4: its `api.rs` and `lib.rs` hunks land as a task on L17's branch, as they would on L10's before M4. Merge point M-S6 for its own files and ADR-0022.

## 5. Dependency graph and merge points

"A -> B" means B needs A merged, or, for a stacked lane, A's reviewed tip. "L2 Pn -> B" means B needs the plan/refresh sha at which piece Pn passed review (L2's entry).

```
M1 (L1: desktop/live into mvp/impl)
 |-> L2 merge (M-docs), L4 (with D26a), L5 merge of mvp/impl, L6, L7, L8, L9, L10, L11 merge, L3 checks, L30
L2 P1 -> L11 T1
L2 P2 -> L7, L10
L2 P3 (Task 2) -> L6; (Tasks 1 and 3) -> L12, L13; (Task 4) -> L16
L2 P4 (Task 5, Task 6 parts) -> L8, L9, L17; (Task 6 settings) -> L21; (Task 7) -> L24
L2 P5 -> L20, L23
L2 P6 (Task 13 pointer, Task 17 Step 5, Task 6 ADR number) -> L3 deploy check, L30 ADR number; (swarm chat parts, D38) -> L22 chat, L24 swarm test
L2 P7 -> L17 login task
L6 tip -> L12, L13                       M2 = L6 merged
L12 + L13 -> M3a
M3a -> L10 T3 (identity_mint)
L7 + L10 T1 to T5 -> M4 (agmend core)
D26a -> L4 -> D26b -> M-plans
M-plans + M1 -> L14, L15
M-plans + M1 -> L5 Task 11 -> M-notices
M3a + L15 merged (M7c) -> L16 -> M3 (roles foundation)
M4 + M3a -> L17 (merges L8 first); M3 -> L17 CLI task; M7a -> L17 login task
L17 -> M5 (launcher) -> L20 (desktop Task 9) -> M5b
M5 + M-S7 (ADR-0009) -> L21 (merges L9 first); M7b -> L21 last task (mcp/mod.rs hunk); L21 -> M6 (listener)
M6 + M5b -> L23 (desktop Task 10); M6 + M7b -> L24 (exit tests); L23 + L24 -> M-RL
L14 -> M7a -> L18 -> L19 (stacked; also needs M-notices) -> M8a
M3 + M5 + M7a + M7c -> L22 -> M7b (onboarding code)
M8a -> L26 first half -> M-H0
M6 + M-H0 -> L26 Task 10 half -> M-H1
M8a + M-H0 -> L27 (Task 9 after M9a; Task 11 after M-H1) -> M8b
M7b + M8a -> L25 -> M9a (self-host done)
M8b + M9a + M-H1 -> L28
M3 -> L5 Task 13; M7b -> L5 Task 14; Tasks 13 and 14 -> M-dist
D37's Apple account -> L5 Task 13b -> M-app (gates neither L5 Task 15 nor the first release tag)
D10 + D11 -> L29 (every placeholder but hub-image); M-main + D10 -> L29 hub-image; M8b + release tag -> L29 Task 15; M-H0 -> L29 Task 16 cluster half
M-checks -> M-main (MR !1) -> dogfood tag -> dogfood round
All docs lanes merged + M-dist + M-H1 -> L5 Task 15 (public tree) -> Andy's publish -> release tag
```

| Merge point | What lands | Unblocks |
|---|---|---|
| M1 | desktop/live (PR #1, MR !2) | every lane that touches agmend, agmen-local, the desktop or the hub |
| M-docs | plan/refresh, after P7 | the boxes lanes tick in the four plan files; lanes already read their pieces from reviewed shas (L2's entry) |
| M-plans | design/onboarding, after D26b | L5 Tasks 11, 13, 14 and 15, L14, L15 |
| M-notices | release/oss-prep Task 11 (LICENSE, notices, about.toml, about.hbs, the script) | L19, and every lane that regenerates notices |
| M-dist | release/oss-prep Tasks 13 and 14 | L5 Task 15 |
| M-app | release/oss-prep Task 13b (after D37's Apple account) | the desktop app's release, after the first release tag |
| M-checks | mvp/checks docs and ADRs | M-main |
| M-main | mvp/impl into main (MR !1) | the Task 16 image, the Task 17 install |
| M2 | roles/proto | a merged base for the L12 and L13 PRs |
| M3a | roles/crate and roles/hub | L10 T3, L16, L17, L18's identities.rs edit, L26 |
| M3 | roles/cli | L17's CLI task, L22, L5 Task 13 |
| M4 | agmend/service and agmend/core (with L30's seam if D36 says in) | L17, L26's agmend reader |
| M-S7 | spike S7 and ADR-0009 | L21 |
| M-S6 | spike/s6-transport (only if D36 says in) | Windows work in section 12 |
| M5 | agmend/launch (with L8 and the login task) | L20, L21, L22's account.rs turn |
| M5b | desktop/agents | L23 |
| M6 | listener/runtime (with L9) | L23, L24, L26's Task 10 half |
| M-RL | desktop/listener and roles/e2e | completes roles-listener.md and desktop-live.md |
| M7a, M7c, M7b | onb/hub, onb/setup, onb/join | self-host lanes, L17's login task, L16, L22, L21's last task, L5 Task 14 |
| M8a | selfhost/hub, then selfhost/infra | L25, L26, L27 |
| M-H0 | hosted/early, first half (Task 9's client part, `/statusz`) | L27, L29 Task 16 |
| M-H1 | hosted/early, Task 10 half | L27 Task 11, L28, the release |
| M8b | hosted/hub | L28, L29 Task 15 |
| M9a | selfhost/cli | L27 Task 9, L28 |

## 6. Waves

A wave starts when its entry merge points land. Lanes in one wave run at the same time, within the build cap of 4 (section 8.8).

- **Wave 0, now, before M1.** L1 (with Andy for T20-0 and the shots). L2 pieces as their decisions land. L3 tasks 1 to 3 on mvp/impl. L5 Task 12. L11 T1 and T2 after L2 P1 (no model spend). Andy works on the Wave 0 decisions, the D31 backups, the D11 push, the D30 email, the D10 infrastructure and the D26a read. Only L1 and L11 build, and L11 pauses during L1's `list_measures_10k_rows` rerun and T20-1a.
- **Wave 1, after M1.** L3 task 4 on, L4 (after D26a), L5's merge of mvp/impl and its plan-hunk reversal, L6, L7, L8, L9, L10 T1, T4, T2, T5, L11 T3 (after D20), L30 if D36 says in. Each code lane waits for its L2 piece (section 5). Under the cap, L6, L10, L7 and L8 build first.
- **Wave 2, after the L6 tip and M-plans.** L12, L13, L14, L15, L5 Task 11 (then M-notices), while L8, L9, L10 and L11 finish. L13 and L14 are the hot pair; L14 merges second in the hub queue.
- **Wave 3, after M3a and M7a.** L10 T3 then M4, L16 (after M7c), L17 (its login task after M7a), L18, L19 (stacked on L18, after M-notices), L29 if D10 is done. M-S7 lands here.
- **Wave 4, after M3, M5.** L20, L21, L22, L26's first half (after M8a), L5 Task 13. L22 takes `mcp/mod.rs` first; L21 rebases its one hunk in its last task, after M7b.
- **Wave 5, after M6, M8a, M7b.** L23 (after M5b), L24, L25, L26's Task 10 half, L27 (after M-H0), L5 Task 14.
- **Wave 6.** L28, the L29 hosted overlay, L5 Task 15 and the release, the dogfood retro when the round ends, and the step 9 plans (section 12).

## 7. Critical path and priority rule

The critical path is M1 -> L6 -> L12 and L13 -> M3a -> L10 T3 -> M4 -> L17 -> M5 -> L21 (with M-S7) -> M6 -> L23 and L24. Three chains feed it, so their lanes are critical too: D26a -> L4 -> D26b (M-plans) -> L15 -> M7c -> L16 -> M3 -> L17's CLI task; M-plans -> L14 -> M7a -> L17's login task; and M7a and M7c -> L22 -> M7b -> L21's last task (its `mcp/mod.rs` hunk) and L24's swarm test. Andy's D26a and D26b reviews sit on the path. The narrow point is the agmend dispatch match at `crates/agmend/src/api.rs:97` to 109. It passes from L10 to L17 to L21 to L23 at M4, M5 and M6, and no two open lanes edit it at once. Desktop Tasks 9 and 10 sit at the end of this path, so PR #1 lands without them, as its body says.

Priority rule:

1. A critical lane (marked in section 3) gets a task reviewer as soon as its implementer finishes. Off-path lanes wait for reviewer capacity.
2. Whenever a critical lane has a ready task, at least 2 of the 4 build slots go to critical lanes.
3. Andy's review queue: the L1 PR first, then D26a and D26b, then the roles, agmend and onboarding merge points on the path (M2, M3a, M7c, M3, M4, M7a, M5, M7b, M6), then the S7 ADR, then everything else in batches.

## 8. Shared-resource rules

### 8.1 Gates

- **Cargo gate** (desktop.md:27 to 34), green before every commit: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`; `cargo nextest run --workspace --locked`; `cargo test --doc --workspace --locked`; `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`; `cargo deny check`.
- **Integration gate** for any lane that touches the hub, agmen-client, agmen-cli, agmend's hub side or `tests/e2e`: `cargo nextest run --workspace --locked --profile it`, then `--profile it-restart`, with the dev stack up and under the it lock (8.7). The default filter excludes `it_*` tests (`.config/nextest.toml:7`), so plain nextest never runs them.
- **Narrow checks** after each edit: `-p <crate>` and a filter. The plans' `-E 'test(roles_proto)'`, `test(it_roles)` and `test(role_cli)` match no test names. Use `-E 'binary(<name>)'` only in a step after the test file exists, because nextest rejects a `binary()` filter that matches no binary (`.config/nextest.toml:2` to 4). Before that, filter on a test name.
- Lanes that touch `mods/agmen-claude/**` run the Plugin stage checks (Jenkinsfile:124 to 142). Shell scripts pass shellcheck and shfmt.
- **Docs gate:** `rg -n '[\x{2013}\x{2014}]'` on the changed files prints nothing, and every cited `file:line` resolves at the lane's base.
- Each lane's final review reruns the cargo and integration gates on the branch tip, and the PR body records the sha and counts.

### 8.2 ADR numbers

On disk: 0000 to 0007 and 0010 to 0013. Desktop keeps 0010 to 0019 (desktop.md:17). Per D25:

| Number | ADR | Lane | Plan text to fix |
|---|---|---|---|
| 0008 | first deploy | L3 | none (m0-m1.md:456 already says 0008-first-deploy) |
| 0009 | listener runtime | L11, then L21 | roles-listener.md:237 and m0-m1.md:163, 186 say 0007, which RMQTT holds |
| 0020 | dogfood retro | L3 | none |
| 0021 | self-host | L25 | self-host.md:713 says "next free" |
| 0022 | local API transport (S6) | L30 | m0-m1.md:146 says 0006 |
| 0023 and up | later spikes (S3, S9, S10) and step 9 plans | assigned at dispatch | |

Only L3 edits ADR-0002, ADR-0005 and ADR-0006.

### 8.3 Migration numbers and the hub merge queue

Today: 0001 to 0005, each an `.up.sql` and `.down.sql` pair, embedded by `sqlx::migrate!()` (`crates/agmen-hub/src/lib.rs:70`). Per D24:

| Number | Migration | Lane | Plan text to fix |
|---|---|---|---|
| 0006 | roles | L13 | roles-listener.md:108 says `0005_roles.sql`, one file |
| 0007 | github_device | L14 | onboarding.md:84 says 0006 |
| 0008 | project_repo_url | L14 | onboarding.md:282 says 0007 |
| 0009 | tenant_roles | L18 | self-host.md:36 computes `<N>` at task start |
| 0010 | hub_group | L18 | self-host.md:213 |
| 0011 | devices | only if D17 brings devices in | m0-m1.md:241 to 242 |
| 0020 to 0023 | hosted groups, usage, paddle events, retention | L27 | hosted.md:137, 218, 306, 430 already pin them |

Rules:

- Every migration is an up and down pair. No lane computes its own number.
- Hub lanes merge through one queue that the coordinator sequences in migration-number order. After each hub merge, every queued hub lane merges mvp/impl in and reruns its gates.
- The coordinator may renumber a migration before it merges, so numbers rise in merge order. It never renumbers one that has run on a shared or deployed database.
- Out-of-order merges are forbidden until L13's sqlx check says they are safe. The dev stack database can be reset.

### 8.4 docs/protocol.md and docs/design.md

- Sections 1, 3, 4 and the section 5.4 NACK rows: L13. After M3a, L27 appends `read_only` and `quota_exceeded`.
- The section 2 roster row (line 44): L13 adds role, then L26 adds the head.
- The section 6 HTTP table (from line 174): append-only. Each lane edits only its own routes, including routes the onboarding and self-host plans forgot to list (onboarding.md:99 to 101, self-host.md:228).
- Nobody rewraps or reorders lines they do not own.
- `docs/design.md`: L5 is the sole writer. Other lanes put what they need in their plan's "Notes for Andy".

### 8.5 Cargo.toml, Cargo.lock, deny.toml

- The root `members` list is append-only, one line per new crate (L11, and L12 if D13 picks a crate). `[workspace.package]` is L5's.
- Add dependencies only with `cargo add`. Never run a bare `cargo update`. Use locked versions (Tech Stack line).
- A new dependency edge needs Andy's yes: aws-lc-rs and base64 (L19), hmac (L27), rusqlite 0.39.0 (bundled) into agmend (L10, D17), cargo-about and dist (L5), the agmen-cli toml_edit edge (L15, already locked at 0.25.17), and whatever L2 P4 picks for the e2e harness to get agmend (L24).
- On a lock conflict: take the target branch's lock, build once so cargo adds the lane's entries, commit, rerun the `--locked` gate.
- deny.toml: only L5, only comments. Any new exception needs Andy (desktop.md:17).

### 8.6 Hotspot files, turn order and agmen-local types

Only the lane holding the turn edits these files. Turns pass at merge points.

| File | Turn order |
|---|---|
| `crates/agmend/src/api.rs` (dispatch at :97 to 109), `lib.rs`, `Cargo.toml`, `tests/support/mod.rs` | L10 (with L30's seam if D36 says in), then L17 at M4 (including the login task), then L21 at M5, then L23 at M6 |
| `crates/agmend/src/main.rs` | L7 |
| `crates/agmend/src/git.rs` | L8, then L17 |
| `crates/agmen-local/src/console.rs` | L13 (`RosterEntry.role`, M3a), then L20, then L23 at M5b |
| `crates/agmend/src/console.rs` | L20, then L23 at M5b |
| `crates/agmen-desktop/src/backend/agmend.rs` | L30 (its connect seam, only if D36 says in, M-S6), then L20, then L23 at M5b |
| `crates/agmen-local/src/message.rs` | L10 |
| `crates/agmen-local/src/roles.rs` | L12 |
| `crates/agmen-cli/src/cmd/join.rs` | L15 (Task 4 hunk), then L16, then L22, then L25 (self-host Task 9) |
| `crates/agmen-cli/src/mcp/{mod.rs,tools.rs}` | L16, then L22 (M7b), then L21 (one hunk, M6), then L26's Task 10 half (hosted Task 10, M-H1) |
| `crates/agmen-cli/src/cmd/account.rs` | L14 (M7a), then L17's login task (M5), then L22 (M7b), then L25 (M9a), then L28 |
| `crates/agmen-cli/src/client.rs` | L22 (M7b), then L25 (M9a), then L28 (hosted Task 2's hunk) |
| `crates/agmen-cli/src/joined.rs` | L26's Task 10 half (M-H1), then L28 (hosted Task 4's hints) |
| `crates/agmen-cli/tests/support/mod.rs` | L14 holds `Ctx` and `run` (onboarding Task 1's FakeGithub, M7a) while L15 holds `Home` and `apply_env` (onboarding Task 4's PATH rule, M7c). Neither edits the other's part, and the second to merge keeps both sides |
| `.config/nextest.toml` | L11 (T0, M-S7); other lanes only read it |
| `crates/agmen-cli/Cargo.toml` | L15 (Task 5's toml_edit edge, M7c), then L16 (M3), then L5 Task 13 (the dist table) |
| `docs/mcp-setup.md` | L15 (M7c), then L22 (M7b), then L5 Task 14 |
| hub `config.rs`, `main.rs`, the non-route parts of `http.rs` (`HubError`, `Inner`, startup) and the `Config` and `TestHub` parts of `tests/support/mod.rs` | L1 (PRE-1 c), then L14 (M7a), then L18, then L19 (M8a), then L26 for `Inner.broker_up` only (M-H0), then L27 |
| hub `mqtt.rs` | L26 (M-H0), then L27 |
| hub `identities.rs` | L13, then L18 |
| hub `ingest.rs` | L13, then L27 |
| hub `presence.rs` | L13, then L26 |
| `crates/agmen-proto/src/reply.rs`, `lib.rs` and `schema/` | L6, then one export line from L14, then L27 |

Additive-only files, which two open lanes may both touch; the second to merge keeps both sides:

- private `mod` lines in `crates/agmend/src/lib.rs`, one per module, alphabetical. Never `pub mod`: that changes agmend's lib API and brings `missing_debug_implementations` (Cargo.toml:28) onto new public types.
- the Command enum arms in `crates/agmen-cli/src/cmd/mod.rs`
- private `mod` lines in `crates/agmen-cli/src/lib.rs` (L14's `mod open`, L15's `mod setup`)
- route lines in hub `http.rs`, mod lines in hub `lib.rs`, new functions in hub `test_hooks.rs` and `tests/support/mod.rs`
- new test functions in `crates/agmen-cli/tests/it_cli.rs` and `it_mcp.rs`
- the `[package.metadata.dist]` table L5 Task 13 adds to `crates/agmend/Cargo.toml`, while L17, L21 or L23 holds that file
- the root Cargo.toml members list

agmen-local console types: every change to `ConsoleSnapshot`, `ConsoleEvent`, `LocalData`, `RosterEntry` and `ErrorCode` is additive, with `#[serde(default)]` (as `console.rs:52` to 74 does) or behind `#[serde(other)]`. Renaming or removing a field needs a migration note and Andy's yes. L13 adds `RosterEntry.role`. `schema/` is generated: on a conflict, regenerate it; never hand-merge it.

### 8.7 The dev stack

One compose stack serves every lane on fixed ports, and the `restart` tests restart the shared EMQX. So only one lane at a time runs `--profile it` or `it-restart`. `flock` is not installed here, so the lock is a directory: `mkdir /Users/andyaldrich/Projects/agmen/.superpowers/it.lock` succeeds for one holder, who removes it when done. A controller that finds it held retries every 60 s for at most 60 minutes, then reports to the coordinator. Unit gates run in parallel freely.

### 8.8 Build load

`CARGO_TARGET_DIR` is unset, so each worktree builds into its own `target/`. The Mac has 12 CPUs. At most 4 lanes build at once; that cap is a judgment, not a measurement. In one worktree, shot.sh and the gate never run together. During L1's `list_measures_10k_rows` rerun and shot.sh run, no other worktree builds (L1's entry).

### 8.9 Ledgers

- Each lane keeps `<worktree>/.superpowers/sdd/<lane>/` (progress.md, common-brief.md, task briefs and reports, review diffs), as desktop-live did. The lane controller is its single writer.
- The coordinator keeps `/Users/andyaldrich/Projects/agmen/.superpowers/sdd/program/progress.md`: lane states, merge points, the registries, hotspot turns, the sqlx check result.
- Before any worktree is removed, its `.superpowers` is copied to the main checkout. `.worktrees/desktop` holds 19 such files, and `.worktrees/desktop-live` holds 44.

## 9. Execution method per lane

Each code lane runs superpowers:subagent-driven-development:

1. The lane controller writes `common-brief.md`: this plan's absolute path (Global Constraints), base sha, owned files, files it must not touch, gates, rulings paths, registries, hotspot turns. It writes one `task-N-brief.md` per task from the plan text: for a plan L2 fixes, the text at the plan/refresh sha the program ledger records (L2's entry); otherwise the text at the lane's base.
2. A fresh implementer subagent takes each task test first (superpowers:test-driven-development). A bug fix gets a test that fails before the fix.
3. A task reviewer subagent checks spec compliance, then code quality. A fix subagent handles findings; the reviewer rechecks. The diff is saved as `review-<from>..<to>.diff`.
4. The controller commits only on a green gate, with one short commit line and no trailers.
5. A final branch review covers the lane's whole diff, traced by execution path. Then the full gates run on the tip, and the lane opens its PR and MR (section 11).
6. A test that asserts replaced behavior without Andy's ruling stops the lane (Global Constraints).

Docs lanes (L2, L4, L3's docs tasks) use one writer subagent per plan and one reviewer that rereads every cited line. L1's comparison uses 5 read-only subagents and one writer. L11 is a spike: one implementer, one reviewer, and the ADR is the deliverable.

The coordinator dispatches lanes, checks owned-file lists are disjoint, holds the registries and the hub queue, and hands hotspot turns at merge points. After each merge point, it tells each open lane to merge the new mvp/impl in at its next task boundary.

## 10. Plans to write or fix before their lanes start

- **New `docs/plans/agmend.md` (L2 P2, for L10).** m0-m1 Task 13 predates the agmend crate, names keyring 4.2.0 and keychain accounts TokenStore does not use, has no task split, and conflicts with roles-listener Task 5's file list. The new plan has tasks with tests for ErrorCode, caller binding (with how the token travels, since `agmen_local::Request` has only v, id, method, params and as, `message.rs:8` to 23), the SQLite state, the identity-scoped methods, and identity_mint and identity_list on the post-M3a body (the hub has no GET `/v1/identities`, `http.rs:124` to 144). P7 adds the `login_start` and `login_status` task after onboarding Task 1 (D19), for L17. It names the tests it replaces and states what moved to L7 and what is deferred (devices, S6 unless D36).
- **roles-listener.md (L2 P3, P4).**
  - Task 1: D13's home; `version`, `worktree at`, `tools allow deny`; whether Catalog holds base (base.xml has no kind; `roles_tests.rs:77` to 80); the claude_tools mapping; the Layers::load protections.
  - Task 2: EditScope and the verb table in proto; StaleRequest moved out; the missing tests.
  - Task 3: migration 0006 as a pair; delegated_by; opening REVIEW and PLAN; the cap intersection with the listener bans.
  - Task 4: what happens to `--kind`; `IdentityFile` lives in agmen-client; agmen-cli has no assert_cmd or fake hub, so tests use `CARGO_BIN_EXE_agmen` and `it_` names; the tool list.
  - Task 5: "Modify", not "Create", for agmend files that exist (roles-listener.md:164); the git runner; service install moved to L7; the worker's identity.json; tests as sibling `*_tests.rs` files, since agmend modules are private.
  - Task 5 and Task 6 feeder parts: the exact unit test names, as sibling `*_tests.rs` files, for L8 T1 to T3 and L9 T1 to T3, and which of Task 6's integration tests stay with L21 (at least `delegate_over_cap_is_pending` and `coordinator_cannot_be_removed`, which run L21's CLI).
  - Task 6: ADR-0009; pending from agmend.md; settings.toml sharing (D22, written last); the fixture path `spikes/s7-listener/fixtures/triage.jsonl` in place of `spikes/s7-dispatcher/` (roles-listener.md:202).
  - Task 7: how the e2e harness gets an agmend binary for two instances (roles-listener.md:245). The e2e crate depends only on agmen-cli and re-exposes it as the `agmen-e2e` bin because cargo shows a package's tests only its own binaries (`tests/e2e/Cargo.toml:10` to 24). Either a second bin in tests/e2e that runs agmend's library (`crates/agmend/src/main.rs:8` uses `agmend::{Agmend, Config}`), or agmend in-process; the choice is a new dependency edge for Andy (8.5).
  - All tasks: rusqlite 0.39.0, and the filter rule in 8.1.
- **m0-m1.md (L2 P1, P6).** Task 7 for the listener shape and ADR-0009. Task 17 Step 5, because no code reads `AGMEN_IT_HUB` and the e2e harness always starts its own hub (`tests/e2e/tests/machines/mod.rs:273` to 290). Task 6's ADR becomes 0022. Per D38, the swarm chat parts of Tasks 14 to 16 re-anchored to today's CLI and MCP code, for L22 and L24. Start from release/oss-prep's cc5c2e4 hunks in this file (L5's entry), keeping the wording that still holds.
- **desktop-live.md (L2 P5).** Task 9: SpawnWorker's inputs per D23, and its dependency on L17. Task 10: `SettingsChangeEntry`, and the handover of `listener/settings.rs` from L21.
- **mvp.md (L3 task 1).** Task 16 for agmen's own infrastructure, and the anchors.
- **onboarding.md, self-host.md, hosted.md, broker-spike.md, the spec (L4).**
  - Re-anchor every cited line by symbol; write in the numbers from 8.2 and 8.3.
  - Drop self-host Task 5's `spikes/s11-rmqtt/tests/it_s11_broker.rs` step (self-host.md:276).
  - Hosted Task 11's ROUTES: add the three desktop/live routes (`http.rs:142` to 144) and drop the `broker/shim.rs` skip.
  - Hosted Task 10: D29's choice, and add `crates/agmend/src/hub.rs:219` to 222 to its files.
  - Mark hosted Task 14 skipped. Point hosted Task 13 Step 6 at desktop.md on mvp/impl.
  - Onboarding Task 1: note that agmend's `login_start` follows it, in L17. Onboarding Task 11: start from release/oss-prep. Onboarding Task 5: toml_edit is already locked.
  - Onboarding Task 13 per D37: agmend opts into dist beside agmen-cli, so each archive holds `agmen` and `agmend`, and Task 14's installers run `agmend service install` (L7) after install. A new Task 13b holds the macOS app bundle, `codesign --options runtime`, `xcrun notarytool submit --wait` and `xcrun stapler staple` (ADR-0010:34 to 38), gated on D37's Apple account. State that agmend and the desktop release together.
  - Hosted Task 2: its `crates/agmen-cli/src/client.rs` hunk moves to L28. Hosted Task 4: its `output.rs` and `joined.rs` hint hunks move to L28. Hosted Task 9: split as L26's entry says (client `skip_to` and `Page` field in L26; migration, retention and hub `Page` in L27), and note it runs after self-host Task 7. Hosted Task 10: lands whole in L26's Task 10 half after M6, because its hub part alone breaks today's CLI and agmend roster readers (L26's entry); hosted Task 11 runs after it. Hosted Task 16: Steps 5 and 6 become Andy actions, like Step 7.
  - Add protocol.md section 6 rows for the new routes.

## 11. Merging lanes back into mvp/impl

1. Before opening a request, the lane merges current mvp/impl into its branch (never rebases a branch with reviewed shas), reruns the gates, and records the sha and counts.
2. The lane pushes its branch to `origin` and `github`. Pushing is outward-facing, so the coordinator asks Andy before every push (Global Constraints), unless Andy grants a standing yes for lane branches.
3. It opens a GitHub PR on AndyAld/agmen and a GitLab MR on groupdrive/agmen into mvp/impl, with one body: tasks done with plan task numbers; gate sha and counts; tests that replaced MVP behavior, with Andy's ruling; migration and ADR numbers used; `shortcut:` comments added (an `rg 'shortcut:'` diff); known follow-ups.
4. Andy merges on one forge (D3), pushes the result to the other remote, and closes the twin. The coordinator checks that `git ls-remote origin mvp/impl` and `git ls-remote github mvp/impl` print the same sha.
5. Feeder branches L8 and L9 open no requests; they arrive inside L17's and L21's. Stacked lanes (L12 and L13 on L6) open requests only after their base lands, so each shows only its own commits.
6. Two merges of mvp/impl into main are planned: M-main for dogfood (MR !1) and the release. main stays at the dogfood tag while the round runs.
7. After a branch lands and its `.superpowers` is copied, the worktree is removed and the branch deleted with `git branch -d`, never `-D` without Andy.

## 12. Step 9: what each later item needs (no builds scheduled)

- **ACP launching for the other agents** (design.md:381, :649). Needs spike S3 (launch, events, a refused tool call, cost or tokens, a message mid-turn, take-over by session id) with ADR 0023 or later, a fresh check of the pinned versions (design.md:385 to 389), and a plan that extends L17's launcher. Decision: which agents first, and token caps for agents with no cost report (design.md:375).
- **Planning rooms live** (design.md 6.14, :617). The desktop fixture UI exists (desktop.md Task 19). Needs a plan for PLAN at the hub, share approval, kick-off per machine and spec text at a commit. Decision: whether L13 opens PLAN now (part of D15) or this plan owns it.
- **GitHub App, read-only** (design.md 6.15, :628, :270 to 272). Needs a plan, an app under the agmen-dev org, a webhook route, and the merged-at-tip rule that allows prune's `-D` (roles-listener.md:24). Decision: permissions, and separate app ids for hosted and self-host.
- **Terminal UI** (design.md:646, :410). Needs a plan for ratatui in `agmen` over the agmend API as it stands at M6, and spike S10 for the PTY crate. Decision: whether it is still wanted now that the desktop exists.
- **Web console** (design.md:648, :251 to 252, :406). Needs a plan for `apps/web` with versions picked then (design.md:321), served by the hub, on the ts-rs types in `schema/ts`. Decision: its order against the hosted launch, since outside teams join through it (design.md:32).
- **Windows and Linux desktop.** Needs S8 rerun on both OSes (ADR-0010 covers macOS), S6 (L30 or later), Windows hardware, and signing accounts (design.md:331). Decision: in or out of the beta.
- **Devices** (m0-m1 Tasks 9 and 13). mvp.md:154 dropped them, and nothing consumes a device id. Decision: keep deferred, or schedule with migration 0011.
- **Push approval** (design.md:645, M2 exit "a PUSH_REQUEST reaches the owner and PUSH_OK reaches the agent"). The hub NACKs every PUSH_REQUEST with `NotRequired` today (`crates/agmen-hub/src/ingest.rs:393` to 397), and mvp.md:293 defers it. Needs a plan for hub routing of PUSH_REQUEST and PUSH_OK, the `push_approval` setting, and the mod's prompt. Decision: whether it ships before the beta.
- **Mod board pane, band and status line** (M3; mvp.md:293 defers all three, and m0-m1.md:480 defers the full mod). `mods/agmen-claude` holds only hooks, scripts and skills. Needs a plan on the mod API that Claude Code 2.1.311 offers. Decision: which of the three comes first.
- **Spike S9 and shared packs** (M3 and M5, m0-m1.md:480). Needs the S9 spike with ADR 0023 or later, then packs for Claude Code, OpenCode and Codex. Decision: whether packs wait for ACP.
- **Live take-over in the desktop** (desktop-live.md:263 puts take-over and switching out of plan). Needs spike S10 for the PTY crate and agmend pause and resume methods in L23's turn on `api.rs` or later. Decision: take-over before or after the terminal UI.
- **The rest of the M5 launcher** (design.md:649): switching an agent mid-task with a handoff brief, re-adoption after an agmend restart, preflight against the reservation, and hour and token caps (roles-listener.md:270 excludes the caps). Needs a plan that extends L17's launcher and the ACP work above. Decision: order against ACP.
- **The rest of M6 delivery** (design.md:650): the desktop updater, deep links, backups beyond hosted Task 16's restore test, and monitoring. Needs a plan after D37's release shape. Decision: which are beta blockers.
- **Desktop Tasks H1 and H2** (hosted.md:636 to 642 on design/onboarding): the hosted or self-host first-run choice, and group seats, members and billing. Needs a desktop lane after M8b (hosted routes) and M5 (agmend `login_start`), in the order D33 sets. Decision: whether hosted launches before they exist, since without them a desktop user cannot create or manage a group.
- **The coordinator-inbox spec** (D39). Needs Andy's rulings on its section 8, then its adopted parts attached to the lanes that own the files (protocol.md, the mod's interrupt filter, the inbox cursors).

## 13. Not checked for this plan

- The remote state of PR #1 and MR !2 beyond the research snapshot.
- Whether Jenkins has `AGMEN_REGISTRY` set, and whether agmen's cluster exists.
- How sqlx orders out-of-sequence migrations (L13's first task).
- Whether claude 2.1.311 accepts the flags at roles-listener.md:172 and :209 (L17's first check).
- Whether `git worktree remove` without `--force` deletes ignored files.
- The contents of the EMQ email draft.
- Whether dist 0.33.1 packs `agmen` and `agmend` from two packages into one archive per target, as D37's recommendation assumes.
- The lane count, the build cap of 4 and the review priority split are judgments, not measurements.
