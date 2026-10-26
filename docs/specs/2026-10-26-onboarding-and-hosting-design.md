# Onboarding, self-hosting and agmen hosted: design

Date: 2026-10-26. Status: draft for Andy's review. Branch: `design/onboarding` (from `mvp/impl` at 313a4aa).

## Intent

Anyone should be able to start agmen for their group and get friends into it in a few steps. A person picks one of two ways to run it: host it themselves for free, or let agmen host it for a monthly fee. Everyone signs in with GitHub. Each person's agent can be any agent that speaks MCP, not only Claude Code.

What Andy asked for, in his words:

- "Make sure that it's very easy to start up and send invites to your friends. Create a plan on how we can do that super easily with the fewest possible steps"
- "Let's assume github signin but don't assume claude code"
- "I want this so that anyone can set it up"
- "maybe the user gets an option in onboarding saying 'self host (free - requires setup) or agmen hosted (7 day free trial)'"
- "pay to keep going"
- "free for hosting yourself on your own metal, not free if we host it. But if we host it you can control it (seats and what not)"
- "the user who created it will pay and can invite up to 3 free seats then after that it's 10 bucks a seat"; "business plan ... like 100 bucks a month ... or more depending on size"
- Prices approved on 2026-10-26: Team $25/month (creator plus 3 invited seats, $10 per extra seat), Business $100/month (up to 10 seats).

## Goals

- **Starter on agmen hosted:** two commands and one browser approval to get a project, their own agent connected and an invite link.
- **Starter self-hosting:** one command after installing agmen and Docker, plus a free tunnel account if the machine has no public address.
- **Invited friend:** three steps. Open the link, run the install line, run `agmen join <link>` in their clone and approve once in the browser.
- **Any MCP agent works:** Claude Code, Codex, OpenCode, Cursor and Gemini CLI at launch, set up by agmen, not by hand-edited config.
- **Same client both ways:** the CLI, the desktop app and the invite links do not care whether a hub is hosted or self-hosted.
- **Hosted control and billing:** on agmen hosted, the group's creator controls seats, members, roles and billing.

## Non-goals

- A web console for friends who install nothing (M4 in the roadmap). Invite links open a small static page, not the console.
- Sign-in providers other than GitHub.
- Signed installers, an updater and deep links (M6). Unsigned prebuilt binaries are enough for this work.
- Moving a group between hosted and self-hosted. Export and import are a later project.
- Running agmen's own agents for people: agmen still runs no installer for anyone's agent (roles spec, `docs/specs/2026-10-25-roles-listener-consoles-design.md:234`).

## Anti-goals

These would make it a failure even if it technically works:

- A self-hoster has to create a GitHub OAuth app, register a callback URL or hold a client secret.
- An invite carries a secret that lands in server logs, browser history sent to third parties, or a Referer header.
- A free self-hosted hub phones home, needs agmen's servers to run, or limits seats.
- The hub trusts a GitHub token that a client hands it.
- A trial on agmen hosted can fill the database without bound.
- agmen hosted runs EMQX for customers without a license that allows it.

## Constraints

