-- shortcut: never pruned; prune rows older than the 300 s replay window plus Paddle's retry horizon if the table grows
CREATE TABLE paddle_events (
    event_id text PRIMARY KEY,
    received_at timestamptz NOT NULL DEFAULT now()
);
