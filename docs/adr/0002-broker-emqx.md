# 0002: Broker identity and ACLs on EMQX

Status: accepted
Date: 2026-10-25

## Context

Design 6.3 requires that a client can publish only to its own `up/{identity_id}`, that one person's agents can never post as another's, that revocation takes effect on live connections, and that no guard fails open. Adam chose EMQX on 2026-10-25 (design 4.3). Spike S1 (MVP plan Task 2) tests those requirements against `emqx/emqx:6.3.2` in the dev stack. The tests are in `spikes/s1-emqx/tests/it_s1_acl.rs`.

## Decision

Keep EMQX, single node, under its Community License. EMQX prints this at boot: "To enable clustering or use EMQX for external commercial purposes, a commercial license is required." That covers the internal MVP and the dogfood round. Outside teams (M6) or a cluster need a commercial license or a different broker (design 9).

### Authentication

One JWT authenticator in JWKS mode, set in read-only `emqx.conf`. With no authenticator, EMQX lets anonymous clients in (ADR-0001), so it must never be missing.

```hocon
authentication = [
  {
    mechanism = jwt
    use_jwks = true
    endpoint = "<hub>/v1/jwks"
    refresh_interval = 60
    from = password
    on_missing_jwt = deny
    disconnect_after_expire = true
    acl_claim_name = acl
    verify_claims {
      sub = "${username}"
      clientid = "${clientid}"
    }
  }
]
authorization { no_match = deny, deny_action = ignore, sources = [] }
durable_sessions { enable = true }
```

