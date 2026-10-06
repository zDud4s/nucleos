-- Work a chat turn launched that can outlive it: subagents and background tasks (spec
-- 2026-10-04 §3 option C.2, §6 slice 5). Written by `chat_tasks.rs` only. No CHECK, no
-- REFERENCES, no project_id, for the reasons 0166 gives: `kind`/`status` are Rust constants and
-- `launched_by_run_id` is held by value so the row outlives its run. `task_id` is the CLI's own
-- id, the only key a `task_updated` event carries. `cost_usd` stays NULL until the CLI reports one.
CREATE TABLE chat_tasks (
    id INTEGER PRIMARY KEY,
    chat_id TEXT NOT NULL,
    launched_by_run_id INTEGER NOT NULL,
    tool_use_id TEXT NOT NULL,
    task_id TEXT,
    kind TEXT NOT NULL,
    subagent_type TEXT,
    model TEXT,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    total_tokens INTEGER,
    cost_usd REAL,
    summary TEXT
);

CREATE UNIQUE INDEX chat_tasks_call ON chat_tasks (chat_id, tool_use_id);
CREATE INDEX chat_tasks_running ON chat_tasks (status) WHERE status = 'running';
