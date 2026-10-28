-- Numbered messages and the publish outbox (MVP Task 8, protocol 5.2).
--
-- One client envelope can be stored several times: a DM has a copy in each
-- recipient's inbox, the sender's inbox and each owning human's inbox, and
-- an ASSIGN has one in the feed and one in the assignee's inbox (ruling
-- R33). Each copy is its own row with its own seq. stream is the wire name
-- ("feed" or "inbox:<identity id>") inside one project; stream_heads keys
-- the same stream as "<project_id>/<stream>".
CREATE TABLE messages (
    project_id text NOT NULL REFERENCES projects (id),
    stream text NOT NULL CHECK (stream ~ '^(feed|inbox:[a-z0-9]{8,32})$'),
    seq bigint NOT NULL CHECK (seq >= 1),
    -- The client's ULID; with from_id it is the dedupe key.
    id text NOT NULL,
    from_id text NOT NULL,
    -- SHA-256 of the client envelope as the hub parsed it. A resend with
    -- the same id and another hash gets NACK conflict.
    payload_hash bytea NOT NULL,
    -- The StampedEnvelope as it was published, this copy's stream and seq
    -- included.
    envelope jsonb NOT NULL,
    hub_at timestamptz NOT NULL,
    -- The copy the ACK reports (ruling R33a): the feed row when there is
    -- one, else the sender's own inbox copy. Exactly one per envelope.
    ack_row boolean NOT NULL,
    PRIMARY KEY (project_id, stream, seq),
    UNIQUE (project_id, stream, from_id, id)
);

-- Dedupe: one ACK row per (sender, id), so two copies of one resend racing
-- each other cannot both commit.
CREATE UNIQUE INDEX messages_ack_row ON messages (from_id, id) WHERE ack_row;

-- reply_to lookups: the message with this id in a stream the sender reads.
CREATE INDEX messages_by_id ON messages (project_id, id);

-- What the sweeper still has to publish, in id order. A row is deleted only
-- after the broker acknowledged it, so a crash republishes it; readers drop
-- the repeat by (stream, seq).
CREATE TABLE publish_outbox (
    id bigserial PRIMARY KEY,
    tenant_id text NOT NULL,
    project_id text NOT NULL,
    topic text NOT NULL,
    payload bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (project_id, tenant_id) REFERENCES projects (id, tenant_id)
);
