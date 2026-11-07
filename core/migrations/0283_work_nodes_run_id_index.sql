-- The PreToolUse hook resolves the calling run to its node with `WHERE run_id = ?` on every tool
-- call; without an index that is a scan of work_nodes per call.
CREATE INDEX IF NOT EXISTS work_nodes_run_id ON work_nodes (run_id);
