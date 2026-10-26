# 0003: MQTT 5 client crate and connection settings

Status: accepted
Date: 2026-10-25

## Context

agmen-client talks MQTT 5 to EMQX (ADR-0002) as `<id>-mcp`, `<id>-watch` and `<id>-cli`, over TCP in dev and over WSS behind HAProxy in the cluster. It needs a persistent session that a new process can resume, manual acks so a message is acked only after the client has handled it, reconnects after network cuts, and a fresh token at each reconnect, since EMQX disconnects a client when its token expires (ADR-0002, `disconnect_after_expire`).

Spike S2 runs five tests against the dev stack (`deploy/dev`, `emqx/emqx:6.3.2` with durable sessions on) for each candidate crate, in the order the plan gives: rumqttc, then rumqttc-next, then mqtt5. The tests are in `spikes/s2-rumqttc/tests/it_s2_client.rs`. A candidate must pass all five and `cargo deny check` with its TLS and WebSocket features on; ADR-0002 found that rumqttc fails the second.

Candidates, checked on crates.io on 2026-10-25:

| Crate | Version | Released | Repository | Notes |
|---|---|---|---|---|
| `rumqttc` | 0.25.1 | 2025-11-21 | bytebeamio/rumqtt | Upstream; no release since |
| `rumqttc-next` | 0.34.0 | 2026-08-13 | thehouseisonfire/rumqtt | Fork of rumqtt, first release 2026-02-16, 15 releases. A facade over `rumqttc-v5-next` 0.34.0; the library name is still `rumqttc` |
| `mqtt5` | 0.48.2 | 2026-10-19 | LabOverWire/mqtt-lib | Client and broker, 95 releases since 2025-08-04 |

## Decision

agmen-client uses **`rumqttc-next` 0.34.0**:

```toml
rumqttc-next = { version = "0.34.0", default-features = false, features = ["websocket", "use-rustls"] }
```

`use-rustls` selects the aws-lc-rs provider, which the workspace already builds for `jsonwebtoken`. `rumqttc::tokio_rustls` re-exports the rustls it was built with, so the client builds its `ClientConfig` from that.

### rustls crypto provider (R46)

Cargo unifies features across the workspace, so rustls 0.23.46 is built with both `aws-lc-rs` (from `jsonwebtoken`, reqwest's `rustls` feature and `use-rustls`) and `ring` (from sqlx's `tls-rustls-ring-native-roots`). With both on, rustls has no automatic process-level provider, and `ClientConfig::builder()` panics with "Could not automatically determine the process-level CryptoProvider". Merging Task 7 turned this on and broke `it_wss_behind_haproxy` and `it_wss_transport`.

aws-lc-rs is the process-level provider. Every binary that may use TLS (`agmen-hub`, `agmen`) calls `rustls::crypto::aws_lc_rs::default_provider().install_default()` first thing in `main` and exits with an error if a provider is already set. Every test file that opens TLS does the same once, before it builds a `ClientConfig`, and ignores the error a second call returns. Libraries never install a provider: a library that did would race the binary's choice. Both backends stay, since sqlx needs `ring` and the rest need aws-lc-rs.

### Connection settings for `<id>-mcp` and `<id>-watch`

```rust
let mut opts = MqttOptions::new(client_id, (host, port));          // TCP
// or, for WSS:
let mut opts = MqttOptions::websocket_with_tls_config(
    client_id, "wss://host:port/mqtt", TlsConfiguration::Rustls(client_config))?;
opts.set_credentials(identity_id, token)
    .set_session_store_arc(store)                  // file-backed SessionStore, see below
    .set_clean_start(false)
    .set_session_expiry_interval(Some(86_400))     // 24 h
    .set_ack_mode(AckMode::Manual)
    .set_keep_alive(30)                            // seconds
    .set_max_packet_size(Some(96 * 1024));
let (client, eventloop) = AsyncClient::builder(opts).capacity(64).try_build()?;
```