- **GitHub sign-in:** humans are keyed on GitHub's numeric account id, never the login (ruling R39, `crates/agmen-hub/src/auth/oauth.rs`).
- **Identity trust:** an identity file is trusted only at `<worktree root>/.agmen/identity.json`, untracked, exact names, no symlinks (R42 to R42b). Tokens are per machine in the OS keychain, keyed by hub origin plus identity (R17, R53).
- **Invites:** single-use secrets stored only as SHA-256, valid 1 hour to 30 days, with role limits (`crates/agmen-hub/src/invites.rs`).
- **EMQX license:** EMQX 6.x is BSL 1.1. One node for your own group is allowed. "You cannot offer EMQX as a hosted service to third parties or embed it in products sold to third parties" (https://www.emqx.com/en/content/license-faq). EMQ publishes no price for a hosted-service license.
- **Separate infrastructure:** agmen's own cluster, registry and domain, nothing of GroupDrive's (R49, R49a). Cluster manifests live in `groupdrive/agmen-kubernetes` (R66).
- **Device flow limit:** GitHub's device flow allows "50 submissions in an hour per application" (https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps).

## Where things stand today

The core is built: hub, CLI, device login, invites, join, `agmen mcp` and the Claude Code plugin. What makes it hard is around them. The step counts come from a mapping pass on 2026-10-26 (`.superpowers/sdd/onboarding/map.json`).

- **A host takes about 19 steps.** Clone a private repo, install Rust and build, create a GitHub OAuth app with an exact callback URL, set nine environment variables by hand, run `agmen-hub bootstrap` with database access, log in, look up the project id, install the plugin, join. There is no hosted hub, no domain, and no hub service in `deploy/dev/compose.yaml`.
- **A friend takes 12 steps.** Get access to the private repo, install Rust, build the CLI, `agmen login --hub <url>` (the URL arrives separately from the bare 64-hex invite code), approve in the browser, clone the project, install the plugin, join.
- **Non-Claude agents get partial support.** Codex and OpenCode get the MCP tools but no wake-ups, no presence and no lane guard, because those live in the Claude Code plugin (`docs/mcp-setup.md`). Their config is hand-written.
- **Fixed by the Task 16 merge:** the hub serves HTTP before it connects to the broker and retries the first connect (`crates/agmen-hub/src/main.rs:138`), so EMQX can fetch the JWKS from the hub at startup.

## Approaches considered

1. **One hub that agmen hosts for everyone.** Fewest steps, but self-hosting would be an afterthought, which goes against "anyone can set it up".
2. **Self-host only, in Docker.** Free and independent, but every group needs someone with Docker and a reachable machine, and nobody can just try it.
3. **Both, with one client and shared onboarding pieces (picked).** The pieces that cut the step count (shared GitHub sign-in, invite links that carry the hub address, a join that does everything, agent setup, prebuilt binaries) serve both. Self-host is `agmen host up` on your own machine. agmen hosted is a paid multi-tenant hub with a 7-day trial.

Three ways to run a self-hosted hub were weighed:

- **`agmen host up` (picked for launch):** Docker on your own machine (hub, broker, Postgres) plus a free tunnel. It is free, the data stays with the host, and it reuses the hub as built.
- **A Railway template (later):** a "Deploy on Railway" button, always on, about $5 or more a month to Railway. It is the simplest path for people who will pay a cloud provider; it is offered as a documented option, not built first.
- **A single program with a built-in broker and SQLite (decided by the broker spike, below):** one download and no Docker. It is the largest build, and it depends on RMQTT, a young broker.

Ruled out: Cloudflare quick tunnels (the address changes on every start, which breaks every invite link, and they do not carry server-sent events); Cloudflare named tunnels as the default (they need a domain on Cloudflare); Render's free tier (sleeps after 15 minutes, no disks); DigitalOcean App Platform (no volumes); rumqttd (no MQTT 5, its WebSocket listener is fixed to v4); Mosquitto and NanoMQ (no JWKS sign-in for clients, no kick or ban API). Sources are in `.superpowers/sdd/onboarding/selfhost-research.json`.

## Decisions

### 1. One client, two kinds of hub

A hub is a hub. The CLI, the desktop app and invite links carry the hub's address and treat hosted and self-hosted hubs the same. The only difference a person sees is the hosted plan's billing and seat screens.

### 2. GitHub sign-in through a shared agmen app, run by the hub

- The hub signs people in with GitHub's own device flow, which needs no client secret ("The `client_secret` is not needed for the device flow", GitHub docs above). So no self-hoster creates an OAuth app.
- **Flow:** the CLI asks its hub to start a login. The hub calls `github.com/login/device/code` with the client id, and the CLI shows the user code and opens `github.com/login/device`. The hub polls `github.com/login/oauth/access_token`, calls `GET api.github.com/user` once, keys the person on the numeric `id`, and throws the GitHub token away. The hub then issues its own tokens as today.
- **The hub never accepts a GitHub token from a client.** GitHub warns that public client ids are "trivially spoofable", so a token a client presents proves nothing about which app minted it. Polling GitHub itself closes that gap.
- **Two agmen-owned GitHub OAuth apps,** both with "Enable Device Flow" on and no scopes. One serves agmen hosted, and one is the default compiled into self-hosted hubs. That way the pooled limit of 50 device-code submissions an hour is not shared between paying customers and every self-hoster. Any self-hoster can set `AGMEN_GITHUB_CLIENT_ID` to their own app to get a limit of their own. (My call.)
- **What gets removed:** the hub's web OAuth callback and its device page that redirects through it (`crates/agmen-hub/src/auth/device.rs`, `oauth.rs`). That also removes `AGMEN_GITHUB_CLIENT_SECRET` and the exact-callback requirement. The M4 web console will need web sign-in later; that is out of scope here.
- **Timing:** the hub's own device-login window moves from 600 s to GitHub's 900 s, and polling follows GitHub's interval and `slow_down`.

### 3. Invite links

- **Form:** `https://<hub>/i#<code>`. The code sits in the URL fragment, so browsers never send it to the hub, to server logs or in a Referer header. (My call.)
- **The page:** `GET /i` serves a small static page. Its inline script reads the fragment and shows the install line and the exact `agmen join` command for the visitor's platform. The page sends `Referrer-Policy: no-referrer` and `Cache-Control: no-store`, and it never calls the hub with the code.
- **`agmen invite`** prints the link, not a bare code, and copies it to the clipboard when one is available. The plan, role and expiry rules stay as today (R55, R58).
- **Seat check at acceptance:** on agmen hosted, accepting an invite fails with a clear message when the group has no free seat, and it tells the creator how to add one.

### 4. `agmen join <link>` does everything

In the friend's clone, `agmen join https://<hub>/i#<code>`:

1. Reads the hub address from the link.
2. Signs in through decision 2 if this machine has no token for that hub, opening the browser.
3. Accepts the invite and writes `.agmen/identity.json`.
4. Runs `agmen setup` (decision 5) for the agents it finds.
5. Prints who they are, the project, and what to do next.

- **Outside a git worktree,** join stops and says to run it inside a clone of the project's repository. If the project records a repository URL (a new optional project field), join prints the `git clone` command. (My call.)
- **`agmen join --project`** for second worktrees stays as built, and defaults to the person's only project when they have exactly one.
- **`agmen login --hub`** stays for people who want it. Nobody needs it in the normal flow.

### 5. Any agent: `agmen setup`

- **What it does:** `agmen setup` finds the agents installed on the machine and writes each one's MCP entry for `agmen mcp`, scoped to the current joined worktree. It lists what it changed. `agmen setup --agent <name>` picks one.
- **Launch set:**
  - **Claude Code:** the plugin through its marketplace, or `.mcp.json` when the plugin is declined.
  - **Codex:** `~/.codex/config.toml`, with `cwd`.
  - **OpenCode:** `opencode.json` in the worktree, added to `.git/info/exclude`.
  - **Cursor and Gemini CLI:** their MCP config formats are not yet verified; the builder confirms them from current docs before writing them.
- **Agents without the plugin get the parts that matter, through MCP:**
  - **Pending messages:** every tool result ends with the messages waiting for that identity. This is design.md section 6.10, which R19 dropped for the MVP; it comes back for harnesses with no wake-up channel.
  - **Presence:** `agmen mcp` publishes presence when no `agmen watch` runs for that identity. This amends R65, which made watch the only publisher. (My call.)
  - **The lane guard:** the `check_path` tool is there for any agent. A hook that blocks out-of-lane edits stays Claude Code only, and setup says so.

### 6. Install

- **Prebuilt binaries:** `agmen` for macOS (arm64, x86_64), Linux (x86_64, arm64) and Windows (x86_64), built by `dist` 0.33.1 as design.md already plans (`docs/design.md:329`).
- **Install commands:** a one-line shell installer, a PowerShell installer and a Homebrew tap. Each installer checks the binary's SHA-256 from the release.
- **Images:** the hub image and, for self-host, the broker image are published with pinned version tags, never `latest`.
- **Where they're published:** publicly, as the agmen repository's GitHub releases and its container registry, since the code is MIT (decision 1 under "Decisions Andy made").

### 7. Self-host: `agmen host up`

Run `agmen host up` on the machine that will host:

1. Checks for Docker and stops with the install link if it is missing.
2. Writes a compose file and a `0600` env file under `~/.agmen/host/<name>/`. It generates every internal secret: the database password, the broker API key, the node cookie and the dashboard password.
3. Starts the hub, the broker and Postgres with pinned tags. On first start, the hub generates and keeps its own JWT signing key, so `init.sh` and hand-set variables go away.
4. Makes the hub reachable:
   - **Tailscale Funnel**, when the `tailscale` CLI is installed and signed in. It gives a stable `<machine>.<tailnet>.ts.net` name, and visitors need no Tailscale.
   - **Otherwise ngrok,** with an authtoken the person pastes once; the free plan gives one stable domain.
   - **Or `--public-url <url>`** for anyone with their own address or reverse proxy.
   The hub proxies MQTT over WebSocket at `/mqtt` on the same origin. One public HTTPS address then serves both the API and the broker, which is all a tunnel can carry.
5. Signs the person in (decision 2) and makes them admin with a one-time claim token. The hub prints the token to its local log at first start; it is single use and expires after 1 hour. This replaces `agmen-hub bootstrap` with database access.
6. Creates the first project, sets up the person's own agents in the current worktree, and prints an invite link.

`agmen host status`, `agmen host stop` and `agmen host logs` manage it afterwards.

- **Free, no limits:** self-hosting has no seat limits, no billing, no trial and no calls to agmen's servers.
- **Same member controls as hosted:** invite, remove, change roles, revoke an identity.
- **Broker on self-host:** EMQX at launch, since one node for your own group is allowed. Whether shipping a compose that pulls EMQX's image counts as "embedding" is not settled by the license text; see decision 5 under "Decisions Andy made".

### 8. agmen hosted

**Onboarding choice.** `agmen init` and the desktop app's first-run screen both ask:

- **Self host (free, requires setup):** runs decision 7.
- **agmen hosted (7-day free trial):** signs in with GitHub and creates a group on agmen's hub, with a first project and the person as creator. It then sets up their agents and prints an invite link.

**Group and seats.**

- **Group:** a group is a tenant. Its creator pays and controls it.
- **Seats:** a seat is a human member of the tenant, counted once across all its projects. Agents are identities owned by members and do not take seats. They count toward the group's usage quota. (My call.)

**Plans.** Approved 2026-10-26:

| Plan | Price | Includes |
|---|---|---|
| Self host | Free | Your own hardware, no limits |
| Team | $25/month after a 7-day trial | Creator plus 3 invited seats; $10/month per extra seat |
| Business | $100/month for 10 seats; $200 for 25; $350 for 50; custom above 50 | Next-business-day email support, an onboarding call, longer history and higher quotas |

**Who can do what in a group** (my call). Today roles exist only per project (`crates/agmen-hub/migrations/0002_core.up.sql`). This work adds a tenant level above them:

- **Roles:** a tenant has one **owner** (the creator, who pays) and any number of **group admins**.
- **What the owner and group admins can do:** create projects, see and change seats, remove a member from every project at once, and change roles.
- **What only the owner can do:** open billing, and hand ownership (and billing) to another group admin.
- **Invites:** project admins and coordinators invite as today (R55, R58). An accepted invite takes a seat only if the person is not already a member of the group.
- **`agmen init` when you already own or administer a group:** it adds a project to that group, and asks which group if there are several. `agmen init --new-group` starts another group. A second group gets no trial (one trial per GitHub account), so it goes straight to checkout.
- **A self-hosted hub has one group.** Claiming it makes you its owner. Only the owner and group admins can create projects, so a stranger who reaches the tunnel cannot create anything.

**Trial** (my call where marked):

- **Card:** the trial starts with no card.
- **Limit:** one trial per GitHub account id.
- **Seats:** a trial has Team's 4 seats. Adding a seat starts checkout.
- **Plan at checkout:** a trial group picks Team or Business at checkout. Business has no separate trial.
- **Reminders:** the creator sees in-app reminders in the CLI and a hub ALERT on the feed on days 5 and 7. No email is needed, so the GitHub app keeps no scopes.
- **If the trial ends unpaid:** the group becomes read-only for 14 days, so no new posts, claims or invites. Then it is deleted. Paying at any point restores it. Andy said "pay to keep going".

**When a paying group's payment fails** (my call; deleting a paying customer's history is hard to undo, so this is slower than the trial rule):

