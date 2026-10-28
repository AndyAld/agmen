-- In-flight logins cannot survive the switch to GitHub-issued codes.
DELETE FROM device_authorizations;
ALTER TABLE device_authorizations
    DROP COLUMN oauth_state_hash,
    DROP COLUMN approval_hash,
    ADD COLUMN github_device_code text NOT NULL,
    ADD COLUMN interval_secs integer NOT NULL CHECK (interval_secs BETWEEN 1 AND 600),
    ADD COLUMN next_poll_at timestamptz NOT NULL;