- **Keep-alive 30 s.** EMQX drops a silent client after 1.5 times the keep-alive (`keepalive_multiplier = 1.5` in the dev node's config), so a dead link is noticed within 45 s. HAProxy's `timeout tunnel 1h` is far longer.
- **Max packet size 96 KiB**, the broker's own cap. Without it rumqttc-next refuses incoming packets over 10 KiB (`default_max_incoming_size` in its `MqttOptions::new`, from source), which is below the 64 KiB envelope cap.
- **Manual acks.** The client acks each QoS 1 publish with `AsyncClient::ack(&publish)` after it has handled it. The dev node has `max_inflight = 32`, so EMQX stops delivering to a session once 32 publishes wait for an ack (config value, not tested).
- **`try_build`, not `build`.** `build` panics when the options fail validation.
- `<id>-cli` keeps ruling R4: clean start, no session expiry, no session store.

### The session store is required

rumqttc-next will not resume a broker session that it has no local record of. Its default `BrokerSessionResumePolicy::Strict` refuses the CONNACK: a new event loop (a new process) connecting with `clean_start = false` to a session EMQX kept gets `ConnectionError::SessionStateMismatch { clean_start: false, session_present: true }` (observed, first rumqttc-next run below). `AllowBrokerOnly` accepts the session, but for the whole connection it then refuses every request that needs a packet id: a QoS 1 publish fails locally with `PublishNoticeError::BrokerOnlySessionResume` (observed by probe), and subscribes and unsubscribes fail the same way (from source, `reject_broker_only_session_request` in `eventloop.rs`). The MCP server starts a new process each Claude session and must publish, so neither works.

With a `SessionStore` the new event loop loads its record and both the resume and the publish work (`it_resume_session_flag`). The crate stores state through the trait and gives `PersistedSession::encode` and `decode`; the application provides the backend. The tests keep the encoded bytes in memory; agmen-client keeps them in a file per client id, next to its outbox, written to a temp file and renamed into place, as the crate's `examples/persistent_session_file_store_v5.rs` does. It sets `set_session_store_scope` to the broker URL, so a session record never follows a client to another broker.

If the record is missing or unreadable while EMQX still holds the session (a new laptop, a deleted file), the connect fails with `SessionStateMismatch`. agmen-client then connects once with `clean_start = true`, which drops the broker's session and its queue, and replays from its cursor over HTTP (Task 16). Not tested here.

### Reconnects and token refresh

rumqttc-next reconnects when `EventLoop::poll` is called again after an error; the caller owns the backoff. The spike waits 1 s between attempts and gives up after 60 failures in a row. Before each attempt the client sets a fresh token with `eventloop.options.set_credentials(..)` (`EventLoop::options` is a public field). MQTT carries the password only in CONNECT, so a refreshed token takes effect at the next connect (re-authentication with AUTH packets was not tried); with `clean_start = false` that resumes the same session (`it_token_refresh_reconnect`). In agmen-client the token comes from the hub over HTTP, fetched when the current one is near expiry, not on every attempt as in the test.

While disconnected, rumqttc-next keeps queued and unacked requests and replays them when the session resumes. When the broker reports no session, it fails their tracked notices with `NoticeFailureReason::SessionReset` (from source, `reset_session_state_internal`), so the outbox must keep each message until its PUBACK and resend after a reset.

## Results

`cargo nextest run -p s2-rumqttc --profile it` (5 tests). RED, with rumqttc 0.25.1 and the client at its defaults (clean start, automatic acks, no reconnect, `Transport::Ws` on the WSS URL), all five failed as expected: `it_resume_session_flag` and `it_manual_ack_redelivery` had no session to resume, `it_wss_transport` got `Websocket(Protocol(HandshakeIncomplete))`, and the cut and token tests saw the poller stop instead of reconnecting.

| Test | Checks | rumqttc 0.25.1 | rumqttc-next 0.34.0 | mqtt5 0.48.2 |
|---|---|---|---|---|
| `it_resume_session_flag` | `<A>-mcp` connects (no session), subscribes, disconnects; a new event loop gets `session_present` and its QoS 1 publish to `up/A` gets PUBACK success | Pass | Pass with a `SessionStore`. Default `Strict`: `SessionStateMismatch`. `AllowBrokerOnly`: resumes, publish refused with `BrokerOnlySessionResume` | Not run |
| `it_manual_ack_redelivery` | The hub sends one QoS 1 message to `inbox/A`; `<A>-watch` gets it (`dup` false) and is dropped without acking; the next connection gets it again with `dup` true, the same packet id and payload; after an ack and a clean disconnect, a third connection gets nothing in 2 s | Pass | Pass with a `SessionStore` (and with `AllowBrokerOnly`). Default `Strict`: `SessionStateMismatch` | Not run |
| `it_wss_transport` | `<A>-mcp` over `wss://localhost:8443/mqtt` (HAProxy, trusting only the dev CA) subscribes to `inbox/A`, publishes to `up/A` (the hub receives it), and receives the hub's reply on `inbox/A` | Pass | Pass | Not run |
| `it_reconnect_after_toxiproxy_cut` | `<A>-mcp` through toxiproxy (`localhost:21883`); the proxy is disabled for 10 s, which closes the connection (the client sees the error within 10 s); a publish queued during the cut reaches the hub after the reconnect (`session_present` true) and does not arrive again within 3 s | Pass | Pass | Not run |
| `it_token_refresh_reconnect` | `<A>-watch` with a token that expires in 5 s gets DISCONNECT 0x87, reconnects with a fresh token to the same session (`session_present` true) and receives a message on `inbox/A` | Pass | Pass | Not run |
| `cargo deny check`, TLS and WebSocket on | | **Fails** advisories: RUSTSEC-2026-0049, -0098, -0099, -0104, -0382 on `rustls-webpki` 0.102.8 and RUSTSEC-2025-0134 on `rustls-pemfile` 2.2.0 (features `websocket`, `use-rustls`) | Passes (features `websocket`, `use-rustls`) | Passes (no default features, `transport-websocket`; its rustls with the ring provider is not optional) |

rumqttc 0.25.1 passed the five tests three times before the publish after the resume was added to `it_resume_session_flag`, and twice after. The final rumqttc-next client passed seven runs in a row, four alone and three within the full `it` profile (S1 and S2, 18 tests, all passing). mqtt5 got only the `cargo deny` probe: the plan tries it only if rumqttc-next fails a test, and rumqttc-next passed all five. `it_resume_session_flag` asserts more than the plan's one line (the publish after the resume) because that is what separates the rumqttc-next settings that work from the ones that do not.

rumqttc 0.25.1 passes every test but needs advisory ignores for `rustls-webpki` 0.102.8, which it uses only for an error type (ADR-0002), and it has had no release in 11 months. It loses on the rule that a crate needing a `cargo deny` exception does not win while another passes.

### Test isolation

Each test mints its own identity ids, client ids and share group, and its own session store. Only `it_reconnect_after_toxiproxy_cut` uses toxiproxy. It disables the shared `emqx_mqtt` proxy and enables it again; it also enables it first, since a run that fails mid-cut leaves it disabled. The cut closes only connections through port 21883, so the test stays in the `it` profile (and in CI). `.config/nextest.toml` puts every `it` test whose name contains `toxiproxy` in a `toxiproxy` test group with one thread, so later toxiproxy tests cannot cut each other.

## Consequences

- agmen-client depends on `rumqttc-next`, not `rumqttc`. Both name their library `rumqttc`, so one crate cannot depend on both without renaming one (`package =`). Spike S1 keeps upstream rumqttc 0.25.1 without TLS.
- agmen-client implements a file-backed `SessionStore` and the `SessionStateMismatch` fallback above. Losing the file costs the broker's queue for that client id, which the HTTP replay covers.
- The client owns the reconnect loop: bounded backoff, a fresh token before each attempt, and every connection error surfaced to the caller.
- The rumqttc-next fork has a short history (first release 2026-02-16). Revisit if it stops releasing or if upstream rumqttc ships a release that passes `cargo deny` with TLS on; mqtt5 is the untested fallback.
- Not tested here: a file-backed store across a real process restart, the `SessionStateMismatch` fallback, EMQX's `max_inflight = 32` with acks held back, `try_websocket_with_default_tls` (native root certificates), and EMQX's `max_mqueue_len = 1000` on a durable session.

## agmen-client (MVP Task 11)

agmen-client implements the file-backed `SessionStore` (`crates/agmen-client/src/mqtt_store.rs`, one record per client id beside the identity's SQLite file, scoped to the broker URL) and the `SessionStateMismatch` fallback. A missing or corrupt record loads as none; the link then connects once with a clean start and the hub replay delivers what the broker had queued. `it_lost_session_record_falls_back_to_replay` covers both cases against EMQX 6.3.2.

Two rumqttc-next 0.34.0 behaviors found while building it bind later changes:

- **`EventLoop::poll` is not safe to cancel.** The crate's docs say direct `poll` calls work in `tokio::select!`, but a `select!` that dropped `poll` while it was sending a graceful DISCONNECT (`send_pending_disconnect` had taken the pending request but not yet set `disconnect_complete`) left the DISCONNECT unsent and the loop running; `Session::close` then waited out its 5 s limit. The link task now never cancels `poll`: it handles and acks each publish between polls, and a timer task wakes the poll for the token swap by subscribing to `ack/{id}` again.
- **An ack queued after DISCONNECT is still sent**, and EMQX then aborts the connection (`MqttState(ConnectionAborted)`). `Session::close` sets a flag before it queues DISCONNECT; from then on the link acks nothing and stops at the next error instead of reconnecting.

Acks go out only between polls and only for publishes that arrived on the current connection (`AckGate` in `link.rs`), so an ack cannot reach a packet id that a reconnect gave to another publish. rumqttc drops acks still queued when a connection fails (`EventLoop::clean`).

Hub tokens (ruling R53): on Linux, agmen-client keeps them in a directory store in `$XDG_STATE_HOME/agmen` (or `~/.local/state/agmen` when `XDG_STATE_HOME` is unset), not in a keychain. The folder is mode 0700, each token file 0600, written to a temporary file and renamed into place, named by the hub origin plus the identity id (R42). The kernel key retention service (keyutils) loses its keys on a reboot, and the Secret Service store pulls in zbus, which fails `cargo deny`. macOS keeps Keychain Services and Windows keeps Credential Manager; `AGMEN_TOKEN_STORE_DIR` still picks a directory store on every platform (R17). See `crates/agmen-client/src/token_store.rs`.
