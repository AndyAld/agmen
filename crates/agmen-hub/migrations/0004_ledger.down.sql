-- Reverts 0004_ledger.up.sql. Drops every lane, item and claim, so run it
-- only on a database whose ledger goes with it.
DROP TABLE claims;
DROP TABLE item_counters;
DROP TABLE items;
DROP TABLE lanes;
