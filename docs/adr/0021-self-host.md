# 0021: Self-host stack, shared broker keys and one public origin

Status: accepted
Date: 2026-10-29

## Context

Self-hosting runs one group on the owner's machine. The CLI generates the
stack and keeps its secrets across reruns, and one public HTTPS address serves
both the API and MQTT. This decision records the implementation at `4bdd83c`
and the tightening of the broker file modes made with it. Number 0021 was free
when this was written.

The [self-host plan](../plans/self-host.md) Task 11 asks for real Docker,
Tailscale, ngrok and second-machine runs. None has been run for this record.
Each is an owner run, listed under "Not verified". Reading the source and
running unit tests do not show that the real containers or public tunnels work.

## Decision

### Compose layout and image pins

`crates/agmen-cli/src/cmd/host/compose.yaml` is rendered with the host name
and CLI package version. The CLI keeps `compose.yaml`, `emqx.conf` and a
`0600` `.env` under `~/.agmen/host/<name>/`, a `0700` directory on Unix.
The default host name is `agmen`; the Compose project is `agmen-<name>`.

| Service | Pinned image at this revision | Storage |
|---|---|---|
| Postgres | `postgres:16.15` | `pg-data` at `/var/lib/postgresql/data` |
| Hub | `ghcr.io/agmen-dev/agmen-hub:0.1.0` | `hub-data` at `/var/lib/agmen`, `broker-keys` at `/var/lib/agmen/broker` |
| EMQX | `emqx/emqx:6.3.2` | `emqx-data` at `/opt/emqx/data`, read-only `broker-keys` at `/opt/emqx/etc/agmen` |
| ngrok, optional profile | `ngrok/ngrok:3.39.13-alpine` | Authtoken supplied from `.env` |

The hub tag follows `CARGO_PKG_VERSION`, currently `0.1.0`. Publication of
that tag is not verified, so the owner builds it locally from the repository
root:

```sh
docker build -f crates/agmen-hub/Dockerfile -t ghcr.io/agmen-dev/agmen-hub:0.1.0 .
```

The Dockerfile builds with `rust:1.99.1-bookworm`. Its runtime is
`gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f`.
Jenkins currently pushes `${AGMEN_REGISTRY}/agmen-hub:build-${BUILD_NUMBER}`;
it does not implement the plan's future GHCR version-tag publication.

The hub waits for healthy Postgres. EMQX waits for the healthy hub, which
creates its secrets before serving health checks. Only the hub's HTTP port
is published, at `127.0.0.1:7080` by default, forwarding to container port
8080. Broker ports stay inside Compose. The optional ngrok agent publishes
its control port 4040 on an ephemeral loopback port. Stopping the host keeps
all four named volumes. Rerunning `up` rewrites the templates and preserves
existing secrets.

### Hub-generated broker key and file modes

The CLI generates the database password, EMQX node cookie and dashboard
password. The hub generates the broker API credential on first start,
alongside its ES256 signing key, through `self_config::load`. It does this
inside its own container: `agmen host` writes only `compose.yaml`, `emqx.conf`
and `.env` on the host, and the hub's files live in named Docker volumes. The
host user's gid therefore does not matter.

- `jwt/` is `0700`; `jwt/hub-1.pem` is `0600`.
- `broker/api-keys.txt` is created with `0640`. Its single bootstrap line has
  API key name `agmen-hub` by default, a generated 64-character hex secret and
  role `administrator`. Before this change the file was `0644`, so any user
  with access to the volume could read it.
- `broker/` is created with `0750` when the hub has to create it. In the
  Compose layout it already exists; see "The `broker/` directory" below.
- The secret writer opens a new temporary file with the requested mode,
  writes and syncs it, then renames it in the same directory. It does not
  chmod a secret after creating it. A lock serializes cooperating starts; an
  existing secret is read, not replaced.

