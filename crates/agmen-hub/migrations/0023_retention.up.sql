ALTER TABLE stream_heads
    ADD COLUMN pruned_through bigint NOT NULL DEFAULT 0;

CREATE INDEX messages_by_hub_at ON messages (hub_at);
