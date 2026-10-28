-- Reverts 0002_core.up.sql. Drops every person, identity, invite and token,
-- so run it only on a database whose data goes with it.
DROP TABLE device_authorizations;
DROP TABLE tokens;
DROP TABLE invites;
DROP TABLE identities;
DROP TABLE bootstrap_admins;
DROP TABLE memberships;
DROP TABLE humans;
DROP TABLE projects;
DROP TABLE tenants;