Each token is bound to **one identity and one client id**. The hub mints a separate token for each connection (`<id>-mcp`, `<id>-watch`, the hub's own clients). EMQX `verify_claims` only checks exact equality, so a prefix rule is not possible. One token per client id is simpler anyway.

### Token claims

ES256, `kid` from the JWKS, 1 hour TTL in production:

```json
{
  "sub": "<identity_id>",
  "clientid": "<identity_id>-mcp",
  "iat": 1761400000,
  "exp": 1761403600,
  "acl": [
    { "permission": "allow", "action": "publish",   "topic": "eq agmen/v1/<t>/<p>/up/<id>", "qos": [1] },
    { "permission": "allow", "action": "publish",   "topic": "eq agmen/v1/<t>/<p>/presence/<id>", "retain": true },
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/<t>/<p>/feed" },
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/<t>/<p>/inbox/<id>" },
    { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/<t>/<p>/ack/<id>" },
    { "permission": "allow", "action": "subscribe", "topic": "agmen/v1/<t>/<p>/state/#" },
    { "permission": "allow", "action": "subscribe", "topic": "agmen/v1/<t>/<p>/presence/+" },
    { "permission": "deny",  "action": "all",       "topic": "#" }
  ]
}
```

Topics are written in full by the hub, with `eq` on every exact topic. No `${username}` placeholders are used, so an id can never widen a rule. Ids are hub-minted `[a-z0-9]{8,32}` (protocol 1), and `agmen-proto` rejects `/`, `+` and `#` (MVP Task 6).

The hub's own clients get:

```json
[
  { "permission": "allow", "action": "subscribe", "topic": "agmen/v1/+/+/up/+" },
  { "permission": "deny",  "action": "publish",   "topic": "agmen/v1/+/+/up/#" },
  { "permission": "allow", "action": "publish",   "topic": "agmen/v1/#" },
  { "permission": "deny",  "action": "all",       "topic": "#" }
]
```

They subscribe with `$share/hub/agmen/v1/+/+/up/+`, so several hub replicas split the load.

### Revocation

The hub stops minting tokens for the identity and kicks its client ids with `DELETE /api/v5/clients/{clientid}` (basic auth with the hub's API key). The kicked client gets DISCONNECT `AdministrativeAction` (0x98). A token that is not refreshed ends the session at `exp` with DISCONNECT `NotAuthorized` (0x87).

## Results

EMQX 6.3.2, rumqttc 0.25.1, run 4 times on 2026-10-25 with all tests passing each time (the restart test failed once before the reconnect retry described below was added).

| Test | Checks | Result |
|---|---|---|
| `it_publish_to_own_up_allowed` | A publishes to `up/A`; the hub receives it | PUBACK Success, delivered |
| `it_publish_to_other_up_refused` | A publishes to `up/B` | PUBACK NotAuthorized (0x87); the hub receives nothing |
| `it_subscribe_other_tenant_refused` | A subscribes to another tenant's `feed` | SUBACK NotAuthorized |
| `it_wildcard_subscriptions_cannot_widen_acl` | `#`, `agmen/#`, `<t>/<p>/#`, `<t>/<p>/+`, `up/+`, `up/A`, `inbox/+`, `ack/+`, `<t>/+/feed`, `+/<p>/feed`, `$share/g/.../inbox/+` | All NotAuthorized; the 6 allowed filters succeed |
| `it_no_acl_claim_is_denied` | Valid signature, no `acl` claim | Connects; every publish and subscribe NotAuthorized (`no_match = deny`) |
| `it_bad_signature_refused` | Wrong key with the right kid; unknown kid; expired token | All refused at CONNACK |
| `it_username_must_match_sub` | A's token with B's username | Refused at CONNACK |
| `it_token_bound_to_clientid` | A `-mcp` token used as `-watch` | Refused at CONNACK |
| `it_expiry_disconnects` | Token with `exp` 5 s ahead | DISCONNECT NotAuthorized after 4.5 to 4.8 s (claims are in whole seconds) |
| `it_rest_kick_disconnects` | REST `DELETE /clients/{id}` | 204; DISCONNECT AdministrativeAction within 0.1 s |
| `it_hub_shared_subscription` | 2 hub clients on `$share/hub/...`, 100 publishes | 50 and 50, none lost or doubled |
| `it_persistent_session_survives_restart` | `clean_start = false`, expiry 24 h, 3 QoS 1 messages queued, broker restarted | With `durable_sessions` on: `session_present = true`, all 3 delivered in order. Off (the default): session and messages lost. Logins are refused for about 8 s after `docker compose restart` returns, so the test retries like a real client |
| `it_packet_over_limit_refused` | 60 KiB accepted; 100 KiB through rumqttc | rumqttc refuses locally, because EMQX advertises 98,304 bytes in CONNACK |
| `it_broker_enforces_packet_limit` | 100 KiB over a raw socket, bypassing the client check | EMQX closes the socket; nothing reaches the hub |
| `it_websocket_listener` | Same as the first test over `ws://localhost:8083/mqtt` | Pass |
| `it_wss_behind_haproxy` | `wss://` through HAProxy 3.4.7 terminating TLS and forwarding to 8083 with `timeout tunnel 3600s` (the cluster pattern in `groupdrive/Kubernetes` `dev-env/emqx`) | Connects; publish to own `up/` succeeds and to another's is refused |

## Consequences

- **Fails closed:** a missing claim, a missing `acl`, a mismatched username or client id, a bad signature and an expired token are all refused.
- **Two settings are load-bearing:** `on_missing_jwt = deny` and `sources = []`. EMQX 6.3.2 ships a file authz source by default, and `no_match = deny` alone would not remove it.
- **Durable sessions** must be on in production too. Replay over HTTP still covers gaps (protocol 5.3); the broker only narrows how often readers need it.
- **The hub mints one token per connection**, so MVP Task 7's `BrokerTokens::mint` takes the client id.
- **The JWKS endpoint is a hard dependency for new connections.** If EMQX cannot fetch keys, no one can log in. That fails closed, but the hub's `/v1/jwks` must be highly available, and EMQX caches keys between refreshes (60 s).
- **rumqttc 0.25.1** pulls `rustls-webpki` 0.102.8 (RUSTSEC-2026-0049, -0098, -0099, -0104, -0382) and the unmaintained `rustls-pemfile` (RUSTSEC-2025-0134), and has had no release since 2025-11-21. `deny.toml` ignores those six IDs for the spike only. Spike S2 must pick a client without them and remove the ignores.
- **Revisit** if we need a cluster, outside tenants (license), or more than one broker node for availability.