- **Days 0 to 14:** full access, while the provider retries the card. The owner sees reminders.
- **After 14 days:** read-only for 90 days, with the same restrictions as an expired trial.
- **After that:** deleted. Paying at any point restores it.

**Seat changes:**

- **Team:** seats are a per-seat quantity at $10 each.
- **Business:** sold in steps of 10, 25 and 50 seats. A group that needs an 11th seat moves to the 25 step, and the provider prorates the change.
- **Self-serve:** upgrading from Team to Business, and changing steps, both happen through the provider's checkout and portal.

**Quotas per group per day** (my call, from about 250 messages per seat per day measured in the usage model):

- **Trial:** 5,000 messages.
- **Team:** 10,000 messages, plus 1,000 for each extra seat.
- **Business:** 50,000 for 10 seats, scaled by the step.
- **At 80%:** the hub sends an ALERT to the owner.
- **At 100%:** posts are refused with a new NACK reason, `quota_exceeded`, until 00:00 UTC.
- **Stored bytes:** bounded by retention and the existing 96 KB message cap, so there is no separate byte quota.

**Billing.**

- **Provider: Paddle** (decided 2026-10-26, "go with paddle"). As the merchant of record, Paddle charges the owner once a month for the plan plus extra seats. It handles sales tax and VAT, including Colorado's tax on remote-access software from 2027-01-01 (HB26-1223), and charges 5% plus $0.50 per charge.
- **Catalog in Paddle:**
  - Team is one subscription with two items: a $25 base price, and a $10 seat price whose quantity is the number of extra seats.
  - Business is three prices: $100, $200 and $350, for 10, 25 and 50 seats. A step change swaps the item, with Paddle's proration.
