# 0002: Broker identity and ACLs on EMQX

Status: accepted
Date: 2026-10-25
Amended: 2026-10-26 (ruling R71: durable sessions off)

## Context

The MVP broker is EMQX on a single node, Adam's choice on 2026-10-25 (design section 4.3, MVP plan). Design section 6.3 requires that a client can publish only to its own `up/{identity_id}`, that one person's agents can never post as another's, that revocation takes effect on live connections, and that no guard fails open. Spike S1 checks that EMQX can carry agmen's identity model on its own: a hub-signed JWT per identity, an `acl` claim that names that identity's topics in full, a deny when nothing matches, a disconnect at expiry, a REST kick for revocation, a shared subscription for the hub, a packet size cap, and MQTT over WebSocket behind HAProxy. Task 7 (`BrokerTokens::mint`, `GET /v1/jwks`, `EmqxAdmin::kick`) and Task 8 (the hub's MQTT client) build on what this ADR records.

This replaces the Mosquitto ADR planned in m0-m1 Task 2. The tests are `spikes/s1-emqx/tests/it_s1_acl.rs`, run against `deploy/dev` on `emqx/emqx:6.3.2`. Behavior marked "from source" was read in the EMQX 6.3.2 tag on GitHub (`apps/emqx_auth_jwt/src/emqx_authn_jwt.erl`, `apps/emqx_auth_cinfo/src/emqx_authn_cinfo.erl`, `apps/emqx/src/emqx_channel.erl`, `apps/emqx_auth/src/emqx_authz/emqx_authz_rule_raw.erl`, `apps/emqx_auth/src/emqx_authn/emqx_authn_utils.erl`) and not tested.

## Decision

### License

EMQX 5.9 and later ship under BSL 1.1. Its Additional Use Grant allows production use of **a single node**, as long as we do not offer EMQX "to third parties on a hosted or embedded basis" (`LICENSE` in the 6.3.2 tag). EMQX also prints at boot: "To enable clustering or use EMQX for external commercial purposes, a commercial license is required." `GET /api/v5/license` on the dev node reports `"type": "community"`, `"max_connections": 10000000`. That covers our own team on one node. Before outside teams join (M6), or before we cluster, we need a commercial license or another broker (design section 9, open question "EMQX license before outside teams").

### Identity token

The hub signs one ES256 JWT per identity. Header: `{"alg": "ES256", "typ": "JWT", "kid": "<signing key id>"}`. Claims, with every topic written in full (no `${username}` or `${clientid}` placeholders, so an id can never widen a rule):

```json
{
  "sub": "<identity_id>", "username": "<identity_id>", "exp": <unix seconds, now + 3600>,
  "acl": [
    { "permission": "allow", "action": "publish",   "topic": "eq agmen/v1/<t>/<p>/up/<identity_id>", "qos": [1] },
    { "permission": "allow", "action": "publish",   "topic": "eq agmen/v1/<t>/<p>/presence/<identity_id>", "qos": [1], "retain": true },
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/<t>/<p>/feed" },
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/<t>/<p>/inbox/<identity_id>" },
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/<t>/<p>/ack/<identity_id>" },
    { "permission": "allow", "action": "subscribe", "topic": "agmen/v1/<t>/<p>/state/#" },
    { "permission": "allow", "action": "subscribe", "topic": "agmen/v1/<t>/<p>/presence/+" },
    { "permission": "deny",  "action": "all",       "topic": "#" }
  ]
}
```

Identity ids are hub-minted `[a-z0-9]{8,32}` (`IdentityId` in `crates/agmen-proto/src/ids.rs`, protocol conventions), so an id never holds `/`, `+` or `#` and cannot widen a topic. The client connects with username = identity id and password = the token. Rules the hub must keep:

- **Always set `exp`.** EMQX checks `exp` only when the claim is present; a token without it never expires and is never disconnected (from source).
- **Never mint `is_superuser`, `clientid_override` or `client_attrs`.** EMQX honors all three from a verified token (from source): `is_superuser: true` skips the ACL, and `clientid_override` replaces the client id the kick relies on.
- **End every `acl` with the deny-all rule**, even though `no_match = deny` also denies. A token missing the claim gets nothing (`it_no_acl_claim_is_denied`).
- `"qos": [1]` on `up/` and `presence/` means a QoS 0 publish there is denied; `"retain": true` on `presence/` means only retained publishes are allowed there (from source, `emqx_authz_rule_raw.erl`; the default for both is "any"). QoS 1 only on `presence/` lets the broker's inflight window bound the presence the hub has not acked yet, as it does for `up/`; a QoS 0 flood there could otherwise outrun the hub.

