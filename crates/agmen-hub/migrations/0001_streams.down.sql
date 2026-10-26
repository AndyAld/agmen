-- Reverts 0001_streams.up.sql. Drops every stream's seq head, so run it only
-- on a database whose messages go with it.
DROP TABLE stream_heads;
