CREATE TABLE feed (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT,
    kind TEXT NOT NULL,
    summary TEXT NOT NULL,
    run_id INTEGER,
    created_at TEXT NOT NULL
);
