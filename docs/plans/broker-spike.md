# Broker spike S11: RMQTT in library mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decide whether agmen hosted can run RMQTT embedded in its own process instead of EMQX, by testing RMQTT 0.24.0 in library mode against every pass criterion of spec decision 9, and record the verdict and its evidence in an ADR, `docs/adr/NNNN-broker-rmqtt.md`.

**Architecture:** A spike crate `spikes/s11-rmqtt` (workspace member, `publish = false`, like `spikes/s1-emqx` and `spikes/s2-rumqttc`) holds a small library and one binary, `s11-broker`. The library embeds RMQTT with agmen's own hooks: a JWT check against the hub's JWKS, today's client id binding (ADR-0002 cinfo rule), an evaluator for the `acl` claim the hub already mints (`crates/agmen-hub/src/broker_tokens.rs:177-213`), a persisted ban list, and a small HTTP shim that answers the EMQX REST calls the hub and its tests make, so the hub runs unchanged. Integration tests start `s11-broker` as a child process on free ports with its own data folder, so they never touch the dev stack's EMQX, and a crash test kills that process to check what survives.

**Tech Stack:** Rust 1.99.1 (edition 2024), `rmqtt` 0.24.0 and `rmqtt-plugins` 0.24.0 (library mode, sled storage), `jsonwebtoken` 11.1.0, axum 0.8.9, tokio 1.54.1, `rumqttc` 0.25.1 as the raw test client (as in S1), `agmen-client` and `agmen-hub` (`test-hooks`) for the WSS check, cargo-nextest.

