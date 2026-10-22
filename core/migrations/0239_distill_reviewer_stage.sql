-- `distill_review_blocking` (0163) queues a failed review run, but it matched only
-- `stage = 'review'`. A run's stage is its graph node's id (`engine.rs`, `&node.id`), and the
-- owner's workflow graphs name the review node `reviewer`: those failed reviews were never
-- distilled (4 of them by 2026-10-22). SQLite cannot alter a trigger, so it is dropped and
-- recreated with both spellings and everything else verbatim from 0163.
--
-- WARNING (from 0163, still true): a future rebuild of `runs` (create new, copy, drop, rename)
-- drops this trigger silently and MUST recreate it.
DROP TRIGGER IF EXISTS distill_review_blocking;

CREATE TRIGGER distill_review_blocking AFTER UPDATE OF status ON runs
WHEN NEW.status = 'failed' AND OLD.status IS NOT 'failed'
     AND NEW.stage IN ('review', 'reviewer') AND NEW.job_id IS NOT NULL
BEGIN
  INSERT OR IGNORE INTO distill_queue (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
  SELECT 'review_blocking', j.project_id, j.id, NULL, NEW.id, 'pending', 0,
         strftime('%Y-%m-%dT%H:%M:%S+00:00', 'now')
    FROM jobs j WHERE j.id = NEW.job_id;
END;
