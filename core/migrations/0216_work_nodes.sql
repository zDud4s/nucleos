-- Work nodes: a bounded tree of delegated work (root -> orchestrator -> leaf).
--
-- A long task is split so no single session carries it all. Each node records its goal, the paths
-- it owns (JSON array of globs, so siblings cannot collide), the budget it was given, and, on
-- close, a short summary plus the path of the handoff file it wrote. `decisions` is an append-only
-- JSON array of strings. The depth/width/summary limits are enforced in Rust (`work_nodes.rs`),
-- not here, because they are configurable; the schema only guards the closed vocabularies.
-- `created_at`, `updated_at` and `closed_at` are RFC 3339 text, the house convention.
CREATE TABLE work_nodes (
    id TEXT PRIMARY KEY,
    project_id TEXT,
    root_id TEXT NOT NULL,
    parent_id TEXT REFERENCES work_nodes (id),
    depth INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('root', 'orchestrator', 'leaf')),
    title TEXT NOT NULL,
    goal TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('planned', 'running', 'done', 'blocked', 'handoff', 'failed')),
    owned_paths TEXT NOT NULL DEFAULT '[]',
    budget_tokens INTEGER,
    context_peak INTEGER,
    summary TEXT,
    handoff_path TEXT,
    decisions TEXT NOT NULL DEFAULT '[]',
    run_id TEXT,
    session_id TEXT,
    chat_id TEXT,
    task_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    closed_at TEXT
);

CREATE INDEX work_nodes_root ON work_nodes (root_id);
CREATE INDEX work_nodes_parent_status ON work_nodes (parent_id, status);
CREATE INDEX work_nodes_session ON work_nodes (session_id);
CREATE INDEX work_nodes_chat ON work_nodes (chat_id);
