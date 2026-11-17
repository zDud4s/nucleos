-- The account a transcript was written under, resolved at ingest from the transcript path against
-- accounts.credential_locator (NULL locator = the engine default account). Nullable: rows ingested
-- before this migration, and transcripts under no known account dir, stay NULL (no backfill).
ALTER TABLE devtime_files ADD COLUMN account_id TEXT;