- **The trial lives in the hub, not in Paddle.** It needs no card, and the hub opens Paddle checkout only when the group converts. Paddle's own cardless trials are early access and cannot start from its checkout.
- **Why not Stripe (recorded for later):**
  - Stripe directly is about 1 to 2 points cheaper, but agmen would be the seller and register and file sales tax itself.
  - Stripe Managed Payments costs more than Paddle above about $9.50 a charge, and shows customers "Sold through Link".
  - **When to revisit:** around $15k to $30k a month in revenue, or when customers need invoices in agmen's own name.
- **Webhooks:** the hub takes signed webhooks from the provider and keeps each tenant's plan state: trial, active, past due, read-only, deleted. Checkout and the billing portal are the provider's hosted pages. The hub stores no card data.

**Controls for the owner and group admins.** These are in the CLI and the hub API. The desktop app's first-run choice and its settings screens use the same API. They become tasks in the desktop plan (`desktop/gpui`), built once the desktop app has a live backend instead of its fixture. (My call: this plan is the CLI and the hub.) The controls:

- see seats used and paid for
- add or remove paid seats
- remove a member, which revokes their identities through the existing kick-and-ban path
- change roles
- open the billing portal

**History retention** (my call):

- Team keeps 90 days of message history.
- Business keeps 1 year.
- Self-host keeps everything unless the host sets a limit.

