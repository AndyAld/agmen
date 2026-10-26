-- The coordinator ledger (MVP Task 10, design 6.5): lanes, items, claims
-- with leases, and the counters item keys are minted from.

-- A project's lanes. A lane's globs are gitignore-style patterns, and no two
-- lanes' globs match one path (PUT /v1/projects/{p}/lanes refuses the set
-- otherwise). Each PUT replaces the whole set; v is the set's version, one
-- more than the highest v before the PUT.
CREATE TABLE lanes (
    project_id text NOT NULL REFERENCES projects (id),
    name text NOT NULL CHECK (name ~ '^L-[A-Z0-9-]{1,40}$'),
    globs text[] NOT NULL CHECK (cardinality(globs) >= 1),
    owner_identity_id text REFERENCES identities (id),
    v bigint NOT NULL CHECK (v >= 1),
    PRIMARY KEY (project_id, name)
);

-- Work items. FOUND mints them; the key is <prefix>-<n> from item_counters.
-- lane names a lane, which need not exist. A security item's title is never
-- shown outside the finder's inboxes. created_by is the finder, an identity
-- or a human; assigned_to is the identity the latest ASSIGN named. DONE from
-- the claim holder moves an item to done, which closes it to new claims.
CREATE TABLE items (
    project_id text NOT NULL REFERENCES projects (id),
    key text NOT NULL CHECK (key ~ '^[A-Z]{1,4}-[0-9]{1,6}$'),
    title text NOT NULL,
    lane text CHECK (lane ~ '^L-[A-Z0-9-]{1,40}$'),
    severity text CHECK (severity IN ('low', 'med', 'high', 'security')),
    state text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'done')),
    created_by text NOT NULL,
    assigned_to text REFERENCES identities (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, key)
);

-- The last n handed out for each key prefix of a project.
CREATE TABLE item_counters (
    project_id text NOT NULL REFERENCES projects (id),
    prefix text NOT NULL CHECK (prefix ~ '^[A-Z]{1,4}$'),
    n bigint NOT NULL CHECK (n BETWEEN 1 AND 999999),
    PRIMARY KEY (project_id, prefix)
);

-- Claims on items. A claim is active until released_at is set: by RELEASE,
-- or by the lease sweeper once lease_until has passed without a HEARTBEAT.
-- Released rows stay as the item's history.
CREATE TABLE claims (
    id bigserial PRIMARY KEY,
    project_id text NOT NULL,
    item_key text NOT NULL,
    identity_id text NOT NULL REFERENCES identities (id),
    granted_at timestamptz NOT NULL DEFAULT now(),
    lease_until timestamptz NOT NULL,
    released_at timestamptz,
    FOREIGN KEY (project_id, item_key) REFERENCES items (project_id, key),
    CHECK (lease_until >= granted_at),
    CHECK (released_at IS NULL OR released_at >= granted_at)
);

-- Exactly one active claim per item: of two CLAIMs racing, one insert fails.
CREATE UNIQUE INDEX claims_active ON claims (project_id, item_key)
WHERE released_at IS NULL;

-- The lease sweeper's scan, and the lanes an identity holds.
CREATE INDEX claims_expiring ON claims (lease_until) WHERE released_at IS NULL;
CREATE INDEX claims_holder ON claims (identity_id) WHERE released_at IS NULL;
