-- NULL permissions preserve identities minted before roles.
ALTER TABLE identities DROP CONSTRAINT identities_kind_check;
ALTER TABLE identities ADD CONSTRAINT identities_kind_check
    CHECK (kind IN ('agent', 'coordinator', 'listener'));
ALTER TABLE identities
    ADD COLUMN role text,
    ADD COLUMN verbs text[],
    ADD COLUMN edits jsonb,
    ADD COLUMN role_sha256 text,
    ADD COLUMN agent text,
    ADD COLUMN delegated_by text REFERENCES identities (id),
    ADD CONSTRAINT identities_role_fields CHECK (
        (role IS NULL AND verbs IS NULL AND edits IS NULL AND role_sha256 IS NULL)
        OR (role IS NOT NULL AND verbs IS NOT NULL AND edits IS NOT NULL AND role_sha256 IS NOT NULL)
    );
CREATE UNIQUE INDEX identities_one_listener
    ON identities (owner_human_id, project_id)
    WHERE kind = 'listener' AND revoked_at IS NULL;

-- One grant per message; retries dedupe before this insert. A lane replacement
-- drops its grants so a reused name cannot inherit an old permission.
CREATE TABLE lane_lends (
    project_id text NOT NULL,
    lane text NOT NULL,
    message_id text NOT NULL,
    from_id text NOT NULL,
    to_id text NOT NULL REFERENCES identities (id),
    paths text[] NOT NULL,
    until_at timestamptz NOT NULL,
    PRIMARY KEY (project_id, from_id, message_id),
    FOREIGN KEY (project_id, lane) REFERENCES lanes (project_id, name) ON DELETE CASCADE
);
CREATE INDEX lane_lends_recipient ON lane_lends (project_id, to_id, until_at);