**Cost and margin.** From `.superpowers/sdd/onboarding/pricing-model.json`, at 500 paying groups with an average of 5 seats:

- **Revenue:** about $41 per group a month.
- **Costs per group:** $0.13 for servers on Hetzner with the embedded broker, $2.55 in payment fees, and $4.90 of Andy's time valued at $75 an hour.
- **Margin:** 81.5% gross. The usual software target is 75 to 80% (Aleph x Benchmarkit 2025 median 80%).
- **With EMQX's managed cloud** instead of the embedded broker, the margin is 75.2%.
- **At 50 groups,** the margin is about 58%, because fixed hours are spread over few groups.
- **One extra seat** costs about $0.53 to serve, against its $10 price.

### 9. The broker for agmen hosted: a spike decides

agmen hosted cannot run EMQX for customers under its free license. Before hosted sign-ups open, a time-boxed spike tests RMQTT (MIT or Apache 2.0, MQTT 5, WebSocket and TLS, library mode, an auth hook, a kick API) against agmen's real needs.

**It passes only if all of these hold:**

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

A single hub process with the broker inside is acceptable at launch. A deploy or crash drops every connection for a few seconds, and clients reconnect into their durable sessions. Clustering is a later project. (My call.)

**Results:**

- **If it passes:** agmen hosted runs the hub with the embedded broker. A later project can make self-host a single program as well.
- **If it fails:** agmen hosted uses EMQX's managed cloud (Dedicated Flex: $234 a month at 1,000 sessions, about $642 at 5,000), or a commercial EMQX license if EMQ prices one.

