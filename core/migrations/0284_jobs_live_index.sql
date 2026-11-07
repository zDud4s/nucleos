-- The scheduler tick, the analytics pipeline and the orphaned-slot sweep all ask for the handful of
-- live jobs with `status IN ('running','awaiting_approval','waiting')`. The only index on `jobs` is
-- by creation time, so each of those scanned every job ever run, prompt text included. The literal
-- list in those queries matches this predicate, the same reliance `runs_live_by_chat` has.
CREATE INDEX IF NOT EXISTS jobs_live ON jobs (id) WHERE status IN ('running','awaiting_approval','waiting');