### Client ids: three per identity, one token

An identity connects only as `<id>-mcp` (the MCP server), `<id>-watch` (`agmen watch`) or `<id>-cli` (one-shot CLI commands: clean start, no persistent session). All three use the same token at the same time without taking over each other's session (`it_client_ids_bound_to_identity`).

**A payload claim check on `${clientid}` cannot enforce this.** `verify_claims` compares each claim to its rendered template for exact equality (from source: `do_verify_claims` in `emqx_authn_jwt.erl`), and a probe against a scratch EMQX 6.3.2 with `verify_claims { sub = "${username}", clientid = "${clientid}" }` gave:

| Token `clientid` claim | Client id | Result |
|---|---|---|
| `"<id>-mcp"` | `<id>-mcp` | Connected |
| `"<id>-mcp"` | `<id>-watch` | CONNACK 0x86 |
| `"<id>"` (prefix) | `<id>-mcp` | CONNACK 0x86 |
| `["<id>-mcp", "<id>-watch", "<id>-cli"]` | `<id>-watch` | CONNACK 0x86 |
| none | `<id>-mcp` | CONNACK 0x86 |

So that route needs one token per client id. Instead, a **Client-Info (`cinfo`) authenticator** runs before the JWT one and binds the client id to the username; the JWT authenticator then binds the username to `sub`. A cinfo match returns `ignore` (on to the JWT check, never `allow`), and the last check denies everything else. Expressions that fail to evaluate, such as a missing username, count as no match, so they fall through to the deny (from source).

### Hub token

The hub connects with username `agmen-hub` and client id `agmen-hub-<instance>` (`<instance>` is `[a-z0-9]{1,32}`, one per replica). The `-` keeps the name outside the identity id alphabet. Claims:

```json
{
  "sub": "agmen-hub", "username": "agmen-hub", "exp": <now + 3600>,
  "acl": [
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/+/+/up/+" },
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/+/+/presence/+" },
    { "permission": "deny",  "action": "publish",   "topic": "agmen/v1/+/+/up/#" },
    { "permission": "allow", "action": "publish",   "topic": "agmen/v1/#" },
    { "permission": "deny",  "action": "all",       "topic": "#" }
  ]
}
```

The hub subscribes with `$share/hub/agmen/v1/+/+/up/+`. EMQX authorizes a shared subscription against the topic filter without the `$share/<group>/` prefix (from source: `do_check_sub_authzs2` in `emqx_channel.erl`), so the rule above allows any group name and a plain subscription too; the ACL cannot pin the group. The tests use one group per test so parallel tests do not split each other's messages. The hub may publish anywhere under `agmen/v1/` except `up/`, and nowhere else (`it_hub_token_scope`).

MVP Task 9 added the `presence/+` rule. The hub builds each project's retained `state/roster` from presence, so it subscribes to `agmen/v1/+/+/presence/+` in the same SUBSCRIBE packet as `up/`, as a plain subscription: every hub replica must see every presence change, and the broker sends a plain subscriber every retained presence after each connect. The hub reads nothing else; `state/`, `feed`, `inbox/` and `ack/` stay closed to it. `hub_acl_matches_adr_0002` in `broker_tokens.rs` checks the claim above rule for rule, and the presence tests in `crates/agmen-hub/tests/it_replay.rs` connect a hub with it.

### EMQX config

`deploy/dev/emqx/emqx.conf`, mounted read-only. The cluster manifests must carry the same blocks in their read-only config:

```hocon
mqtt { max_packet_size = 96KB }

authentication = [
  {
    mechanism = cinfo
    checks = [
      { is_match = "str_eq(clientid, concat([username, '-mcp']))", result = ignore },
      { is_match = "str_eq(clientid, concat([username, '-watch']))", result = ignore },
      { is_match = "str_eq(clientid, concat([username, '-cli']))", result = ignore },
      {
        is_match = ["str_eq(username, 'agmen-hub')", "regex_match(clientid, '^agmen-hub-[a-z0-9]{1,32}$')"]
        result = ignore
      },
      { is_match = "true", result = deny }
    ]
  },
  {
    mechanism = jwt
    use_jwks = true
    endpoint = "http://host.docker.internal:8090/v1/jwks"   # cluster: the hub Service
    refresh_interval = 60
    from = password
    on_missing_jwt = deny
    disconnect_after_expire = true
    acl_claim_name = acl
    verify_claims { sub = "${username}" }
  }
]

authorization { no_match = deny, deny_action = ignore, sources = [] }

durable_sessions { enable = false }
```

