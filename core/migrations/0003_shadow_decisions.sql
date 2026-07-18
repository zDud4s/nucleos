CREATE TABLE IF NOT EXISTS shadow_decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL,
    tool_name TEXT NOT NULL,
    tool_input TEXT,
    decision TEXT NOT NULL,
    reason TEXT,
    action_class TEXT NOT NULL,
    classifier_version INTEGER NOT NULL,
    human_verdict TEXT,
    reviewed_at TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_shadow_decisions_run_id ON shadow_decisions(run_id);
