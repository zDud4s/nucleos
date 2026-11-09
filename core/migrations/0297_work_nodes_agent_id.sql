-- The Claude Code subagent that opened a node (the hook's `agent_id`, which the model cannot write).
-- Nullable: a root, a run's node and every node opened before this have none. Recorded only; no
-- decision reads it yet.
ALTER TABLE work_nodes ADD COLUMN agent_id TEXT;