### Durable sessions off (ruling R71)

Spike S1 first turned `durable_sessions` on, so that `clean_start = false` sessions with an expiry and their queued QoS 1 messages survive a broker restart. Ruling R71 turns it off again, because EMQX 6.3.2's durable session storage stalls deliveries (Task 15s):

- **Measured.** With the same harness on fresh EMQX 6.3.2 brokers that differ only in `durable_sessions.enable`, 358 of 640 sessions (56%) stalled at 32 projects x 2 sessions with it on (5,018 of 9,600 messages never delivered within 30 s), and 0 of 1,136 sessions stalled with it off, from 4 x 2 through 64 x 2 (17,040 messages, worst latency 47 ms). A stall is all or nothing for one subscription: its messages never arrive until something forces EMQX to rescan its streams.
- **Root cause.** A bug in EMQX's filter-against-filter trie search, `emqx_trie_search:matches_filter/3` (`apps/emqx/src/emqx_trie_search.erl`, the same in 6.3.2 and master on 2026-10-26). While the storage layer is still learning a topic structure it creates a new stream per new tenant or project and notifies the sessions whose filters match it. The search ends early when another connected session's filter sorts before the match and diverges later, so the matching session never learns of the stream that holds its messages. It happens only while EMQX learns a topic shape (a new broker, a wiped data volume, new tenants and projects, a new topic shape), and only next to other connected sessions.

With durable sessions off, EMQX keeps persistent sessions in memory: `clean_start = false` and the clients' session expiry still apply, and a reconnect within the expiry resumes the session and its queue (at most `max_mqueue_len` 1000 messages, `max_inflight` 32). **A broker restart now drops every session and the messages queued for it.** agmen-client recovers them: on every reconnect it replays each watched stream from the hub, from its own cursors, and its outbox resends every post the hub has not acknowledged. `it_offline_session_replays_after_restart` checks this across a real restart.

### JWKS

EMQX fetches the JWKS every `refresh_interval` (60 s) and tries every key in it, whatever the token's `kid` says (from source: `do_verify` walks the key list). For rotation, Task 7 publishes the new key in the JWKS at least 60 s before signing with it, and keeps the old key until the last token it signed has expired (1 hour).

In dev and tests (ruling R2), one shared JWKS outlives every test process: the `jwks` compose service (`nginx:1.30.6-alpine`, `deploy/dev/jwks/nginx.conf`) serves `deploy/dev/.secrets/jwks/jwks.json` at `http://localhost:8090/v1/jwks`, which EMQX reaches as `host.docker.internal:8090`. No test binds 8090. `deploy/dev/init.sh` generates three P-256 keys into the gitignored `deploy/dev/.secrets/jwt/` (`agmen-dev-1.pem` and `agmen-dev-2.pem`, both in the JWKS so a rotation test can switch keys while EMQX accepts both, and `untrusted.pem`, never published) and rebuilds `jwks.json` from the first two on every run. The key id is the file name without `.pem`. `dev.env` gets `AGMEN_IT_JWT_DIR` pointing at that directory. In dev, Task 7's hub signs with these keys and the `jwks` service keeps serving them; in the cluster, EMQX reads the hub's own `/v1/jwks` by Service name.

### REST kick

```
DELETE /api/v5/clients/{clientid}
Authorization: Basic base64(<api key>:<api secret>)
```

204 when the client was connected; it gets DISCONNECT 0x98 (administrative action) at once, and `GET /api/v5/clients/{clientid}` returns 404 afterwards. 404 when the client is not connected, which the hub treats as success. Client ids are `[a-z0-9-]`, so they need no URL encoding. Revocation kicks all three client ids: `<id>-mcp`, `<id>-watch` and `<id>-cli`. The API key comes from `api_key.bootstrap_file` (`deploy/dev/.secrets/api-keys.txt`, key name `agmen-hub`, role administrator).

### REST ban

