# agmen MVP plan: Claude Code plugin on EMQX

Status: written 2026-10-25. Supersedes the ordering in [m0-m1.md](m0-m1.md); tasks below point into that plan where its detail still holds.

**Goal:** Claude Code sessions on several machines coordinate one effort through a Claude Code plugin: typed posts and DMs with confirmed delivery, wake-ups when an idle session gets an assignment or DM, and atomic claims with a lane edit guard. Senders are authenticated by the broker and stamped by the hub.

**Scope decisions (Adam, 2026-10-25):**

- Broker is **EMQX 5.9+ or 6.x, single node**, under its BSL 1.1 community terms. That is fine while only our own team uses it. Before outside teams join (M6) we need a commercial license or a different broker; see design 9.
- MVP shape is **plugin plus a thin hub**. Keep the hub for authenticated senders, numbering, ACK and NACK, replay, and claims. Drop the desktop app, the launcher and budgets, and agmend as an OS service.
- MVP features: **feed, post and DMs; wake-ups; claims and lanes.** Out: board pane, band, status line, push approval (the default `off` already lets agents push), LEND, PROPOSAL and DECISION, RULING, spend tracking.

**Architecture:**

```
 Claude Code session
   ├─ agmen plugin mod (hooks): spawns `agmen watch --json`, wake-ups, context rows, lane guard, /agmen
   └─ plugin MCP server: `agmen mcp` (stdio), owns the outbox and publishes
                │ MQTT 5 over TLS (WSS on 443), JWT password
                ▼
   EMQX (single node, JWT authn with per-identity `acl` claim, authz no_match = deny)
                ▲ shared subscription $share/hub/agmen/v1/+/+/up/+
                ▼
   agmen-hub (axum + Postgres): login, identities, JWKS, ingest, numbering, sweeper,
                                replay, DMs, lanes, items, claims
```

- No local daemon. Each joined worktree has `.agmen/identity.json` (`{tenant, project, identity_id, handle, owner_id, hub_url, broker_url}`, gitignored; `broker_url` per ruling R57). `agmen mcp` and `agmen watch` both resolve the identity from the working directory and are inert outside a joined worktree.
- `agmen mcp` holds one MQTT connection (client id `<identity_id>-mcp`) for publishing and ACKs. `agmen watch` holds another (`<identity_id>-watch`) for `feed` and `inbox/`. They share one SQLite file per identity under `$XDG_STATE_HOME/agmen/` (macOS: `~/Library/Application Support/agmen/`) in WAL mode: the outbox belongs to `mcp`, the cursors to `watch`.
- Tokens: the human refresh token and each identity's token live in the OS keychain (keyring, service `agmen`). A person's own agents are that person's delegates (design 6.3), so this guards against mistakes, not against the person's own processes.

## Global constraints

