# 0007: RMQTT embedded in the hub as the broker for agmen hosted

Status: accepted
Date: 2026-10-26

The spike's code (`spikes/s11-rmqtt`) and its four `deny.toml` exceptions live only on branch `spike/rmqtt` (3174d6e), not on `main`. Check that branch out to run anything under "How to run".

## Context

Spec decision 9 (`docs/specs/2026-10-26-onboarding-and-hosting-design.md`, "The broker for agmen hosted: a spike decides"): "agmen hosted cannot run EMQX for customers under its free license. Before hosted sign-ups open, a time-boxed spike tests RMQTT (MIT or Apache 2.0, MQTT 5, WebSocket and TLS, library mode, an auth hook, a kick API) against agmen's real needs." It passes only if all of these hold:

- `agmen-client` works over MQTT 5 on WSS.
- An auth hook verifies the hub's JWT in process.
- Client ids are bound to identities (today's cinfo rule).
- Each token's topic permissions apply, with everything else denied.
- Packets are capped at 96 KB.
- Shared subscriptions, retained QoS 1 messages, wills and 1-day durable sessions work.
- Kick and ban work.
- The reason codes the clients rely on (0x87, 0x8A, 0x98) come back as expected.
- The hub's `it` test profile passes with RMQTT in place of EMQX.
- Durable sessions and retained state survive a restart of the hub process.

Rulings R70 and R71 add one more: 8 concurrent durable `agmen-client` Mcp sessions each get their first ack within 5 s, in 5 of 5 batches.

The controller's security review of the spike adds another: a resumed durable session delivers nothing, on a subscription or from its queue, that the token of the new connection does not allow.

The spike is `spikes/s11-rmqtt`: a library that embeds `rmqtt` 0.24.0 and `rmqtt-plugins` 0.24.0 (both `MIT OR Apache-2.0`, pinned with `=0.24.0`) with agmen's own hooks, and a binary, `s11-broker`, that the integration tests start as a child process on free ports with its own data folder. The time box is 5 working days from the start of Task 1 on Monday 2026-10-26, so it ends on Friday 2026-10-30. A criterion not shown to pass by then counts as failed.

### RMQTT 0.24.0 API, checked against the plan

The plan named the API from the `rmqtt/rmqtt` master branch. Read in the 0.24.0 sources (`rmqtt-0.24.0`, `rmqtt-net-0.5.0`, `rmqtt-codec-0.3.1`, `rmqtt-plugins-0.24.0` and the plugin crates it re-exports), these names exist as the plan uses them: `ServerContext::new()...build().await`, `MqttServer::new(scx).listener(..).build()`, `Builder::new().name(..).laddr(..).allow_anonymous(false).max_packet_size(..).bind()?.tcp()?/.ws()?/.wss()?`, `rmqtt_plugins::<name>::register(&scx, true, false)`, `Register::add(Type, Box<dyn Handler>)`, `Handler::hook(&self, &Parameter, Option<HookResult>) -> ReturnType`, the hook types `ClientAuthenticate`, `ClientConnack`, `ClientSubscribeCheckAcl`, `MessagePublishCheckAcl` and `ClientConnected`, `AuthResult::{Allow, BadUsernameOrPassword, NotAuthorized}` (plus `NotFound`), `AuthInfo { superuser, expire_at: Option<Duration>, rules }`, `SubscribeReturn::new_failure`, `PublishAclResult::rejected(IsDisconnect, Option<ByteString>)`, and the `Shared::entry(id)` methods `kick(clean_start, clear_subscriptions, is_admin)`, `is_connected()`, `exist()` and `subscriptions()`. The features `retainer-sled` and `session-storage-sled` exist. The differences:

1. **Shared subscriptions need a plugin.** The `rmqtt` feature `shared-subscription` alone does not parse `$share/`. `Subscribe::from_v5` parses the prefix only when `scx.extends.shared_subscription().is_supported()` (`rmqtt-0.24.0/src/session.rs:1175-1178`, `types.rs:947-958`), and the default `DefaultSharedSubscription` keeps the trait default `is_supported() -> false` (`subscribe.rs:74-76`, `103-107`). So `rmqtt-plugins` also needs its feature `shared-subscription` (crate `rmqtt-shared-subscription` 0.24.0), registered like the other plugins. That feature resolves only with `lru` 0.18, which the lockfile gained through `cargo update -p rmqtt-plugins` (it added `lru` 0.18.5 and `rmqtt-shared-subscription` 0.24.0 and moved nothing else).
2. **The `ClientConnack` hook cannot change the CONNACK code.** On success, `_handshake` calls `client_connack(.., Success)` and drops the answer (`let _ =`, `rmqtt-0.24.0/src/v5.rs:364-370`; v3: `v3.rs:327`). On a refusal, `process` calls `refused_ack(&scx, &mut sink, None, ack_code, ..)` (`v5.rs:96`), and with `connect_info = None` the hook is skipped (`v5.rs:462-466`; v3: `v3.rs:369`). `AuthResult` has no banned variant, so a banned username gets 0x87 from `ClientAuthenticate`, not 0x8A. The spike registers the `ClientConnack` answer anyway, as the plan says. Task 4 measured it: a banned client gets 0x87 (`it_ban_refuses_connect`, `it_ban_survives_broker_crash`).
3. **A server-side close sends no DISCONNECT.** When the session loop ends with a reason (expiry, kick), the session first calls `sink.close()` (`rmqtt-0.24.0/src/session.rs:214`), which is `Framed::close` and shuts down the write half of the socket (`rmqtt-net-0.5.0/src/stream.rs:595-610`). Only afterwards does it call `send_disconnect(d)` and ignore its error (`session.rs:266-277`). `Message` has no variant that sends a DISCONNECT and keeps the socket open: the one that would has been commented out (`types.rs:2005`, `// Disconnect(Disconnect),`). `it_expiry_disconnects` confirms it: the client sees the socket close (rumqttc `StateError::ConnectionAborted`) with no DISCONNECT packet. This fails the 0x87-at-expiry sub-check of the reason code criterion, and 0x98 on kick fails the same way (`it_rest_kick_disconnects`).
4. **No token expiry in the core.** `AuthInfo::expire_at` is read only by `rmqtt-auth-jwt`, whose `disconnect_if_expiry` runs in the `ClientKeepalive` hook, that is only when the client sends a packet, and closes with `Disconnect::Other("JWT Auth expired")`, which maps to 0x00 (`rmqtt-auth-jwt-0.24.0/src/lib.rs:388-399`, `rmqtt-0.24.0/src/types.rs:2561`). The spike sends the same `Message::Closed` through the same `Shared::entry(id).tx()` from a timer set at `ClientConnected` for the token's `exp`, with `Disconnect::V5` and reason `NotAuthorized`, after checking `Entry::id_same()` so a late timer cannot close a newer connection with the same client id.
5. **Hooks are off until started.** `HookEntry::new` sets `enabled: false` (`hook.rs:604-606`); only `Register::start()` enables them (`hook.rs:884-886`).
6. **`MqttServer::run()` never returns; `start()` exits the process on failure.** Listeners bind synchronously in `Builder::bind()` (`rmqtt-net-0.5.0/src/builder.rs:494-530`), so `Broker::start` binds every listener and then calls `MqttServer::start()`, which spawns `run()` and calls `std::process::exit(1)` if it fails (`server.rs:181-188`).
7. **`listener()` keys a listener by its port** and panics on a duplicate (`server.rs:118-145`), so two listeners on port 0 would collide. The spike uses `listener_by_id` with fixed ids 1, 2 and 3.
8. **TLS takes file paths.** `Builder::tls_cert` and `tls_key` take paths read with `from_pem_file` (`builder.rs:557-562`), so `Broker::start` splits `S11_TLS_PEM` into `<data_dir>/tls/cert.pem` and `key.pem` (mode 0600).
9. **A retained will needs retain storage.** A will with retain set is refused with 0x9A unless retain storage is enabled (`v5.rs:240-259`); the retainer plugin enables it.
10. **Plugin config files are named after the plugin crate**: `rmqtt-retainer.toml` and `rmqtt-session-storage.toml` in the plugins folder (`plugin.rs:749-783`). `spikes/s11-rmqtt/plugins/` holds the two files from `rmqtt-plugins/` at the 0.24.0 tag, changed only in `storage.type = "sled"` (retainer) and each `storage.sled.path`, which `Broker::start` points under the data folder. The shared subscription plugin runs on its defaults.
11. **A busy check can refuse CONNECT.** `ServerContext` enables `busy_check_enable` by default, and a CONNECT is refused with 0x88 while the 1-minute load average is over 80 or CPU load over 90% (`v5.rs:132-137`, `node.rs:67-70`). The spike keeps the default.
12. **Every DISCONNECT discards the will, 0x04 included.** Any DISCONNECT sets `StateFlags::DisconnectReceived` (`session.rs:835-845`), and `last_will_enable` is false whenever that flag is set, whatever the reason code (`session.rs:871-876`). MQTT 5 defines 0x04 (Disconnect with Will Message) as the client asking the server to publish the will. `it_will_sent_on_disconnect_with_will_message` confirms it.
13. **A client's DISCONNECT does not end the connection.** The packet handler returns `Ok(())` after a DISCONNECT, and the session loop goes on reading (`session.rs:392-401`), so the connection lasts until the client closes the socket or keep-alive runs out. A raw client that sent DISCONNECT 0x00 and kept its socket open saw no close within 10 s. MQTT 5 says the server SHOULD close the connection if the client has not (section 3.14.4), and EMQX does. rumqttc 0.25.1 relies on it: after its DISCONNECT, its event loop keeps running until the broker closes the connection. So every product test that waits for that close fails on RMQTT (see "The product `it` profile on RMQTT", cause 1). agmen-client's own `Session::close` waits for its link task up to 5 s (`CLOSE_WAIT`, `crates/agmen-client/src/session.rs`); whether it takes the full 5 s on RMQTT was not measured.
14. **Listeners leave SO_REUSEADDR unset.** `Builder::reuseaddr` defaults to `None` (`rmqtt-net-0.5.0/src/builder.rs:204`, `502-504`). A new `s11-broker` on the same ports right after a crash or a stop failed to start with "Address already in use", because the old connections sat in TIME_WAIT. `Broker::start` now sets `reuseaddr(Some(true))`.
15. **The sled storage keeps the tokio runtime from dropping.** rmqtt-storage serves each sled database from a `spawn_blocking` task that loops on a channel and never returns (`rmqtt-storage-0.11.1/src/storage_sled.rs`, `SledStorageDB::new`), and dropping a tokio runtime waits for every blocking task. With `#[tokio::main]`, `s11-broker` was still running 90 s after SIGTERM, and a startup error hung the process before it printed the error. `s11-broker` now builds its runtime itself and ends it with `shutdown_timeout`.
16. **sled writes to disk every 500 ms, and nothing makes it write sooner.** `SledConfig::to_sled_config` sets only the path, the cache capacity and `Mode::HighThroughput` (`storage_sled.rs:304-315`), so sled keeps its default `flush_every_ms = Some(500)` (`sled-0.34.7/src/config.rs:238`). rmqtt-storage has no flush call, and the plugin TOML files have no setting for it. See "Durable state".

No other `rmqtt` or `rmqtt-plugins` feature was needed for the build.

### Dependencies

`cargo deny check` failed only for crates that `rmqtt` and `rmqtt-plugins` bring in, so `deny.toml` gained, each with the reason "only through rmqtt in spike s11 (docs/adr/0007-broker-rmqtt.md)":

- `[advisories] ignore`: `RUSTSEC-2023-0089` (`atomic-polyfill` 1.0.3, unmaintained, through `rust-box`, `task-exec-queue`, `heapless` and `postcard`), `RUSTSEC-2024-0384` (`instant` 0.1.13, unmaintained, through `parking_lot` 0.11 in `sled`), `RUSTSEC-2025-0057` (`fxhash` 0.2.1, unmaintained, through `sled`).
- `[[licenses.exceptions]]`: `systemstat` 0.2.7 under `Unlicense` (rmqtt's CPU load check, `node.rs`).

No package already in `Cargo.lock` changed version: comparing the lockfile's name and version pairs with `d73f808` shows only additions. The `-version` lines that `git diff Cargo.lock | rg '^-version'` prints are hunk alignment, where a new version of a crate is inserted next to the existing one.

## Decision

Verdict: fail

RMQTT 0.24.0 embedded in the hub does not meet spec decision 9, so agmen hosted does not run the hub with an embedded RMQTT broker and takes the spec's fallback (see Consequences). Four criteria failed; none is untested:

- "Shared subscriptions, retained QoS 1 messages, wills and 1-day durable sessions work.": RMQTT never sends the will on DISCONNECT 0x04 (Context, item 12), which agmen-client sends when it closes a presence session (`close_disconnect`). `it_will_sent_on_disconnect_with_will_message`: no will within 2 s after 0x04, while a dropped socket gets it within 5 s. `it_watch_presence_follows_the_connection` (agmen-client): after `close(session)` the roster still did not list the identity offline 20 s later, in all three runs. Shared subscriptions, retained QoS 1, the will on a dropped connection and 1-day durable sessions pass.
- "The reason codes the clients rely on (0x87, 0x8A, 0x98) come back as expected.": 0x87 comes back in CONNACK, PUBACK and SUBACK, but never as a DISCONNECT, and 0x8A and 0x98 never come back (Context, items 2 and 3). `it_expiry_disconnects`: the connection ends at `exp` with no DISCONNECT (rumqttc `StateError::ConnectionAborted`). `it_rest_kick_disconnects` and `it_hub_emqx_admin_against_shim`, and the hub's `it_revoke_kicks_and_refuses_refresh` and `it_revoke_bans_and_kicks_when_the_roster_publish_fails`: no DISCONNECT 0x98 (`None` where `Some(AdministrativeAction)` was expected), only a closed socket. `it_ban_refuses_connect`, `it_ban_survives_broker_crash` and `it_hub_emqx_admin_against_shim`: CONNACK 0x87 (`NotAuthorized`) where 0x8A (`Banned`) was expected.
- "The hub's `it` test profile passes with RMQTT in place of EMQX.": 91, 91 and 92 of N = 108 tests passed in the three runs, against 108 of 108 on EMQX 6.3.2 (see "The product `it` profile on RMQTT").
- "Durable sessions and retained state survive a restart of the hub process.": they survive SIGTERM (`it_durable_state_survives_broker_stop`), but `it_durable_state_survives_broker_crash` lost all of it after SIGKILL: CONNACK `session_present` false, none of `m1`, `m2` and `m3`, and no retained message. sled writes every 500 ms, after the PUBACK (see "Durable state").

### Criteria

| Criterion | Tests | Result |
|---|---|---|
| "`agmen-client` works over MQTT 5 on WSS." | `it_agmen_client_over_wss`, `it_wss_listener` | Pass |
| "An auth hook verifies the hub's JWT in process." | `good_token_gives_grant_with_acl`, `untrusted_key_refused`, `expired_token_refused`, `token_without_exp_refused`, `sub_must_equal_username`, `it_bad_signature_refused`, `it_hub_minted_identity_token_acl`, `it_hub_minted_hub_token_scope` | Pass |
| "Client ids are bound to identities (today's cinfo rule)." | `client_ids_bound_to_username`, `it_client_ids_bound_to_identity` | Pass |
| "Each token's topic permissions apply, with everything else denied." | the `acl_*` unit tests, `it_publish_to_other_up_refused`, `it_subscribe_other_tenant_refused`, `it_no_acl_claim_is_denied`, `it_share_prefix_does_not_widen_acl`, `it_will_on_foreign_topic_refused`, `it_hub_token_scope`, `it_hub_minted_hub_token_scope`, `it_hub_minted_identity_token_acl` | Pass |
| "Packets are capped at 96 KB." | `it_packet_over_limit_refused` | Pass |
| "Shared subscriptions, retained QoS 1 messages, wills and 1-day durable sessions work." | `it_hub_shared_subscription`, `it_retained_qos1_delivered_to_late_subscriber`, `it_will_published_on_abrupt_drop`, `it_will_sent_on_disconnect_with_will_message`, `it_durable_session_queues_while_offline`, `it_session_expiry_honored`; agmen-client's `it_watch_presence_follows_the_connection` | Fail: no will on DISCONNECT 0x04 |
| "Kick and ban work." | `it_rest_kick_disconnects`, `it_ban_refuses_connect`, `it_ban_survives_broker_crash`, `it_hub_emqx_admin_against_shim` | Pass: a kicked client is dropped within 2 s and its session is gone; a banned username is refused, after a SIGKILL too; `agmen_hub::EmqxAdmin` drives both unchanged. These tests fail only on the codes, which the next row counts |
| "The reason codes the clients rely on (0x87, 0x8A, 0x98) come back as expected." | `reason_codes`, `it_expiry_disconnects`, `it_rest_kick_disconnects`, `it_ban_refuses_connect`, `it_ban_survives_broker_crash`, `it_hub_emqx_admin_against_shim`; the hub's `it_revoke_kicks_and_refuses_refresh` and `it_revoke_bans_and_kicks_when_the_roster_publish_fails` | Fail: no DISCONNECT 0x87 at expiry, no DISCONNECT 0x98 on kick, 0x87 instead of 0x8A for a ban |
| "The hub's `it` test profile passes with RMQTT in place of EMQX." | the product `it` profile, three runs | Fail: 91, 91 and 92 of 108 |
| "Durable sessions and retained state survive a restart of the hub process." | `it_durable_state_survives_broker_stop`, `it_durable_state_survives_broker_crash`, `it_ban_survives_broker_crash` | Fail: a SIGKILL loses the writes of the last 0.5 s or so |
| "No first-ack stall under concurrent durable sessions (rulings R70 and R71)" | `it_concurrent_durable_first_ack` | Pass: 5 of 5 clean batches in the RMQTT run (15 of 15 in three runs on fresh brokers, 15 of 15 on one long-lived broker), and the control on EMQX 6.3.2 with durable sessions on stalled in 3 of 15 batches |
| A resumed durable session delivers nothing, on a subscription or from its queue, that the token of the new connection does not allow (controller security review) | `it_resumed_subscription_needs_current_grant`, `it_queued_message_needs_current_grant` | Pass, with the spike's own checks on resume and on every delivery (see "Subscriptions of a resumed session") |

## Results

Unit tests run with `cargo nextest run -p s11-rmqtt --locked`; integration tests (`it_*`) run each against their own `s11-broker`.

| Test | What it checks | Result |
|------|----------------|--------|
| `acl_identity_publish_rules` | An identity may publish QoS 1 to its own `up/` and retained to its own `presence/`, nothing else | pass |
| `acl_identity_subscribe_rules` | An identity may subscribe to its feed, inbox, ack, `state/#` and `presence/+`, not to another's inbox, `up/`, `#` or another tenant | pass |
| `acl_wildcard_request_not_covered_by_eq_rule` | A requested `+` or `#` is not covered by an `eq` rule; `$share/<g>/` is stripped before the check | pass |
| `acl_hub_rules` | The hub may subscribe to every `up/` and `presence/` and publish under `agmen/v1/` except `up/` | pass |
| `acl_missing_claim_denies_everything` | A token without `acl` may do nothing | pass |
| `acl_malformed_claim_is_an_error` | An `acl` that is not a list, or a rule with a bad field, is an error | pass |
| `acl_rule_shapes_outside_the_format_are_errors` | Unknown fields, QoS 3, misplaced wildcards, empty topics and unknown actions are errors | pass |
| `acl_subscribe_applies_a_constrained_rule_only_when_it_denies` | A `qos` or `retain` constrained rule applies to SUBSCRIBE only when it denies, as in EMQX | pass |
| `topic_matches_follows_mqtt_wildcards` | A topic filter matches a topic name with MQTT's `+` and `#` (`#` also the parent level), after `$share/<g>/` is stripped | pass |
| `client_ids_bound_to_username` | ADR-0002's cinfo rule: `<id>-mcp`, `-watch`, `-cli`, and `agmen-hub-[a-z0-9]{1,32}` for the hub, nothing else (trailing newline included) | pass |
| `good_token_gives_grant_with_acl` | A good token gives a grant carrying its `acl` | pass |
| `untrusted_key_refused` | A token signed by a key outside the JWKS is refused | pass |
| `expired_token_refused` | An expired token is refused | pass |
| `token_without_exp_refused` | A token without `exp` is refused (review focus 3) | pass |
| `sub_must_equal_username` | `sub` must equal the username; a foreign client id is a binding failure first | pass |
| `superuser_claim_grants_nothing_extra` | `is_superuser` in the claims widens nothing | pass |
| `reason_codes` | Refusals map to 0x86, 0x87 and 0x8A | pass |
| `ban_list_persists_and_expires` | A ban survives reopening the file, ends at its time, and can be lifted | pass |
| `ban_replaces_an_expired_ban` | Banning again after a ban ended replaces it | pass |
| `split_pem_separates_cert_and_key` | `split_pem` returns the certificate block and the key block, each alone | pass |
| `from_lookup_names_missing_variable` | With `S11_TCP` unset, the config error names `S11_TCP` | pass |
| `it_publish_to_own_up_allowed` | A publishes QoS 1 to its own `up/` over TCP: PUBACK Success, and the hub's `$share/<g>/agmen/v1/+/+/up/+` subscriber receives `hello` within 5 s | pass |
| `it_websocket_listener` | The same round trip over `ws://<ws>/mqtt` | pass |
| `it_wss_listener` | The same round trip over `wss://localhost:<wss>/mqtt` with the dev server certificate, the client trusting only the dev CA | pass |
| `it_bad_signature_refused` | Claims signed with `untrusted.pem` under `kid` `agmen-dev-1`: CONNACK 0x87; the same claims signed with `agmen-dev-1`: Success | pass |
| `it_client_ids_bound_to_identity` | One token connects as `<A>-mcp`, `-watch` and `-cli` at once and each publish reaches the hub; eight wrong client ids get 0x86; A's token under B's or the hub's username gets 0x87; the hub's token on `<A>-mcp` gets 0x86; a client id ending in a newline is refused | pass |
| `it_publish_to_other_up_refused` | A publishing to B's `up/`: PUBACK 0x87, and the hub receives nothing within 2 s | pass |
| `it_subscribe_other_tenant_refused` | A's own feed: SUBACK QoS 1; tenant `t2`'s feed: SUBACK 0x87 | pass |
| `it_no_acl_claim_is_denied` | Without an `acl` claim, every publish (retained or not) and every subscribe gets 0x87 | pass |
| `it_share_prefix_does_not_widen_acl` | `$share/g/.../inbox/<B>` and `.../inbox/+`: SUBACK 0x87; `$share/g/.../feed`: SUBACK QoS 1 (review focus 2) | pass |
| `it_will_on_foreign_topic_refused` | A retained QoS 1 will on B's `presence/`: CONNACK 0x87, and a hub-token subscriber on `agmen/v1/+/+/presence/+` receives nothing within 2 s; the will on A's own `presence/` connects (review focus 1) | pass |
| `it_packet_over_limit_refused` | CONNACK `max_packet_size` is 98304; a 60 KiB publish gets PUBACK Success and reaches the hub; a raw 100 KiB publish gets no PUBACK, the broker closes the socket without a DISCONNECT, and the hub receives nothing within 2 s | pass |
| `it_expiry_disconnects` | A token with `exp` now + 5 s: the broker ends the connection after `exp` and within 10 s with DISCONNECT 0x87 | **fail**: the connection ends after `exp` (about 5 s in), but with no DISCONNECT; rumqttc reports `StateError::ConnectionAborted` (Context, item 3) |
| `it_hub_token_scope` | The hub's token publishes to A's inbox and A receives it; publishing to `up/A` or `elsewhere/x` and subscribing to `inbox/+` get 0x87 | pass |
| `it_rest_kick_disconnects` | `DELETE /api/v5/clients/<A>-mcp`: 204; the client gets DISCONNECT 0x98 within 2 s; then `GET` and a second `DELETE` are 404 | **fail**: 204, the connection ends within 2 s, `GET` and the second `DELETE` are 404, but no DISCONNECT arrives (rumqttc sees the socket close) |
| `it_ban_refuses_connect` | `POST /api/v5/banned` for A until now + 3900 s: 200, repeated: 400 `ALREADY_EXISTS`; `<A>-mcp` with a valid token: CONNACK 0x8A; `<B>-mcp` connects; `DELETE /api/v5/banned/username/<A>`: 204, then `<A>-mcp` connects; a second `DELETE`: 404 | **fail**: every step holds except the code: the banned CONNECT gets 0x87, not 0x8A |
| `it_ban_survives_broker_crash` | A connects (control); ban A; SIGKILL the broker and start it again on the same data folder; a fresh identity connects (control); `<A>-mcp` with its old, unexpired token gets CONNACK 0x8A (review focus 4) | **fail** on the code only: the ban survives (refused after the restart), with 0x87, not 0x8A. With the ban file renamed per process (a mutation), the same test fails with "was accepted" |
| `it_admin_lists_client_and_subscriptions` | `<A>-watch` (`clean_start = false`, session expiry 86400) subscribes to its inbox at QoS 1; `GET /clients/<A>-watch`: 200, `connected` true, `listener` `tcp:default`, `connected_at` RFC 3339; `GET .../subscriptions`: `[{"topic": "agmen/v1/t1/p1/inbox/<A>", "qos": 1}]`; after a clean DISCONNECT the session is listed with `connected` false; `DELETE` of that offline session: 204 and it stays (200); a never-seen id: 404 | pass |
| `it_admin_deletes_retained_topic_with_slashes` | The hub token publishes a retained QoS 1 message to `agmen/v1/t1/p1/state/roster` and a subscriber gets it (control); `DELETE /api/v5/mqtt/retainer/message/agmen%2Fv1%2Ft1%2Fp1%2Fstate%2Froster`: 204; a new subscriber to `agmen/v1/t1/p1/state/#` receives nothing within 2 s | pass |
| `it_admin_requires_basic_auth` | All six calls without credentials and with a wrong secret: 401; with the right ones, `GET` of an unknown client: 404 (control) | pass |
| `it_hub_emqx_admin_against_shim` | `agmen_hub::EmqxAdmin`, unchanged, against the shim: `ban_username(A, now + 3900 s)` twice and `kick("<A>-mcp")` return `Ok(())`; the connected `<A>-mcp` gets DISCONNECT 0x98 and its reconnect CONNACK 0x8A | **fail** on the codes only: all three calls return `Ok(())` and the client is dropped and then refused, but with no DISCONNECT and with 0x87 |
| `it_hub_minted_hub_token_scope` | A token from the hub's own `BrokerTokens::mint_hub` connects as `agmen-hub-<x>`: SUBACK QoS 1 for `$share/<g>/agmen/v1/+/+/up/+` and `agmen/v1/+/+/presence/+`, 0x87 for `agmen/v1/t1/p1/inbox/+`; PUBACK Success to `inbox/<A>`, 0x87 to `up/<A>` and `elsewhere/x` | pass |
| `it_hub_shared_subscription` | Two hub-token clients, `agmen-hub-<x>` and `agmen-hub-<y>`, share `$share/<g>/agmen/v1/+/+/up/+`; A publishes 100 QoS 1 messages to `up/A`: all 100 arrive within 10 s, each exactly once across both (and no copy within 1 s more), and each client gets at least one | pass |
| `it_retained_qos1_delivered_to_late_subscriber` | The hub token publishes `{"v":1}` retained at QoS 1 to `agmen/v1/t1/p1/state/roster`; A then subscribes to `agmen/v1/t1/p1/state/#` at QoS 1 and gets it within 2 s with `retain` true and QoS 1; a retained empty payload on the topic clears it, and a third subscriber gets nothing within 2 s | pass |
| `it_will_published_on_abrupt_drop` | `<A>-watch` connects with agmen-client's will (`presence/A`, `{"status":"offline"}`, QoS 1, retained), and its socket closes without a DISCONNECT: a hub-token subscriber gets the will within 5 s, and a later subscriber to `agmen/v1/t1/p1/presence/+` gets it as a retained message | pass |
| `it_will_sent_on_disconnect_with_will_message` | The same will: after DISCONNECT 0x04 (what `close_disconnect(true)` sends) the will arrives within 2 s; after DISCONNECT 0x00 none arrives within 2 s; after a close without DISCONNECT (the control) it arrives within 5 s | **fail** on 0x04 only: no will within 2 s (Context, item 12). 0x00 and the control behave as expected |
| `it_durable_session_queues_while_offline` | `<A>-watch` (`clean_start = false`, session expiry 86400) subscribes to `inbox/A` at QoS 1 and disconnects cleanly; the hub publishes `m1`, `m2`, `m3` at QoS 1; `<A>-watch` reconnects: CONNACK `session_present` true, its `session_expiry_interval` absent or 86400, and the three arrive in order within 5 s | pass; the CONNACK carried `session_expiry_interval` 86400 (`v5.rs:405-407`; `max_session_expiry_interval` is 0, no cap, `builder.rs:225`) |
| `it_session_expiry_honored` | The same with session expiry 2 s and 4 s offline: CONNACK `session_present` false, and nothing arrives within 2 s | pass |
| `it_durable_state_survives_broker_crash` | As `it_durable_session_queues_while_offline`, plus the hub publishes `{"v":1}` retained at QoS 1 to `state/roster`; then SIGKILL and a new start on the same data folder; `<A>-watch` reconnects with a fresh token: `session_present` true and `m1`, `m2`, `m3` in order within 5 s; a new subscriber to `state/#` gets the retained message (review focus 5) | **fail**: all of it is lost: `session_present` false, no message, no retained message. The kill comes right after the writes, before sled has written them to disk (see "Durable state") |
| `it_durable_state_survives_broker_stop` | The same with SIGTERM in place of SIGKILL | pass, after Context items 14 and 15 were fixed in `s11-broker` |
| `it_resumed_subscription_needs_current_grant` | `<A>-watch` (durable) subscribes to `inbox/A` and `feed` with its own token and disconnects; it reconnects with a token whose `acl` lacks the `inbox/A` rule: `session_present` true; the hub publishes to both topics, and within 2 s only `feed` arrives | pass, with the spike's check on resume; without it, both topics arrive (see "Subscriptions of a resumed session") |
| `it_queued_message_needs_current_grant` | `<A>-watch` (durable) subscribes to `inbox/A` and `feed` and disconnects; the hub publishes one QoS 1 message to each while it is offline; it reconnects with a token whose `acl` lacks the `inbox/A` rule: `session_present` true, and within 2 s only `feed` arrives | pass, with the spike's check on every delivery; before it, the queued `inbox/A` message arrived too (see "Subscriptions of a resumed session") |
| `it_agmen_client_over_wss` | agmen-client itself, through its own WSS path (`Broker::options`, `try_websocket_with_default_tls`, roots from `SSL_CERT_FILE` = the dev CA), with an in-process hub on its own database connected over TCP: B's `Watch` and A's `Mcp` session connect to `wss://localhost:<wss>/mqtt` and each sees `Connection { connected: true }` first; A's FYI: `Delivery` `Recorded { Feed, Seq(1) }` within 10 s, and B gets it as a `Message` within 10 s | pass. With `SSL_CERT_FILE` pointed at the system bundle instead (a mutation, guard removed), `Session::connect` fails with `InvalidCertificate(UnknownIssuer)`, so the session does use TLS to the spike's listener |
| `it_hub_minted_identity_token_acl` | Identities A and B seeded through the in-process hub; A's token from `BrokerTokens::mint` with the dev key: `<A>-mcp` over TCP gets CONNACK Success; a hub-token `$share/<g>/agmen/v1/+/+/up/+` subscriber receives A's QoS 1 publish to `up/A` (PUBACK Success) within 5 s, and not A's QoS 0 publish there within 2 s; QoS 1 to `up/B`: 0x87; retained QoS 1 to `presence/A`: Success, unretained: 0x87; SUBACK QoS 1 for `feed`, `inbox/A`, `ack/A`, `state/#`, `presence/+`; 0x87 for `inbox/B`, `inbox/+` and `#` | pass |
| `it_concurrent_durable_first_ack` | 8 `agmen-client` Mcp sessions (`clean_start = false`, session expiry 60 s), each on its own in-process hub, start at once and each posts one FYI; every first ack (a `Delivery` that is not `Pending`) comes within 5 s, in 5 of 5 batches (rulings R70 and R71, review focus 6) | pass: 8 of 8 in every batch, 30 batches in 6 runs, slowest ack 1.31 s. The same probe stalls on EMQX 6.3.2 with durable sessions on (see "First acks under concurrent durable sessions (R70, R71)") |
| Product `it` profile on EMQX 6.3.2 (baseline) | `cargo nextest run -p agmen-hub -p agmen-client -p agmen-cli --locked --profile it`, then `-p agmen-e2e`, against the dev stack | pass: 99 of 99 and 9 of 9, so N = 108 |
| Product `it` profile on RMQTT, run 1 | The same two commands against `s11-broker` and the spike's toxiproxy | **fail**: 90 of 99 and 1 of 9, 91 of 108 |
| Product `it` profile on RMQTT, run 2 | The same | **fail**: 90 of 99 and 1 of 9, 91 of 108 |
| Product `it` profile on RMQTT, run 3 | The same | **fail**: 91 of 99 and 1 of 9, 92 of 108 |

The DISCONNECT tests use a raw client: rumqttc 0.25.1 cannot send a DISCONNECT reason code, and its `Disconnect::write` for a reason code without properties writes two bytes after a remaining length of 1. The raw client sends `E0 02 <code> 00` and then closes its socket (Context, item 13).

`it_will_on_foreign_topic_refused` and the will tests subscribe their hub-token watcher to `agmen/v1/+/+/presence/+`, not to `agmen/v1/t1/p1/presence/+` as the plan wrote: the hub's rule is `eq agmen/v1/+/+/presence/+` (`crates/agmen-hub/src/broker_tokens.rs`, `hub_acl`), so the narrower filter gets 0x87. The test support's `hub_claims` mirrors today's `hub_acl`, which added that rule after S1.

## Kick and ban

`s11-broker` serves an admin shim on `S11_ADMIN` (`spikes/s11-rmqtt/src/admin.rs`) that answers the EMQX 6 REST calls the hub (`crates/agmen-hub/src/emqx.rs`) and the product's integration tests make, so both run unchanged with `AGMEN_EMQX_API_URL` (or `AGMEN_IT_EMQX_API`) pointed at it. Every call needs HTTP Basic auth with `S11_API_KEY` and `S11_API_SECRET`, or it gets 401; nothing else of EMQX's API is served (no `GET /banned`: no caller lists bans).

| Call | Answer |
|---|---|
| `DELETE /api/v5/clients/{id}` | 204 when connected (and kicked); 204 for an offline session, which stays (as EMQX does); 404 when there is no session |
| `GET /api/v5/clients/{id}` | 200 `{"clientid", "connected", "connected_at", "listener"}` (`connected_at` RFC 3339, which `crates/agmen-cli/tests/it_mcp_session.rs` reads; `listener` EMQX's listener id, `tcp:default`, `ws:default` or `wss:default` for the listener the connection came in on, which `tests/e2e/tests/it_m1.rs` reads); 404 when there is no session |
| `GET /api/v5/clients/{id}/subscriptions` | 200 `[{"topic", "qos"}]`, a shared one as `$share/<group>/<filter>`; 404 when there is no session |
| `POST /api/v5/banned` with `{"as": "username", "who", "by", "reason", "until"}` | 200 when added; 400 `{"code": "ALREADY_EXISTS"}` while an unexpired ban is there; 400 `{"code": "BAD_REQUEST"}` for any `as` but `username` or a body that does not parse. `by` and `reason` are ignored |
| `DELETE /api/v5/banned/username/{u}` | 204 when lifted; 404 when there was no ban |
| `DELETE /api/v5/mqtt/retainer/message/{topic}` | 204 once the retainer no longer holds the message; the topic is one percent-encoded path segment, slashes included |

How each works on RMQTT 0.24.0:

- **Kick** is `Shared::entry(id).kick(true, true, true)` (clean start, clear subscriptions, by an admin) when `is_connected()`. RMQTT sends the session `Message::Kick`, which ends it with `Reason::ConnectKicked(true)`; that maps to 0x98 (`rmqtt-0.24.0/src/types.rs:2566-2572`), but the session closes the socket before it would send the DISCONNECT (Context, item 3), so the client sees only the close. The session and its subscriptions are gone afterwards (`GET` 404).
- **Ban** writes the persisted ban list (`<data_dir>/bans.json`), which `ClientAuthenticate` reads on every CONNECT. It does not kick: the hub kicks after banning (ADR-0002, "REST ban"). A banned username is refused with 0x87, because `AuthResult` has no banned variant and RMQTT drops the `ClientConnack` answer (Context, item 2). The ban survives SIGKILL (`it_ban_survives_broker_crash`), so review focus 4 holds; only the code differs.
- **Retained delete** stores an empty retained message on the topic through `scx.extends.retain()`, which MQTT defines as a delete. The retainer applies writes from a queue (`rmqtt-retainer-0.24.0/src/storage.rs:807-809`, a channel to a writer task), so the call then checks `get` every 20 ms, up to 1 s, until the topic is gone, and answers only then, as EMQX's delete is done when it answers.

What a client sees differs from EMQX in two codes: no DISCONNECT 0x98 on kick, and 0x87 instead of 0x8A for a ban. No product code branches on either: `rg 'Banned|AdministrativeAction|NotAuthorized'` finds nothing in `crates/agmen-client/src`, `crates/agmen-cli/src` or `crates/agmen-hub/src`, and agmen-client handles every lost or refused connection the same way (`crates/agmen-client/src/link.rs`, `on_error`: reconnect with backoff, give up after `MAX_FAILURES`). So the kicked or banned client behaves as it does on EMQX; only the reason code a test or a log could read is different, which fails the reason code criterion as the spec words it.

## Durable state

`s11-broker` keeps all its state under its data folder (`S11_DATA_DIR`):

| State | Kept by | Where |
|---|---|---|
| Sessions: subscriptions, inflight messages and queued QoS 1 messages | `rmqtt-session-storage` 0.24.0 on sled | `<data_dir>/session` |
| Retained messages | `rmqtt-retainer` 0.24.0 on sled | `<data_dir>/retainer` |
| Bans | the spike's `BanList`: a JSON file, written to a temporary file, synced and renamed over the old one on every change | `<data_dir>/bans.json` |

On a new start the session storage plugin reads the stored sessions in its `init` (`rmqtt-session-storage-0.24.0/src/lib.rs:136`, `436`) and rebuilds them in the `BeforeStartup` hook, which `MqttServer::run` calls before it accepts connections (`rmqtt-0.24.0/src/server.rs:193-196`). The retainer reads its sled tree on demand.

What a stop and a crash lose:

- **Stop (SIGTERM): nothing,** in every run (`it_durable_state_survives_broker_stop`, 5 of 5 runs of the passing set, plus the focused runs). That needed two fixes: SO_REUSEADDR on the listeners (Context, item 14), and an exit that does not wait for the storage tasks (Context, item 15). The exit waits 1 s (`SHUTDOWN_WAIT` in `src/main.rs`), and that wait is what saves the data: with no wait, the same test lost the session, the three messages and the retained message in 5 of 5 runs. So a stop is durable only when the process lives at least one sled interval (500 ms) after its last write.
- **Crash (SIGKILL): every write of the last 0.5 s or so.** Measured by killing the broker a set time after the hub's last PUBACK, in the test as it is (the broker had started well under a second earlier): with 0, 100 or 200 ms, everything was lost (5 of 5 runs each); with 300, 600, 1000 or 2000 ms, nothing was lost (3 of 3 runs each). On a broker that had run for 1 s or more and was killed right after the last PUBACK, 5 of 10 runs lost everything, and the other 5 kept the session but lost the three messages and the retained message. The broker sends PUBACK before sled writes the message to disk, so a crash can lose QoS 1 messages the publisher already counts as delivered.

The test kills the broker right after the writes, so `it_durable_state_survives_broker_crash` fails every time; with a 300 ms wait before the kill it passed 3 of 3. No setting in RMQTT 0.24.0 or its plugin TOML files changes the interval (Context, item 16).

## Subscriptions of a resumed session

RMQTT 0.24.0 runs `ClientSubscribeCheckAcl` only on SUBSCRIBE. A resumed session gets its old subscriptions back after `ClientConnected`, through `Message::SessionStateTransfer` and `transfer_session_state` (`rmqtt-0.24.0/src/v5.rs:372-380`, `session.rs:1440-1495`), with no further check and no hook per subscription. So a token that no longer allows a topic kept receiving it: without a fix, `it_resumed_subscription_needs_current_grant` got both `inbox/A` and `feed`.

The spike's `ClientAuthenticate` hook now drops, from the session the client id left behind, each subscription that the new token's `acl` does not allow (`Hooks::drop_ungranted_subscriptions` in `src/broker.rs`). Authentication runs before RMQTT takes that session over (`v5.rs:278` vs `293`), so the dropped subscriptions never reach the new connection. If a subscription cannot be dropped, the CONNECT is refused with 0x87. The router drops a subscription only for the session's own `Id`, addresses and creation time included (`router.rs:456-467`), so the hook looks the session up first and uses its `Id`. A probe that also stopped and restarted the broker between the two connections passed 3 of 3 runs, and failed without the fix.

That prune alone left two gaps, which the controller's second security note named. A SUBSCRIBE or a delivery between the prune and the moment the new grant was stored still saw the old grant, and messages already queued for the session (offline queue, inflight) went out whatever the new grant said. Both are closed now:

- `authenticate` stores the new grant before it prunes, so a SUBSCRIBE that the old connection sends during the hand-over is checked against the new token. If the prune fails, the CONNECT is refused and the new grant stays: the token is valid for this client id.
- Every PUBLISH to a client is checked as it leaves. RMQTT runs the `MessageExpiryCheck` hook first thing in `deliver`, which holds the only `sink.publish` in the session (`rmqtt-0.24.0/src/session.rs:1554-1580`, called from `session.rs:370`), so queued, resent inflight and retained messages all pass it; a hook answer of `HookResult::MessageExpiry` drops the message. The spike's hook (`Hooks::allows_delivery`) lets a message through only when one of the session's subscriptions matches its topic and the client id's current grant allows that subscription. A matching subscription that the grant does not allow, one that a hand-over brought back after the prune, is dropped there too. RMQTT reports such a message to `MessageDropped` hooks as expired, the only reason this hook can give.

`it_queued_message_needs_current_grant` shows the second: before the delivery check, the message queued on `inbox/A` while the client was offline arrived after it reconnected with a token that no longer allows `inbox/A`; with it, only `feed` arrives. No test covers the race the first closes. EMQX was not measured for queued messages.

EMQX 6.3.2 has the same gap for resumed subscriptions. On a throwaway container with the dev config at `3e40dfb` (durable sessions on, MQTT on port 11884, removed afterwards), the same steps delivered both `inbox/A` and `feed` to the resumed session (2 of 2 runs), while a new SUBSCRIBE to `inbox/A` with the narrower token got 0x87. The hub on EMQX today therefore relies on a later token for a client id never allowing less than an earlier one did; this spike did not check whether that holds.

## First acks under concurrent durable sessions (R70, R71)

`it_concurrent_durable_first_ack` (`tests/it_s11_client.rs`) ports Task 15's probe with the two changes the rulings set. Each batch starts 8 tasks at once. Each task makes its own database, in-process hub and project, seeds one identity, connects an `agmen-client` Mcp session (`clean_start = false`, session expiry 60 s as `HubCtx::config` sets it), waits for `Connection { connected: true }`, posts one FYI, and waits up to 5 s from the publish for that FYI's first `Delivery` that is not `Pending`. A session with no ack by then counts as stalled and is closed, not reconnected. The test runs all 5 batches even after a stalled one, prints one `s11-probe` line per batch, and passes only if all 5 are clean. It starts its own `s11-broker`, or, with `S11_PROBE_MQTT` and `S11_PROBE_API` set, uses that broker and its REST API with the dev EMQX API key (`AGMEN_IT_EMQX_API_KEY`, `AGMEN_IT_EMQX_API_SECRET`).

The EMQX runs used a throwaway `emqx/emqx:6.3.2` container, one at a time, on 127.0.0.1:11884 (MQTT) and 127.0.0.1:18184 (REST API), with the dev API key file and the dev JWKS service on port 8090, stopped and removed after each set of runs. Its config is the dev config at `0d5d0bf` (durable sessions on); the off config differs from it in one line, `enable = true` to `enable = false` inside `durable_sessions { }`. `diff` shows that one line, and `emqx ctl conf show durable_sessions`, which works on 6.3.2, showed `enable = false` and `enable = true` in the two containers.

```bash
set -a; . deploy/dev/.secrets/dev.env; set +a
S11_PROBE_MQTT=mqtt://localhost:11884 S11_PROBE_API=http://localhost:18184/api/v5 \
  SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it --no-capture it_concurrent_durable_first_ack
```

Results, 2026-10-26. RMQTT, the test's own `s11-broker` (first run):

```text
s11-probe mqtt://127.0.0.1:57445 batch 1: 8 of 8 first acks within 5 s, slowest Some(1.310758292s)
s11-probe mqtt://127.0.0.1:57445 batch 2: 8 of 8 first acks within 5 s, slowest Some(310.460625ms)
s11-probe mqtt://127.0.0.1:57445 batch 3: 8 of 8 first acks within 5 s, slowest Some(337.0055ms)
s11-probe mqtt://127.0.0.1:57445 batch 4: 8 of 8 first acks within 5 s, slowest Some(305.416209ms)
s11-probe mqtt://127.0.0.1:57445 batch 5: 8 of 8 first acks within 5 s, slowest Some(384.541458ms)
```

EMQX 6.3.2 with durable sessions off (first run on a fresh container):

```text
s11-probe mqtt://localhost:11884 batch 1: 8 of 8 first acks within 5 s, slowest Some(393.246542ms)
s11-probe mqtt://localhost:11884 batch 2: 8 of 8 first acks within 5 s, slowest Some(264.925292ms)
s11-probe mqtt://localhost:11884 batch 3: 8 of 8 first acks within 5 s, slowest Some(267.558583ms)
s11-probe mqtt://localhost:11884 batch 4: 8 of 8 first acks within 5 s, slowest Some(279.17575ms)
s11-probe mqtt://localhost:11884 batch 5: 8 of 8 first acks within 5 s, slowest Some(265.212209ms)
```

EMQX 6.3.2 with durable sessions on, the control. Its first two runs on a fresh container were clean (8 of 8 in all 10 batches, slowest 426 ms), so it ran a third time on the same container, as the plan says, and that run stalled:

```text
s11-probe mqtt://localhost:11884 batch 1: 7 of 8 first acks within 5 s, slowest Some(391.722ms)
s11-probe mqtt://localhost:11884 batch 2: 8 of 8 first acks within 5 s, slowest Some(382.074041ms)
s11-probe mqtt://localhost:11884 batch 3: 6 of 8 first acks within 5 s, slowest Some(333.579083ms)
s11-probe mqtt://localhost:11884 batch 4: 8 of 8 first acks within 5 s, slowest Some(363.226083ms)
s11-probe mqtt://localhost:11884 batch 5: 7 of 8 first acks within 5 s, slowest Some(356.669417ms)
```

Because the control stalled only in its third run on one container, a single clean run of 5 batches proves little. So the other two brokers also ran 15 batches, in the same order:

| Broker | Runs | Clean batches | Stalled sessions | Slowest ack |
|---|---|---|---|---|
| RMQTT, a fresh `s11-broker` per run | 3 | 15 of 15 | 0 of 120 | 1.31 s (the first batch of the first run) |
| RMQTT, one `s11-broker` on the spike's ports for all three runs | 3 | 15 of 15 | 0 of 120 | 437 ms |
| EMQX 6.3.2, durable sessions off: the run above, then three runs on a second fresh container | 4 | 20 of 20 | 0 of 160 | 446 ms |
| EMQX 6.3.2, durable sessions on, one container | 3 | 12 of 15 | 4 of 120 | 426 ms |

On EMQX with durable sessions on, the acks split in two: every ack that came took at most 426 ms, and the 4 stalled sessions had none after 5 s. The probe catches the stall less often on a fresh container (3 of 15 batches) than Task 15 saw on the dev EMQX (5 of 6); this spike did not check why.

R71: EMQX 6.3.2 with durable sessions off did not stall, in 20 of 20 batches, three runs in a row on one container among them, the sequence in which the control stalled.

## The product `it` profile on RMQTT

Baseline on the dev stack's EMQX 6.3.2, 2026-10-26: `agmen-hub`, `agmen-client` and `agmen-cli` 99 passed of 99 (`agmen-e2e` alone after them, as ruling R71 sets it); `agmen-e2e` 9 of 9 (`it_network_kill_during_done` slow, over 30 s). So N = 108, and no test failed on EMQX under concurrency.

Against `s11-broker` on the spike's ports, with the spike's own toxiproxy and the six broker variables pointed at them ("How to run", steps 3 and 4; `rg -o --no-filename 'AGMEN_IT_[A-Z_]+' crates tests | sort -u` printed the same ten names as on 0d5d0bf), three runs:

| Run | `agmen-hub`, `agmen-client`, `agmen-cli` | `agmen-e2e` | Passed of 108 |
|---|---|---|---|
| 1 | 90 of 99 | 1 of 9 | 91 |
| 2 | 90 of 99 | 1 of 9 | 91 |
| 3 | 91 of 99 | 1 of 9 | 92 |

The failing tests, by cause:

1. **No close after the client's DISCONNECT** (Context, item 13), 13 tests in every run. The hub's test support ends a client with DISCONNECT 0x00 and waits 10 s for the connection to close (`crates/agmen-hub/tests/support/mqtt.rs`, `Client::disconnect`), and panics with `the connection closed after DISCONNECT: "still connected"`. `agmen-hub`: `it_presence_last_will`, `it_presence_roster_fields_and_bye`, `it_presence_roster_resent_by_a_new_hub`, `it_rollcall_answered_by_hub`, `it_roster_follows_mint_revoke_and_invite_accept`. `agmen-e2e`: every test but `it_claim_race_two_machines`, in the teardown that forgets each durable session (`forget_session`, `tests/e2e/tests/machines/mod.rs`); each of them printed only this panic, so its body passed, except `it_wss_exchange` (cause 4).
2. **No DISCONNECT 0x98 on kick** (Context, item 3), 2 tests: `it_revoke_kicks_and_refuses_refresh` and `it_revoke_bans_and_kicks_when_the_roster_publish_fails` get `None` from `closed_within` where they expect `Some(AdministrativeAction)`. Each would next expect CONNACK 0x8A for the banned identity, which RMQTT answers with 0x87.
3. **No will on DISCONNECT 0x04** (Context, item 12), 1 test: in `it_watch_presence_follows_the_connection` the roster still lists the identity as not offline 20 s after `close(session)` (`crates/agmen-client/tests/it_state.rs:143`, a backtrace of a run alone). The will on the dropped socket earlier in the same test arrives.
4. **The listener name**, 1 test: `it_wss_exchange` expects `GET /clients/<id>-watch` to give `listener` `ws:default`; the shim gives `wss:default`. In the dev stack HAProxy ends TLS and EMQX sees a plain WebSocket on its `ws:default` listener; `s11-broker` ends TLS on its own WSS listener, which EMQX would name `wss:default`. This comes from the test setup, not from RMQTT, and the test fails in its teardown as well (cause 1).
5. **Load**, 1 test in runs 1 and 2: `it_rate_limit_retryable`. In run 1 its 40 sends took 1.199 s, over the test's 1 s limit; in run 2 "nothing was rate limited" (the bucket refilled while the hub took the 40). It passed in run 3, and in 3 of 3 runs alone against `s11-broker`. The machine's load average was 25 to 30 during the runs, from other work on it. `mvp/impl` has since changed this test to send 80 FYIs so no plausible delay refills the bucket (472530c), which this branch does not have.

An earlier run, before the shim answered `listener`, failed the same tests but for `it_rate_limit_retryable`, with `listener` missing in `it_wss_exchange`; it is not counted. No product crate was changed.

## How to run

From the repository root. `deploy/dev/.secrets` is the dev stack's, written by `deploy/dev/init.sh`; in a worktree, source and point at the main checkout's copy.

The spike's own tests (Task 3, step 5):

```bash
cargo nextest run -p s11-rmqtt --locked
set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it it_
```

The integration tests need the dev secrets (`AGMEN_IT_JWT_DIR`, `AGMEN_IT_WSS_CA` and the files beside them), and those in `tests/it_s11_client.rs` also the dev Postgres (`AGMEN_IT_PG`), where each makes and drops a database of its own for its in-process hub. None of them uses the dev stack's EMQX.

The first-ack probe (Task 7, steps 2 to 4). On RMQTT:

```bash
set -a; . deploy/dev/.secrets/dev.env; set +a; SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it --no-capture it_concurrent_durable_first_ack
```

On EMQX 6.3.2 with durable sessions off, then on (the control), each in a throwaway container on ports 11884 and 18184:

```bash
mkdir -p target/s11-emqx
git show 0d5d0bf:deploy/dev/emqx/emqx.conf >target/s11-emqx/ds-on.conf
sed 's/^  enable = true$/  enable = false/' target/s11-emqx/ds-on.conf >target/s11-emqx/ds-off.conf
diff target/s11-emqx/ds-on.conf target/s11-emqx/ds-off.conf

set -a; . deploy/dev/.secrets/dev.env; set +a
docker run -d --rm --name agmen-s11-emqx \
  -p 127.0.0.1:11884:1883 -p 127.0.0.1:18184:18083 \
  -v "$PWD/target/s11-emqx/ds-off.conf:/opt/emqx/etc/emqx.conf:ro" \
  -v "$PWD/deploy/dev/.secrets/api-keys.txt:/opt/emqx/etc/agmen/api-keys.txt:ro" \
  --add-host host.docker.internal:host-gateway emqx/emqx:6.3.2
for i in $(seq 1 30); do docker exec agmen-s11-emqx /opt/emqx/bin/emqx ctl status && break; sleep 2; done
docker exec agmen-s11-emqx /opt/emqx/bin/emqx ctl conf show durable_sessions
S11_PROBE_MQTT=mqtt://localhost:11884 S11_PROBE_API=http://localhost:18184/api/v5 \
  SSL_CERT_FILE="$AGMEN_IT_WSS_CA" cargo nextest run -p s11-rmqtt --locked --profile it --no-capture it_concurrent_durable_first_ack
docker stop agmen-s11-emqx
```

For the control, the same `docker run`, wait, check, probe and `docker stop` with `ds-on.conf` in place of `ds-off.conf`.

The product `it` profile (Task 8, steps 1 to 5). The spike's toxiproxy is `spikes/s11-rmqtt/compose.yaml` (compose project `agmen-s11`: API on 127.0.0.1:18474, and a proxy named `emqx_mqtt`, as the product tests expect, from 127.0.0.1:31883 to `s11-broker` on the host's port 11883, in `spikes/s11-rmqtt/toxiproxy.json`). The baseline, on the dev stack's EMQX:

```bash
set -a; . deploy/dev/.secrets/dev.env; set +a
cargo nextest run -p agmen-hub -p agmen-client -p agmen-cli --locked --profile it
cargo nextest run -p agmen-e2e --locked --profile it
```

Then start the spike broker and its toxiproxy:

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

`target/s11-broker.log` reads `s11-broker ready` once the listeners are bound. In the same shell, run the product profile against it three times:

```bash
AGMEN_IT_MQTT=mqtt://localhost:11883 AGMEN_IT_MQTT_PROXIED=mqtt://localhost:31883 \
AGMEN_IT_MQTT_WS=ws://localhost:18093/mqtt AGMEN_IT_MQTT_WSS=wss://localhost:18443/mqtt \
AGMEN_IT_TOXIPROXY=http://localhost:18474 AGMEN_IT_EMQX_API=http://localhost:18183/api/v5 \
  sh -c 'cargo nextest run -p agmen-hub -p agmen-client -p agmen-cli --locked --profile it &&
         cargo nextest run -p agmen-e2e --locked --profile it'
```

The recorded runs added `--no-fail-fast` to both commands and ran the second whatever the first returned, so every failure of every run is counted. Before the runs, `rg -o --no-filename 'AGMEN_IT_[A-Z_]+' crates tests | sort -u` lists the variables the tests read; a broker address among them not set above must be pointed at the spike too. Then stop the spike stack:

```bash
kill "$(cat target/s11-broker.pid)"
docker compose -f spikes/s11-rmqtt/compose.yaml down
```

`docker compose -f spikes/s11-rmqtt/compose.yaml ps` then lists nothing, and `docker compose -f deploy/dev/compose.yaml ps` shows the dev stack's services as before.

## Consequences

The spike failed, so agmen hosted takes the spec's fallback: "agmen hosted uses EMQX's managed cloud (Dedicated Flex: $234 a month at 1,000 sessions, about $642 at 5,000), or a commercial EMQX license if EMQ prices one." Self-host keeps EMQX 6.3.2 as it is.

The spike is worth repeating when an RMQTT release fixes the failed rows. Each needs a change in RMQTT itself, which no hook in 0.24.0 can make:

- The DISCONNECT goes out before the socket closes, so a kick sends 0x98 and the spike's expiry timer 0x87 (`session.rs`, `sink.close()` before `send_disconnect`).
- An auth or CONNACK hook can answer CONNACK 0x8A (`AuthResult` has no banned variant, and the `ClientConnack` answer is dropped).
- DISCONNECT 0x04 keeps the will (`StateFlags::DisconnectReceived` turns it off whatever the reason code).
- The broker closes the connection after the client's DISCONNECT.
- A QoS 1 message, a retained message and a session change are on disk before the PUBACK, or the sled flush interval can be set, so a crash loses nothing acknowledged.

Patching these in a fork of RMQTT was not tried. To repeat the spike, bump the two `=0.24.0` pins in `spikes/s11-rmqtt/Cargo.toml`, rerun every command in "How to run", and update this ADR's tables.

Findings that hold for EMQX as well:

- A resumed durable session on EMQX 6.3.2 keeps subscriptions that the token of its new connection no longer allows (see "Subscriptions of a resumed session"). The hub relies on a later token for a client id never allowing less than an earlier one; this spike did not check whether that holds.
- EMQX 6.3.2 with durable sessions off did not stall the first ack in 20 of 20 batches (R71, see "First acks under concurrent durable sessions (R70, R71)").

`deny.toml` keeps the four exceptions listed under "Dependencies" (the three advisories with the reason "only through rmqtt in spike s11 (docs/adr/0007-broker-rmqtt.md)", the `systemstat` license exception with a comment saying the same) for as long as `spikes/s11-rmqtt` is a workspace member; they go when the spike crate is removed.