**Spec:** `docs/specs/2026-10-26-onboarding-and-hosting-design.md`, decision 9 ("The broker for agmen hosted: a spike decides"). Cross-plan contracts: `.superpowers/sdd/onboarding/plan-contracts.md` (plan 3 of 4 in the contracts' list, but by controller ruling it runs first, on `mvp/impl` at 0d5d0bf, and consumes no task of the other three plans). Controller rulings R70 and R71 (`.superpowers/sdd/mvp/progress.md`) add one pass criterion, the first-ack stall check in Task 7.

## Global Constraints

Copied from the spec (decision 9), verbatim:

- "agmen hosted cannot run EMQX for customers under its free license. Before hosted sign-ups open, a time-boxed spike tests RMQTT (MIT or Apache 2.0, MQTT 5, WebSocket and TLS, library mode, an auth hook, a kick API) against agmen's real needs."
- "It passes only if all of these hold:"
  - "`agmen-client` works over MQTT 5 on WSS."
  - "An auth hook verifies the hub's JWT in process."
  - "Client ids are bound to identities (today's cinfo rule)."
  - "Each token's topic permissions apply, with everything else denied."
  - "Packets are capped at 96 KB."
  - "Shared subscriptions, retained QoS 1 messages, wills and 1-day durable sessions work."
  - "Kick and ban work."
  - "The reason codes the clients rely on (0x87, 0x8A, 0x98) come back as expected."
  - "The hub's `it` test profile passes with RMQTT in place of EMQX."
  - "Durable sessions and retained state survive a restart of the hub process."
- "A single hub process with the broker inside is acceptable at launch. A deploy or crash drops every connection for a few seconds, and clients reconnect into their durable sessions. Clustering is a later project."
- "If it passes: agmen hosted runs the hub with the embedded broker. A later project can make self-host a single program as well."
- "If it fails: agmen hosted uses EMQX's managed cloud (Dedicated Flex: $234 a month at 1,000 sessions, about $642 at 5,000), or a commercial EMQX license if EMQ prices one."
- EMQX license (Constraints): "You cannot offer EMQX as a hosted service to third parties or embed it in products sold to third parties".

From controller rulings R70 and R71 and Task 15's review (`.superpowers/sdd/mvp/progress.md`, "Ruling R70", "Task 15 review", "Ruling R71"):

- One more pass criterion: no first-ack stall under concurrent durable sessions. Task 15 found that EMQX 6.3.2 durable sessions can miss a stream created on another DS shard, so a session's first ack stalls under concurrency (alone the first ack arrives in 0.8 to 3 s; 8 sessions at once stalled 1 to 6 sessions in 5 of 6 batches). On RMQTT it passes only if 8 concurrent `agmen-client` Mcp sessions (`clean_start = false`, durable), each posting once, all get their first ack within 5 s, in 5 of 5 batches. Test: Task 7 `it_concurrent_durable_first_ack`.
- For comparison (it feeds R71's decision, not the verdict), the same probe runs against EMQX 6.3.2 with `durable_sessions` off in a throwaway container. As a control, it also runs against a second throwaway EMQX 6.3.2 with the dev config exactly as it is at 0d5d0bf (durable sessions on), to show the ported probe still catches the stall. Neither run touches the dev stack's EMQX, which R71 may reconfigure while the spike runs.

From the cross-plan contracts, the controller's start ruling and the house rules:

- Start point: the spike starts on `mvp/impl` at 0d5d0bf, before any task of `docs/plans/onboarding.md`, `docs/plans/self-host.md` or `docs/plans/hosted.md` exists, and no task here consumes one of theirs. Where the spike calls the hub's admin client it uses `agmen_hub::EmqxAdmin` exactly as it is there (`crates/agmen-hub/src/emqx.rs:62-183`): `EmqxAdmin::new(config: &EmqxConfig, http: reqwest::Client) -> Result<EmqxAdmin, EmqxError>` with `EmqxConfig { api_url: Url, api_key: String, api_secret: Secret }` (`crates/agmen-hub/src/config.rs:98-105`), `kick(&self, client_id: &str) -> Result<(), EmqxError>` and `ban_username(&self, username: &str, until: agmen_proto::Timestamp) -> Result<(), EmqxError>` (`Timestamp` is `jiff::Timestamp`). The hub there still requires `AGMEN_GITHUB_CLIENT_ID` and `AGMEN_GITHUB_CLIENT_SECRET` (`crates/agmen-hub/src/config.rs:297-298`), so the spike's in-process hubs set both, as `crates/agmen-client/tests/support/mod.rs:193-194` does. `spikes/s11-rmqtt` is a workspace member that the gate compiles, so a later plan that changes one of these hub interfaces (self-host Task 5 changes `EmqxAdmin::new` and `EmqxConfig`) also updates the spike's tests that call it.
- Self-host keeps "EMQX 6.3.2 single node (pinned)". This spike changes nothing outside `spikes/s11-rmqtt`, the workspace member list, `Cargo.lock`, `deny.toml` (only if Task 3 Step 6 needs it) and the new `docs/adr/NNNN-broker-rmqtt.md`. No product crate (`crates/*`) changes; a product change the spike finds necessary goes into the ADR as a consequence.
- ADR number: `NNNN` is the next free number in `docs/adr/` when Task 1 starts, written with four digits (one more than the highest file number there). On `mvp/impl` at 0d5d0bf the highest is `0006-claude-code-plugin.md`, so expect `0007`; the self-host plan, which runs later, takes the next free number after this one for its own ADR. Every `NNNN` in this plan is that one number; the file name always ends in `-broker-rmqtt.md`, which is how `docs/plans/hosted.md` finds it (`docs/adr/*-broker-rmqtt.md`).
- The dev stack (`deploy/dev`) is used as it is: no edits under `deploy/dev`, and no stopping, restarting or reconfiguring its services. The spike reads `deploy/dev/.secrets` (JWT keys, JWKS, dev CA, server certificate, `api-keys.txt`), lets Task 7's throwaway EMQX containers fetch the dev JWKS service on port 8090, and uses the dev Postgres through `AGMEN_IT_PG` (each test makes its own database, ruling R26).
- Spike ports, all on 127.0.0.1, none used by the dev stack: MQTT TCP 11883, WS 18093, WSS 18443, admin shim 18183; the spike's own toxiproxy: API 18474, proxied MQTT 31883; the throwaway EMQX containers of Task 7, one at a time: MQTT 11884, REST API 18184. Tests that start their own broker use free ephemeral ports instead.
- Crate versions, from crates.io on 2026-10-26: `rmqtt` 0.24.0 and `rmqtt-plugins` 0.24.0, both released 2026-09-19, both `MIT OR Apache-2.0`, both `rust-version` 1.94.0 (below the workspace's 1.99.1). Pin both with `=0.24.0`.
- Packet cap: 96 KB is `96 * 1024` = 98304 bytes, the value of `MAX_PACKET` in `crates/agmen-client/src/session.rs:51` and EMQX's `max_packet_size = 96KB` (ADR-0002).
- Durable session expiry: 86400 s (1 day), as agmen-client sets it (`crates/agmen-client/src/mqtt_store.rs:177`, ADR-0003).
- Reason codes: 0x86 (bad user name or password: client id binding failure, as EMQX answers per ADR-0002), 0x87 (not authorized: bad token at CONNECT, ACL deny on PUBACK and SUBACK, DISCONNECT at token expiry), 0x8A (banned: CONNACK for a banned username), 0x98 (administrative action: DISCONNECT on kick).
- Integration tests are named `it_*` and run with `set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it`. No spike test name contains `restart`, so all of them stay in the `it` profile (`.config/nextest.toml` sends `restart` tests to `it-restart`, which restarts the dev EMQX); the spike's crash tests restart only their own child broker.
- Gate before each commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`, `cargo nextest run --workspace --locked`, `cargo test --doc --workspace --locked`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`, `cargo deny check`.
- Time box (my call; the spec gives none): 5 working days from the start of Task 1. When it runs out, the executor stops, marks every criterion not yet shown to pass as "not tested" in the ADR, and an untested criterion counts as failed.
- R46: `s11-broker` calls `rustls::crypto::aws_lc_rs::default_provider().install_default()` first thing in `main`, and every spike test file that opens TLS installs it once and ignores the error a second call returns.

RMQTT API this plan relies on, read on 2026-10-26 from the `rmqtt/rmqtt` master branch (README, `rmqtt/src/hook.rs`, `shared.rs`, `types.rs`, `acl.rs`, `rmqtt/examples/plugins`, `docs/en_US/auth-jwt.md`), not yet checked against the 0.24.0 tag. Task 3 Step 1 checks each name in `~/.cargo/registry/src/*/rmqtt-0.24.0/` and records any difference in the ADR:

- Startup: `ServerContext::new()...build().await`, then `MqttServer::new(scx).listener(Builder::new().name(..).laddr(..).allow_anonymous(false)...bind()?.tcp()?/.ws()?/.wss()?).build().run().await`.
- Plugins: `rmqtt_plugins::<name>::register(&scx, true, false).await?`, configured from TOML files in the context's plugins config folder. Features `retainer-sled` and `session-storage-sled` exist in `rmqtt-plugins` 0.24.0.
- Hooks: `Register::add(Type, Box<dyn Handler>)`, `Handler::hook(&self, param: &Parameter, acc: Option<HookResult>) -> ReturnType`, types `ClientAuthenticate`, `ClientConnack`, `ClientSubscribeCheckAcl`, `MessagePublishCheckAcl`, `ClientConnected`; results `HookResult::AuthResult(AuthResult::{Allow(Superuser, Option<AuthInfo>), BadUsernameOrPassword, NotAuthorized})`, `HookResult::ConnectAckReason(..)`, `HookResult::SubscribeAclResult(SubscribeReturn::new_failure(..))`, `HookResult::PublishAclResult(PublishAclResult::rejected(..))`. `AuthResult` has no "banned" variant, so the ban answer comes from the `ClientConnack` hook.
- In-process admin: `Shared::entry(id)` returns an `Entry` with `kick(clean_start, clear_subscriptions, is_admin)`, `is_connected()`, `exist()` and `subscriptions()`.
- Token expiry: `auth-jwt`'s `disconnect_if_expiry` option shows RMQTT can drop a client at token expiry; `AuthInfo` has `expire_at: Option<Duration>`.

## Review Focus

1. **A last will on a topic the token may not publish to.** A client sets its will to another identity's `presence/` or `up/` topic and then drops. Expected: the will never reaches anyone; the CONNECT is refused with 0x87. Test: Task 3 `it_will_on_foreign_topic_refused`.
2. **A `$share/` prefix or a wildcard used to widen a subscription.** An identity subscribes to `$share/g/agmen/v1/t1/p1/inbox/<B>` or to `agmen/v1/t1/p1/inbox/+`. Expected: SUBACK 0x87 for both; the share prefix is stripped before the check and a requested `+` is covered only by `+` or `#` in a rule. Tests: Task 1 `acl_wildcard_request_not_covered_by_eq_rule`, Task 3 `it_share_prefix_does_not_widen_acl`.
3. **A token without `exp`.** EMQX accepts one and never expires it (ADR-0002, "Always set `exp`"). Expected: the spike's check refuses it with 0x87. Test: Task 2 `token_without_exp_refused`.
4. **A ban lost when the hub process restarts.** A revoked identity keeps an unexpired broker token for up to an hour (R36); if the ban list lives only in memory, a deploy lets it back in. Expected: after a crash and a new start, the banned username still gets CONNACK 0x8A. Test: Task 4 `it_ban_survives_broker_crash`.
5. **Durable state after a crash, not only after a clean stop.** sled may lose writes it has not flushed when the process is killed. Expected: queued QoS 1 messages and retained messages are there after SIGKILL as well as after SIGTERM, or the ADR records exactly what was lost. Tests: Task 5 `it_durable_state_survives_broker_crash` and `it_durable_state_survives_broker_stop`.
6. **A first ack that stalls when many durable sessions start at once.** EMQX 6.3.2 shows it (R71); a broker that passes every one-client test can still fail it. Expected: on RMQTT, 8 concurrent Mcp sessions each get their first ack within 5 s, in 5 of 5 batches, and the same probe still catches the stall on EMQX 6.3.2 with durable sessions on (the control), so a pass is not an artifact of the port. Test: Task 7 `it_concurrent_durable_first_ack`.

---

### Task 1: Spike crate and the `acl` claim evaluator

The hub mints EMQX's `acl` claim format and must keep doing so for self-host on EMQX, so the embedded broker reads the same claim. EMQX's semantics, from ADR-0002: rules are tried in order, the first rule whose topic, action, `qos` and `retain` all match decides, `eq <topic>` matches only that literal string, and no match means deny.

**Files:**
- Modify: `Cargo.toml:3-11` (add `"spikes/s11-rmqtt"` to `members`, after `"spikes/s4-fake-watch"`)
- Modify: `Cargo.lock` (new entries only)
- Create: `spikes/s11-rmqtt/Cargo.toml`
- Create: `spikes/s11-rmqtt/src/lib.rs`
- Create: `spikes/s11-rmqtt/src/acl.rs` (tests in the same file under `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `s11_rmqtt::Acl` with `Acl::from_claims(claims: &serde_json::Value) -> Result<Acl, AclError>`, `Acl::allows_publish(&self, topic: &str, qos: u8, retain: bool) -> bool`, `Acl::allows_subscribe(&self, filter: &str) -> bool`.
  - `s11_rmqtt::AclError` (`thiserror`, `#[non_exhaustive]`): `NotAList`, `BadRule { index: usize, reason: String }`.
  - `pub fn strip_share(filter: &str) -> &str` (removes a leading `$share/<group>/`).

- [ ] **Step 1: Create the crate**

`spikes/s11-rmqtt/Cargo.toml`: `name = "s11-rmqtt"`, `description = "Spike S11: RMQTT 0.24.0 in library mode against agmen's broker needs."`, `version`, `edition`, `rust-version`, `license` from the workspace, `publish = false`, `[lints] workspace = true`, `[lib] path = "src/lib.rs"`. Dependencies for this task: `serde = { version = "1.0.229", features = ["derive"] }`, `serde_json = "1.0.154"`, `thiserror = "2.0.21"` (versions already in `Cargo.lock`). `src/lib.rs` has a `//!` doc pointing at `docs/adr/NNNN-broker-rmqtt.md`, `mod acl;` and `pub use acl::{Acl, AclError, strip_share};`.

- [ ] **Step 2: Write the failing tests in `spikes/s11-rmqtt/src/acl.rs`**

The claims are the exact shapes `identity_acl` and `hub_acl` produce (`crates/agmen-hub/src/broker_tokens.rs:179-213`), with `b = "agmen/v1/t1/p1"`, identity `A = "ida0000000"`, other identity `B = "idb0000000"`.

```rust
#[test]
fn acl_identity_publish_rules() {
    let acl = Acl::from_claims(&identity_claims(A)).unwrap();
    assert!(acl.allows_publish(&format!("{b}/up/{A}"), 1, false));
    assert!(!acl.allows_publish(&format!("{b}/up/{A}"), 0, false), "qos [1] only");
    assert!(!acl.allows_publish(&format!("{b}/up/{B}"), 1, false));
    assert!(acl.allows_publish(&format!("{b}/presence/{A}"), 1, true));
    assert!(!acl.allows_publish(&format!("{b}/presence/{A}"), 1, false), "retain only");
    assert!(!acl.allows_publish(&format!("{b}/feed"), 1, false));
    assert!(!acl.allows_publish("agmen/v1/t2/p1/up/ida0000000", 1, false));
}

#[test]
fn acl_identity_subscribe_rules() {
    let acl = Acl::from_claims(&identity_claims(A)).unwrap();
    for ok in [format!("{b}/feed"), format!("{b}/inbox/{A}"), format!("{b}/ack/{A}"),
               format!("{b}/state/#"), format!("{b}/state/roster"), format!("{b}/presence/+"),
               format!("{b}/presence/{B}")] {
        assert!(acl.allows_subscribe(&ok), "{ok}");
    }
    for denied in [format!("{b}/inbox/{B}"), format!("{b}/up/{A}"), "#".to_owned(),
                   "agmen/v1/t2/p1/feed".to_owned(), format!("{b}/#")] {
        assert!(!acl.allows_subscribe(&denied), "{denied}");
    }
}

#[test]
fn acl_wildcard_request_not_covered_by_eq_rule() {
    let acl = Acl::from_claims(&identity_claims(A)).unwrap();
    assert!(!acl.allows_subscribe(&format!("{b}/inbox/+")));
    assert!(!acl.allows_subscribe(&format!("{b}/inbox/#")));
    assert!(!acl.allows_subscribe(&format!("$share/g/{b}/inbox/{B}")));
    assert!(acl.allows_subscribe(&format!("$share/g/{b}/feed")), "share prefix is stripped");
}

#[test]
fn acl_hub_rules() {
    let acl = Acl::from_claims(&hub_claims()).unwrap();
    assert!(acl.allows_subscribe("$share/hub/agmen/v1/+/+/up/+"));
    assert!(acl.allows_subscribe("agmen/v1/+/+/presence/+"));
    assert!(!acl.allows_subscribe("agmen/v1/t1/p1/inbox/+"));
    assert!(acl.allows_publish(&format!("{b}/inbox/{A}"), 1, false));
    assert!(acl.allows_publish(&format!("{b}/state/roster"), 1, true));
    assert!(!acl.allows_publish(&format!("{b}/up/{A}"), 1, false));
    assert!(!acl.allows_publish("elsewhere/x", 1, false));
}

#[test]
fn acl_missing_claim_denies_everything() {
    let acl = Acl::from_claims(&serde_json::json!({ "sub": A, "exp": 1 })).unwrap();
    assert!(!acl.allows_publish(&format!("{b}/up/{A}"), 1, false));
    assert!(!acl.allows_subscribe(&format!("{b}/feed")));
}

#[test]
fn acl_malformed_claim_is_an_error() {
    assert!(matches!(Acl::from_claims(&serde_json::json!({ "acl": "allow" })), Err(AclError::NotAList)));
    let bad = serde_json::json!({ "acl": [{ "permission": "maybe", "action": "publish", "topic": "x" }] });
    assert!(matches!(Acl::from_claims(&bad), Err(AclError::BadRule { index: 0, .. })));
}
```

`identity_claims(id)` and `hub_claims()` are test helpers that return the claims `identity_acl` and `hub_acl` build (`crates/agmen-hub/src/broker_tokens.rs:179-213`), written out as JSON for tenant `t1`, project `p1`: `eq ` topics stay `eq `, the identity's `presence/+` and `state/#` rules are filters, and each list ends in the deny-all rule. A hand copy can drift from the hub, so Task 4 `it_hub_minted_hub_token_scope` and Task 6 `it_hub_minted_identity_token_acl` repeat the checks with tokens that `agmen_hub::BrokerTokens` mints itself.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo nextest run -p s11-rmqtt --locked acl_`
Expected: FAIL to compile (`Acl` has no `from_claims`).

- [ ] **Step 4: Implement `Acl`, `AclError` and `strip_share` in `spikes/s11-rmqtt/src/acl.rs`**

Parse each rule into `permission` (`allow` | `deny`), `action` (`publish` | `subscribe` | `all`), a topic that is either `Eq(String)` (the `eq ` prefix removed) or `Filter(String)`, an optional `qos` list and an optional `retain` bool; any other value is `BadRule`. A missing `acl` claim gives an `Acl` with no rules. The first rule that matches decides; none matching denies. Subscribe checks run on `strip_share(filter)`. The covering rule for a `Filter` rule against a requested topic or filter is not determined by the tests alone, so implement exactly this:

```rust
/// True when every topic `requested` can match is matched by `rule`.
fn covers(rule: &str, requested: &str) -> bool {
    let mut r = rule.split('/');
    let mut q = requested.split('/');
    loop {
        match (r.next(), q.next()) {
            (Some("#"), _) => return true,
            (Some("+"), Some(level)) if level != "#" => {}
            (Some(a), Some(b)) if a == b && a != "+" && a != "#" => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run -p s11-rmqtt --locked acl_`
Expected: PASS, 6 tests. Then `git diff Cargo.lock | rg '^-version'` prints nothing (no existing package changed version).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock spikes/s11-rmqtt
git commit -m "Add spike S11 crate with an evaluator for the hub's acl claim"
```

### Task 2: Token check, client id binding and the ban list

**Files:**
- Create: `spikes/s11-rmqtt/src/auth.rs` (tests in the same file)
- Modify: `spikes/s11-rmqtt/src/lib.rs` (add `mod auth;` and its `pub use`)
- Modify: `spikes/s11-rmqtt/Cargo.toml` (add `jsonwebtoken = { version = "11.1.0", features = ["aws_lc_rs"] }`, `jiff = { version = "0.2.38", default-features = false, features = ["std", "serde"] }`; dev-dependencies `aws-lc-rs = "1.18.2"`, `tempfile = "3.27.0"`)

**Interfaces:**
- Consumes: `Acl::from_claims` (Task 1).
- Produces:
  - `pub const HUB_USERNAME: &str = "agmen-hub";`
  - `pub fn client_id_allowed(client_id: &str, username: &str) -> bool`: true only for `<username>-mcp`, `<username>-watch`, `<username>-cli`, or username `agmen-hub` with a client id matching `^agmen-hub-[a-z0-9]{1,32}$` (ADR-0002 cinfo rule, `deploy/dev/emqx/emqx.conf`).
  - `pub struct TokenVerifier` with `TokenVerifier::from_jwks(jwks: &jsonwebtoken::jwk::JwkSet) -> Result<TokenVerifier, AuthError>` and `TokenVerifier::verify(&self, username: &str, client_id: &str, password: &[u8]) -> Result<Grant, Refusal>`.
  - `pub struct Grant { pub username: String, pub exp: i64, pub acl: Acl }`.
  - `pub enum Refusal { ClientIdNotBound, BadToken, Banned }` (`Debug, Clone, Copy, PartialEq, Eq`) with `pub const fn reason_code(self) -> u8` returning 0x86, 0x87, 0x8A.
  - `pub struct BanList` with `BanList::open(path: impl Into<PathBuf>) -> Result<BanList, AuthError>`, `ban(&self, username: &str, until: jiff::Timestamp) -> Result<BanOutcome, AuthError>`, `unban(&self, username: &str) -> Result<bool, AuthError>`, `is_banned(&self, username: &str, now: jiff::Timestamp) -> bool`.
  - `pub enum BanOutcome { Added, AlreadyExists }`.
  - `AuthError` (`thiserror`, `#[non_exhaustive]`): `BadJwk { kid: Option<String>, source: jsonwebtoken::errors::Error }`, `BanFile { path: PathBuf, source: std::io::Error }`, `BanFileFormat { path: PathBuf, source: serde_json::Error }`.

- [ ] **Step 1: Write the failing tests in `spikes/s11-rmqtt/src/auth.rs`**

Tests generate their own P-256 key with `aws_lc_rs::signature::EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &SystemRandom::new())`, sign with `EncodingKey::from_ec_der`, and build the JWKS with `Jwk::from_encoding_key(&key, Algorithm::ES256)`, so no key file is committed. A second generated key, not in the JWKS, is the untrusted one.

```rust
#[test]
fn client_ids_bound_to_username() {
    let a = "ida0000000";
    for ok in ["ida0000000-mcp", "ida0000000-watch", "ida0000000-cli"] {
        assert!(client_id_allowed(ok, a), "{ok}");
    }
    // The refused list from ADR-0002 it_client_ids_bound_to_identity.
    for bad in ["idb0000000-mcp", "ida0000000", "ida0000000-", "ida0000000-mcpx",
                "ida0000000-mcp-watch", "ida0000000-MCP", "xida0000000-mcp",
                "agmen-hub-x", "ida0000000-mcp\n"] {
        assert!(!client_id_allowed(bad, a), "{bad:?}");
    }
    assert!(client_id_allowed("agmen-hub-main", HUB_USERNAME));
    assert!(!client_id_allowed("agmen-hub-", HUB_USERNAME));
    assert!(!client_id_allowed(&format!("agmen-hub-{}", "a".repeat(33)), HUB_USERNAME));
    assert!(!client_id_allowed("ida0000000-mcp", HUB_USERNAME));
}

#[test]
fn good_token_gives_grant_with_acl() {
    let grant = verifier().verify(A, &format!("{A}-mcp"), sign(&good, &claims(A, now() + 3600)).as_bytes()).unwrap();
    assert_eq!(grant.username, A);
    assert!(grant.acl.allows_publish(&format!("agmen/v1/t1/p1/up/{A}"), 1, false));
}

#[test]
fn untrusted_key_refused() {
    let token = sign(&untrusted, &claims(A, now() + 3600));
    assert_eq!(verifier().verify(A, &format!("{A}-mcp"), token.as_bytes()).unwrap_err(), Refusal::BadToken);
}

#[test]
fn expired_token_refused() {
    let token = sign(&good, &claims(A, now() - 1));
    assert_eq!(verifier().verify(A, &format!("{A}-mcp"), token.as_bytes()).unwrap_err(), Refusal::BadToken);
}

#[test]
fn token_without_exp_refused() {
    let mut c = claims(A, 0);
    c.as_object_mut().unwrap().remove("exp");
    assert_eq!(verifier().verify(A, &format!("{A}-mcp"), sign(&good, &c).as_bytes()).unwrap_err(), Refusal::BadToken);
}

#[test]
fn sub_must_equal_username() {
    let token = sign(&good, &claims(A, now() + 3600));
    assert_eq!(verifier().verify(B, &format!("{B}-mcp"), token.as_bytes()).unwrap_err(), Refusal::BadToken);
    assert_eq!(verifier().verify(A, &format!("{B}-mcp"), token.as_bytes()).unwrap_err(), Refusal::ClientIdNotBound);
}

#[test]
fn superuser_claim_grants_nothing_extra() {
    let mut c = claims(A, now() + 3600);
    c["is_superuser"] = true.into();
    let grant = verifier().verify(A, &format!("{A}-mcp"), sign(&good, &c).as_bytes()).unwrap();
    assert!(!grant.acl.allows_publish(&format!("agmen/v1/t1/p1/up/{B}"), 1, false));
}

#[test]
fn reason_codes() {
    assert_eq!(Refusal::ClientIdNotBound.reason_code(), 0x86);
    assert_eq!(Refusal::BadToken.reason_code(), 0x87);
    assert_eq!(Refusal::Banned.reason_code(), 0x8A);
}

#[test]
fn ban_list_persists_and_expires() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bans.json");
    let now = jiff::Timestamp::now();
    let until = now + jiff::SignedDuration::from_secs(3900);
    let bans = BanList::open(&path).unwrap();
    assert_eq!(bans.ban(A, until).unwrap(), BanOutcome::Added);
    assert_eq!(bans.ban(A, until).unwrap(), BanOutcome::AlreadyExists);
    let reopened = BanList::open(&path).unwrap();
    assert!(reopened.is_banned(A, now));
    assert!(!reopened.is_banned(A, until + jiff::SignedDuration::from_secs(1)));
    assert!(reopened.unban(A).unwrap());
    assert!(!reopened.unban(A).unwrap());
    assert!(!BanList::open(&path).unwrap().is_banned(A, now));
}
```

`claims(id, exp)` builds ADR-0002's identity claims; `verifier()` is `TokenVerifier::from_jwks` over a JWKS holding only `good`'s public key.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p s11-rmqtt --locked`
Expected: FAIL to compile (`client_id_allowed`, `TokenVerifier`, `BanList` undefined).

- [ ] **Step 3: Implement `spikes/s11-rmqtt/src/auth.rs`**

`from_jwks` turns each JWK into a `DecodingKey` with `DecodingKey::from_jwk`. `verify` checks `client_id_allowed` first (`ClientIdNotBound`), then tries every key, as EMQX does whatever the header's `kid` says (ADR-0002, "JWKS"): `Validation::new(Algorithm::ES256)` with `leeway = 0`, `required_spec_claims = {"exp", "sub"}`, `validate_aud = false`; then `sub == username`, then `Acl::from_claims`. Every failure is `BadToken`. `verify` reads only `sub`, `exp` and `acl`, so `is_superuser`, `clientid_override` and `client_attrs` have no effect. `BanList` keeps `HashMap<String, i64>` (username to `until` in Unix seconds) in a `std::sync::Mutex` and rewrites the whole file on each change: write `bans.json.tmp`, then rename it over `bans.json`. A missing file opens as empty.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p s11-rmqtt --locked`
Expected: PASS, 15 tests (6 from Task 1, 9 here).

- [ ] **Step 5: Commit**

```bash
git add spikes/s11-rmqtt Cargo.lock
git commit -m "Add the S11 token check, client id binding and persisted ban list"
```

### Task 3: Embedded RMQTT with agmen's hooks, the `s11-broker` binary, and the auth, ACL, packet cap and expiry criteria

**Files:**
- Create: `spikes/s11-rmqtt/src/broker.rs`
- Create: `spikes/s11-rmqtt/src/main.rs` (`[[bin]] name = "s11-broker"`)
- Create: `spikes/s11-rmqtt/plugins/rmqtt-retainer.toml`, `spikes/s11-rmqtt/plugins/rmqtt-session-storage.toml` (templates; see Step 3)
- Create: `spikes/s11-rmqtt/tests/support/mod.rs`
- Create: `spikes/s11-rmqtt/tests/it_s11_broker.rs`
- Create: `docs/adr/NNNN-broker-rmqtt.md` (`Status: proposed`, from `docs/adr/0000-template.md`)
- Modify: `spikes/s11-rmqtt/Cargo.toml`, `spikes/s11-rmqtt/src/lib.rs`, `Cargo.lock`, and `deny.toml` only as Step 6 says

**Interfaces:**
- Consumes: `Acl` (Task 1); `TokenVerifier`, `Grant`, `Refusal`, `BanList`, `client_id_allowed` (Task 2).
- Produces:
  - `pub const MAX_PACKET: u32 = 96 * 1024;`
  - `pub struct WssListener { pub addr: SocketAddr, pub pem: PathBuf }` (`pem` is a certificate followed by its key, as `deploy/dev/.secrets/haproxy/server.pem`).
  - `pub struct BrokerConfig { pub tcp: SocketAddr, pub ws: Option<SocketAddr>, pub wss: Option<WssListener>, pub admin: SocketAddr, pub data_dir: PathBuf, pub jwks: JwkSet, pub api_key: String, pub api_secret: Secret }` with `BrokerConfig::from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<BrokerConfig, BrokerError>` reading `S11_TCP`, `S11_WS`, `S11_WSS`, `S11_TLS_PEM` (required when `S11_WSS` is set), `S11_ADMIN`, `S11_DATA_DIR`, `S11_JWKS` (path to a JWKS JSON file), `S11_API_KEY`, `S11_API_SECRET`. `Secret` is a newtype whose `Debug` prints `***`.
  - `#[derive(Clone)] pub struct Broker` with `Broker::start(config: &BrokerConfig) -> Result<Broker, BrokerError>` (returns once every listener is bound).
  - `pub fn split_pem(pem: &str) -> Result<(String, String), BrokerError>` (certificate blocks, key block).
  - `BrokerError` (`thiserror`, `#[non_exhaustive]`), one variant per failure with the variable name or path in its message.
  - Binary `s11-broker`: reads its config from the environment, prints the line `s11-broker ready` to stdout once `Broker::start` returns and the admin listener is bound, and runs until SIGINT or SIGTERM.
  - Test support (`tests/support/mod.rs`): `BrokerProc::start() -> BrokerProc` (fresh temp data folder, free ports for TCP, WS, WSS and admin), `BrokerProc::crash(&mut self)` (SIGKILL, waits for exit), `BrokerProc::stop(&mut self)` (runs `kill -TERM <pid>`, waits up to 10 s), `BrokerProc::start_again(&mut self)` (same ports and folder), fields `tcp`, `ws`, `wss`, `admin: SocketAddr`, `api_key`, `api_secret: String`; plus `admin_url(&self, segments: &[&str]) -> Url` under `/api/v5`.

- [ ] **Step 1: Add RMQTT and check its API**

Add to `spikes/s11-rmqtt/Cargo.toml`: `rmqtt = { version = "=0.24.0", default-features = false, features = ["ws", "tls", "retain", "shared-subscription", "plugin"] }`, `rmqtt-plugins = { version = "=0.24.0", default-features = false, features = ["retainer-sled", "session-storage-sled"] }`, `tokio = { version = "1.54.1", features = ["macros", "rt-multi-thread", "net", "signal", "time", "sync", "fs"] }`, `anyhow = "1.0.104"`, `rustls = { version = "0.23.46", default-features = false, features = ["aws_lc_rs", "std"] }`. Dev-dependencies, as in `spikes/s1-emqx/Cargo.toml`: `rumqttc = { version = "0.25.1", default-features = false, features = ["websocket"] }`, `async-tungstenite = { version = "0.29", default-features = false, features = ["tokio-runtime"] }`, `tokio-rustls = "0.26.6"`, `bytes = "1.12.1"`, `futures-util = "0.3.34"`, `reqwest = { version = "0.13.5", default-features = false, features = ["json"] }`, `url = "2.5.8"`. Run `cargo build -p s11-rmqtt`. If the build needs another `rmqtt` or `rmqtt-plugins` feature, add it and note it in the ADR. Then read the 0.24.0 sources in `~/.cargo/registry/src/*/rmqtt-0.24.0/` and `rmqtt-plugins-0.24.0/` and check every name in Global Constraints "RMQTT API this plan relies on"; write each difference into the ADR's Context.

- [ ] **Step 2: Write the failing integration tests in `spikes/s11-rmqtt/tests/it_s11_broker.rs`**

Port S1's helpers into `tests/support/mod.rs`: `new_id`, `up_topic`, `identity_acl`, `identity_claims`, `hub_claims`, `sign_with`, `sign`, `identity_token` (`spikes/s1-emqx/tests/it_s1_acl.rs:81-165`), `Ev`, `Client`, `connect`, `connect_ok`, `assert_refused`, `Client` methods, `hub_subscriber`, `share_group` (`:197-383`), `raw_publish`, `connect_packet`, `publish_packet`, `encode`, `read_packet`, `install_crypto_provider`, `wss_publish`, `ws_read_packet` (`:796-958`). Each helper that read `AGMEN_IT_MQTT` or `AGMEN_IT_MQTT_WSS` takes the broker's address from a `&BrokerProc` instead, and `wss_publish` trusts `AGMEN_IT_WSS_CA`. `assert_refused` takes the expected `ConnectReturnCode`. `BrokerProc::start` spawns `env!("CARGO_BIN_EXE_s11-broker")` with `kill_on_drop(true)`, `S11_JWKS` = `$AGMEN_IT_JWT_DIR/../jwks/jwks.json`, `S11_TLS_PEM` = the parent of `$AGMEN_IT_WSS_CA`'s folder joined with `haproxy/server.pem`, `S11_API_KEY` = `s11`, a fresh random `S11_API_SECRET`, and waits up to 10 s for `s11-broker ready` on the child's stdout.

Each test starts its own `BrokerProc`. Tests, with A and B fresh ids in tenant `t1`, project `p1`:

- `it_publish_to_own_up_allowed`: as S1's: A publishes QoS 1 to `up/A` over TCP; PUBACK `Success`; a hub subscriber on `$share/<g>/agmen/v1/+/+/up/+` receives the payload `hello` within 5 s.
- `it_websocket_listener`: the same round trip with A over `ws://<ws>/mqtt`.
- `it_wss_listener`: the same round trip with A through `wss_publish` to `wss://localhost:<wss>/mqtt`, trusting only the dev CA: `wss_publish` returns `PubAckReason::Success` and the hub receives `hello`.
- `it_bad_signature_refused`: claims signed with `untrusted.pem` (header `kid` still `agmen-dev-1`): CONNACK `ConnectReturnCode::NotAuthorized` (0x87); the same claims signed with `agmen-dev-1`: CONNACK `Success`.
- `it_client_ids_bound_to_identity`: one token for A connects as `<A>-mcp`, `<A>-watch` and `<A>-cli` at the same time and each one's publish reaches the hub; refused with `ConnectReturnCode::BadUserNamePassword` (0x86): `<B>-mcp`, `<A>`, `<A>-`, `<A>-mcpx`, `<A>-mcp-watch`, `<A>-MCP`, `x<A>-mcp`, `agmen-hub-<x>`; refused with 0x87: A's token under username B on `<B>-mcp`, and A's token under username `agmen-hub` on `agmen-hub-<x>` (the client id binds, `sub` does not match); refused with 0x86: the hub token under username `agmen-hub` on `<A>-mcp`. A client id with a trailing newline is refused (any refusal or a closed socket).
- `it_publish_to_other_up_refused`: A publishes to `up/B`: PUBACK `PubAckReason::NotAuthorized` (0x87); the hub receives nothing within 2 s.
- `it_subscribe_other_tenant_refused`: A's own feed: SUBACK `Success(QoS::AtLeastOnce)`; `agmen/v1/t2/p1/feed`: SUBACK `SubscribeReasonCode::NotAuthorized` (0x87).
- `it_no_acl_claim_is_denied`: as S1's (`spikes/s1-emqx/tests/it_s1_acl.rs:444-478`): every publish and subscribe listed there gets 0x87.
- `it_share_prefix_does_not_widen_acl`: A subscribes to `$share/g/agmen/v1/t1/p1/inbox/<B>` and to `agmen/v1/t1/p1/inbox/+`: both SUBACK 0x87; `$share/g/agmen/v1/t1/p1/feed`: SUBACK `Success(QoS::AtLeastOnce)`.
- `it_will_on_foreign_topic_refused`: `<A>-watch` with a retained QoS 1 will on `presence/B` gets CONNACK 0x87; a subscriber with the hub token on `agmen/v1/t1/p1/presence/+` receives nothing within 2 s. The same client with its will on `presence/A` connects (`Success`).
- `it_packet_over_limit_refused`: CONNACK carries `max_packet_size == Some(98304)`; a 60 KiB publish to `up/A` gets PUBACK `Success` and reaches the hub; a 100 KiB publish through `raw_publish` gets no PUBACK (`raw_publish` returns `None`), and the hub receives nothing within 2 s.
- `it_expiry_disconnects`: a token with `exp` = now + 5 s connects, then gets DISCONNECT `DisconnectReasonCode::NotAuthorized` (0x87) after `exp` and within 10 s (`ConnectionError::MqttState(StateError::ServerDisconnect { reason_code, .. })`, as rumqttc 0.25.1 reports it, ADR-0002).
- `it_hub_token_scope`: as S1's (`:706-741`): the hub token publishes to `inbox/A` and A receives it; publishing to `up/A` or `elsewhere/x` and subscribing to `inbox/+` get 0x87.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it it_`
Expected: FAIL to compile: `CARGO_BIN_EXE_s11-broker` is not defined, because the `s11-broker` binary target does not exist yet.

- [ ] **Step 4: Implement `Broker::start`, `split_pem`, `BrokerConfig::from_lookup` and `main`**

- `Broker::start` writes the two plugin TOML templates into `<data_dir>/plugins/`, with their storage paths set to `<data_dir>/retainer` and `<data_dir>/session` (start each template from the default config file shipped for that plugin in the `rmqtt/rmqtt` repository at the 0.24.0 tag, and change only the storage type to `sled` and its path), builds the `ServerContext` with that plugins folder, registers `retainer` and `session_storage` with `register(&scx, true, false)`, and registers the hooks below.
- Listeners: TCP on `tcp`, WS on `ws`, WSS on `wss` (certificate and key from `split_pem`, written to `<data_dir>/tls/cert.pem` and `<data_dir>/tls/key.pem` with mode 0600), each with `allow_anonymous(false)` and a maximum packet size of `MAX_PACKET`.
- `ClientAuthenticate`: refuse with `NotAuthorized` when `BanList::is_banned`; else `TokenVerifier::verify`, mapping `ClientIdNotBound` to `BadUsernameOrPassword` and `BadToken` to `NotAuthorized`; refuse with `NotAuthorized` when the CONNECT carries a will whose topic, QoS and retain flag `Grant::acl.allows_publish` denies; on success store the `Grant` in a `Mutex<HashMap<String, Grant>>` keyed by client id and answer `Allow(false, Some(AuthInfo { superuser: false, expire_at: Some(exp), rules: vec![] }))`.
- `ClientConnack`: when the username is banned, answer the MQTT 5 reason 0x8A.
- `ClientSubscribeCheckAcl`: `SubscribeReturn::new_failure(NotAuthorized)` unless the stored grant's `allows_subscribe(filter)`; no stored grant means deny.
- `MessagePublishCheckAcl`: `PublishAclResult::rejected(false, None)` unless `allows_publish(topic, qos, retain)`.
- Expiry: drop each client at its `exp` with DISCONNECT 0x87 the way `auth-jwt`'s `disconnect_if_expiry` does it in `rmqtt-plugins` 0.24.0 (read that module; reuse its call). If no RMQTT 0.24.0 call can send 0x87 on that DISCONNECT, record that in the ADR as a failed sub-check of the reason code criterion, with the source file and line.
- `main`: install the aws-lc-rs provider (R46), `BrokerConfig::from_lookup(|name| std::env::var(name).ok())`, `Broker::start`, serve `axum::Router::new()` on `admin` for now (Task 4 adds the routes), print `s11-broker ready`, wait for `tokio::signal::ctrl_c()` or SIGTERM, then return. `main` returns `anyhow::Result<()>` with `.context()` at each step.

Add unit tests in `broker.rs`: `split_pem_separates_cert_and_key` (input: one `CERTIFICATE` block then one `PRIVATE KEY` block; output: each block alone, in that order) and `from_lookup_names_missing_variable` (`S11_TCP` unset: the error message contains `S11_TCP`).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run -p s11-rmqtt --locked` (unit), then `set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it it_`
Expected: PASS, 17 unit tests and 13 integration tests. A test that cannot pass because RMQTT 0.24.0 lacks the behavior stays as written, failing, and goes into the ADR's results with the observed value; do not weaken its assertion.

- [ ] **Step 6: Check licenses and advisories**

Run: `cargo deny check`
Expected: PASS. If it fails only for crates that `rmqtt` or `rmqtt-plugins` bring in, add each advisory to `deny.toml` `[advisories] ignore` as `{ id = "<RUSTSEC id>", reason = "only through rmqtt in spike s11 (docs/adr/NNNN-broker-rmqtt.md)" }` and each license to a `[[licenses.exceptions]]` entry for that crate with the same reason, rerun until it passes, and list every entry in the ADR. Then `git diff Cargo.lock | rg '^-version'` prints nothing; if an existing package moved, record which one and why in the ADR.

- [ ] **Step 7: Start the ADR**

`docs/adr/NNNN-broker-rmqtt.md` (the number from Global Constraints "ADR number"; check again that no other file in `docs/adr/` uses it), from the template: Status `proposed`, the date, Context (spec decision 9, the API differences from Step 1, the time box start date), and a Results table with one row per test so far: test name, what it checks, result. Add a "How to run" section with the Step 5 commands.

- [ ] **Step 8: Commit**

```bash
git add spikes/s11-rmqtt Cargo.lock docs/adr/NNNN-broker-rmqtt.md deny.toml
git commit -m "Embed RMQTT in spike S11 with JWT, client id and ACL hooks"
```

### Task 4: Kick, ban, and the EMQX-shaped admin shim

The hub kicks and bans through `EmqxAdmin` (`crates/agmen-hub/src/emqx.rs:108-182`), and the product tests call more of EMQX's API (`crates/agmen-hub/tests/support/mod.rs:564-652`, `crates/agmen-client/tests/support/mod.rs:249-355`, `crates/agmen-cli/tests/support/mod.rs:225-300`, `tests/e2e/tests/machines/mod.rs:342-370`, all on `mvp/impl` at 0d5d0bf). The shim answers exactly those, so the hub and its tests run unchanged. No caller on 0d5d0bf lists bans (`rg '"banned"|/banned' crates tests` finds only the hub's `POST` and the tests' `DELETE /banned/username/{u}`), and self-host Task 3 now proves a ban by a CONNACK 0x8A reconnect, so the shim serves no `GET /banned`.

**Files:**
- Create: `spikes/s11-rmqtt/src/admin.rs`
- Modify: `spikes/s11-rmqtt/src/broker.rs`, `spikes/s11-rmqtt/src/main.rs`, `spikes/s11-rmqtt/src/lib.rs`, `spikes/s11-rmqtt/Cargo.toml` (`axum = { version = "0.8.9", default-features = false, features = ["tokio", "http1", "json"] }`, `base64 = "0.23.1"`, the version agmen-client already uses, to build the expected `Authorization: Basic` value)
- Modify: `spikes/s11-rmqtt/tests/it_s11_broker.rs`, `docs/adr/NNNN-broker-rmqtt.md`

**Interfaces:**
- Consumes: `Broker`, `BanList`, `BanOutcome` (Tasks 2 and 3); from `agmen-hub` as it is on `mvp/impl` at 0d5d0bf (Global Constraints "Start point"): `agmen_hub::{EmqxAdmin, EmqxConfig, Secret}` with `EmqxAdmin::new(config: &EmqxConfig, http: reqwest::Client)`, `EmqxConfig { api_url, api_key, api_secret }`, `Secret::new(value: impl Into<String>)` (`crates/agmen-hub/src/config.rs:18`); `agmen_hub::{BrokerTokens, SigningKey}` (`crates/agmen-hub/src/broker_tokens.rs:218-305`, `BrokerTokens::new(signing: SigningKey, extra: Vec<SigningKey>)`, `mint_hub(&self, ttl: std::time::Duration) -> Result<String, TokenError>`, `SigningKey::from_pem_file(path)`).
- Produces:
  - `Broker::kick(&self, client_id: &str) -> Result<bool, BrokerError>` (true when the client was connected; it gets DISCONNECT 0x98).
  - `Broker::ban_username(&self, username: &str, until: jiff::Timestamp) -> Result<BanOutcome, BrokerError>`, `Broker::unban_username(&self, username: &str) -> Result<bool, BrokerError>`.
  - `Broker::client(&self, client_id: &str) -> Option<ClientInfo>`, `pub struct ClientInfo { pub connected: bool, pub subscriptions: Vec<(String, u8)> }` (`None` when RMQTT holds no session for that id).
  - `Broker::delete_retained(&self, topic: &str) -> Result<(), BrokerError>`.
  - `pub fn router(broker: Broker, api_key: String, api_secret: Secret) -> axum::Router`, all routes under `/api/v5`, HTTP Basic auth with the key and secret, 401 otherwise:

| Call | Answer |
|---|---|
| `DELETE /clients/{id}` | 204 when connected (and kicked); 204 for an offline session, which stays (EMQX does this, `tests/e2e/tests/machines/mod.rs:299-301` on `mvp/impl` at 0d5d0bf); 404 when no session |
| `GET /clients/{id}` | 200 `{"clientid": id, "connected": bool}`, 404 when no session |
| `GET /clients/{id}/subscriptions` | 200 `[{"topic": filter, "qos": n}]`, 404 when no session |
| `POST /banned` with `{"as": "username", "who", "by", "reason", "until": RFC 3339}` | 200 when added, 400 `{"code": "ALREADY_EXISTS"}` when present, 400 `{"code": "BAD_REQUEST"}` for any `as` but `username` |
| `DELETE /banned/username/{u}` | 204 when removed, 404 when absent |
| `DELETE /mqtt/retainer/message/{topic}` | 204 (the topic is one percent-encoded path segment, slashes included) |

- [ ] **Step 1: Write the failing tests in `spikes/s11-rmqtt/tests/it_s11_broker.rs`**

- `it_rest_kick_disconnects`: `<A>-mcp` connected; `DELETE /api/v5/clients/<A>-mcp`: 204; the client gets DISCONNECT `DisconnectReasonCode::AdministrativeAction` (0x98) within 2 s; then `GET` is 404 and a second `DELETE` is 404 (as S1's, `spikes/s1-emqx/tests/it_s1_acl.rs:508-546`).
- `it_ban_refuses_connect`: `POST /api/v5/banned` for A with `until` = now + 3900 s: 200; repeated: 400 with body code `ALREADY_EXISTS`; `<A>-mcp` with a valid token: CONNACK `ConnectReturnCode::Banned` (0x8A); `<B>-mcp` still connects; `DELETE /api/v5/banned/username/<A>`: 204, then `<A>-mcp` connects; a second `DELETE`: 404.
- `it_ban_survives_broker_crash`: ban A; `BrokerProc::crash`; `BrokerProc::start_again`; `<A>-mcp` with its old, unexpired token gets CONNACK 0x8A.
- `it_admin_lists_client_and_subscriptions`: `<A>-watch` (`clean_start = false`, session expiry 86400) subscribes to `inbox/A` at QoS 1; `GET /clients/<A>-watch`: 200, `connected` true; `GET /clients/<A>-watch/subscriptions`: `[{"topic": "agmen/v1/t1/p1/inbox/<A>", "qos": 1}]`; after a clean DISCONNECT, `GET /clients/<A>-watch`: 404 or `connected` false; for a never-seen id: 404.
- `it_admin_deletes_retained_topic_with_slashes`: the hub token publishes a retained QoS 1 message to `agmen/v1/t1/p1/state/roster`; `DELETE /api/v5/mqtt/retainer/message/agmen%2Fv1%2Ft1%2Fp1%2Fstate%2Froster` (built with `Url::path_segments_mut().extend`, as the product tests build it): 204 or 200; a new subscriber to `agmen/v1/t1/p1/state/#` receives nothing within 2 s.
- `it_admin_requires_basic_auth`: any route without credentials or with a wrong secret: 401.
- `it_hub_emqx_admin_against_shim`: `agmen_hub::EmqxAdmin::new(&EmqxConfig { api_url: <admin>/api/v5, api_key, api_secret: Secret::new(api_secret) }, reqwest::Client::new())` (the signature on `mvp/impl` at 0d5d0bf, `crates/agmen-hub/src/emqx.rs:69`), then `kick("<A>-mcp")` and `ban_username(A, now + 3900 s)` both return `Ok(())` against a connected `<A>-mcp`, which gets 0x98, and its reconnect gets 0x8A. Add `agmen-hub = { path = "../../crates/agmen-hub" }` and `agmen-proto = { path = "../../crates/agmen-proto" }` as dev-dependencies.
- `it_hub_minted_hub_token_scope`: a hub token from `BrokerTokens::new(SigningKey::from_pem_file(<AGMEN_IT_JWT_DIR>/agmen-dev-1.pem).unwrap(), vec![]).unwrap().mint_hub(Duration::from_secs(3600)).unwrap()`, so the claims are the hub's own and not the test helper's copy, connects as `agmen-hub-<x>` with username `agmen-hub`: CONNACK `Success`; SUBACK `Success(QoS::AtLeastOnce)` for `$share/<g>/agmen/v1/+/+/up/+` and for `agmen/v1/+/+/presence/+`; SUBACK 0x87 for `agmen/v1/t1/p1/inbox/+`; a QoS 1 publish to `agmen/v1/t1/p1/inbox/<A>` gets PUBACK `Success`, to `agmen/v1/t1/p1/up/<A>` and to `elsewhere/x` PUBACK 0x87.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it it_rest_kick it_ban it_admin it_hub_emqx it_hub_minted`
Expected: FAIL: every admin call answers 404 (the router has no routes), so `it_rest_kick_disconnects`, `it_ban_refuses_connect`, `it_ban_survives_broker_crash`, and the three `it_admin_` tests fail on their first admin call (`it_admin_requires_basic_auth` gets 404, not 401); `it_hub_emqx_admin_against_shim` fails because no DISCONNECT 0x98 arrives (`EmqxAdmin::kick` counts a 404 as kicked, `crates/agmen-hub/src/emqx.rs:120-122`) and `ban_username` returns `EmqxError::Status { status: 404, .. }`. `it_hub_minted_hub_token_scope` may already pass, since Task 3 built the ACL hooks; it is here because the `agmen-hub` dev-dependency arrives in this task.

- [ ] **Step 3: Implement `Broker::kick`, the ban calls, `Broker::client`, `Broker::delete_retained` and `admin::router`**

`kick` uses `Shared::entry(id)`: `is_connected()` false gives `Ok(false)` (the `DELETE /clients/{id}` route then answers 204 when `Broker::client(id)` is `Some`, else 404); otherwise `kick(true, true, true)` (clean start, clear subscriptions, by admin). The tests decide whether that DISCONNECT carries 0x98; if it does not, record the code RMQTT sends. `ban_username` writes the `BanList` (Task 2), which `ClientAuthenticate` and `ClientConnack` already read; it does not kick (the hub kicks after banning, ADR-0002 "REST ban"). `delete_retained` sets an empty retained message on the topic, which MQTT defines as clearing it, through RMQTT's retain API. `POST /banned` reads `who` and `until` and ignores `by` and `reason`, which the hub sends but nothing reads back. `main` serves `admin::router` on `admin`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: the Step 2 commands, then the whole crate: `cargo nextest run -p s11-rmqtt --locked` and `set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it`
Expected: PASS, 17 unit tests (all from Tasks 1 to 3) and 21 integration tests (13 from Task 3, 8 here), apart from failures already recorded in the ADR.

- [ ] **Step 5: Record and commit**

Add the eight integration rows to the ADR's Results table and a "Kick and ban" section with the answers above.

```bash
git add spikes/s11-rmqtt Cargo.lock docs/adr/NNNN-broker-rmqtt.md
git commit -m "Add kick, a persisted ban and an EMQX-shaped admin shim to spike S11"
```

### Task 5: Shared subscriptions, retained QoS 1, wills, 1-day durable sessions, and state across a restart

**Files:**
- Modify: `spikes/s11-rmqtt/tests/it_s11_broker.rs`
- Modify: `spikes/s11-rmqtt/src/broker.rs` and the plugin templates, only if a test shows a setting is wrong
- Modify: `docs/adr/NNNN-broker-rmqtt.md`

**Interfaces:**
- Consumes: `BrokerProc::{start, crash, stop, start_again}` and the S1 helpers (Task 3).
- Produces: nothing new for later tasks; results rows in the ADR.

- [ ] **Step 1: Write the tests**

- `it_hub_shared_subscription`: two hub clients (`agmen-hub-<x>`, `agmen-hub-<y>`) in one group on `$share/<g>/agmen/v1/+/+/up/+`; A sends 100 QoS 1 publishes to `up/A`; within 10 s all 100 payloads arrive, each exactly once across both clients, and each client gets at least one (as S1's, `spikes/s1-emqx/tests/it_s1_acl.rs:548-575`).
- `it_retained_qos1_delivered_to_late_subscriber`: the hub token publishes `{"v":1}` retained at QoS 1 to `agmen/v1/t1/p1/state/roster`; A then subscribes to `agmen/v1/t1/p1/state/#` at QoS 1 and receives it with `retain` true and `qos` `AtLeastOnce` within 2 s; a retained empty payload on the same topic clears it, and a third subscriber receives nothing within 2 s.
- `it_will_published_on_abrupt_drop`: `<A>-watch` connects with the will agmen-client sets (`crates/agmen-client/src/session.rs:639-646`): topic `presence/A`, payload `{"status":"offline"}`, QoS 1, retained; its socket is dropped without DISCONNECT (abort the poller task); a hub-token subscriber on `agmen/v1/t1/p1/presence/+` receives that payload within 5 s (keep-alive 30 s means the broker may need up to 45 s if it only notices by keep-alive; the test closes the TCP socket so the broker sees EOF at once); a later subscriber gets it as a retained message.
- `it_will_sent_on_disconnect_with_will_message`: the same client sends DISCONNECT 0x04 (`DisconnectWithWillMessage`, what `close_disconnect(true)` sends, `crates/agmen-client/src/session.rs:592-605`): the will arrives within 2 s. With DISCONNECT 0x00: no will within 2 s.
- `it_durable_session_queues_while_offline`: `<A>-watch` with `clean_start = false`, session expiry 86400, subscribes to `inbox/A` at QoS 1, disconnects cleanly; the hub publishes 3 QoS 1 messages to `inbox/A`; `<A>-watch` reconnects: CONNACK `session_present` true, its `session_expiry_interval` property is `None` or `Some(86400)` (the broker did not shorten the day), and it receives the 3 messages in order within 5 s.
- `it_session_expiry_honored`: the same with session expiry 2 s and 4 s offline: CONNACK `session_present` false, nothing received within 2 s.
- `it_durable_state_survives_broker_crash`: as `it_durable_session_queues_while_offline`, plus the hub publishes a retained QoS 1 message to `state/roster`; then `BrokerProc::crash` and `start_again`; `<A>-watch` reconnects with a fresh token: `session_present` true and the 3 messages in order within 5 s; a new subscriber to `state/#` gets the retained message.
- `it_durable_state_survives_broker_stop`: the same with `BrokerProc::stop` in place of `crash`.

- [ ] **Step 2: Run the tests**

Run: `set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it it_hub_shared it_retained it_will_ it_durable it_session_expiry`
Expected: PASS, 9 tests (these 8, plus Task 3's `it_will_on_foreign_topic_refused`, which the `it_will_` filter also selects). A test that fails first because of a broker setting (plugin config, feature flag, flush interval) gets that setting fixed in `broker.rs` or the templates and a rerun; a test that fails because RMQTT 0.24.0 lacks the behavior stays failing and goes into the ADR with the observed values. Run the passing set five times in a row: `for i in 1 2 3 4 5; do cargo nextest run -p s11-rmqtt --locked --profile it || break; done` with the same environment; all five runs pass.

- [ ] **Step 3: Record and commit**

Add the eight rows to the ADR, plus a "Durable state" section: which plugin and storage keep sessions, queued messages and retained messages, where they live under the data folder, and what (if anything) a crash lost compared with a stop.

```bash
git add spikes/s11-rmqtt docs/adr/NNNN-broker-rmqtt.md
git commit -m "Test shared subscriptions, retained QoS 1, wills and durable state on spike S11"
```

### Task 6: agmen-client over MQTT 5 on WSS

The real client, through its own WSS path (`crates/agmen-client/src/session.rs:192-195`, `try_websocket_with_default_tls`, which loads roots through `rustls-native-certs`; that crate reads `SSL_CERT_FILE` in place of the system store, `rustls-native-certs` 0.8.4 `src/lib.rs:8-9`), against the spike broker's own WSS listener and an in-process hub.

**Files:**
- Create: `spikes/s11-rmqtt/tests/it_s11_client.rs`
- Modify: `spikes/s11-rmqtt/Cargo.toml` (dev-dependencies: add `features = ["test-hooks"]` to the `agmen-hub` entry from Task 4, and add `agmen-client = { path = "../../crates/agmen-client" }`, `sqlx = { version = "0.9.0", default-features = false, features = ["runtime-tokio", "postgres"] }`)
- Modify: `docs/adr/NNNN-broker-rmqtt.md`

**Interfaces:**
- Consumes: `BrokerProc` (Task 3) and the admin shim (Task 4); `agmen_hub::{Config, Hub}` with `Config::from_lookup`, `Hub::connect`, `Hub::router`, `Hub::start_messaging`, `Hub::seed_project`, `Hub::seed_identity` (`crates/agmen-hub/src/test_hooks.rs:39-98`); `agmen_client::{Session, SessionConfig, ClientKind, HubClient, Broker, ClientEvent, OutboxState}`; `agmen_hub::{BrokerTokens, SigningKey}` and `BrokerTokens::mint(&self, identity: &Identity, ttl: std::time::Duration) -> Result<String, TokenError>` (`crates/agmen-hub/src/broker_tokens.rs:276`); the hub config as it is on `mvp/impl` at 0d5d0bf (`Config::from_lookup`, which requires `AGMEN_GITHUB_CLIENT_ID`, `AGMEN_GITHUB_CLIENT_SECRET`, `AGMEN_JWT_SIGNING_KEY` and the three `AGMEN_EMQX_API_*` values).
- Produces (test harness in `tests/it_s11_client.rs`, used again by Task 7):
  - `#[derive(Clone)] struct Target { mqtt: agmen_client::Broker, api: Url, api_key: String, api_secret: String }`: the broker the in-process hub and the clients use, and its EMQX-shaped REST API. `Target::spike(broker: &BrokerProc) -> Target` (its TCP listener and admin shim); `Target::from_env() -> Option<Target>`: `Some` when `S11_PROBE_MQTT` is set, then `api` from `S11_PROBE_API` (required with it) and the key and secret from `AGMEN_IT_EMQX_API_KEY` and `AGMEN_IT_EMQX_API_SECRET`.
  - `async fn with_hub<F, Fut>(target: &Target, body: F) where F: FnOnce(HubCtx) -> Fut, Fut: Future<Output = ()> + Send + 'static`: a fresh database, an in-process hub with its own instance id and share group, a fresh project, then `body`; afterwards it deletes the retained `presence/<id>` of every identity the body seeded through `<api>/mqtt/retainer/message/{topic}` and drops the database, also when `body` panics, as the client harness's `run` does (`crates/agmen-client/tests/support/mod.rs:175-247`).
  - `struct HubCtx { hub: Hub, base: Url, topics: Topics, broker: agmen_client::Broker, state: TempDir }` with `HubCtx::identity(&self) -> (Identity, String)` (seeds one through `Hub::seed_identity` and remembers it for the cleanup) and `HubCtx::config(&self, kind: ClientKind, identity: &Identity, token: &str) -> SessionConfig` (as the client harness's `Ctx::config`, `:412-422`, including its 60 s session expiry).

- [ ] **Step 1: Write the failing tests**

The harness copies the database setup of `crates/agmen-client/tests/support/mod.rs:104-147` (a fresh database on `AGMEN_IT_PG`, dropped at the end) and the hub start of `:175-216`, with `AGMEN_MQTT_URL` = `target.mqtt`, `AGMEN_EMQX_API_URL` = `target.api`, and `target.api_key` and `target.api_secret` as `AGMEN_EMQX_API_KEY` and `AGMEN_EMQX_API_SECRET`; `AGMEN_JWT_SIGNING_KEY` stays the dev key and `AGMEN_GITHUB_CLIENT_ID` and `AGMEN_GITHUB_CLIENT_SECRET` get the same placeholder values the client harness uses. Both tests below use `Target::spike` of a fresh `BrokerProc`.

```rust
#[tokio::test(flavor = "multi_thread")]
async fn it_agmen_client_over_wss() {
    assert_eq!(std::env::var("SSL_CERT_FILE").ok(), std::env::var("AGMEN_IT_WSS_CA").ok(),
        "run with SSL_CERT_FILE=\"$AGMEN_IT_WSS_CA\"");
    // broker: BrokerProc::start(); hub: in process; a, b: hub.seed_identity(&topics)
    let wss: Broker = format!("wss://localhost:{}/mqtt", broker.wss.port()).parse().unwrap();
    // b watches over WSS; a publishes an FYI over WSS as Mcp.
    // a's events: Delivery { id, state: OutboxState::Recorded { stream: Stream::Feed, seq: Seq(1) } } within 10 s
    // b's events: ClientEvent::Message whose message id == the FYI's id within 10 s
    // both sessions saw ClientEvent::Connection { connected: true } first
}
```

The FYI comes from a copy of the client harness's `fyi` (`crates/agmen-client/tests/support/mod.rs:86-93`) in this file.

In the same file, the second test checks the identity ACL with a token the hub mints itself, not the test helper's copy of the claims (Task 1):

- `it_hub_minted_identity_token_acl`: identities a and b from `hub.seed_identity(&topics)`, and `base` = `agmen/v1/<tenant>/<project>` of `topics`; a's token is `BrokerTokens::new(SigningKey::from_pem_file(<AGMEN_IT_JWT_DIR>/agmen-dev-1.pem).unwrap(), vec![]).unwrap().mint(&a, Duration::from_secs(3600)).unwrap()`; a connects to the `BrokerProc`'s TCP listener as `<a>-mcp`: CONNACK `Success`. A hub-token subscriber on `$share/<g>/agmen/v1/+/+/up/+` receives a's QoS 1 publish to `topics.up(&a.id)` (PUBACK `Success`) within 5 s and does not receive a's QoS 0 publish to the same topic within 2 s. QoS 1 publish to `topics.up(&b.id)`: PUBACK 0x87. Retained QoS 1 publish to `topics.presence(&a.id)`: PUBACK `Success`; the same without retain: PUBACK 0x87. SUBACK `Success(QoS::AtLeastOnce)` for `topics.feed()`, `topics.inbox(&a.id)`, `topics.ack(&a.id)`, `<base>/state/#` and `<base>/presence/+`; SUBACK 0x87 for `topics.inbox(&b.id)`, `<base>/inbox/+` and `#`.

- [ ] **Step 2: Run the tests to verify they fail, then make them pass**

Run: `set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it it_agmen_client_over_wss it_hub_minted_identity_token_acl`
Expected: PASS, 2 tests, once the harness compiles; Tasks 3 and 4 already built everything the broker side needs, so a failure here is a finding, not missing spike code. A failure caused by the broker (WebSocket path, subprotocol `mqtt`, TLS) is fixed in the spike's listener setup if a setting can fix it, else recorded in the ADR with the client's error.

- [ ] **Step 3: Record and commit**

Add the two rows to the ADR's Results table.

```bash
git add spikes/s11-rmqtt Cargo.lock docs/adr/NNNN-broker-rmqtt.md
git commit -m "Run agmen-client over WSS against spike S11"
```

### Task 7: First acks under concurrent durable sessions (rulings R70 and R71)

Task 15's probe (`/private/tmp/claude-501/-Users-andyaldrich-Projects-agmen/fc9edad9-32e3-4f49-9d23-935593aea4c7/scratchpad/t15/it_probe_stall.rs`, never committed and in a temporary folder that may be gone, so this paragraph restates what it does; it ran inside `crates/agmen-client/tests` with that crate's harness) starts 8 tasks at once; each runs the client harness's `run` (its own database, in-process hub and project), seeds one identity, connects an Mcp `Session`, posts an FYI and waits for that post's first `ClientEvent::Delivery` whose state is not `OutboxState::Pending`. On EMQX 6.3.2 with durable sessions on, 1 to 6 of the 8 waited past 4 s in 5 of 6 batches. This task ports that probe into the spike with two changes the rulings set: each session posts once (the probe posted three times), and a session that has no ack after 5 s counts as stalled and is closed instead of reconnected.

**Files:**
- Modify: `spikes/s11-rmqtt/tests/it_s11_client.rs`
- Modify: `docs/adr/NNNN-broker-rmqtt.md`

**Interfaces:**
- Consumes: `BrokerProc` (Task 3); `Target`, `with_hub`, `HubCtx` (Task 6); `agmen_client::{Session, ClientEvent, ClientKind, OutboxState}`, `Session::connect(config: SessionConfig<T>) -> Result<(Session, Events), ClientError>` and `Session::close(self) -> Result<(), ClientError>` (`crates/agmen-client/src/session.rs:381`, `:558`).
- Produces: nothing new for later tasks; three results in the ADR (RMQTT, EMQX durable sessions off, EMQX durable sessions on).

- [ ] **Step 1: Write the probe test in `spikes/s11-rmqtt/tests/it_s11_client.rs`**

```rust
const SESSIONS: usize = 8;
const BATCHES: usize = 5;
const FIRST_ACK: Duration = Duration::from_secs(5);

/// One Mcp session on a fresh hub: posts one FYI and returns how long its
/// first ack took, or `None` when none came within `FIRST_ACK`.
async fn first_ack(target: Target) -> Option<Duration> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    with_hub(&target, move |ctx| async move {
        let (identity, token) = ctx.identity().await;
        let (session, mut events) =
            Session::connect(ctx.config(ClientKind::Mcp, &identity, &token)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = events.recv().await {
                if event == (ClientEvent::Connection { connected: true }) {
                    return;
                }
            }
            panic!("the session's events ended before it connected");
        })
        .await
        .expect("connected within 10 s");
        let env = fyi("probe");
        let sent = tokio::time::Instant::now();
        session.publish(env.clone()).await.unwrap();
        let waited = tokio::time::timeout_at(sent + FIRST_ACK, async {
            loop {
                match events.recv().await {
                    Some(ClientEvent::Delivery { id, state })
                        if id == env.id && !matches!(state, OutboxState::Pending { .. }) =>
                    {
                        return sent.elapsed();
                    }
                    Some(_) => {}
                    None => panic!("the session's events ended before the first ack"),
                }
            }
        })
        .await
        .ok();
        tokio::time::timeout(Duration::from_secs(3), session.close())
            .await
            .expect("the session closes within 3 s")
            .expect("the session ran without an error");
        tx.send(waited).unwrap();
    })
    .await;
    rx.await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn it_concurrent_durable_first_ack() {
    let spike; // keeps the spike broker alive for the whole test
    let target = match Target::from_env() {
        Some(target) => target,
        None => {
            spike = BrokerProc::start().await;
            Target::spike(&spike)
        }
    };
    let mut clean = 0;
    for batch in 1..=BATCHES {
        let tasks: Vec<_> = (0..SESSIONS).map(|_| tokio::spawn(first_ack(target.clone()))).collect();
        let mut waits = Vec::with_capacity(SESSIONS);
        for task in tasks {
            waits.push(task.await.unwrap());
        }
        let stalled = waits.iter().filter(|w| w.is_none()).count();
        eprintln!(
            "s11-probe {} batch {batch}: {} of {SESSIONS} first acks within 5 s, slowest {:?}",
            target.mqtt, SESSIONS - stalled, waits.iter().flatten().max()
        );
        if stalled == 0 {
            clean += 1;
        }
    }
    assert_eq!(clean, BATCHES, "batches in which every first ack came within 5 s");
}
```

`fyi` is the client harness's (`crates/agmen-client/tests/support/mod.rs:87-93`), copied into the file in Task 6. Every batch runs even after a stalled one, so each run prints all five batch lines for the ADR. Every wait is bounded (connect 10 s, ack 5 s, close 3 s), so a run takes well under the `it` profile's limit of 120 s per test (`slow-timeout` 30 s, `terminate-after` 4, `.config/nextest.toml:14`).

- [ ] **Step 2: Run it on RMQTT**

Run: `set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it --no-capture it_concurrent_durable_first_ack`
Expected: PASS, 1 test, and five `s11-probe` lines each reading `8 of 8`. A stalled batch is a finding: leave the assertion as it is and record the five lines.

- [ ] **Step 3: Run the same probe on EMQX 6.3.2 with durable sessions off**

Build both configs from the dev config as it is at 0d5d0bf, which differ in one line, so nothing but `durable_sessions.enable` changes:

```bash
mkdir -p target/s11-emqx
git show 0d5d0bf:deploy/dev/emqx/emqx.conf >target/s11-emqx/ds-on.conf
sed 's/^  enable = true$/  enable = false/' target/s11-emqx/ds-on.conf >target/s11-emqx/ds-off.conf
diff target/s11-emqx/ds-on.conf target/s11-emqx/ds-off.conf
```

Expected: `diff` prints exactly one changed line, `enable = true` to `enable = false`, inside `durable_sessions { }` (the only `enable` key in that file).

Start a throwaway EMQX on the spike's ports with the off config, the dev API key file and the dev JWKS service (`host.docker.internal:8090`, read only), wait until it is up, and check the setting:

```bash
set -a; . deploy/dev/.secrets/dev.env; set +a
docker run -d --rm --name agmen-s11-emqx \
  -p 127.0.0.1:11884:1883 -p 127.0.0.1:18184:18083 \
  -v "$PWD/target/s11-emqx/ds-off.conf:/opt/emqx/etc/emqx.conf:ro" \
  -v "$PWD/deploy/dev/.secrets/api-keys.txt:/opt/emqx/etc/agmen/api-keys.txt:ro" \
  --add-host host.docker.internal:host-gateway emqx/emqx:6.3.2
for i in $(seq 1 30); do docker exec agmen-s11-emqx /opt/emqx/bin/emqx ctl status && break; sleep 2; done
docker exec agmen-s11-emqx /opt/emqx/bin/emqx ctl conf show durable_sessions
```

Expected: `emqx ctl status` succeeds within 60 s, and the config shows `enable = false`. `emqx ctl conf show` is an EMQX 5 command not yet checked on 6.3.2; if it is missing, record that and rely on the one-line `diff`. Then, in the same shell:

```bash
S11_PROBE_MQTT=mqtt://localhost:11884 S11_PROBE_API=http://localhost:18184/api/v5 \
  cargo nextest run -p s11-rmqtt --locked --profile it --no-capture it_concurrent_durable_first_ack
docker stop agmen-s11-emqx
```

Expected: the five `s11-probe` lines, whatever they say; the test fails if any batch stalled, and that is a result to record, not a spike failure. `docker stop` removes the container (`--rm`), so nothing of it is left.

- [ ] **Step 4: Run the control on EMQX 6.3.2 with durable sessions on**

Repeat Step 3's `docker run`, wait, check and probe commands with `ds-on.conf` in place of `ds-off.conf` (the check shows `enable = true`), then `docker stop agmen-s11-emqx`.
Expected: at least one stalled batch, as Task 15 saw. If all five batches are clean, run the control twice more; if it never stalls in 15 batches, the ported probe does not reproduce the stall, and the R70 criterion row in Task 8 says "Not tested" with that reason, whatever RMQTT showed.

- [ ] **Step 5: Record and commit**

Add to the ADR a section "First acks under concurrent durable sessions (R70, R71)": the probe's conditions (8 sessions, one post each, 5 s, 5 batches, 60 s session expiry as the client harness sets it), the five `s11-probe` lines of each of the three runs, and one sentence for R71: whether EMQX with durable sessions off stalled. Add the RMQTT run to the Results table.

```bash
git add spikes/s11-rmqtt docs/adr/NNNN-broker-rmqtt.md
git commit -m "Probe first acks under concurrent durable sessions on S11 and on EMQX"
```

### Task 8: The product `it` profile on RMQTT, and the verdict

**Files:**
- Create: `spikes/s11-rmqtt/compose.yaml`
- Create: `spikes/s11-rmqtt/toxiproxy.json`
- Modify: `spikes/s11-rmqtt/src/admin.rs`, only for a shim answer that differs from EMQX's
- Modify: `docs/adr/NNNN-broker-rmqtt.md` (results, how to run, verdict, `Status: accepted`)

**Interfaces:**
- Consumes: the `s11-broker` binary and its environment (Task 3), the admin shim (Task 4), the results of Tasks 3 to 7 in the ADR.
- Produces (for `docs/plans/hosted.md`, which finds the file as `docs/adr/*-broker-rmqtt.md`): `docs/adr/NNNN-broker-rmqtt.md` with `Status: accepted` and, under Decision, exactly one line `Verdict: pass` or `Verdict: fail`, followed by the failed or untested criteria, if any, each with its test name and observed value.

- [ ] **Step 1: Write the spike's own toxiproxy**

The product client tests cut the link through toxiproxy's proxy named `emqx_mqtt` (`crates/agmen-client/tests/support/mod.rs:485`), and the dev stack's proxy points at EMQX. So the spike runs its own, under compose project `agmen-s11`:

`spikes/s11-rmqtt/compose.yaml`: one service `toxiproxy`, `image: ghcr.io/shopify/toxiproxy:2.13.0` (the dev stack's pinned tag), `command: ["-host=0.0.0.0", "-config=/config/toxiproxy.json"]`, ports `127.0.0.1:18474:8474` and `127.0.0.1:31883:31883`, the JSON mounted read-only, `extra_hosts: ["host.docker.internal:host-gateway"]`.

`spikes/s11-rmqtt/toxiproxy.json`: `[{"name": "emqx_mqtt", "listen": "0.0.0.0:31883", "upstream": "host.docker.internal:11883", "enabled": true}]`.

- [ ] **Step 2: Baseline on EMQX**

Run two commands, as ruling R71 sets the e2e gate (`agmen-e2e` alone):

```bash
set -a; . deploy/dev/.secrets/dev.env; set +a
cargo nextest run -p agmen-hub -p agmen-client -p agmen-cli --locked --profile it
cargo nextest run -p agmen-e2e --locked --profile it
```

Expected: all pass. Record the sum of both counts as N in the ADR. `agmen-e2e` is `tests/e2e` (added on `mvp/impl` in 2329e12); the `it` profile runs its tests one at a time (`.config/nextest.toml:29`). If a test fails, rerun it alone with the same environment (`cargo nextest run -p <package> --locked --profile it <test name>`). If it passes alone, record it in the ADR as an EMQX failure under concurrency (the R71 stall shows this way) and go on; on RMQTT that test must still pass in every run. If it also fails alone, stop and report it: the baseline must be green before RMQTT is compared with it.

- [ ] **Step 3: Start the spike broker and its toxiproxy**

```bash
set -a; . deploy/dev/.secrets/dev.env; set +a
docker compose -f spikes/s11-rmqtt/compose.yaml up -d
cargo build -p s11-rmqtt --bin s11-broker --locked
mkdir -p target/s11-data
S11_TCP=127.0.0.1:11883 S11_WS=127.0.0.1:18093 S11_ADMIN=127.0.0.1:18183 \
S11_WSS=127.0.0.1:18443 S11_TLS_PEM=deploy/dev/.secrets/haproxy/server.pem \
S11_DATA_DIR=target/s11-data S11_JWKS=deploy/dev/.secrets/jwks/jwks.json \
S11_API_KEY="$AGMEN_IT_EMQX_API_KEY" S11_API_SECRET="$AGMEN_IT_EMQX_API_SECRET" \
  target/debug/s11-broker >target/s11-broker.log 2>&1 &
echo $! >target/s11-broker.pid
```

Expected: `target/s11-broker.log` contains `s11-broker ready` within 10 s. The WSS listener uses the dev stack's server certificate (`deploy/dev/init.sh:96`, issued for `localhost` by the dev CA), so clients that trust `AGMEN_IT_WSS_CA` trust it. The binary runs directly, not under `cargo run`, so the saved pid is the broker's own.

- [ ] **Step 4: Run the product `it` profile against RMQTT**

In the same shell as Step 3:

```bash
AGMEN_IT_MQTT=mqtt://localhost:11883 AGMEN_IT_MQTT_PROXIED=mqtt://localhost:31883 \
AGMEN_IT_MQTT_WS=ws://localhost:18093/mqtt AGMEN_IT_MQTT_WSS=wss://localhost:18443/mqtt \
AGMEN_IT_TOXIPROXY=http://localhost:18474 AGMEN_IT_EMQX_API=http://localhost:18183/api/v5 \
  sh -c 'cargo nextest run -p agmen-hub -p agmen-client -p agmen-cli --locked --profile it &&
         cargo nextest run -p agmen-e2e --locked --profile it'
```

`deploy/dev/init.sh:24-31,102-104` writes the dev stack's values. On `mvp/impl` at 0d5d0bf, `rg -o --no-filename 'AGMEN_IT_[A-Z_]+' crates tests | sort -u` prints ten names: five broker addresses, which the command above points at the spike (`AGMEN_IT_MQTT`, `AGMEN_IT_MQTT_PROXIED`, `AGMEN_IT_MQTT_WSS`, `AGMEN_IT_TOXIPROXY`, `AGMEN_IT_EMQX_API`), and five that stay as the dev env sets them (`AGMEN_IT_PG`, `AGMEN_IT_JWT_DIR`, `AGMEN_IT_WSS_CA`, `AGMEN_IT_EMQX_API_KEY`, `AGMEN_IT_EMQX_API_SECRET`). `AGMEN_IT_MQTT_WS` is in the dev env (`deploy/dev/init.sh:27`) but no test reads it on 0d5d0bf; it points at the spike anyway, so a test that starts reading it does not reach EMQX. Before the run, run that `rg` again; if it prints a broker address variable not set above, add it pointing at the spike, or no test that reads it counts toward the criterion.

Expected: N passed, 0 failed, with the same two commands as the baseline. Run them three times; all three must pass, including any test the baseline recorded as an EMQX failure under concurrency. For a failure, read the test and the broker log: if the shim answers differently from EMQX (status, body field, path), fix `admin.rs` to match EMQX and rerun; if RMQTT behaves differently (reason code, ordering, a missing will, a lost session), leave the product crates alone and record the test, the assertion and the observed value.

- [ ] **Step 5: Stop the spike stack**

```bash
kill "$(cat target/s11-broker.pid)"
docker compose -f spikes/s11-rmqtt/compose.yaml down
```

Expected: `docker compose -f spikes/s11-rmqtt/compose.yaml ps` lists nothing; the dev stack's services are as they were (`docker compose -f deploy/dev/compose.yaml ps` shows the same services running as before Step 3).

- [ ] **Step 6: Write the verdict**

In `docs/adr/NNNN-broker-rmqtt.md`: the Results table complete (every spike test plus the three product runs with their counts), a criteria table with one row per pass criterion from the spec (copied verbatim) plus one row for "No first-ack stall under concurrent durable sessions (rulings R70 and R71)" (Task 7: Pass only if the RMQTT run had 5 of 5 clean batches and the control reproduced the stall), the tests that prove each, and Pass, Fail or Not tested; the "How to run" section with the commands of Steps 1 to 5, of Task 3 Step 5 and of Task 7 Steps 2 to 4; `Verdict: pass` only if every criterion row says Pass, else `Verdict: fail`. Consequences: on pass, what the hosted plan must build to move the broker into `agmen-hub` (the hooks, the ban list fed from `identities.revoked_at`, the admin calls turned into in-process calls, the plugin storage on the hub's volume) and every `deny.toml` exception, noting that each one is scoped "only through rmqtt in spike s11" and must be re-scoped to `agmen-hub` (or removed with the spike) when hosted Task 14 moves the code there; that hosted Task 14 defines its own `AGMEN_IT_BROKER=embedded` harness switch, and that this spike's EMQX-shaped shim (`spikes/s11-rmqtt/src/admin.rs`) is one way for the hub's test support to reach the embedded broker's admin calls; on fail, the spec's fallback (EMQX Dedicated Flex) and what would make the spike worth repeating (an RMQTT release that fixes the failed rows). Set `Status: accepted`.

- [ ] **Step 7: Run the full gate**

Run each command in Global Constraints "Gate before each commit".
Expected: each exits 0.

- [ ] **Step 8: Commit**

```bash
git add spikes/s11-rmqtt docs/adr/NNNN-broker-rmqtt.md
git commit -m "Record the RMQTT spike verdict in its ADR"
```
