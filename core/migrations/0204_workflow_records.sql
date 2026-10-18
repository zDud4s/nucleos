-- The AI workflow's ledgers, moved out of the project tree and into the daemon (F4).
--
-- Until now each workflow bundle appended JSONL to `.ai/local/ledgers/*.jsonl` from its own Python
-- scripts: per-checkout, invisible to the dashboard, and unable to say which step caught a task's
-- rework. One table holds the four record kinds those scripts wrote; what differs between them is
-- the `payload`, a JSON object the daemon stores without interpreting.
--
-- `task_id` and `caught_by` are columns rather than payload fields because they are the join the
-- audit could not make: a rework row names the task and the step (gate, review, test) that caught
-- it, and `verify_runs.task_id` below names the task a verification was run for.
--
-- The CHECK on `kind` is deliberate, as in `0191_context_refs.sql`: the four kinds are the
-- contract the `record` tool announces, and the schema is the last line of defence for what the
-- Rust side already validates. `created_at` is RFC 3339 text, the house convention.
CREATE TABLE workflow_records (
    id INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL,
    worktree TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('metric', 'rework', 'intent', 'event')),
    task_id TEXT NOT NULL,
    phase TEXT,
    caught_by TEXT,
    created_at TEXT NOT NULL,
    payload TEXT NOT NULL
);

CREATE INDEX workflow_records_project_task ON workflow_records (project_id, task_id);
CREATE INDEX workflow_records_kind_created ON workflow_records (kind, created_at);

-- The task a `verify` call was made for, when the caller named one. NULL for every gate the daemon
-- runs on its own behalf and for every row written before this migration.
ALTER TABLE verify_runs ADD COLUMN task_id TEXT;
