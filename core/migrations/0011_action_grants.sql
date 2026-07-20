CREATE TABLE action_grants (
    run_id INTEGER PRIMARY KEY,
    tool_name TEXT NOT NULL,
    proposal_id INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    consumed_at TEXT
);
