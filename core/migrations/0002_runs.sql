CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT,
    cwd TEXT,
    prompt TEXT NOT NULL,
    -- status is free-form TEXT (no CHECK): running -> completed | failed | timed_out | cancelled
    -- | awaiting_approval (§8.4 active-termination) | interrupted (§3.2 startup reconciliation).
    status TEXT NOT NULL,
    exit_code INTEGER,
    stdout TEXT,
    stderr TEXT,
    session_id TEXT,
    cost_usd REAL,
    created_at TEXT NOT NULL,
    completed_at TEXT
);