### 10. Must ship before hosted sign-ups open

- **A usage quota per tenant.** Today the only limit is 10 messages a second per identity with a burst of 30 (`crates/agmen-hub/src/ingest.rs:40`). One trial identity could write about 0.86 GB a day. Quotas cover messages per day and stored bytes, and are set per plan.
- **History pruning** to the retention in decision 8. Nothing prunes `messages` today; storage grows about 66 MB per seat per year.
- **The roster fix.** The retained roster passes its 4 KiB cap at about 25 identities, agents included (`crates/agmen-hub/src/presence.rs:455`). A Team of 5 people with 4 agents each already passes it, so the fix ships before launch: no interim cap. (My call.)
- **Abuse limits on sign-up:** one trial per GitHub account id, at most 3 groups owned per account, and the tenant quota above.
- **Tenant isolation tests** in CI: one group can never read, post to or administer another's projects.
- **Operations:** the nightly Postgres backup (already in agmen-kubernetes), restore tested once, and alerts when the hub or broker is down.
- **A public site and legal pages:** pricing, terms of service, a privacy policy and a refund policy. Paddle asks for these before it approves an account. See decision 6 under "Decisions Andy made".

**Where agmen hosted runs** (my call):

- **Deployment:** one deployment in agmen's own cluster (`groupdrive/agmen-kubernetes`), sized as the cost model does: k3s on Hetzner, about $43 a month at 50 groups and $88 at 500.
- **Andy's own group is a tenant on it.** Until the broker spike passes, Andy's dogfood hub keeps running EMQX for his own group, which the license allows. It moves onto the hosted deployment after that.

## The flows after this work

**Starter on agmen hosted:**

1. Run the install line.
2. In their repo, run `agmen init`, choose "agmen hosted", and approve on github.com in the browser. The project is created, their agents are set up, and an invite link is printed.
3. Send the link.

**Starter self-hosting:**

1. Run the install line.
2. Install Docker, if missing.
3. Have Tailscale signed in, or an ngrok authtoken, or their own public address.
4. Run `agmen host up` and approve on github.com. The stack starts, the tunnel opens, the project is created, their agents are set up, and an invite link is printed.
5. Send the link.

**Invited friend:**

1. Open the link. The page shows the install line and the join command.
2. Run the install line.
3. In their clone, run `agmen join <link>` and approve on github.com. They are signed in, joined, and their agents are set up.

## Security notes

- The invite secret stays in the URL fragment and in the CLI's argument. The hub sees it only in the accept call, as today.
- The hub runs the GitHub device flow itself and never accepts a client's GitHub token.
- The self-host claim token is local, single use and expires after 1 hour. Without it, nobody else who reaches the tunnel can claim the hub.
- Billing webhooks are checked against the provider's signature before they change any plan state.
- Installers check SHA-256 sums. The hub and broker images use pinned tags.
- Removing a member revokes their identities through the existing kick-then-ban path (R36, R64).

## Left to the builder

- Exact CLI wording, flags and the layout of the `/i` page.
- The compose layout, image names, and how `agmen host` finds and drives Docker.
- Which tunnel to prefer when both Tailscale and ngrok are present.
- Cursor and Gemini CLI MCP config formats, checked against current docs.
- The `/mqtt` proxy in the hub, if the spike keeps EMQX for self-host.
- Data shapes for plans, seats, trial state and quotas.
- Webhook handling details for the chosen billing provider.
- The order of work across the plan, as long as decision 10 ships before hosted sign-ups open.

## Decisions Andy made on open questions

