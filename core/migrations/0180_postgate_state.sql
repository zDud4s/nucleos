-- Per-project state of the post-merge gate (spec 2026-10-05 §6.1; F3-1). Nothing writes it
-- until F3-2. No CHECK constraints, as in 0161/0167. One row per project (§6.3).
CREATE TABLE postgate_state (
    project_id TEXT PRIMARY KEY,
    target TEXT NOT NULL,
    last_green_sha TEXT,
    last_attempted_sha TEXT,
    running_sha TEXT,
    running_request_id INTEGER,
    running_started_at TEXT,
    red_groups TEXT NOT NULL DEFAULT '[]',
    red_since_sha TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
