CREATE TABLE proposals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    run_id INTEGER,
    session_id TEXT,
    project_id TEXT,
    tool_name TEXT,
    reasoning TEXT NOT NULL,
    tool_input TEXT,
    created_at TEXT NOT NULL,
    decided_at TEXT
);

CREATE UNIQUE INDEX one_open_action_proposal_per_run
    ON proposals (run_id)
    WHERE status = 'pending' AND kind = 'action-approval';

CREATE TABLE proposal_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    proposal_id INTEGER NOT NULL,
    from_status TEXT,
    to_status TEXT NOT NULL,
    note TEXT,
    at TEXT NOT NULL
);
