-- Agent-memory capture (spec 2026-11-16-memoria-agentes-captura), slice 1: every column inert.
-- Nobody writes them yet; the defaults keep every existing row and query as it was.

-- The cause a learning explains; NULL for every row that has none.
ALTER TABLE knowledge ADD COLUMN cause_key TEXT;
-- A learning on trial is not yet trusted; 0 is the ordinary, trusted state.
ALTER TABLE knowledge ADD COLUMN trial INTEGER NOT NULL DEFAULT 0;
ALTER TABLE knowledge ADD COLUMN trial_started_at TEXT;

-- Distill jobs for trees, nodes, requests and decisions.
ALTER TABLE distill_queue ADD COLUMN tree_id TEXT;
ALTER TABLE distill_queue ADD COLUMN node_id TEXT;
ALTER TABLE distill_queue ADD COLUMN request_id INTEGER;
ALTER TABLE distill_queue ADD COLUMN decision_id TEXT;

-- The unique key now covers every id (NULLs go through IFNULL, see 0163).
DROP INDEX distill_queue_once;
CREATE UNIQUE INDEX distill_queue_once
  ON distill_queue (cause, IFNULL(job_id, -1), IFNULL(item_id, -1), IFNULL(run_id, -1),
                    IFNULL(tree_id, ''), IFNULL(node_id, ''), IFNULL(request_id, -1),
                    IFNULL(decision_id, ''));

-- One live learning per cause and scope; dead rows (archived, expired, reverted) do not count.
-- The machine scope has scope_id NULL, hence the IFNULL.
CREATE UNIQUE INDEX knowledge_cause_key_live
  ON knowledge (scope_kind, IFNULL(scope_id, ''), cause_key)
  WHERE cause_key IS NOT NULL AND status IN ('active', 'proposed');