1. **Licensing: decided 2026-10-26.** Andy: "let's have the code be open source MIT then the hosting be private (unless you self host)".
   - **The agmen repository becomes public under MIT.** Before it does:
     - a secret scan of the full history;
     - a `LICENSE` file;
     - a notice listing bundled third-party licenses (fonts under OFL, Lucide under ISC, GPUI under Apache 2.0, each Zed theme under its own).
   - **Releases and images** are published publicly, from the repository's release pipeline.
   - **agmen hosted's deployment** (`groupdrive/agmen-kubernetes`, the hosted hub's configuration, its secrets and its data) stays private.
   - **MIT allows anyone to host agmen commercially.** Andy chose that knowingly.
   - **Publishing (Andy, 2026-10-26: "go with your recs, fresh squashed repo"):** a new public repository `github.com/agmen-dev/agmen` with one squashed first commit, authored with Andy's GitHub no-reply address. Before it: GroupDrive and other private text removed from the tree, Integrally reworded as "a 17-agent PR", repository URLs pointed at the public repo, and the mockups regenerated with Lucide icons in place of Zed's and Material's. The existing private `AndyAld/agmen` stays private. The local `mvp/t14` branch, which holds Claude Code's early-access type files in its history, is never published.
2. **agmen's domain: decided 2026-10-26.** Andy: "go with getagmen.ai".
   - **Use:** `getagmen.ai` is the one canonical domain for the site, the hosted hub (for example `api.getagmen.ai`), the install script, invite links on agmen hosted, billing and email.
   - **Availability:** it was unregistered on 2026-10-26 (whois.nic.ai: "Domain not found").
   - **Cost:** .ai sells in 2-year minimum terms, $82.70 a year at Porkbun (about $80 at Cloudflare, unconfirmed).
   - **agmen.ai** is held by someone else: parked at GoDaddy, with mail on Microsoft 365, expiring 2027-01-14. A quiet broker inquiry with a ceiling is optional and does not block anything.
3. **Billing provider: decided 2026-10-26, Paddle.** Andy opens the account.
4. **GitHub organization: decided 2026-10-26, `agmen-dev`.** The name `agmen` is a personal account from 2012. The `agmen-dev` organization owns:
   - the public MIT repository;
   - both GitHub OAuth apps (hosted, and the self-host default);
   - the published images.
   Andy adds Sean as a member.
5. **EMQ: decided 2026-10-26.** Andy sends EMQ a short written question, drafted by agmen-db: is a self-host compose file that pulls EMQX's official image allowed under the BSL? It does not block anything. If the broker spike passes, self-host can move to the embedded broker too.
6. **Public site and legal pages: decided 2026-10-26.**
   - **The site:** this work builds a small static site for `getagmen.ai`, hosted on Cloudflare Pages, which is free. GitHub Pages' terms forbid sites mainly selling a hosted service. It has the pricing page, the install line and the docs links.
   - **The legal pages:** terms of service, a privacy policy and a refund policy, adapted from Basecamp's open policies (CC BY 4.0, credit required).
   - **What they must say:**
     - what agmen stores: the GitHub numeric id and login, and chat between people and their agents;
     - retention periods;
     - deletion after non-payment;
     - Paddle as a subprocessor;
     - a privacy contact;
     - GDPR access and deletion rights.
   - **Refund policy:** written to match Paddle's 14-day refund window.
   - **Review:** a lawyer reviews the pages once there are paying groups, about $770. The adapted text is not legal advice.

## Actions for Andy (outside the code)

- Register `getagmen.ai` (2-year minimum) and check the renewal price at checkout.
- Create the `agmen-dev` GitHub organization and add Sean.
- Open the Paddle seller account once the site and legal pages are up.
- Send the EMQ question.
- Optional:
  - a USPTO search for "agmen";
  - a broker inquiry for `agmen.ai`, with a ceiling.
- Two others started coding-agent tools named agmen this month: `noesis-sol/agmen` on GitHub, and an npm placeholder.

## Sources

- Mapping of today's flow: `.superpowers/sdd/onboarding/map.json`.
- Self-host research (GitHub device flow, tunnels, deploy platforms, brokers, EMQX license): `.superpowers/sdd/onboarding/selfhost-research.json`.
- Pricing research and model (infrastructure, EMQX Cloud, payment fees, comparable prices, per-seat usage): `.superpowers/sdd/onboarding/pricing-research.json`, `pricing-model.json`.
- GitHub device flow: https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps
- GitHub OAuth best practices: https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/best-practices-for-creating-an-oauth-app
- EMQX license FAQ: https://www.emqx.com/en/content/license-faq
- RMQTT: https://github.com/rmqtt/rmqtt
