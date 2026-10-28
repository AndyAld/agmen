-- Reverts 0003_messages.up.sql. Drops every stored message and anything not
-- yet published, so run it only on a database whose messages go with it.
-- The stream heads in stream_heads belong to 0001_streams and stay.
DROP TABLE publish_outbox;
DROP TABLE messages;