A kick alone does not cut a revoked identity off: EMQX checks only the token's signature and `exp`, so the kicked client can reconnect with its unexpired broker token and read again. Revocation therefore also bans the identity's username (ruling R36, `EmqxAdmin::ban_username`):

```
POST /api/v5/banned
Authorization: Basic base64(<api key>:<api secret>)
{"as": "username", "who": "<id>", "by": "agmen-hub", "reason": "identity revoked", "until": "<RFC 3339>"}
```

`until` is the stored revocation time plus the broker token TTL (1 hour) plus 5 minutes, so the ban outlasts every token minted before the revocation. A banned username gets CONNACK 0x8A (banned) on CONNECT, whatever its token. 200 is success; 400 with code `ALREADY_EXISTS` also counts as success, so a repeated revocation reuses the first ban.

Order: the hub commits `revoked_at` first, so the identity can get no fresh broker token; then it bans the username; then it kicks the three client ids. Banning before kicking means a kicked client that reconnects at once is already refused. If the ban fails, the kicks still run and the revocation call answers 502, so a retry bans again. Once `until` has passed, every old token has expired, so a late retry skips the ban and only kicks. Task 7's `it_revoke_kicks_and_refuses_refresh` checks this against EMQX 6.3.2: after revocation, reconnecting each of the three client ids with the old token gets 0x8A, and another identity of the same owner still connects. Spike S1 did not test the ban.

### Observed broker behavior

