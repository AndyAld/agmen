CREATE TABLE hub_group (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    tenant_id text NOT NULL UNIQUE REFERENCES tenants (id),
    claim_hash bytea,
    claim_expires_at timestamptz,
    CHECK ((claim_hash IS NULL) = (claim_expires_at IS NULL))
);
