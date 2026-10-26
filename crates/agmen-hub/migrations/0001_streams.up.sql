-- The last seq handed out on each stream. next_seq bumps it with an upsert, and
-- the row lock it takes holds until commit, so seqs commit in order with no gaps
-- (protocol 5.2). A stream's row appears with its first message.
CREATE TABLE stream_heads (
    stream text PRIMARY KEY,
    seq bigint NOT NULL
);
