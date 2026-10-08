-- The devtime ingest looks attempts up by (session_id, tool_use_id) for every tool result and
-- reads a session's bg_task_ids for every tool call, inside the chunk's write transaction. With
-- only (session_id, started_at) indexed, each lookup scanned the session's attempts, and a long
-- session held the write lock past the pool's 10s busy_timeout: an owner's capture answer failed
-- with "database is locked" on 2026-10-08.
CREATE INDEX IF NOT EXISTS idx_devtime_attempts_tool_use
    ON devtime_attempts (session_id, tool_use_id);
CREATE INDEX IF NOT EXISTS idx_devtime_attempts_bg_task
    ON devtime_attempts (session_id, bg_task_id) WHERE bg_task_id IS NOT NULL;
