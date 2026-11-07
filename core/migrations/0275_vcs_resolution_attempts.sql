-- How many resolution runs have been minted against this conflict. `resolution_run_id` still names
-- the CURRENT attempt (and NULL means none under way); this counts them, so that an attempt that
-- ended without a verdict (run `cancelled` or `interrupted`) can be released for a fresh one and
-- the retries stay bounded (`resolver::MAX_RESOLUTION_ATTEMPTS`). Rows that already hold a run
-- count as one attempt.
ALTER TABLE vcs_requests ADD COLUMN resolution_attempts INTEGER NOT NULL DEFAULT 0;
UPDATE vcs_requests SET resolution_attempts = 1 WHERE resolution_run_id IS NOT NULL;
