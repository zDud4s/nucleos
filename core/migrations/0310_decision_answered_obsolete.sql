-- The owner's inbox needs to know WHEN a tree decision was answered and WHY a stale one was
-- closed without an answer. `answered_at` is RFC 3339 text, set by every answer path; before this
-- column the answer time lived only in `at`, so rows answered earlier are backfilled from it.
-- `status` gains the value `obsolete` (validated in Rust, like the other values): a question whose
-- subject went away, closed by `tree_job::decisions::mark_obsolete` with its reason, never deleted.
ALTER TABLE decisions ADD COLUMN answered_at TEXT;
ALTER TABLE decisions ADD COLUMN obsolete_reason TEXT;
UPDATE decisions SET answered_at = at WHERE status = 'answered' AND answered_at IS NULL;
