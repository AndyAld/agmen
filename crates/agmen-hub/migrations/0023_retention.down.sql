DROP INDEX messages_by_hub_at;

ALTER TABLE stream_heads DROP COLUMN pruned_through;
