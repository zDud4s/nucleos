-- Why a run has a successor: `context_full`, `approval_approved` or `approval_declined`.
-- Backfill is best effort from recorded facts. The reason stays NULL for any old row neither
-- rule covers, and the shell then shows the literal status.
ALTER TABLE runs ADD COLUMN successor_reason TEXT;

UPDATE runs SET successor_reason = 'context_full'
WHERE successor_run_id IS NOT NULL
  AND EXISTS (SELECT 1 FROM run_events e WHERE e.run_id = runs.id AND e.kind = 'context_handoff');

UPDATE runs SET successor_reason = 'approval_approved'
WHERE successor_reason IS NULL
  AND successor_run_id IS NOT NULL
  AND EXISTS (SELECT 1 FROM runs s WHERE s.id = runs.successor_run_id
              AND s.pending_resume_proposal_id IS NOT NULL);
