-- The roster (MVP Task 9, protocol section 2) shows each identity's agent
-- and model from its latest HELLO. This index finds that HELLO without
-- reading the rest of the project's feed.
CREATE INDEX messages_latest_hello ON messages (project_id, from_id, seq DESC)
WHERE stream = 'feed' AND envelope ->> 'verb' = 'HELLO';
