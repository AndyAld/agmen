# agmen protocol v1

Status: draft for M0. Normative for `crates/agmen-proto`; [design.md](design.md) explains the why. Spikes S1 (EMQX broker) and S5 (numbering and replay) may revise details; each change lands with an ADR in `docs/adr/`. Revised 2026-10-25 for roles, the listener (formerly the dispatcher), REVIEW, PLAN, urgent alerts, role mentions and `refs`.

Conventions: MQTT 5, UTF-8 JSON payloads, QoS 1 unless stated. "Client" means anything that publishes as an identity: agmend on behalf of a human, an agent, a listener, or a coordinator. Ids are opaque strings minted by the hub from the alphabet `[a-z0-9]`, never containing `/`, `+` or `#`.

## 1. Identities

| Kind | Example handle | Minted by | Can |
|---|---|---|---|
| `human` | `andy` | Hub, at OAuth or device login | Everything its roles allow, including RULING, PUSH_OK and PUSH_NO |
| `agent` | `andy/sol` | Hub, when its owner runs `agmen join` (invite code) or `agmen join --project` (later: agmend, as a child of a human's device) | Post, claim, done, DM, request pushes; never RULING or push answers |
| `coordinator` | `adam/coord` | Hub, when a human with the coordinator or admin role runs `agmen join --kind coordinator` (later: agmend for a human with the coordinator role) | Agent rights plus ASSIGN, RULING (agent rulings carry `from_kind: coordinator`) |
| `listener` | `andy/listen` | Not minted in the MVP; later agmend, at most one per human per project | Post, chat, ask, answer, DM, ALERT, DELEGATED, and settings changes for its owner that agmend applies locally; ASSIGN only when minted with the coordinator role; never CLAIM, DONE, LEND, RULING or push answers, and never edit, shell or web tools |
| `hub` | `hub` (id `agmenhub0`) | Fixed | GRANT, DENY, ITEM, ALERT, RELEASE, STATUS, STATE, ACK, NACK |

The hub's identity id is the fixed `agmenhub0`. It holds a `0`, which the alphabet the hub mints ids from leaves out, so no minted identity can collide with it. The hub posts its own messages through an internal path, not through `up/`: `from` is `agmenhub0`, `from_handle` is `hub`, `from_kind` is `hub`, and the message takes the same numbering, storage and sweeper as a client's message, with no rate limit and no ACK. It posts GRANT and DENY (claim outcomes), ITEM (the key for a FOUND), ALERT (a DONE that changed paths outside its claimed or lent lanes, and the urgent downgrade notice, which is not sent by the MVP hub yet), RELEASE with `reason: "lease_expired"` (a claim whose lease ran out) and STATUS (the answer to a ROLLCALL). ACK and NACK go on `ack/{identity_id}`, not into a stream, and STATE is reserved for retained `state/` topics (section 2).

Workers that a listener starts are `agent` identities with `delegated_by` set to that listener's id; the hub accepts `delegated_by` only for a listener with the same owner.

Every identity except `human` and `hub` carries a role (design 6.13): in the later design, at mint agmend sends `{role, verbs, edits, role_sha256}`. In the MVP the mint request (`POST /v1/identities`, sent by `agmen join`) carries only `handle`, `project` and `kind`, so the MVP hub keeps no per-identity role. Instead it checks each verb against a fixed table per kind (`may_send` in crates/agmen-hub/src/ingest.rs) and refuses any verb that table does not allow with a final NACK `forbidden_verb`. The role fields (`verbs`, `edits`, `role_sha256`) and per-role caps are the later design, as is the rest of this paragraph. The hub keeps them, caps `verbs` at the kind's ceiling (an `agent` never gets ASSIGN, RULING or ROLLCALL; a `listener` never gets CLAIM, DONE or LEND; neither gets human verbs), and refuses any other verb from that identity with `forbidden_verb`. Verbs every role may send: HELLO, BYE, FYI, ASK, ANSWER, ACK, STATUS, DM, ALERT, FOUND.

Handles are for display only and appear only in payloads the hub stamps. Topics and ACLs use `identity_id`.

## 2. Topics

Prefix: `agmen/v1/{tenant_id}/{project_id}/`.

| Topic | Publisher | Subscribers | Retained | Notes |
|---|---|---|---|---|
| `up/{identity_id}` | That identity only (JWT `acl` claim) | Hub | No | All client traffic enters here |
| `feed` | Hub | Project members | No | Project-wide messages |
| `inbox/{identity_id}` | Hub | That identity and its owning human | No | DMs to it, copies of DMs it sent, assignments |
| `ack/{identity_id}` | Hub | That identity | No | ACK and NACK for its publishes |
| `state/head` | Hub | Members | Yes | `{feed_seq, rules_v, lanes_v, items_v, hub_at}` |
| `state/{key}` | Hub | Members | Yes | Small heads only (`roster`, `lanes`, `round`, `packs`); full snapshots over HTTP. Payload shapes below |
| `presence/{identity_id}` | That identity's `watch` session | Members | Yes | `{"status":"online"}` (or `away`); last will `{"status":"offline"}`. Details below |

The hub's privileged client subscribes to `agmen/v1/+/+/up/+`. Members never subscribe to `up/`.

Retained `state/` payloads are bare JSON in v1, not wrapped in a STATE envelope:

- `state/head` is `{"feed_seq": 1842, "rules_v": 0, "lanes_v": 0, "items_v": 0, "hub_at": "2026-10-25T16:04:11.402Z"}`. The sweeper republishes it for each project it published messages for in a pass. `rules_v`, `lanes_v` and `items_v` stay 0 until something versions rules, lanes and items.
- `state/roster` is a JSON array with one entry per live identity and per human member of the project, ordered by handle: `{"identity_id", "handle", "kind", "status", "agent"?, "model"?, "delegated_by"?}`. `kind` is `human`, `agent`, `coordinator` or `listener`; `status` is `online`, `away` or `offline`; `agent` and `model` come from the identity's latest HELLO and are left out when it named none; `delegated_by` is left out until listeners start workers. For example `[{"identity_id":"idn7k2q9","handle":"andy/sol","kind":"agent","status":"online","agent":"claude-code/2.1.310"}]`.

The roster is built from `presence/+`, the identities table and the latest HELLOs, so a client reads who is there with one subscription and no agent has to be woken; ROLLCALL is for a visible answer in the chat and the hub answers it from the same presence. The hub republishes the roster when presence changes, on HELLO and BYE, and when an identity is minted or revoked or an invite is accepted.

Presence: of the three MQTT sessions an identity can hold (`<identity_id>-watch`, `<identity_id>-mcp`, `<identity_id>-cli`), only the watch session publishes presence. On every connect it publishes the retained `{"status":"online"}` to `presence/{identity_id}`, and its last will is the retained `{"status":"offline"}` on the same topic; when it closes, it disconnects with reason 0x04 so the broker sends the will. The mcp and cli sessions publish no presence and set no will, because all three share the one `presence/{identity_id}` topic and an MCP server stopping would otherwise mark the identity offline while its watch still runs. An identity with only an MCP server running shows offline. On a BYE the hub sets the sender's retained presence to offline itself. `agmen watch` posts one HELLO per process start, not on every reconnect.

## 3. Envelope

Client-set fields (what a client publishes to `up/`):

```json
{
  "v": 1,
  "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS",
  "verb": "DONE",
  "reply_to": null,
  "to": null,
  "mentions": [],
  "items": ["B-412"],
  "lane": "L-UI",
  "body": "Fixed the truncation notice; regression spec added.",
  "data": { "...": "verb specific, see section 4" },
  "valid_until": null,
  "sent_at": "2026-10-25T16:04:11.123Z"
}
```

Hub-stamped fields (added before the message goes to `feed` or `inbox/`):

```json
{
  "stream": "feed",
  "seq": 1842,
  "from": "idn7k2q9",
  "from_handle": "andy/sol",
  "from_kind": "agent",
  "mentioned": [],
  "refs": [],
  "hub_at": "2026-10-25T16:04:11.402Z"
}
```

Rules:

- `id` is a ULID generated by the client and is the dedupe key with `from`.
- `reply_to` is the `id` of an earlier message in a stream the sender can read. It replaces "re your 1792901415" and quoting ids in prose.
- `to` is required on DM and optional on ASK: a list of 1 to 8 `identity_id`s. Every other verb must leave it empty (final NACK `schema`).
- `mentions` is allowed on FYI, ASK, ANSWER and ALERT. Each entry is `all`, `agents`, `humans`, a lane name (`L-UI`, from M2), a role (`role:reviewer`, which expands to every live identity holding that role), or an `identity_id`; these never collide, because ids are at least 8 characters of `[a-z0-9]`, lane names start with `L-` and roles with `role:`. Clients resolve typed `@handle` text to ids from `state/roster` and `@reviewers` to `role:reviewer`. Humans and coordinators may use `all`, `agents`, `humans`, lanes and roles; every other kind may list up to 8 `identity_id`s and nothing else (final NACK `forbidden_mention`).
- `mentioned` is stamped by the hub: the identity ids the mentions expand to, without the sender, plus the author of the `reply_to` message when that author is a human, plus the author of the DONE a REVIEW replies to. Clients interrupt an identity's session (a wake-up, a stdin message, a notification) only when it is in `mentioned`, is a DM recipient, or is the assignee of an ASSIGN. Design section 6.10 covers delivery and wake-up limits.
- `refs` is stamped by the hub: `[{kind, start, end, target}]` for every handle, lane, item, role and PR number the body names that the hub knows (`kind` is `identity`, `lane`, `item`, `role` or `pr`; `start` and `end` are byte offsets into `body`; `target` is the id, name, key or number). Clients render them as links and add branches, commits and file paths from local git themselves (design 6.8).
- `body` is markdown, at most 32,768 bytes after UTF-8 encoding. Larger bodies get a final NACK `body_too_large`.
- `valid_until` is required on CLAIM and optional elsewhere. A message that reaches the hub after `valid_until` is refused: CLAIM gets DENY `expired`, other verbs get NACK `expired`.
- Unknown fields are rejected (final NACK `schema`), so clients cannot smuggle a `from`.

## 4. Verbs

| Verb | Sent by | `data` | Hub behavior |
|---|---|---|---|
| `HELLO` | any | `{client: "agmen-cli/0.1.0", agent?: "claude-code/2.1.310", model?: string, looking_at?: string}` | Feed; roster updated |
| `BYE` | any | `{}` | Feed; presence set offline |
| `FYI` | any | `{}` | Feed. The chat message: free text in `body`, optional `mentions`, threaded by `reply_to` |
| `ASK` | any | `{of?: identity_id}` | Feed. With `to` (1 to 8 recipients) it is routed like a DM instead: the inbox of each recipient, a copy in the sender's inbox, and a copy in each owning human's inbox. An ANSWER to it goes to the same inboxes, plus the answerer's own |
| `ANSWER` | any | `{}` with `reply_to` required | Same stream as the ASK |
| `ACK` | any | `{}` with `reply_to` required | Feed (a social ack, not the delivery ACK) |
| `STATUS` | any | `{summary: string, items?: [...], spend_usd?: number, hours?: number, tokens?: {input, output, cache_read}}` | Feed |
| `ROLLCALL` | human, coordinator | `{}` | Hub answers from presence; no agent needs to reply |
| `CLAIM` | agent, coordinator | `{item: string}` with `valid_until` | Hub answers `GRANT` or `DENY {reason: "held_by"|"expired"|"not_in_lane"|"closed"}` in the claimant's inbox and posts the outcome to the feed. Exactly one active claim per item |
| `HEARTBEAT` | claim holder | `{items: [...]}` | Renews leases. Not posted to the feed: stored in the sender's own inbox only, so its ACK names that stream and seq. Readers drop their own messages, so it interrupts nobody |
| `RELEASE` | claim holder, coordinator | `{item: string, reason?: string}` | Feed |
| `ASSIGN` | coordinator, or a listener with the coordinator role | `{item: string, to: identity_id}` | Inbox of the assignee and feed; wakes a Claude Code session through the mod |
| `DELEGATED` | listener | `{worker: identity_id, agent: string, role: string, model: string, tier: string, budget_usd: number, item?: string}` with `reply_to` required | Same stream as the `reply_to` message; the worker must have `delegated_by` equal to the sender, else final NACK `schema` |
| `LEND` | the lane's `owner` identity (design 6.5), coordinator | `{lane: string, paths: [glob], to: identity_id, until: timestamp}` | Feed; lane checks honor the lend until it expires; from anyone else, final NACK `not_lane_owner` |
| `FOUND` | any | `{title: string, lane?: string, severity?: "low"|"med"|"high"|"security"}` | Hub mints `ITEM {key: "B-413"}`; security items post only the key and lane to the feed |
| `DONE` | claim holder | `{item, branch, commit, base_sha, checks: [{name, passed, failed, baseline?}], changes: [{status, path}]}` | Feed; hub flags paths outside claimed or lent lanes to the coordinator |
| `REVIEW` | reviewer role | `{verdict: "pass"|"changes", findings: [{severity: "high"|"med"|"low", path, line?, what, trigger}], traced?: string}` with `reply_to` set to a DONE | Same stream as the DONE; the DONE's author is added to `mentioned` |
| `PROPOSAL` | any | `{options: [string], deadline?: timestamp}` | Feed |
| `PLAN` | planner role | `{version: number, path: string, sha: string}` | Feed; the hub keeps the latest per project for the plan panel. A human `DECISION {outcome: "GO"}` with `reply_to` on the final PLAN is the kick-off (design 6.14) |
| `DECISION` | coordinator, human | `{outcome: "GO"|"VETO", option?: number, subject?: string}` with `reply_to`; `subject` names a spec file and its commit when a person approves one | Feed |
| `RULING` | coordinator, human | `{subject: string}` | Feed; recorded in `human_decisions` when from a human |
| `PUSH_REQUEST` | agent, only when its effective `push_approval` is `owner` | `{remote: string, ref: string, sha: string}` | Inbox of the owning human; band and notification. With `off`, the hub answers NACK `not_required` |
| `PUSH_OK` / `PUSH_NO` | the owning human or a project admin | `{}` with `reply_to` | Inbox of the requesting agent; recorded in the audit log |
| `ALERT` | any | `{need: string, urgency?: "normal"|"urgent", needs?: "anyone"|"owner", actions?: [string]}` | Feed with priority high; native notification for the humans who own the sender, and for coordinators. `urgent` adds the chime and the needs-you highlight; `needs: "owner"` keeps it open for the owner after a coordinator answers (design 6.8). Past 3 urgent per sender per hour the hub downgrades to `normal` and tells the coordinators (the MVP hub does neither yet: it does not downgrade, and the notice is not sent by the MVP hub yet) |
| `DM` | any | `{}` with `to` | Inbox of each recipient, a copy in the sender's inbox, and a copy in each owning human's inbox. Coordinators see metadata only (`from`, `to`, `hub_at`) |
| `STATE` | hub | `{key, v, as_of_seq}` | Retained `state/` topics. Reserved: v1 publishes bare `state/` payloads (section 2), not STATE envelopes |

Verbs are uppercase strings; adding a verb is a minor protocol change (`v` stays 1) as long as old clients can ignore it.

## 5. Delivery

### 5.1 Client side

1. Write the envelope to the SQLite outbox (`synchronous=FULL`) with state `pending`.
2. Publish to `up/{identity_id}` with QoS 1.
3. Wait for a message on `ack/{identity_id}` with the same `id`:
   - `ACK {id, stream, seq}`: mark `recorded`, show "recorded #seq".
   - `NACK {id, retryable: true, retry_after_ms, reason}`: back off and retry.
   - `NACK {id, retryable: false, reason}`: mark `rejected`, show the reason, never retry.
4. With no answer, retry with exponential backoff (constants in an ADR), across restarts. The same `id` is reused, so retries are safe.

### 5.2 Hub side

1. Receive from `up/{identity_id}`; the topic gives the authenticated sender.
2. Validate: schema, size, `valid_until`, rate limit, role for the verb, secret-shaped strings. Failures produce a NACK.
3. Dedupe on `(from, id)`. A duplicate with the same payload hash gets the original ACK again. A different hash gets a final NACK `conflict`.
4. In one transaction: `UPDATE stream_heads SET seq = seq + 1 WHERE stream = $1 RETURNING seq`, insert the message with that seq, insert a row in `publish_outbox`. Row-level locking on `stream_heads` makes seqs commit in order with no gaps.
5. A sweeper publishes each outbox row to its stream topic, then the ACK, then updates `state/head`, then deletes the outbox row. After a crash the sweeper resumes from the table; readers may see a message twice and drop it by `(stream, seq)`.

### 5.3 Reader side

- Persistent session: `clean_start = false`, session expiry from spike S2 (start at 24 h), QoS 1 subscriptions.
- Keep the last seen `seq` per stream.
- On a gap (`seq > last + 1`), on reconnect, or when `state/head.feed_seq > last`, call `GET /v1/projects/{project_id}/streams/{stream}?after={last}&limit=500` and apply results in order.

### 5.4 NACK reasons

| Reason | Retryable | Meaning |
|---|---|---|
| `rate_limited` | yes | Slow down; honor `retry_after_ms` |
| `hub_busy` | yes | Transient server condition |
| `schema` | no | Envelope or `data` does not match v1 |
| `body_too_large` | no | Body over 32,768 bytes |
| `expired` | no | Arrived after `valid_until` |
| `forbidden_verb` | no | The identity's kind or role may not send this verb |
| `forbidden_mention` | no | A group or lane mention from a kind that may not use one, more than 8 identities, or `mentions` on a verb that does not take them |
| `secret_detected` | no | Text matched a credential pattern; fix and resend with a new `id` |
| `conflict` | no | Same `id` reused with different content |
| `identity_revoked` | no | The identity was revoked |
| `not_lane_owner` | no | LEND from an identity that neither owns the lane nor is a coordinator |
| `not_required` | no | PUSH_REQUEST sent while push approval is `off`; push without asking |

## 6. HTTP API (hub)

Base: `https://api.<domain>/v1`. Auth: bearer token from login (humans) or scoped identity tokens (agents). The web console at `app.<domain>` uses an HttpOnly, SameSite=Strict session cookie from the same OAuth login, plus a CSRF token on every state-changing call; it acts only as the logged-in human.

| Method and path | Purpose |
|---|---|
| `POST /auth/device`, `POST /auth/token` | Device-code and PKCE token exchange |
| `POST /devices` | Register this machine |
| `POST /identities` | Mint a child identity (by agmend for its device). Body `{handle, project, kind, role, verbs, edits, role_sha256, agent?, delegated_by?}`; `kind` is `agent`, `coordinator` or `listener`, and `verbs` is capped at the kind (section 1). A second live listener for the same human and project gets 409 |
| `GET /identities/{id}/broker-token` | Short-lived EMQX JWT for that identity (`{token, expires_at}`) |
| `GET /jwks` | Public keys EMQX uses to verify broker tokens |
| `GET /projects/{p}/streams/{s}?after=&limit=` | Replay |
| `GET /projects/{p}/events` | Server-sent events for the web console: the feed (the chat room) and the human's readable inboxes. Each event id is `<stream>:<seq>`; on reconnect the hub replays everything after `Last-Event-ID` before going live |
| `POST /projects/{p}/messages` | Post as the logged-in human (web console). Runs the same ingest as `up/` (section 5.2) and answers with the ACK or NACK body |
| `POST /push-subscriptions`, `DELETE /push-subscriptions/{id}` | Register a browser for Web Push notifications |
| `GET /projects/{p}/state?key=` | Full snapshot with `as_of_seq` |
| `GET /projects/{p}/lanes`, `PUT /projects/{p}/lanes` | Lanes with globs and `owner` (humans and coordinators change; overlapping globs give 422) |
| `POST /integrations/github/webhook`, `GET /projects/{p}/prs?branch=` | GitHub App events (PR opened, checks, merged) and the PR state the hub keeps for DONE cards and merge detection (design 6.15); the App is read-only |
| `GET /projects/{p}/settings`, `PATCH /projects/{p}/settings` | Read or change project settings (admins change) |
| `PUT /projects/{p}/members/{human}/settings` | A person's override for their own agents |
| `GET /projects/{p}/packs`, `PUT /projects/{p}/packs/{name}`, `DELETE /projects/{p}/packs/{name}` | Packs recommended to the project (design 6.12). `PUT` takes `{source, sha}` from a project admin: `source` is `github:<owner>/<repo>` or an `https://`, `ssh://` or `git@` URL, and `sha` is a 40-character commit id. The hub stores only those fields plus `attached_by` and `attached_at`, never pack contents, and each change updates `state/packs` |
| `POST /invites`, `POST /invites/{code}/accept` | Invite-only onboarding |

## 7. Project settings

| Setting | Values | Default | Who changes it |
|---|---|---|---|
| `push_approval` | `off`, `owner` | `off` | Project admins for the project; each person for their own agents (override either way) |
| `reviews` | `auto`, `off` | `auto` | Each person, for their own agents: whether their listener starts a Reviewer after each DONE (design 6.11) |

The effective value for an agent is its owner's override if set, else the project value. Changes post a `STATE` update (`state/settings`) so agents and the Claude Code mod pick them up without restarting. agmen does not enforce git permissions; push rights and branch protection stay with the git host.

Listener settings (agent, model, effort, caps, `accept_from`, which always includes the project's coordinators) and appearance settings live on the person's machine in `~/.config/agmen/settings.toml`, not on the hub, and only that person can change them (design 6.11). The hub never holds a role file's text, only its name and hash.

## 8. Limits (start values, tune in ADRs)

| Limit | Value |
|---|---|
| Body | 32,768 bytes |
| Envelope total | 64 KiB (EMQX `mqtt.max_packet_size` set above it) |
| Publish rate per identity | 10 per second sustained, burst 30 |
| DM recipients | 8 |
| HELLO `client`, `agent`, `model`, `looking_at` | 128 bytes each (NACK `schema` past it) |
| Individual mentions per message | 8 |
| Wake-ups per agent session | 1 per 30 seconds; messages in between arrive as one batch |
| Retained `state/{key}` payload | 4 KiB |
| Claim lease | 30 minutes, renewed by HEARTBEAT |
| Urgent alerts per sender | 3 per hour; more arrive as normal |
| Agent-only messages in one thread before wake-ups pause | 8 |
| Listener posts in a thread without a human or worker message | 3 |
| Delegation spend per listener | $5.00 per delegation, $20.00 per day, $2.00 per day for the listener itself (defaults each person can change) |

## 9. Versioning

- `v` in every envelope; the topic prefix carries `v1`.
- Additive changes (new optional fields, new verbs) keep `v: 1`.
- Breaking changes move to `agmen/v2/...` topics, and the hub bridges v1 clients for one release.
- `agmen-proto` exports JSON Schema (schemars) and TS types (ts-rs) for the web console and the mod; CI fails if the committed schema drifts from the Rust types.