- Expiry: at `exp`, EMQX sends DISCONNECT 0x87 (not authorized) and closes.
- Bad signature: CONNACK 0x87; the broker log says `authentication_failure ... reason: not_authorized`. A client id or username that fails the binding gets CONNACK 0x86.
- A client id ending in a newline is dropped before authentication: the socket closes with no CONNACK.
- Packet over 96 KB: EMQX closes the socket (log `{shutdown,emsgsize}`) with no DISCONNECT 0x95 and no PUBACK. rumqttc never sends such a packet: it reads the limit from CONNACK and fails the publish locally: `EventLoop::poll` returns `OutgoingPacketTooLarge` and drops the connection. The client must check envelope size (64 KiB) before publishing.
- rumqttc 0.25.1 reports a server DISCONNECT as `ConnectionError::MqttState(StateError::ServerDisconnect { reason_code, .. })`, not as an incoming packet.
- A QoS 1 publish to a topic nobody subscribes to gets PUBACK 0x10 (no matching subscribers), which is a success.
- Timings from adam's run (9866443): a token with `exp` 5 s ahead was disconnected after 4.5 to 4.8 s, because claims are whole seconds; a REST kick produced DISCONNECT 0x98 within 0.1 s; 100 publishes to a two-member share group split 50 and 50.
- After `docker compose restart emqx` returns, EMQX refuses logins for about 8 s (adam's run). A client reconnecting after a broker restart must retry; agmen-client's link task does after its first connect (1 s doubling to 30 s, `crates/agmen-client/src/link.rs`).

### WSS behind HAProxy

The m0-m1 check said "TLS passthrough to 8080", but EMQX's 8083 listener is plain WebSocket, and the cluster's HAProxy ingress terminates TLS with cert-manager certificates (design section 6.9). So the dev check terminates TLS too: the `haproxy` compose service (`haproxy:3.4.7-alpine`, `deploy/dev/haproxy/haproxy.cfg`, HTTP mode, `timeout tunnel 1h`) listens on 8443 with a certificate from a dev CA that `init.sh` creates (`.secrets/tls/ca.pem`, `.secrets/haproxy/server.pem`, SAN `localhost` and `127.0.0.1`) and forwards to `emqx:8083`. `it_wss_behind_haproxy` connects to `wss://localhost:8443/mqtt` (`AGMEN_IT_MQTT_WSS`), trusting only `AGMEN_IT_WSS_CA`, publishes to its `up/`, and the hub receives it; the HAProxy log shows the `wss~` frontend answering `GET /mqtt` with 101. Task 3's `it_wss_transport` reuses this service.

That test uses a small raw client (tokio-rustls, async-tungstenite 0.29 and rumqttc's packet codec), not rumqttc. rumqttc 0.25.1, the newest release, can only do WSS with its `use-rustls` or `use-rustls-no-provider` feature, and both pull `rustls-webpki 0.102.8` and `rustls-pemfile 2.2.0`, which fail `cargo deny check advisories`: RUSTSEC-2026-0049, -0098, -0099, -0104 and -0382 on webpki 0.102.8, and RUSTSEC-2025-0134 (unmaintained) on rustls-pemfile. rumqttc uses webpki 0.102 only for its `webpki::Error` type (`src/tls.rs`); certificate checks run in rustls 0.23.46 on webpki 0.103.16. So the spike depends on rumqttc with `default-features = false, features = ["websocket"]`, and async-tungstenite is pinned to 0.29, the version rumqttc already pulls, so the tree holds one tungstenite. **Task 3 must settle this before agmen-client ships WSS on rumqttc**: an advisory ignore with that reasoning, a patched rumqttc, or another client.

## Results

`cargo nextest run -p s1-emqx --profile it` (14 tests) and `--profile it-restart` (1 test), on `emqx/emqx:6.3.2`.

RED, before this spike changed `emqx.conf` (JWKS already served): `it_client_ids_bound_to_identity` failed (`<B>-mcp` connected with A's token), and `it_persistent_session_survives_restart` (since replaced, see below) failed (`session_present` false after the restart). The first runs also showed that EMQX closes an oversized packet without DISCONNECT and that rumqttc reports DISCONNECT as an error; those tests were corrected to assert what EMQX does.

| Test | Checks | Result |
|---|---|---|
| `it_publish_to_own_up_allowed` | A publishes QoS 1 to `up/A`; a hub-token subscriber on `$share/<g>/agmen/v1/+/+/up/+` receives it | Pass |
| `it_publish_to_other_up_refused` | A publishes to `up/B`: PUBACK 0x87, nothing reaches the hub within 2 s | Pass |
| `it_subscribe_other_tenant_refused` | A's own feed: SUBACK granted QoS 1; `agmen/v1/t2/p1/feed`: SUBACK 0x87 | Pass |
| `it_wildcard_subscriptions_cannot_widen_acl` | A as `<A>-watch` subscribes to `#`, `agmen/#`, `agmen/v1/<t>/<p>/#`, `agmen/v1/<t>/<p>/+`, `up/+`, `up/A`, `inbox/+`, `ack/+`, `agmen/v1/<t>/+/feed`, `agmen/v1/+/<p>/feed`, `agmen/v1/+/+/state/#`, `agmen/v1/<t>/+/presence/+` and `$share/g/agmen/v1/<t>/<p>/inbox/+`: each SUBACK 0x87; its own `feed`, `inbox/A`, `ack/A`, `state/head`, `state/#` and `presence/+`: granted QoS 1 | Pass |
| `it_no_acl_claim_is_denied` | Valid token without `acl`: publishes to `up/A`, `presence/A`, `feed` and `other` (retained or not) get 0x87; subscribes to `feed`, `inbox/A`, `ack/A`, `state/#`, `presence/+` and `#` get 0x87 | Pass |
| `it_bad_signature_refused` | Same claims signed with `untrusted.pem` (header `kid` still `agmen-dev-1`): CONNACK refused; signed with the dev key with `exp` 600 s in the past: CONNACK refused; signed with the dev key: connects | Pass |
| `it_expiry_disconnects` | `exp` = now + 5 s: dropped after `exp` and within 10 s, with DISCONNECT 0x87 | Pass |
| `it_rest_kick_disconnects` | `DELETE /api/v5/clients/<A>-mcp`: 204, DISCONNECT 0x98 within 2 s, then GET is 404 and a second DELETE is 404 | Pass |
| `it_hub_shared_subscription` | Two hub clients in one group, 100 QoS 1 publishes: all 100 arrive once, both clients get some | Pass |
| `it_offline_session_replays_after_restart` | Replaces `it_persistent_session_survives_restart`, which asserted the broker-side persistence R71 removes. Through agmen-client and an in-process hub: a watch session (`clean_start = false`, expiry 300 s) gets feed seq 1 live and closes; feed seq 2 to 4 and a DM are recorded while it is offline; `docker compose restart emqx`; EMQX no longer lists the session; the watch reconnects and gets feed seq 2 to 4 in order and the DM by replay, and nothing twice within 2 s | Not yet run against a restart (R71 run pending). With the restart replaced by a clean-start connect that discards the session: pass; with no session loss at all, the test fails as meant (the broker still lists the session) |
| `it_packet_over_limit_refused` | 60 KiB publish: PUBACK success and the hub receives it; 100 KiB: rumqttc fails locally, a raw client gets the socket closed and the hub receives nothing | Pass |
| `it_websocket_listener` | `it_publish_to_own_up_allowed` with A over `ws://localhost:8083/mqtt` | Pass |
| `it_wss_behind_haproxy` | A raw MQTT client over `wss://localhost:8443/mqtt` through HAProxy: CONNACK success, PUBACK success, the hub receives it; a second connection as `<A>-cli` publishing to `up/B`: PUBACK 0x87 | Pass |
| `it_client_ids_bound_to_identity` | One token: `-mcp`, `-watch` and `-cli` connected together, each publish reaches the hub; refused: `<B>-mcp`, `<A>`, `<A>-`, `<A>-mcpx`, `<A>-mcp-watch`, `<A>-MCP`, `x<A>-mcp`, `agmen-hub-<x>`, a newline suffix, A's token under username B, A's token as `agmen-hub`, and the hub token on `<A>-mcp` | Pass |
| `it_hub_token_scope` | Hub token publishes to `inbox/A` (A receives it); publishing to `up/A` or `elsewhere/x` and subscribing to `inbox/+` get 0x87 | Pass |

Adam ran Spike S1 independently (commit 9866443, 2026-10-25, rumqttc 0.25.1, 16 tests, 4 runs, all passing) and agrees with these results on every test both runs share. Its wildcard filter test, its expired-token case and its ACL check through HAProxy are kept above, and its timings are under "Observed broker behavior". Its unknown-`kid` case is not kept: EMQX tries every JWKS key whatever the `kid` says (see JWKS), so it repeats the untrusted-key case. Two of its decisions are superseded: one token per client id through a `clientid` claim (see "Client ids"), and durable sessions on (R71).

### Test isolation

Every test mints its own identity ids (`id` plus 8 random `[a-z0-9]`), client ids and share group, so the `it` profile runs them 4 at a time; five consecutive runs passed. `it_offline_session_replays_after_restart` restarts EMQX, which would cut every other test's connection, so `.config/nextest.toml` keeps any `it_` test with `restart` in its name out of the `it` profile and runs it under `--profile it-restart` with one thread. It calls `docker compose -f deploy/dev/compose.yaml restart emqx`, so it needs the Docker CLI where the tests run.

## Consequences

- Task 7 mints the two token shapes above, serves `/v1/jwks` with the rotation rule above, and on revocation bans the identity's username, then kicks three client ids, not two (see "REST ban"). Task 8 connects as `agmen-hub-<instance>` with the hub claims.
- The authentication chain has two authenticators, and both must stay in read-only config. Removing cinfo lets any client id use a valid token; removing both lets anonymous clients in (ADR-0001).
- Two settings are load-bearing: `on_missing_jwt = deny` and `sources = []`. EMQX 6.3.2 ships a file authz source by default, and `no_match = deny` alone would not remove it.
- The JWKS endpoint is a hard dependency for new connections. If EMQX cannot fetch keys, no one can log in. That fails closed, but the hub's `/v1/jwks` (in the cluster) and the `jwks` compose service (in dev) must be up whenever clients connect.
- Durable sessions are off (R71): a broker restart drops every session and its queued messages, and each client pays one replay pass from the hub when it reconnects. Turn them on again only on an EMQX release that fixes `matches_filter`, after the Task 15s stall harness (`spikes/s5-ds-stall`, commit `68e20c3` on branch `mvp/t15s`) shows no stalls with them on.
- rumqttc's TLS features fail `cargo deny` today (see WSS above); Task 3 decides how agmen-client gets WSS.
- CI runs `--profile it-restart` in the Jenkins Integration stage, after the `it` run and before the stack is torn down. `dockerd` in the `docker` container also listens on a socket in an `emptyDir` shared with the `rust` container (`ci/kubePod.yaml`), and the `rust` container installs the Docker CLI and the compose plugin as static binaries, each checked against a sha256 pinned per architecture in the `Jenkinsfile`, then runs the profile with `DOCKER_HOST` set to that socket.
- Not tested here: whether a kick also discards a persistent session's queue, and token behavior with no `exp` (from source only).
- Revisit if EMQX's license terms change, at a second node, or if a later EMQX version changes `verify_claims` or cinfo semantics.
