-- Tenants, projects, people, identities, invites, hub tokens and device
-- logins (MVP Task 7). Every id the hub mints is [a-z0-9]{8,32}, so it is
-- safe as an MQTT topic level (protocol section 1). Secrets the hub hands
-- out (tokens, invite codes, device codes) are stored only as SHA-256 hashes.

-- Names are unique (a tenant's among tenants, a project's within its
-- tenant), so `agmen-hub bootstrap` finds the rows it made before.
CREATE TABLE tenants (
    id text PRIMARY KEY CHECK (id ~ '^[a-z0-9]{8,32}$'),
    name text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE projects (
    id text PRIMARY KEY CHECK (id ~ '^[a-z0-9]{8,32}$'),
    tenant_id text NOT NULL REFERENCES tenants (id),
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- Lets other tables reference (project, tenant) together, so a row can
    -- never name a project under the wrong tenant.
    UNIQUE (id, tenant_id),
    UNIQUE (tenant_id, name)
);

-- A person, keyed by the login provider's stable account id. The handle is
-- the provider login at first sign-in, lowercased; children are named
-- <handle>/<name>.
CREATE TABLE humans (
    id text PRIMARY KEY CHECK (id ~ '^[a-z0-9]{8,32}$'),
    provider text NOT NULL CHECK (provider IN ('github')),
    provider_sub text NOT NULL,
    handle text NOT NULL UNIQUE CHECK (handle ~ '^[a-z0-9][a-z0-9-]{0,38}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider, provider_sub)
);

-- Who belongs to a project. admin can invite; coordinator and admin can mint
-- coordinator identities; every member can mint agents.
CREATE TABLE memberships (
    human_id text NOT NULL REFERENCES humans (id),
    project_id text NOT NULL REFERENCES projects (id),
    role text NOT NULL CHECK (role IN ('admin', 'coordinator', 'member')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (human_id, project_id)
);

-- A project's first admin, named by `agmen-hub bootstrap` before they have
-- signed in. The first sign-in with that GitHub login (lowercased) claims
-- the row and becomes an admin member. A claimed row stays, so running
-- bootstrap again never hands the role to a later holder of the login.
CREATE TABLE bootstrap_admins (
    project_id text NOT NULL REFERENCES projects (id),
    github_login text NOT NULL CHECK (github_login ~ '^[a-z0-9][a-z0-9-]{0,38}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    claimed_by text REFERENCES humans (id),
    claimed_at timestamptz,
    PRIMARY KEY (project_id, github_login),
    CHECK ((claimed_by IS NULL) = (claimed_at IS NULL))
);

-- An agent or coordinator that a person owns. It is owned by a human and
-- nothing else (no devices in the MVP). Revocation sets revoked_at; the row
-- stays so stamped messages keep their sender.
CREATE TABLE identities (
    id text PRIMARY KEY CHECK (id ~ '^[a-z0-9]{8,32}$'),
    tenant_id text NOT NULL,
    project_id text NOT NULL,
    kind text NOT NULL CHECK (kind IN ('agent', 'coordinator')),
    handle text NOT NULL,
    owner_human_id text NOT NULL REFERENCES humans (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    FOREIGN KEY (project_id, tenant_id) REFERENCES projects (id, tenant_id)
);

-- Handles are how people and mentions name identities, so two live
-- identities in one project never share one.
CREATE UNIQUE INDEX identities_live_handle ON identities (project_id, handle)
WHERE revoked_at IS NULL;

CREATE INDEX identities_owner ON identities (owner_human_id);

-- One-use invite codes. The code is shown once, at creation.
CREATE TABLE invites (
    code_hash bytea PRIMARY KEY,
    tenant_id text NOT NULL,
    project_id text NOT NULL,
    role text NOT NULL CHECK (role IN ('admin', 'coordinator', 'member')),
    created_by text NOT NULL REFERENCES humans (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_by text REFERENCES humans (id),
    used_at timestamptz,
    FOREIGN KEY (project_id, tenant_id) REFERENCES projects (id, tenant_id),
    CHECK ((used_by IS NULL) = (used_at IS NULL))
);

-- Bearer tokens for the hub's HTTP API. access and refresh tokens act as the
-- human; an identity token acts only as its identity and lasts until the
-- identity is revoked.
CREATE TABLE tokens (
    hash bytea PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('access', 'refresh', 'identity')),
    human_id text NOT NULL REFERENCES humans (id),
    identity_id text REFERENCES identities (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    CHECK ((kind = 'identity') = (identity_id IS NOT NULL)),
    CHECK ((kind = 'identity') = (expires_at IS NULL))
);

CREATE INDEX tokens_human ON tokens (human_id);

-- RFC 8628 device logins in progress. The browser step moves a row through
-- oauth_state_hash (sent to GitHub), then human_id and approval_hash (the
-- confirm page), then approved_at; the CLI's poll then gets tokens and the
-- row is deleted.
CREATE TABLE device_authorizations (
    device_code_hash bytea PRIMARY KEY,
    user_code text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    oauth_state_hash bytea UNIQUE,
    human_id text REFERENCES humans (id),
    approval_hash bytea UNIQUE,
    approved_at timestamptz,
    CHECK (approved_at IS NULL OR human_id IS NOT NULL)
);
