CREATE TABLE worktrees (
    run_id INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL,
    project_root TEXT NOT NULL,
    path TEXT NOT NULL,
    branch TEXT NOT NULL,
    created_at TEXT NOT NULL,
    removed_at TEXT
);
