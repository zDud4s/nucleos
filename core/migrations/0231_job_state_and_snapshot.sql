-- The phase names leave `jobs.status`, planning becomes a node row, and the job photographs its graph.
--
-- Order matters: the plan-row backfill reads the phase, so it runs BEFORE the statuses are rewritten.

-- 1. A plan row for every LIVE job already past planning: planned-ness stops being derivable from
--    `jobs.status`. A parked job's stage is its `resume_status`.
INSERT INTO job_nodes (job_id, node_id, ordinal, description, status, round)
SELECT id, 'plan', NULL, 'plan', 'passed', 0 FROM jobs
WHERE status IN ('planning', 'implementing', 'gating', 'reviewing', 'awaiting_approval', 'waiting')
  AND (CASE WHEN status IN ('waiting', 'awaiting_approval')
            THEN COALESCE(resume_status, 'planning') ELSE status END) <> 'planning';

-- 2. Phase names leave `jobs.status` and `resume_status` (which only ever held a phase or NULL).
UPDATE jobs SET status = 'running'        WHERE status IN ('planning', 'implementing', 'gating', 'reviewing');
UPDATE jobs SET resume_status = 'running' WHERE resume_status IN ('planning', 'implementing', 'gating', 'reviewing');

-- 3. Write-only column.
ALTER TABLE jobs DROP COLUMN stage_cursor;

-- 4. The job photographs the graph. NULL = a job that predates the engine.
ALTER TABLE jobs ADD COLUMN graph_snapshot    TEXT;
ALTER TABLE jobs ADD COLUMN graph_bundle_ref  TEXT;
ALTER TABLE jobs ADD COLUMN graph_bundle_hash TEXT;
