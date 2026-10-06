-- The distiller's durable queue: closed jobs and runs whose text is worth reading once.
-- Design: .ai/specs/2026-10-05-destilador-design.md section 3.
--
-- No CHECK constraints (the rule of 0143_knowledge.sql): the vocabularies live in Rust
-- constants (`distill::CAUSES` and the status constants) and are verified before a write.

CREATE TABLE distill_queue (
  id          INTEGER PRIMARY KEY,
  cause       TEXT NOT NULL,     -- job_landed | job_failed | gate_recovered | review_blocking | run_exhausted
  project_id  TEXT NOT NULL,
  job_id      INTEGER,
  item_id     INTEGER,
  run_id      INTEGER,
  status      TEXT NOT NULL DEFAULT 'pending',  -- pending | running | done | failed
  attempts    INTEGER NOT NULL DEFAULT 0,
  not_before  TEXT,              -- backoff: not taken before this instant
  error       TEXT,              -- category + message, never dossier text (S11)
  created_at  TEXT NOT NULL,
  finished_at TEXT
);

-- SQLite treats every NULL as distinct in a unique index, so the ids go through IFNULL: the same
-- cause for the same subject is one row whether or not each id is set.
CREATE UNIQUE INDEX distill_queue_once
  ON distill_queue (cause, IFNULL(job_id, -1), IFNULL(item_id, -1), IFNULL(run_id, -1));

-- The worker's claim query: pending rows that are due, oldest first.
CREATE INDEX distill_queue_due ON distill_queue (status, not_before, id);

-- Which cause produced a distilled learning (NULL for every other source).
ALTER TABLE knowledge ADD COLUMN distill_cause TEXT;

-- A review run's terminal `failed` is written at many bare UPDATE sites in runs.rs; a trigger is
-- the one way to queue it in the same statement as all of them. INSERT OR IGNORE keeps a fault
-- here from aborting the run's own write. The literal 'review_blocking' is pinned against
-- `distill::Cause::ReviewBlocking.as_str()` by a test.
--
-- WARNING: a future rebuild of `runs` (create new, copy, drop, rename) drops this trigger
-- silently and MUST recreate it.
CREATE TRIGGER distill_review_blocking AFTER UPDATE OF status ON runs
WHEN NEW.status = 'failed' AND OLD.status IS NOT 'failed'
     AND NEW.stage = 'review' AND NEW.job_id IS NOT NULL
BEGIN
  INSERT OR IGNORE INTO distill_queue (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
  SELECT 'review_blocking', j.project_id, j.id, NULL, NEW.id, 'pending', 0,
         strftime('%Y-%m-%dT%H:%M:%S+00:00', 'now')
    FROM jobs j WHERE j.id = NEW.job_id;
END;
