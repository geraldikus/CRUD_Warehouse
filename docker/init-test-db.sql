-- Runs only the first time the data volume is created.
-- The image itself creates POSTGRES_DB. This adds a second database for tests,
-- so `cargo test` does not wipe the data you create while trying the API.
CREATE DATABASE warehouse_test;
