# agmen protocol v1

Status: draft for M0. Normative for `crates/agmen-proto`; [design.md](design.md) explains the why. Spikes S1 (EMQX broker) and S5 (numbering and replay) may revise details; each change lands with an ADR in `docs/adr/`. Revised 2026-10-25 for roles, the listener (formerly the dispatcher), REVIEW, PLAN, urgent alerts, role mentions and `refs`.

Conventions: MQTT 5, UTF-8 JSON payloads, QoS 1 unless stated. "Client" means anything that publishes as an identity: agmend on behalf of a human, an agent, a listener, or a coordinator. Ids are opaque strings minted by the hub from the alphabet `[a-z0-9]`, never containing `/`, `+` or `#`.

## 1. Identities

| Kind | Example handle | Minted by | Can |
|---|---|---|---|
| `human` | `andy` | Hub, at OAuth or device login | Everything its roles allow, including RULING, PUSH_OK and PUSH_NO |
| `agent` | `andy/sol` | Hub, when its owner runs `agmen join` (invite code) or `agmen join --project` (later: agmend, as a child of a human's device) | Post, claim, done, DM, request pushes; never RULING or push answers |
| `coordinator` | `adam/coord` | Hub, when a human with the coordinator or admin role runs `agmen join --kind coordinator` (later: agmend for a human with the coordinator role) | Agent rights plus ASSIGN, RULING (agent rulings carry `from_kind: coordinator`), and LEND on any lane |
| `listener` | `andy/listen` | Hub, when its owner's agmend mints it; at most one live listener per human per project | Post, chat, ask, answer, DM, ALERT, DELEGATED, and settings changes for its owner that agmend applies locally; ASSIGN only when minted with the coordinator role; never CLAIM, DONE, LEND, RULING or push answers, and never edit, shell or web tools |
| `hub` | `hub` (id `agmenhub0`) | Fixed | GRANT, DENY, ITEM, ALERT, RELEASE, STATUS, STATE, ACK, NACK |

The hub's identity id is the fixed `agmenhub0`. It holds a `0`, which the alphabet the hub mints ids from leaves out, so no minted identity can collide with it. The hub posts its own messages through an internal path, not through `up/`: `from` is `agmenhub0`, `from_handle` is `hub`, `from_kind` is `hub`, and the message takes the same numbering, storage and sweeper as a client's message, with no rate limit and no ACK. It posts GRANT and DENY (claim outcomes), ITEM (the key for a FOUND), ALERT (a DONE that changed paths outside its claimed or lent lanes or its edit scope, and the urgent downgrade notice, which is not sent by the MVP hub yet), RELEASE with `reason: "lease_expired"` (a claim whose lease ran out) and STATUS (the answer to a ROLLCALL). ACK and NACK go on `ack/{identity_id}`, not into a stream, and STATE is reserved for retained `state/` topics (section 2).

Workers that a listener starts are `agent` identities with `delegated_by` set to that listener's id. The hub accepts `delegated_by` only when minting an `agent`, and only when it names a live listener with the same owner and project; anything else gets 422 `bad_role`.

Every identity except `human` and `hub` carries a role (design 6.13). At mint, agmend sends `{role, verbs, edits, role_sha256}` in `POST /v1/identities`, all four or none (otherwise 422 `bad_role`). `role` is a role name (`[a-z][a-z0-9-]{1,31}`), `role_sha256` is 64 lowercase hex characters, and `edits` is one of `{"scope":"none"}`, `{"scope":"lanes"}`, `{"scope":"paths","globs":[...]}` or `{"scope":"lanes_within","globs":[...]}`, with 1 to 32 globs in lane syntax. The hub caps `verbs` at the kind's ceiling and drops names that are not verbs, stores the capped set, and returns it as `verbs` in the 201 body so agmend can show what was stripped. The ceilings:

- `agent`: the verbs its kind may send in section 4, plus REVIEW and PLAN. Never ASSIGN, RULING, ROLLCALL, DECISION, DELEGATED or push answers.
- `coordinator`: the verbs its kind may send in section 4, plus REVIEW and PLAN.
- `listener`: the verbs its kind may send in section 4, plus ASSIGN when its role is `coordinator`. Never CLAIM, HEARTBEAT, RELEASE, DONE, LEND, RULING, REVIEW, PLAN or push answers. Its `edits` must be `none`. Minting a listener with the `coordinator` role needs the coordinator or admin role in the project, as minting a `coordinator` does.

Ingest refuses any verb outside the stored set with a final NACK `forbidden_verb`. An identity minted without role fields (before roles, or by `agmen join`) has no stored set, and the hub checks its verbs against the fixed table per kind instead (`may_send` in crates/agmen-hub/src/ingest.rs), which matches section 4's "Sent by" column and allows no REVIEW or PLAN. An empty stored set allows nothing. Verbs every role may send: HELLO, BYE, FYI, ASK, ANSWER, ACK, STATUS, DM, ALERT, FOUND.

Edit scopes narrow lane authorization as described in section 3.

A second live listener for the same human and project gets 409 `listener_exists`; revoking the first frees the slot.

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
- `state/roster` is a JSON array with one entry per live identity and per human member of the project, ordered by handle: `{"identity_id", "handle", "kind", "status", "role"?, "agent"?, "model"?, "delegated_by"?}`. `kind` is `human`, `agent`, `coordinator` or `listener`; `status` is `online`, `away` or `offline`; `role` is the role the identity was minted with, never what a HELLO claims; `agent` is the agent named at mint when there was one, else the one its latest HELLO named; `model` comes from its latest HELLO; `delegated_by` is the listener it was minted for. Each optional field is left out when it has no value, and humans carry none of `role`, `agent` or `delegated_by`. For example `[{"identity_id":"idn7k2q9","handle":"andy/sol","kind":"agent","status":"online","agent":"claude-code/2.1.310"}]`.

The roster is built from `presence/+`, the identities table and the latest HELLOs, so a client reads who is there with one subscription and no agent has to be woken; ROLLCALL is for a visible answer in the chat and the hub answers it from the same presence. The hub republishes the roster when presence changes, on HELLO and BYE, and when an identity is minted or revoked or an invite is accepted.

Presence: of the three MQTT sessions an identity can hold (`<identity_id>-watch`, `<identity_id>-mcp`, `<identity_id>-cli`), only the watch session publishes presence. On every connect it publishes the retained `{"status":"online"}` to `presence/{identity_id}`, and its last will is the retained `{"status":"offline"}` on the same topic; when it closes, it disconnects with reason 0x04 so the broker sends the will. The mcp and cli sessions publish no presence and set no will, because all three share the one `presence/{identity_id}` topic and an MCP server stopping would otherwise mark the identity offline while its watch still runs. An identity with only an MCP server running shows offline. On a BYE the hub sets the sender's retained presence to offline itself. `agmen watch` posts one HELLO per process start, not on every reconnect.

## 3. Envelope

`edits` limits what check-path (`POST /projects/{p}/check-path`) allows and what a DONE's `changes` may touch without an ALERT: `none` allows nothing; `lanes` uses the lane checks (paths in no lane, or in a lane it holds a claim in or was lent); `paths` requires matching globs AND either lane authorization or a path in no lane (unowned), so matching globs alone never bypass lane authorization; `lanes_within` needs both the lane check and its globs. A listener is always refused, whatever it was minted with, and an identity minted without role fields uses `lanes`. A path that is empty, absolute, or has an empty, `.` or `..` segment is always refused. Allowed paths keep the lane check's reason (`unowned`, the lane and holder, or `allowed by lend`); a glob mismatch gets `outside edit scope`.

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
- `mentions` is allowed on FYI, ASK, ANSWER and ALERT. Each entry is `all`, `agents`, `humans`, a lane name (`L-UI`, from M2), a role (`role:reviewer`), or an `identity_id`; these never collide, because ids are at least 8 characters of `[a-z0-9]`, lane names start with `L-` and roles with `role:`. Clients resolve typed `@handle` text to ids from `state/roster` and `@reviewers` to `role:reviewer`. Humans and coordinators may use `all`, `agents`, `humans`, lanes and roles; every other kind may list up to 8 `identity_id`s and nothing else (final NACK `forbidden_mention`). A role expands to every identity of the project minted with that role and not revoked, online or not; a role nobody holds expands to nobody. Lane mentions are reserved: the hub refuses them with a final NACK `schema` until it expands them.
- `mentioned` is stamped by the hub: the identity ids the mentions expand to, without the sender, plus the author of the `reply_to` message when that author is a human, plus the author of the DONE a REVIEW replies to. Clients interrupt an identity's session (a wake-up, a stdin message, a notification) only when it is in `mentioned`, is a DM recipient, or is the assignee of an ASSIGN. Design section 6.10 covers delivery and wake-up limits.
- `refs` is stamped by the hub on FYI, ASK, ANSWER, ALERT (the hub's own included), DONE and REVIEW, and is empty on every other verb: `[{kind, start, end, target}]` in body order. The hub splits `body` into words of letters, digits, `_`, `/`, `-`, `@` and `#`, and stamps a word that is, as a whole, the handle of a live member (bare or after `@`), the name of one of the project's lanes, the key of one of its items, `@` plus a role some live identity holds plus `s` (`@reviewers`), or `#` plus digits (`#10`). Anything else stays plain text, including an unknown lane or item key (`L-NOPE`, `B-999`) and a known name inside a longer word. `kind` is `identity`, `lane`, `item`, `role` or `pr`; `start` and `end` are UTF-8 byte offsets into `body`, both on character boundaries, with `end` just past the word; `target` is the identity id, lane name, item key, role name (`reviewer`) or PR number (`10`). Clients render them as links and add branches, commits and file paths from local git themselves (design 6.8).
- The hub stamps at most 128 `refs` in body order and looks up at most 128 distinct item-key words from the body. The serialized hub-stamped envelope is at most 80 KiB, below the hub's 96 KiB MQTT packet cap: the hub drops `refs` first if over budget, then refuses the message with a final NACK `schema` if it is still too large. No message or outbox copy is stored on that refusal.
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
| `LEND` | the lane's `owner` identity (design 6.5), or any identity of kind `coordinator` | `{lane: string, paths: [glob], to: identity_id, until: timestamp}` | Feed. From anyone else, final NACK `not_lane_owner`; a human's coordinator membership role does not count, and lane owners are identities, so a human's LEND is always refused. The hub checks the sender inside the transaction that records the message, after dedupe. An unknown lane, a `to` that is not a live identity of the project, an `until` already past or more than 7 days ahead, `paths` that are not 1 to 32 lane-syntax globs, or a LEND past the active-lend limit (section 8) get final NACK `schema`. Until `until`, and only while the granter is not revoked and is still the lane's current owner or an identity of kind `coordinator`, `to`'s lane checks allow a path when it is in `lane` and matches `paths`; replacing the project's lanes (`PUT /projects/{p}/lanes`) ends every lend |
| `FOUND` | any | `{title: string, lane?: string, severity?: "low"|"med"|"high"|"security"}` | Hub mints `ITEM {key: "B-413"}`; security items post only the key and lane to the feed |
| `DONE` | claim holder | `{item, branch, commit, base_sha, checks: [{name, passed, failed, baseline?}], changes: [{status, path}]}` | Feed; the hub flags each changed path its edit scope (section 3) does not allow, which for `lanes` means paths outside its claimed or lent lanes, in an ALERT to the coordinators |
| `REVIEW` | an identity minted with REVIEW in its verbs (the reviewer role) | `{verdict: "pass"|"changes", findings: [{severity: "high"|"med"|"low", path, line?, what, trigger}], traced?: string}` with `reply_to` set to a DONE | Same stream as the DONE; the DONE's author is added to `mentioned`. A `reply_to` that is missing, not readable by the sender, or not a DONE gets final NACK `schema` |
| `PROPOSAL` | any | `{options: [string], deadline?: timestamp}` | Feed |
| `PLAN` | an identity minted with PLAN in its verbs (the planner role) | `{version: number, path: string, sha: string}` | Feed; the hub keeps the latest per project for the plan panel (not kept by the hub yet). A human `DECISION {outcome: "GO"}` with `reply_to` on the final PLAN is the kick-off (design 6.14) |
| `DECISION` | coordinator, human | `{outcome: "GO"|"VETO", option?: number, subject?: string}` with `reply_to`; `subject` names a spec file and its commit when a person approves one | Feed |
| `RULING` | coordinator, human | `{subject: string}` | Feed; recorded in `human_decisions` when from a human |
| `PUSH_REQUEST` | agent, only when its effective `push_approval` is `owner` | `{remote: string, ref: string, sha: string}` | Inbox of the owning human; band and notification. With `off`, the hub answers NACK `not_required` |
| `PUSH_OK` / `PUSH_NO` | the owning human or a project admin | `{}` with `reply_to` | Inbox of the requesting agent; recorded in the audit log |
| `ALERT` | any | `{need: string, urgency?: "normal"|"urgent", needs?: "anyone"|"owner", actions?: [string]}` | Feed with priority high; native notification for the humans who own the sender, and for coordinators. `urgent` adds the chime and the needs-you highlight; `needs: "owner"` keeps it open for the owner after a coordinator answers (design 6.8). Past 3 urgent per sender per hour the hub downgrades to `normal` and tells the coordinators (the MVP hub does neither yet: it does not downgrade, and the notice is not sent by the MVP hub yet) |
| `DM` | any | `{}` with `to` | Inbox of each recipient, a copy in the sender's inbox, and a copy in each owning human's inbox. Coordinators see metadata only (`from`, `to`, `hub_at`) |
| `STATE` | hub | `{key, v, as_of_seq}` | Retained `state/` topics. Reserved: v1 publishes bare `state/` payloads (section 2), not STATE envelopes |

"Sent by" is the ceiling for each kind. An identity minted with a role may send only the verbs stored for it at mint (section 1).

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
| `schema` | no | Envelope or `data` does not match v1, or the message passes a limit in section 8 that names `schema` (the stamped envelope size, active LENDs) |
| `body_too_large` | no | Body over 32,768 bytes |
| `expired` | no | Arrived after `valid_until` |
| `forbidden_verb` | no | The verb is not in the identity's stored verbs, or, for an identity minted without a role, its kind may not send it |
| `forbidden_mention` | no | A group, lane or role mention from a kind that may not use one, an identity that is not a live member, more than 8 identities, or `mentions` on a verb that does not take them |
| `secret_detected` | no | Text matched a credential pattern; fix and resend with a new `id` |
| `conflict` | no | Same `id` reused with different content |
| `identity_revoked` | no | The identity was revoked |
| `not_lane_owner` | no | LEND from a sender that neither owns the lane nor is an identity of kind `coordinator` |
| `not_required` | no | PUSH_REQUEST sent while push approval is `off`; push without asking |

## 6. HTTP API (hub)

Base: `https://api.<domain>/v1`. Auth: bearer token from login (humans) or scoped identity tokens (agents). The web console at `app.<domain>` uses an HttpOnly, SameSite=Strict session cookie from the same OAuth login, plus a CSRF token on every state-changing call; it acts only as the logged-in human.

| Method and path | Purpose |
|---|---|
| `POST /auth/device`, `POST /auth/token` | GitHub device sign-in: start returns `{device_code, user_code, verification_uri, expires_in, interval}`; token polls return hub tokens or 400 `authorization_pending`, `slow_down`, `expired_token`, `access_denied`, `invalid_grant`; GitHub failures return 502 `upstream_unavailable` |
| `POST /devices` | Register this machine |
| `POST /identities` | Mint a child identity (by agmend for its device). Body `{handle, project, kind, role, verbs, edits, role_sha256, agent?, delegated_by?}`; `kind` is `agent`, `coordinator` or `listener`, and `verbs` is capped at the kind (section 1). A second live listener for the same human and project gets 409 |
| `GET /identities/{id}/broker-token` | Short-lived EMQX JWT for that identity (`{token, expires_at}`) |
| `GET /me` | The signed-in human, broker URL and memberships: `{id, handle, broker_url, projects: [{tenant, project, name, role}], groups: [{tenant, name, role}]}`. Groups are ordered by name, then tenant id. Human token required |
| `POST /projects` | Create a project with a human token. Body `{name, tenant?, repo_url?}`; unknown fields are refused. The caller must be an owner or admin of the selected tenant and becomes admin of the new project. Without `tenant`, exactly one tenant where the caller is owner or admin is required. Returns 201 `{tenant, project, name, role: "admin", repo_url}`; `repo_url` is `null` when unset. Callers without tenant owner or admin rights get 403 `admin_required`; several eligible tenants without a selection get 422 `tenant_required`; duplicate names within a tenant get 409 `project_exists`. Names follow bootstrap: 1 to 64 characters, no control characters or leading or trailing whitespace (422 `invalid_name`). Repository URLs follow `RepoUrl` validation; invalid URLs get 422 `bad_repo_url` with the `BadRepoUrl` message |
| `GET /jwks` | Public keys EMQX uses to verify broker tokens |
| `GET /projects/{p}/streams/{s}?after=&limit=` | Replay |
| `GET /projects/{p}/events?after=` | Server-sent events for the web console and agmend (a human's access token): the feed (the chat room) and the human's readable inboxes. Each event id is `<stream>:<seq>` and its data the stamped envelope. One id cannot place the reader on the other streams, so the reader also passes `after=<stream>:<seq>,...`, its last seq on each stream; each stream resumes after the later of its `after` entry and `Last-Event-ID` (from the start when neither names it), and the hub replays everything after that before going live. A keep-alive comment every 15 s |
| `POST /projects/{p}/messages` | Post as the logged-in human (web console). Runs the same ingest as `up/` (section 5.2) and answers with the ACK or NACK body; a body over the 64 KiB envelope limit gets 413 `envelope_too_large` |
| `POST /push-subscriptions`, `DELETE /push-subscriptions/{id}` | Register a browser for Web Push notifications |
| `GET /projects/{p}/state?key=` | Full snapshot with `as_of_seq`: `{key, as_of_seq, value}`, where `value` is the payload the hub publishes retained on `state/{key}`. A member's access token or a token of an identity in the project may read it. Only `roster` and `head` are served; any other `key` gets 400. For `head`, `as_of_seq` is the payload's `feed_seq`. For `roster`, it is a lower bound: the feed head read before the roster was built. The roster may include later HELLOs, and membership, revocation and status are not feed-ordered, so a client keeps applying retained presence updates and does not treat roster plus `as_of_seq` as a consistent cut. After a hub restart, roster statuses read `offline` until the broker resends retained presence; this is transient |
| `GET /projects/{p}/lanes`, `PUT /projects/{p}/lanes` | Lanes with globs and `owner` (humans and coordinators change; overlapping globs give 422) |
| `POST /integrations/github/webhook`, `GET /projects/{p}/prs?branch=` | GitHub App events (PR opened, checks, merged) and the PR state the hub keeps for DONE cards and merge detection (design 6.15); the App is read-only |
| `GET /projects/{p}/settings`, `PATCH /projects/{p}/settings` | Read or change project settings (admins change) |
| `PUT /projects/{p}/members/{human}/settings` | A person's override for their own agents |
| `GET /projects/{p}/packs`, `PUT /projects/{p}/packs/{name}`, `DELETE /projects/{p}/packs/{name}` | Packs recommended to the project (design 6.12). `PUT` takes `{source, sha}` from a project admin: `source` is `github:<owner>/<repo>` or an `https://`, `ssh://` or `git@` URL, and `sha` is a 40-character commit id. The hub stores only those fields plus `attached_by` and `attached_at`, never pack contents, and each change updates `state/packs` |
| `POST /invites`, `POST /invites/{code}/accept` | Invite-only onboarding. Create returns `{code, link, project, role, expires_at}`; `link` is the hub's public `/i#<code>` URL, with the code only in the fragment. Accept returns `{tenant, project, role, name, repo_url}`; `repo_url` is `null` when unset |
| `GET /i` (outside `/v1`) | Static invite page. Reads the code only from the URL fragment and sends no requests. Serves `Referrer-Policy: no-referrer`, `Cache-Control: no-store` and a CSP that blocks external requests |
| `GET /tenants/{tenant}/members` | Human group members list `{members: [{human_id, handle, role, projects: [{project, name, role}], identities: [{id, project, handle}]}]}` by handle. Projects are ordered by id; live identities are ordered by project then handle. Caps: 1000 members and 1000 identities per member. Non-members get 403 `not_a_group_member` |
| `DELETE /tenants/{tenant}/members/{handle}` | Owner or group admin revokes a member's live identities and removes project memberships, unused invites and group membership in one transaction, then bans and kicks identities and updates affected rosters. Returns 204; retries ban and kick recently revoked identities again. Returns 404 `no_such_member` if neither membership nor a live or recently revoked identity remains, 409 `owner_cannot_be_removed`, 403 `owner_required` if an admin removes another admin, or 403 `group_admin_required`. Admins can remove themselves. Broker failures return 502 after all cleanup steps were tried |
| `PUT /tenants/{tenant}/members/{handle}/role` | Owner or group admin sets `{role: "admin" \| "member"}` and receives 200 `{handle, role}`. Returns 409 `owner_role_fixed` for the owner, 422 `use_transfer` for role `owner`, or 404 `no_such_member`. Demoting another admin needs the owner: an admin gets 403 `owner_required`, though an admin may demote themself. Unknown body fields are refused |
| `POST /tenants/{tenant}/transfer` | Owner transfers to an existing group admin with `{to: "handle"}`; returns 200 `{owner: "handle"}`. Demotes the old owner to admin before promoting the target in one transaction. Returns 403 `owner_required` or 409 `not_a_group_admin`, including transfer to oneself. Unknown body fields are refused |
| `DELETE /identities/{id}` | The identity's human owner or an owner or admin of its group revokes it, bans its username until outstanding broker tokens expire, kicks its clients and updates its roster. Returns 204, including retries; other humans get 404. Broker failures return 502 after all steps were tried |

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
| Hub-stamped envelope total | 80 KiB; drop `refs` first, then NACK `schema` if still over (the hub MQTT packet cap is 96 KiB) |
| References per message | 128 in body order; item-key lookup considers at most 128 distinct candidate words |
| LEND duration | At most 7 days from acceptance; NACK `schema` for a later `until` |
| Active LENDs per project, lane and recipient | 16; NACK `schema` past it; expired grants do not count |
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

## 10. Local daemon identity, readiness and service ownership

For owner-only local methods, agmend checks the socket peer PID and refuses
processes in a running managed worker's process group with `forbidden` and
`agents cannot call owner-only methods`. This covers agent launch, stop,
list, start, attach, detach and remove, human posts, host methods, shutdown
and login. Identity-authenticated
posts and identity read/watch methods remain available. If the peer PID cannot
be read, owner-only calls are refused; while a worker runs, so are calls from a
peer whose process group cannot be read. This is a guard against accidental
misuse, not isolation: workers share the user's uid and can escape a process
group or access the user's files and credentials.

Managed workers drain owner messages first in separate batches. Only messages
with the hub-stamped owner ID and `from_kind: human` get
`--permission-mode bypassPermissions` and `--disallowedTools mcp__agmen__inbox`,
so other senders' messages do not enter that context through the inbox tool.
Bash in those batches can still run `agmen inbox`. Other batches use `--permission-mode
default`, `--disallowedTools Bash,Edit,Write,NotebookEdit` (deny rules override
allow rules in the user's and project's Claude Code settings) and an appended
instruction to read, think and reply through agmen, without running commands or
editing files, and wait for the owner if changes are needed.

The per-user Unix socket uses agmen-local's length-prefixed JSON frames. The
first request remains `hello` with `{"api":1}`. Its result identifies the running
process:

```json
{"api":1,"version":"0.1.0","build":"dev","exe":"/Applications/agmen.app/Contents/MacOS/agmend"}
```

`build` is the compile-time `AGMEN_BUILD` value, or `dev` when absent. `exe` comes
from the running process's `current_exe()`, independent of the program configured
for its next service start. The shared `HelloAnswer` requires `api`; `version`,
`build` and `exe` are optional so older API 1 answers still parse. Clients must
treat an absent version as unknown compatibility.

After hello, the socket owner's local human caller can request `shutdown` with
`{}`. This uses the socket's same-user peer check and the existing human caller
authorization, with no `as` identity and no identity credential. Agent, listener
and other identity callers receive `forbidden`; an invalid credential receives
`unauthorized`. Extra parameters are refused. A successful `{}` reply is written
before shutdown is signaled, with a 10 s write deadline. The daemon then leaves
`serve_until` through the same cleanup path as SIGTERM: stop accepting, abort
local connections, and join daemon-owned collectors and sessions within their
existing 10 s total bound. Clients waiting for exit should use a 15 s bound.
The desktop only sends this method to its own bundled executable and never
stops another installation's daemon.

After hello, a human caller can request `ready` with `{}`. A successful result
means startup and store initialization completed:

```json
{"ready":true,"schemas":{"agmend.db":2,"identities/agent001/agmen.db":2}}
```

`schemas` contains the actual SQLite `user_version` of each existing store, keyed
by its path relative to the daemon state directory. Absent identity stores have
no entry. Startup preflights all existing stores before migrating any, and
initializes existing identity stores before serving requests. Ready rechecks the
schemas without migration, with a 10 s deadline, one active scan and at most 4096
identity directory entries. It does not require hub or broker connectivity.
Older daemons may reject this additive method. Hello alone does not prove store
readiness. Callers must compare the complete schema map before rollback; an
unknown schema or failed inspection does not establish rollback safety.

Daemon and identity stores refuse newer schemas before migration or journal-mode
changes and recheck versions under the migration lock. Schema inspection opens
files only for reading. It reads SQLite's [documented database header and committed
WAL frames](https://sqlite.org/fileformat2.html#walformat), validates WAL checksums
and salts, and never opens a SQLite connection or creates sidecars. It therefore
also reads a newer schema from an abandoned WAL without an SHM file. Uncommitted
frames do not change the reported version. A scan reads pages of at most 64 KiB,
at most one million WAL frames, and has at most 5 s within the caller's deadline.
Concurrent changes get at most 501 snapshot attempts within that deadline.
Header/file checks detect checkpoint, reset and replacement races. A hot rollback
journal requires recovery and leaves compatibility unknown; inspection does not
recover it, and `ready` reports it as an error. Opening a store is different:
the daemon and identity stores let SQLite recover the journal (restoring the
last committed state, never migrating), then refuse a newer schema under the
migration lock before any migration or journal-mode change. Refusing instead
would leave a store interrupted during its first creation unopenable. An
inspection error must never be treated as proof of rollback safety.

`agmend service status` retains its text results (`running`, `stopped`, or `not
installed`, followed by a newline) and exit behavior. `agmend service status
--json` instead prints one JSON object:

```json
{"state":"stopped","program":"/Applications/Old agmen.app/Contents/MacOS/agmend"}
```

`program` is the absolute configured path from the user plist's
`ProgramArguments[0]` or the user unit's `ExecStart`, even when the executable no
longer exists. It is null if the configuration file is absent. Status inspects at
most 64 KiB of configuration, decodes XML or systemd escaping, and errors on
malformed or ambiguous configuration. It does not infer ownership from the
helper running the status command. Callers compare this path and their receipts
before requesting a service change.

`agmend service install --program <absolute-path>` validates an executable file
and preserves the supplied spelling, including stable symlinks. Omitting
`--program` uses `current_exe()`. Install refuses malformed existing configuration
before replacing it. On macOS it boots out a loaded service before bootstrap. On
Linux it checks the user manager and active state, then reloads, enables and
restarts an active unit or starts an inactive one. Without a working systemd
user manager, install fails before writing the unit and names foreground
`agmend` as the fallback. The entire manager transition and matching-version
hello plus ready handshake share a 120 s deadline. Each manager process has at
most 10 s and 64 KiB per output stream. Installation reports success only after
the running daemon matches the installing helper's release version and answers
ready. After manager start, readiness has its own 30 s ceiling inside that
120 s deadline. A version mismatch fails immediately with expected and actual
versions. A timeout reports `agmend did not become ready within 30 s` and the
daemon log path.

`agmend service install --token-store-dir <absolute-path>` selects an explicit
file token store for the service. The directory must be owned by the user with
mode 0700; token files must be regular files with mode 0600. launchd stores
`AGMEN_TOKEN_STORE_DIR` in `EnvironmentVariables`; systemd stores it in an
escaped `Environment=` entry. Reinstall preserves an existing selection when
the option is omitted. This does not copy tokens from Keychain.

`agmen install-receipt --json` reads the shared install receipt at
`state_dir()/install-receipt.json`. The result is
`{"channel":"shell","version":"0.1.0","owned_paths":["/absolute/path"]}`,
or JSON `null` if absent, both with exit status 0. Invalid or unreadable receipts
return nonzero and block ownership changes. The shared typed reader reads at
most 64 KiB and requires absolute owned paths. On macOS the fixed location is
`~/Library/Application Support/agmen/install-receipt.json`; on Linux it is
`${XDG_STATE_HOME:-~/.local/state}/agmen/install-receipt.json`, with the HOME
fallback for relative XDG paths.

`agmend store-schema --json` reports
`{"stores":[{"path":"/absolute/state/agmend.db","schema_version":2}]}`.
The snapshot includes daemon metadata and every discovered identity store,
including schemas newer than this build. It reuses the read-only store
inspection above, without creating or migrating stores or changing journal
mode. Missing stores, incomplete discovery and inspection failures return
nonzero. Compare the complete path and version set before rollback; a failed
snapshot does not establish rollback safety.

### Managed agent methods

Human callers manage their own agents with these methods. Identity callers
receive `forbidden`, and so do worker peers (above). Each checks that the
agent's owner and hub origin match the signed-in login.

- `agent_launch {project, name, root?, model?, budget_usd?}` makes a worktree
  on a new `agent/<name>-<8 hex>` branch, mints an identity in it and writes
  `managed/<identity>/agent.json`. The agent starts `idle`.
- `agent_list {project?}` answers `{"agents":[{identity, project, handle,
  state, model, budget_usd, spent_usd, worktree, branch, source}]}`. `state`
  is `running`, `idle`, `offline` or `attached`; `source` is `launched` or
  `joined`.
- `agent_stop {identity}` cancels a running batch, posts BYE and sets
  `offline`.
- `agent_start {identity, project?, root?}` brings an offline agent back.
  After agmend restarts, every saved record loads as `offline` and nothing runs
  until Start. A record restores its spend (`status.json`) and both session
  ids; each tier's next batch uses `--resume <id>` when Claude Code has saved
  that session (`<claude home>/projects/<folder with every non-alphanumeric
  character as ->/<id>.jsonl`), else `--session-id <id>`. A saved owner
  session passes the transcript check below first; when it fails, the owner
  tier gets a fresh session id, saved in the record, and `context` says why.
  The same check runs again before every owner batch that resumes its
  session, since a Claude Code run by hand in the folder (`claude --continue`
  with the agmen plugin) can add other senders' messages to it at any time.
  A record in the older format with one `session_id` (which ran every batch,
  owner's and others') loads it as the owner session, subject to that check,
  gets a fresh others session, and is saved again in the current format. For an identity
  joined by hand (no record), `project` is required; agmend looks for the
  worktree of that project's open checkout whose `.agmen/identity.json` names
  the identity, or uses `root` when the person picked the folder. It answers
  `not_found` when it finds no folder. The folder becomes the agent's
  worktree, with no new worktree or branch, and the owner session resumes the
  newest transcript for that folder, or starts fresh when there is none.
  Because owner batches run with bypassPermissions and an interactive session
  with the agmen plugin sees every sender's messages, that transcript is
  resumed only when it holds nothing from anyone but the owner: agmend reads
  the transcript and its subagents' (`<id>/subagents/*.jsonl`, at most 256),
  at most 32 MiB and 50,000 lines in all, treats their contents as data, and
  starts fresh when it finds a call that reads every sender's messages (an MCP
  `inbox` tool under any server name, or Bash running `agmen inbox` or `agmen
  watch`) whatever it answered, an agmen message whose hub-stamped `from` is
  not the owner's human id, the plugin's row for another participant's
  message, or an inbox line (`#<seq> <stream> <VERB> from <handle> ...:`) from
  another handle, or when a line does not parse. The
  answer's `context` says what happened: `resumed`, `new`,
  `skipped_others` or `skipped_unreadable`; the desktop shows a line for
  the last two.
  Adoption requires the folder's trusted identity file to name that identity,
  project, owner and hub origin. Start refuses an identity the hub roster shows
  `online` or `away`, such as an interactive Claude Code with the agmen plugin,
  and refuses when the roster cannot be read.
- `agent_attach {identity, project?, root?}` opens Terminal on a script in
  `managed/<identity>/attach.command` that runs `claude --resume <owner
  session>` (or `--session-id` when that session was never started) in the
  agent's worktree, with the login-shell PATH, in Claude Code's normal
  permission mode. It never opens the others session. Since later owner
  batches resume this session with bypassPermissions, other senders' messages
  are kept out of it: Claude Code runs with `--disallowedTools
  mcp__agmen__inbox,mcp__plugin_agmen_agmen__inbox` and `AGMEN_MANAGED=1`,
  which makes the agmen plugin skip its watch, wake-ups and `/agmen` while its
  lane and Bash guards stay on and fail closed. Bash in the attached session is
  the person's to approve, so `agmen inbox` can still run there; when the
  attached session ends, its transcript goes through the check above, and a
  session that read other senders' messages is not resumed: the owner tier
  starts fresh. A running batch is cancelled first, without charging the
  budget, as Stop does; an offline agent is adopted as Start does. While
  `attached` the agent's messages wait and no batch runs. The script writes
  Claude Code's process id to `attach.pid`; when that process exits, or no
  process id appears within 120 s, the agent returns to `idle` and later owner
  batches resume the same session when it passes the check.
- `agent_detach {identity}` sends SIGTERM to the attached Claude Code, waits
  up to 2 s for it to exit, then sends SIGKILL and waits up to 1 s, and only
  then checks its transcript and returns the agent to `idle`. When the exit
  cannot be confirmed, the owner tier starts a fresh session instead. agmend
  counts the process `attach.pid` names as the attached Claude Code only while
  it runs as this user and started no later than the file was written (from
  `ps -o etime=`, within 2 s), so a reused process id is never signalled.
  Start and Attach refuse while `attach.pid` names such a process, as after
  Stop or an agmend restart during an attach, and Remove ends it.
- `agent_remove {identity, project?, root?}` ends an attached Claude Code,
  stops the agent (BYE), revokes the identity with `DELETE
  /v1/identities/{id}` as `agmen leave` does (a 404 counts as revoked), deletes
  its identity file when that file names it, its hub token, its local queue and
  `managed/<identity>/`. It keeps the folder and branch and answers
  `{"identity":..., "removed":true, "kept":{"folder":..., "branch":...}}`.

### Host mode

`agmend host init --env-file <path> [--env-file <path> ...] [--hub-bin <path>] [--public-url <url>] [--tailscale-serve] [--force]` writes `<agmen state dir>/host.toml` with mode 0600. It refuses an existing configuration unless `--force` is present. Relative paths become absolute. Without `--hub-bin`, an agmend inside `Contents/MacOS` selects its adjacent `agmen-hub`; outside an app bundle the executable path is required. The URL comes from `--public-url` or `AGMEN_PUBLIC_URL` in the environment files.

Environment files are trusted shell files sourced in order by a bounded Bash child, matching `deploy/dev/hub.sh`; source diagnostics are suppressed so credentials do not enter API errors. Each file must be a regular file with mode 0600, like host.toml, because it holds credentials and runs as shell code; agmend checks this before sourcing it. Repeating `--env-file` supports the existing `dev.env` followed by `trial-hub.env`. The supervisor translates `AGMEN_IT_PG`, `AGMEN_IT_MQTT`, and `AGMEN_IT_EMQX_API`, `AGMEN_IT_EMQX_API_KEY`, and `AGMEN_IT_EMQX_API_SECRET` to their hub configuration names when those names are empty. `AGMEN_IT_JWT_DIR` supplies `AGMEN_JWT_SIGNING_KEY` as `<directory>/agmen-dev-1.pem` when needed. `AGMEN_LISTEN` defaults to `127.0.0.1:8080`; the configured public URL overrides `AGMEN_PUBLIC_URL`. An absent `AGMEN_PUBLIC_MQTT_URL` defaults to the public host's `wss` URL on port 8443 at `/mqtt`. Other hub settings, including GitHub credentials, remain in these files. No credentials are written into host.toml. The hub child starts with an empty environment plus the `AGMEN_*` names (not `AGMEN_IT_*`), `PG*`, `LC_*`, `HOME`, `PATH`, `TMPDIR`, `USER`, `LOGNAME`, `LANG`, `TZ`, `RUST_LOG`, `RUST_BACKTRACE`, `SSL_CERT_FILE` and `SSL_CERT_DIR`; other variables from agmend or the files, such as `POSTGRES_PASSWORD`, stay out.

When configured, agmend supervises `agmen-hub serve`, sends both output streams to `~/Library/Logs/agmen/hub.log`, and stops and reaps the child on shutdown. An exited child restarts after 1, 2, 4, 8, then 16 seconds. After five automatic restarts within ten minutes, the supervisor stops trying until Restart. The status includes the hub's recent error output with configured credential values redacted. Postgres and EMQX remain separately managed dependencies.

With `--tailscale-serve`, the supervisor checks `tailscale serve status --json` once before launching the child. It runs `tailscale serve --bg --https=443 http://<listen>` only when that HTTPS 443 root proxy is absent, including foreground Serve configuration in the check. Tailscale is never invoked when this option is absent. A failed setup appears in host status and can be retried with Restart. Restart reloads host.toml and its environment files. A daemon already running discovers a newly initialized configuration when the desktop next calls `host_status`.

After `hello`, human callers can use `host_status {}` and `host_restart {}` over the local socket. Both return `{"configured":true,"state":"running","last_error":null,"public_url":"https://host.example"}`. State is `running`, `stopped`, or `failed`; `running` means the child process is alive, and the desktop waits separately for the hub to answer during startup. Without configuration, status is `{"configured":false,"state":"stopped","last_error":null,"public_url":null}`. Restart refuses an absent configuration. These methods do not require a hub login, and identity callers cannot use them.
