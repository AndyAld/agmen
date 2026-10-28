-- Refuse rollback while listener rows remain instead of deleting identities.
ALTER TABLE identities DROP CONSTRAINT identities_kind_check;
ALTER TABLE identities ADD CONSTRAINT identities_kind_check
    CHECK (kind IN ('agent', 'coordinator'));
DROP TABLE lane_lends;
DROP INDEX identities_one_listener;
ALTER TABLE identities
    DROP CONSTRAINT identities_role_fields,
    DROP COLUMN delegated_by,
    DROP COLUMN agent,
    DROP COLUMN role_sha256,
    DROP COLUMN edits,
    DROP COLUMN verbs,
    DROP COLUMN role;