Same as [m0-m1.md Global Constraints](m0-m1.md#global-constraints), with these changes:

- The dev stack is EMQX, Postgres and Toxiproxy (no Mosquitto). Integration tests read `AGMEN_IT_MQTT`, `AGMEN_IT_EMQX_API`, `AGMEN_IT_PG` and `AGMEN_IT_HUB`.
- Workspace members: `agmen-proto`, `agmen-client`, `agmen-cli` (bin `agmen`), `agmen-hub`. No `agmend` crate in the MVP.
- EMQX image: pin the newest `emqx/emqx` 5.9+ or 6.x patch tag at task time. The Docker Official `emqx` image stopped at 5.8.x after the license change, so don't use it. Record the tag in ADR-0001.

## Review focus

1. **ACL fails open.** A JWT without an `acl` claim, or a topic that no rule matches, must be refused. Expected: authorization `no_match = deny`, no other authorizers, and every minted token ends with a deny-all rule. Test: Task 2 `it_no_acl_claim_is_denied`.
2. **Revocation while connected.** A revoked identity with a live connection. Expected: the hub kicks both client ids through the EMQX REST API, refuses to refresh the token, and NACKs anything still queued with `identity_revoked`. Test: Task 7 `it_revoke_kicks_and_refuses_refresh`.
3. **Wake-up loops.** Two agents DMing each other while both are idle. Expected: wake-ups are rate limited per session (start at 1 per 60 s, coalescing what arrives in the window), and a wake-up never fires for the session's own messages. Test: Task 14 `wakeup_rate_limited`.
4. **Lane guard error path.** A guard that throws, or `agmen check-path` that times out. Expected: the tool call is denied with a reason (fail closed). Test: Task 14 `guard_denies_on_error`.
5. **Two claims race.** Expected: exactly one GRANT (design M2 exit check). Test: Task 10 `it_twenty_claims_one_grant`.
6. Items 1 to 5 of [m0-m1.md Review Focus](m0-m1.md#review-focus) still apply (hub clock, outbox after revocation, duplicate deliveries, session expiry, long backlog).

---

## Phase A: scaffold and spikes

### Task 1: Workspace scaffold, dev stack, CI gate

As [m0-m1 Task 1](m0-m1.md#task-1-workspace-scaffold-dev-stack-ci-gate), with these changes:

- Crates: `agmen-proto`, `agmen-client`, `agmen-cli`, `agmen-hub`.
- `deploy/dev/compose.yaml`: `emqx` (1883 TCP, 8083 WS, 18083 dashboard and REST API), `postgres`, `toxiproxy`. Put EMQX config in `deploy/dev/emqx/emqx.conf`: `allow_anonymous`-style access off (authentication chain is JWT only), `authorization.no_match = deny`, `authorization.sources = []`, `mqtt.max_packet_size = 96KB`, and an API key bootstrap file for the hub (`api_key.bootstrap_file`).
- nextest profiles filter on test names (`test(/(^|::)it_/)`) instead of binary names; see ADR-0001.
- JWT authn in dev reads the hub's JWKS at `http://host.docker.internal:<port>/v1/jwks`. Until the hub exists (Task 7), tests sign tokens with a fixed dev ES256 key that compose mounts as a static public key.

### Task 2: Spike S1, EMQX identity and ACLs

**Files:** `spikes/s1-emqx/Cargo.toml` (workspace member, `publish = false`), `spikes/s1-emqx/tests/it_s1_acl.rs`, `docs/adr/0002-broker-emqx.md` (replaces the Mosquitto ADR named in m0-m1).

**Produces (for Task 7):** the exact JWT claims the hub mints, the EMQX authn and authz config, and the REST calls for kicking a client, all written into ADR-0002.

Token shape to test (ES256, `kid` from the JWKS, `sub` = `username` = identity id):

```json
{
  "sub": "idaaaaaaaa", "username": "idaaaaaaaa", "exp": 1761412800,
  "acl": [
    { "permission": "allow", "action": "publish",   "topic": "eq agmen/v1/t1/p1/up/idaaaaaaaa", "qos": [1] },
    { "permission": "allow", "action": "publish",   "topic": "eq agmen/v1/t1/p1/presence/idaaaaaaaa", "retain": true },
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/t1/p1/feed" },
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/t1/p1/inbox/idaaaaaaaa" },
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/t1/p1/ack/idaaaaaaaa" },
    { "permission": "allow", "action": "subscribe", "topic": "agmen/v1/t1/p1/state/#" },
    { "permission": "allow", "action": "subscribe", "topic": "agmen/v1/t1/p1/presence/+" },
    { "permission": "deny",  "action": "all",       "topic": "#" }
  ]
}
```

Topics are written in full with `eq` (no `${username}` placeholders), so an id can never widen a rule. The JWT must also be bound to the client ids `<sub>-mcp` and `<sub>-watch`; test whether a payload claim check on `${clientid}` can enforce the prefix, and record the result.

- [ ] **Step 1: Write the failing tests** against the compose broker:

```rust
#[tokio::test] async fn it_publish_to_own_up_allowed()         // A publishes to up/A; hub-role subscriber receives it
#[tokio::test] async fn it_publish_to_other_up_refused()       // A publishes to up/B; nothing arrives within 2 s; PUBACK reason 0x87
#[tokio::test] async fn it_subscribe_other_tenant_refused()    // A subscribes to agmen/v1/t2/p1/feed; SUBACK 0x87
#[tokio::test] async fn it_no_acl_claim_is_denied()            // valid signature, no acl claim: every publish and subscribe refused
#[tokio::test] async fn it_bad_signature_refused()             // token signed by another key: CONNACK 0x86 or 0x87
#[tokio::test] async fn it_expiry_disconnects()                // exp = now + 5 s; session dropped within 10 s (disconnect_after_expire)
#[tokio::test] async fn it_rest_kick_disconnects()             // DELETE /api/v5/clients/{clientid} with the hub API key; client gone within 2 s
#[tokio::test] async fn it_hub_shared_subscription()           // two hub clients on $share/hub/agmen/v1/+/+/up/+; 100 publishes split across them, none lost
#[tokio::test] async fn it_persistent_session_survives_restart() // clean_start=false, expiry 86400; restart EMQX; record whether 3 queued QoS 1 messages arrive (replay covers it if not)
#[tokio::test] async fn it_packet_over_limit_refused()         // 100 KiB refused; 60 KiB accepted
#[tokio::test] async fn it_websocket_listener()                // first test over ws://localhost:8083/mqtt
```

- [ ] **Step 2:** Run `cargo nextest run -p s1-emqx --profile it` and record which tests fail.
- [ ] **Step 3:** Adjust `emqx.conf` until every test passes, or a test proves a requirement EMQX cannot meet on a single node.
- [ ] **Step 4:** Write ADR-0002: the license position (single node, BSL community terms, internal use; commercial license needed before outside teams or clustering), the claims and config, the results table, and the WSS-behind-HAProxy check from m0-m1 Task 2 Step 4 against port 8083.
- [ ] **Step 5:** Commit: `Spike S1: EMQX JWT identity and ACL tests`

### Task 3: Spike S2, rumqttc v5 against EMQX

As [m0-m1 Task 3](m0-m1.md#task-3-spike-s2-rumqttc-v5-client-behavior), against EMQX, plus one test:

```rust
#[tokio::test] async fn it_token_refresh_reconnect()  // connect with a token expiring in 5 s; client fetches a new token and reconnects with clean_start=false; session_present == true
```

Commit: `Spike S2: MQTT 5 client against EMQX`

### Task 4: Spike S5, gapless per-stream numbering

Unchanged: [m0-m1 Task 4](m0-m1.md#task-4-spike-s5-gapless-per-stream-numbering).

### Task 5: Spike S4, mod streams a child process, wakes the session, guards a tool

As [m0-m1 Task 5](m0-m1.md#task-5-spike-s4-claude-code-mod-streams-a-child-process-into-a-pane), but the spike proves the three MVP mechanisms instead of the pane:

- `fake-watch` prints one JSON line every 5 s: `{"seq":n,"verb":"DM","body":"tick n","to_me":true}`.
- **Wake-up:** on a `to_me` line while the session is idle, call `$.prompt.submit({ text })` without `asUser`. Check that the model sees "The agmen plugin sent a message: ..." and that a busy session queues rather than interrupts.
- **Context:** `$.session.append` adds the line as a meta row; check that the model reads it as data.
- **Guard:** a `tool.call` handler on Edit, Write and NotebookEdit denies paths under `forbidden/` and denies on any thrown error.

- [ ] Tests (`register.test.tsx`): `wakeup_on_dm_when_idle`, `no_wakeup_for_own_message`, `guard_denies_forbidden_path`, `guard_denies_on_error`.
- [ ] Interactive check in `claude --plugin-dir mods/agmen-claude` on Claude Code 2.1.310; record the results and the version in ADR-0005.
- [ ] Commit: `Spike S4: mod wake-ups, context rows, and edit guard`

(Spike S6, the Windows local API, is dropped for the MVP because there is no daemon. Unix and Windows both work, since `mcp` and `watch` talk to EMQX directly.)

---

## Phase B: hub

### Task 6: agmen-proto

As [m0-m1 Task 7](m0-m1.md#task-7-agmen-proto-envelope-verbs-topics-schema-export). The MVP implements HELLO, BYE, FYI, ASK, ANSWER, ACK, STATUS, ROLLCALL, ALERT, DM, CLAIM, GRANT, DENY, HEARTBEAT, RELEASE, ASSIGN, FOUND, ITEM and DONE. The other verbs exist as types only. Add tests `claim_requires_valid_until` and `done_requires_item_and_commit`.

### Task 7: Hub auth, identities, JWKS, broker tokens

As [m0-m1 Task 8](m0-m1.md#task-8-hub-data-model-auth-identities-broker-credentials), with these changes:

- **Login:** GitHub only for the MVP, through the device flow (`agmen login` prints a code and URL). Google and PKCE loopback come later.
- **No dynsec.** Replace `Dynsec` with `BrokerTokens`: `fn mint(&self, identity: &Identity, ttl: Duration) -> Result<String, TokenError>` signs an ES256 JWT with the ADR-0002 claims, and `GET /v1/jwks` serves the public keys (two keys during rotation). `pub struct EmqxAdmin` has `async fn kick(&self, client_id: &str) -> Result<(), EmqxError>`.
- `GET /v1/identities/{id}/broker-token` (identity token required) returns `{token, expires_at}` with a 1 hour TTL. Clients refresh 5 minutes before expiry.
- **Revocation:** sets `revoked_at`, kicks `<id>-mcp` and `<id>-watch`, and refuses further broker tokens.
- **Devices:** no `devices` table. An identity is owned by a human and nothing else; `agmen join` mints it.

Tests: keep `it_device_flow_issues_human_token`, `it_mint_child_identity`, `it_child_handle_prefixed_by_owner`, `it_identity_token_cannot_mint_for_others` and `it_invite_once` from m0-m1. Add:

```rust
#[tokio::test] async fn it_broker_token_connects()             // token from the hub connects to EMQX and can publish to its up/ only
#[tokio::test] async fn it_jwks_rotation()                     // rotate the signing key; old tokens valid until exp, new tokens signed with the new kid; EMQX accepts both
#[tokio::test] async fn it_revoke_kicks_and_refuses_refresh()  // revoke A; both of A's client ids are disconnected; GET broker-token returns 403
```

Commit: `Add hub auth, identities, JWKS, and broker tokens`

### Task 8: Hub ingest, sweeper, ACK and NACK

As [m0-m1 Task 9](m0-m1.md#task-9-hub-ingest-sweeper-ack-and-nack). The hub's MQTT client subscribes with `$share/hub/agmen/v1/+/+/up/+` and authenticates with its own JWT, which allows subscribing to `up/+` and publishing everywhere else under `agmen/v1/`.

### Task 9: Hub replay and presence

As [m0-m1 Task 10](m0-m1.md#task-10-hub-replay-presence-roster).

### Task 10: Hub ledger: lanes, items, claims, DONE check

**Files:** `crates/agmen-hub/migrations/0004_ledger.sql`, `crates/agmen-hub/src/{lanes.rs,items.rs,claims.rs}`, `crates/agmen-hub/tests/it_ledger.rs`

**Produces:**

- Tables: `lanes (project_id, name, globs text[], owner_identity_id, v)`, `items (project_id, key, title, lane, state, created_by)`, `claims (item_key, identity_id, granted_at, lease_until, released_at)` with a partial unique index on `(item_key) where released_at is null`, and `item_counters (project_id, prefix, n)`.
- HTTP: `PUT /v1/projects/{p}/lanes` (human or coordinator; body is a TOML or JSON lane list; overlapping globs give 422), `GET /v1/projects/{p}/lanes`, `GET /v1/projects/{p}/items`, `POST /v1/projects/{p}/check-path {identity, paths}` returning `[{path, lane, allowed, reason}]`.
- Verbs: CLAIM gives GRANT or DENY (`held_by`, `expired`, `not_in_lane`, `closed`). A lease is 30 minutes, renewed by HEARTBEAT; a sweeper task expires stale claims and posts RELEASE `lease_expired`. FOUND gives ITEM with the next key. ASSIGN (coordinator) goes to the assignee's inbox. DONE gets its `changes` checked against the sender's claimed lanes; the hub posts an ALERT to coordinators listing out-of-lane paths.
- Lane matching uses `globset` with gitignore semantics. A path in no lane is allowed and reported as `unowned`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test] async fn it_twenty_claims_one_grant()       // 20 identities CLAIM B-1 at once: exactly one GRANT, 19 DENY held_by
#[tokio::test] async fn it_lease_expiry_releases()          // lease 2 s (test config), no HEARTBEAT: RELEASE lease_expired posted, a new CLAIM is granted
#[tokio::test] async fn it_heartbeat_renews()               // HEARTBEAT every 1 s keeps the claim past 2 s
#[tokio::test] async fn it_overlapping_lanes_rejected()     // L-A "ui/**" and L-B "ui/src/**" give 422
#[tokio::test] async fn it_check_path()                     // A holds an L-UI item: ui/x.ts allowed; api/y.rs denied with reason "lane L-API held by adam/coord"
#[tokio::test] async fn it_done_out_of_lane_alerts()        // DONE with a change outside the claimed lanes: ALERT to coordinators names the path
#[tokio::test] async fn it_found_mints_item()               // FOUND gives ITEM {key:"B-1"}, then "B-2"; security severity posts only key and lane to the feed
```

- [ ] **Step 2:** Run them (expect FAIL), implement, run the tests and the gate.
- [ ] **Step 3:** Commit: `Add hub ledger: lanes, items, claims, and DONE checks`

---

## Phase C: client, CLI, MCP, plugin

### Task 11: agmen-client

As [m0-m1 Task 11](m0-m1.md#task-11-agmen-client-session-outbox-cursors), with these changes:

- `SessionConfig` takes a token source (`async fn token() -> Result<String, ClientError>`) and reconnects with a fresh token before `exp`.
- Outbox and cursors live in one SQLite file per identity, which two processes open at once (WAL, `busy_timeout` 5 s). Add the test `two_processes_share_file`: the outbox writer and the cursor writer in separate threads with separate connections both make progress.

### Task 12: agmen CLI

As [m0-m1 Task 13](m0-m1.md#task-13-agmen-cli), without agmend: the CLI talks to the hub over HTTP and to EMQX through `agmen-client`.

- Commands: `login`, `logout`, `whoami`, `join <invite> [--handle <name>]` (mints `<human>/<name>`, writes `.agmen/identity.json`, adds `.agmen/` to `.git/info/exclude`), `leave`, `post <verb>`, `ask`, `answer`, `dm`, `inbox`, `watch [--json]`, `claim <item>`, `release <item>`, `heartbeat`, `done <item> --commit <sha>` (fills `changes` from `git diff --name-status --no-renames <base_sha_at_claim>...<commit>` and `branch` from `git rev-parse`), `found <title>`, `assign <item> <identity>`, `lanes [set <file>]`, `items`, `check-path <path>...`.
- `watch --json` writes one `ClientEvent` per line, plus `{"kind":"self","identity_id":...}` first, so the mod knows which messages are its own.
- The exit codes are as in m0-m1.

### Task 13: `agmen mcp`

As [m0-m1 Task 14](m0-m1.md#task-14-agmen-mcp-server), with the identity resolved from the working directory (`--identity` is an optional override). Tools: `post`, `ask`, `answer`, `dm`, `inbox`, `state`, `claim`, `release`, `done`, `found`, `lanes`, `check_path`. Outside a joined worktree, every tool answers `not joined: run agmen join <invite> in this worktree` (not an MCP error). The MCP server sends HEARTBEAT for held claims every 5 minutes while it runs.

### Task 14: Claude Code plugin

**Files:** `mods/agmen-claude/.claude-plugin/plugin.json`, `mods/agmen-claude/.mcp.json`, `mods/agmen-claude/hooks/{hooks.json,register.tsx,register.test.tsx}`, `mods/agmen-claude/skills/agmen/SKILL.md`, `mods/agmen-claude/tsconfig.json`, `.claude-plugin/marketplace.json` (repo root)

**Produces:**

- `.mcp.json` starts `agmen mcp`. Claude Code then lists the tools from Task 13.
- `session.start`: if `.agmen/identity.json` is missing, do nothing. If `agmen` is not on `PATH` or `agmen --version` is older than the plugin's minimum, show one `$.ui.notify` and do nothing else. Otherwise spawn `agmen watch --json` for the session's life.
- **Context:** each message addressed to this identity (DM, ASSIGN, GRANT and DENY, ANSWER to its ASK, ALERT from its human) goes into `$.session.append` as a meta row with sender handle, verb, item and body.
- **Wake-up:** when idle and such a message arrives, call `$.prompt.submit` (never `asUser`). The text says what arrived and which tool to use (for example: "agmen: ASSIGN B-412 from adam/coord. Read it with the agmen inbox tool and claim it."). Wake-ups are rate limited and coalesced (Review Focus 3).
- **Lane guard:** `tool.call` on Edit, Write and NotebookEdit resolves the real path (`$.fs.stat(path, { resolve: true }).realPath`) and runs `agmen check-path --json`. If the path is not allowed, deny with the reason. Every error or timeout (2 s) denies. After each Bash call, `git status --porcelain` paths outside the lanes go into a meta row as a warning (the guard can't block shell writes).
- **Command:** `/agmen` with `join`, `inbox`, `claim`, `done`, `dm`, `lanes`. Each runs `agmen ...` through `$.process.run` and shows the result.
- **Skill:** `skills/agmen/SKILL.md` covers when to post, how to claim before editing, what DONE needs, and that `rejected:` results must not be retried unchanged.

- [ ] **Step 1: Write the failing tests** (`claude plugin test`, terminal and desktop surfaces) against a scripted fake `agmen` on `PATH`:

```ts
inert_when_not_joined          // no .agmen/identity.json: no spawn, no guard denials
notifies_when_cli_missing      // agmen absent: one notify, session otherwise untouched
appends_context_for_dm         // DM to me arrives: one meta row with handle and body
wakeup_when_idle               // ASSIGN to me while idle: one prompt.submit, not asUser
wakeup_rate_limited            // 5 DMs in 10 s: one wake-up naming all 5
no_wakeup_for_own_message      // my own post echoed on the feed: no wake-up
guard_denies_out_of_lane       // check-path returns allowed:false: Edit denied with the reason
guard_denies_on_error          // check-path exits 1 or hangs past 2 s: Edit denied
guard_allows_in_lane           // allowed:true: Edit proceeds
```

- [ ] **Step 2:** Run `claude plugin validate mods/agmen-claude`, `tsc -p mods/agmen-claude` and `claude plugin test mods/agmen-claude` (expect FAIL), then implement until all pass.
- [ ] **Step 3:** Interactive check on a real hub from compose: two worktrees, two sessions. One DMs the other while it is idle, and the other wakes and answers.
- [ ] **Step 4:** Commit: `Add the agmen Claude Code plugin`

---

## Phase D: exit, deploy, dogfood

### Task 15: MVP end-to-end exit tests

As [m0-m1 Task 15](m0-m1.md#task-15-m1-end-to-end-exit-tests), using two temp homes and two worktrees instead of two agmend instances, plus:

```rust
#[tokio::test] async fn it_claim_race_two_machines()   // both machines CLAIM B-1 at once: one GRANT, one DENY held_by
#[tokio::test] async fn it_done_reaches_coordinator()  // claim, commit, `agmen done`; coord's watch yields DONE with branch@commit and changes
```

### Task 16: Deploy on the GroupDrive cluster

As [m0-m1 Task 16](m0-m1.md#task-16-minimal-deploy-on-the-groupdrive-cluster), with EMQX in place of Mosquitto:

- One EMQX StatefulSet replica (single node keeps the BSL community terms; no clustering) on Ceph RBD, pinned `emqx/emqx` tag from ADR-0001. Config from a ConfigMap; the hub's EMQX API key comes from Vault through VSO.
- `mqtt-ws.` ingress points at EMQX 8083. The dashboard (18083) gets no ingress.
- The hub serves `/v1/jwks` inside the cluster; EMQX fetches it by the Service name.

### Task 17: Dogfood

One real multi-agent round with Adam's team on the plugin instead of ntfy. Install with `cargo install --locked --git https://git.groupdrive.xyz/groupdrive/agmen agmen-cli`, then `claude plugin marketplace add https://git.groupdrive.xyz/groupdrive/agmen.git` and `/plugin install agmen@agmen`. Record a retro ADR measured against design 1 "What success looks like": no lost DONE, no spoofed message, no claim race settled by hand.

## Spec coverage

| Spec item | Task |
|---|---|
| Broker identity and ACLs (EMQX) | 2, 7 |
| Envelope, verbs, topics, limits | 6 |
| Delivery: outbox, dedupe, numbering, sweeper, ACK and NACK, replay | 4, 8, 9, 11 |
| DMs and their visibility | 8, 9 |
| Ledger: lanes, items, claims, DONE checks (design 6.5, M2) | 10 |
| CLI and MCP (design 6.8) | 12, 13 |
| Claude Code mod: wake-ups, context, lane guard, command (design 6.8) | 5, 14 |
| Deploy | 16 |

Deferred past the MVP: agmend as an OS service and Windows local IPC (S6), Google login and PKCE, board pane, band, status line, push approval, LEND, PROPOSAL, DECISION and RULING, the launcher and budgets, the desktop app, and the beta work in M6, which needs an EMQX commercial license or a broker change before outside teams join.
