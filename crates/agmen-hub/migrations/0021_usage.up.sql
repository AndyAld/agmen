CREATE TABLE tenant_usage (
    tenant_id text NOT NULL REFERENCES tenants (id),
    day date NOT NULL,
    messages integer NOT NULL CHECK (messages >= 0),
    warned boolean NOT NULL DEFAULT false,
    PRIMARY KEY (tenant_id, day)
);