**Why EMQX can read a `0640` file.** The hub image runs as `65532:65532`
(`USER` in `crates/agmen-hub/Dockerfile`) and copies `/var/lib/agmen` with
`--chown=65532:65532`. The Compose file gives the hub no `user:` and no
`group_add:`, so the files it creates belong to group `65532`. The EMQX service
gets `group_add: ["65532"]` and no `user:`: it keeps its image's user and
joins that group. Group read on the file is enough, and nobody else needs it.
[Compose's group_add reference](https://docs.docker.com/reference/compose-file/services/#group_add)
describes this case, containers running as different users that read one file
on a shared volume through a shared group. The test
`compose_shares_the_hub_secret_group_with_emqx` in `host/files.rs` ties the
Dockerfile's user, the hub's lack of overrides and EMQX's `group_add`
together, so change them together.

**The `broker/` directory.** The hub's `0750` request applies only when the
hub creates the directory. In Compose, `/var/lib/agmen/broker` is the mount
point of the `broker-keys` volume, so it already exists when the hub starts.
The Dockerfile makes it with a plain `mkdir -p` (expected mode `0755`) and
copies it with `--chown=65532:65532`. Docker copies an image directory into a
new empty volume
([Docker's volume documentation](https://docs.docker.com/engine/storage/volumes/#mounting-a-volume-over-existing-data)).
Whether the volume root also takes the directory's owner and mode is Docker
behavior this record has not verified. If it does, `broker/` in a fresh volume
is `65532:65532` with mode `0755`: other users could list it but not read the
key. A `0750` mount point would need `-m 0750` on that `mkdir`, an image change
this decision does not make.

**Umask.** Creation modes are requests, and the process umask can only remove
bits from them. A hub run under `umask 077` writes the key as `0600` (and a
`broker/` it creates as `0700`), and EMQX then cannot read the key through its
group. The Compose file sets no umask for the hub, so the container runtime's
default applies; that default was not checked.

**Existing hosts.** The hub reads an existing file and never changes its
mode. A host created before this change keeps its `0644` key file, and its
`broker/` was never tightened. At every start the hub logs a warning, with the
path and mode and never the secret, while the key file's mode is not `0640`.
Moving such a host to `0640` is an owner run, listed below. This change does
not rotate secrets or rewrite volumes. Unix modes do not establish Windows ACL
protection.

### Why `/mqtt` passes bytes

The hub advertises a `wss` broker URL derived from its public HTTPS URL,
ending in `/mqtt`, and proxies it to `ws://emqx:8083/mqtt`. The proxy performs
the HTTP upgrade with only the WebSocket header allowlist. It forwards
neither the API Authorization header nor cookies. After the upgrade it
copies bytes in both directions without decoding or rebuilding WebSocket
frames. This preserves MQTT framing and leaves CONNECT authentication and
authorization at EMQX. A tunnel needs to carry only one public origin.

The proxy bounds connections at 1024, upgrade heads at 8 KiB and upstream
handshakes at 10 seconds. When either direction ends, it gives the other
five seconds to flush. Local byte-stream tests exercise this path without
claiming a real broker or tunnel connection.

### Funnel and ngrok findings

Source and fixture tests establish the intended tunnel flow. An explicit
`--public-url` takes precedence, followed by `--tunnel`, the stored choice,
signed-in Tailscale, then ngrok. Tailscale JSON parsing trims the DNS name's
trailing dot. Before enabling Funnel, the CLI refuses another mapping on
443, including foreground mappings. Stop disables only the mapping that
still points to this host's port. A competing agmend host-mode mapping can
therefore block setup; it is not cleared automatically.

The CLI probes the public `/livez` within `--wait`, 90 seconds by default.
Timeout leaves the stack running and tells the person to rerun. Its Funnel
diagnostic mentions a possible 15-minute DNS delay. Neither that delay nor
the 30-second idle cutoff the plan mentions was measured here. The client's
configured MQTT keep-alive is 30 seconds, so an application-idle connection
still has protocol traffic.

ngrok runs with `--inspect=false`. The CLI obtains its mapped port through
Compose and polls `/api/tunnels` at most 30 times for an HTTPS URL. The
authtoken stays out of command arguments and is saved in `.env`. A stable
free-plan domain, browser interstitial behavior on `/i`, and the pinned
image's control API on port 4040 are owner-run questions, not established
findings.

### Retention and the existing trial hub

Without a retention limit, self-host keeps all history. `--retention-days`
accepts 1 to 36500 and stores `AGMEN_RETENTION_DAYS` in `.env`. The plan's
"stored but not yet applied" README caveat applies only while hosted Task 9
has not shipped, and it has: `ec91379` added `retention.rs`, and
`Hub::start_messaging` spawns `run_retention` for a self-host hub with a limit.
`run_retention` prunes at startup and hourly. The tests
`it_self_host_limit_applies` and `it_self_host_limit_runs_on_start` in
`crates/agmen-hub/tests/it_retention.rs` cover it against Postgres. They need
the dev stack and were not run here.

The README keeps the existing [Trial hub](../../README.md#trial-hub)
launcher and adds [Host it yourself](../../README.md#host-it-yourself)
beside it, with links both ways. The plan asks for an added section, not a
replacement.

## Verified / not verified

Verified in source at `4bdd83c`, plus this change:

- Compose layout, image pins, loopback port publication, dependencies and
  named volumes.
- The hub image's user and group are `65532:65532`, neither Compose service
  overrides its image's user, and EMQX's `group_add` is `65532`. A unit test
  checks the three together.
- Generated secret format, atomic publication and preservation of existing
  contents. On fresh Unix storage under a umask of `022` or `027`, `jwt/` is
  `0700`, `jwt/hub-1.pem` is `0600`, `broker/` is `0750` and `api-keys.txt` is
  `0640`. An existing key file keeps its mode, and the start-up warning fires
  for a mode other than `0640` and not for a new file.
- README command names and flags against the clap definitions in
  `host/mod.rs`, `group.rs`, `account.rs`, `join.rs`, `post.rs` and `watch.rs`.
- Proxy byte-copy implementation and bounds, tunnel selection and cleanup
  code, client keep-alive, and the retention task's startup wiring.

Not verified, owner run:

- Build the hub image with the command above. On fresh Docker volumes, check
  the `broker-keys` mount point's owner and mode (expected `65532:65532`, and
  `0755` with today's Dockerfile), that `api-keys.txt` is `65532:65532` with
  mode `0640`, and the umask inside the hub container. Check which user the
  pinned EMQX image runs as, that its process has group `65532`, and that it
  reads its bootstrap key.
- Move a host created before this change to `0640`. With the stack stopped,
  mount its `broker-keys` volume (Compose names it `agmen-<name>_broker-keys`;
  confirm with `docker volume ls`) into a throwaway container as root and run
  `chmod 0640 api-keys.txt`. Confirm that the hub's start-up warning is gone.
- Run `agmen host up` in a test repository with Tailscale signed in and
  HTTPS and Funnel allowed. Check healthy hub, EMQX and Postgres, GitHub
  sign-in, owner claim, first project, `.agmen/identity.json` and invite.
  Check claim expiry and single use, a claim-token log line while unclaimed,
  and no new claim token after restarting a claimed hub.
- From a second machine or network, open the invite and run `agmen join
  <link>`. Confirm `agmen post FYI hi` reaches `agmen watch --json` over
  `wss://<machine>.<tailnet>.ts.net/mqtt`. Keep an application-idle connection
  open for 10 minutes and record reconnects and observed DNS delay.
- Stop and start again. Confirm the same `.env` secrets, no duplicate
  project, preserved volumes and the friend's watch reconnecting.
- Repeat the onboarding and MQTT checks with Tailscale signed out and an
  ngrok account. Confirm discovery through the ephemeral loopback control
  port, port 4040 inside `ngrok/ngrok:3.39.13-alpine`, stable domain after
  stop/up, and whether the free-plan interstitial appears on `/i`.
- Exercise a real HTTPS reverse proxy, group member controls and retention
  against Docker and Postgres. The default suite excludes the `it_*` tests
  that need the dev stack; those were not run here.

## Consequences

A new key file is `0640` instead of `0644`: other users on the machine cannot
read it, and EMQX reads it through the hub's group. There is no new
dependency, secret or host-gid setting. The Dockerfile's user and the Compose
`group_add` must change together. A fresh volume's `broker/` directory stays
as loose as the image's directory until the Dockerfile sets a mode.

End-to-end deployment remains an owner-run check. Existing volumes keep their
old modes until their owner changes them, and the hub warns at each start
until then.
