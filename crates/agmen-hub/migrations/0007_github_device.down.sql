-- GitHub device codes cannot be used by the former browser flow.
DELETE FROM device_authorizations;
ALTER TABLE device_authorizations
    DROP COLUMN github_device_code,
    DROP COLUMN interval_secs,
    DROP COLUMN next_poll_at,
    ADD COLUMN oauth_state_hash bytea UNIQUE,
    ADD COLUMN approval_hash bytea UNIQUE;
