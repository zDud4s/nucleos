-- `verify_runs` becomes the verification queue as well as the log (spec 2026-10-05, "Executor").
--
-- The table is rebuilt because SQLite cannot drop a NOT NULL: a row that is still queued or
-- running has no `duration_ms`, `started_at` or `finished_at` yet, so those three become nullable.
-- The queue columns are new: `priority`, `weight`, `timeout_ms`, `enqueued_ms` and `interruptions`.
-- Rows the gates wrote before this migration are terminal and keep their values untouched.
--
-- No CHECK constraints, as in `0161_verify_runs.sql`: the vocabularies (`status`, `priority`) are
-- Rust constants in `verify_runs.rs`. `output_tail` stays the last column, as in
-- `0154_runs_large_columns_last.sql`.

CREATE TABLE verify_runs_new (
    id INTEGER PRIMARY KEY,
    project_id TEXT,
    worktree TEXT NOT NULL,
    sha TEXT,
    scope TEXT NOT NULL,
    origin TEXT NOT NULL,
    origin_id INTEGER,
    ordinal INTEGER,
    requested_by TEXT NOT NULL,
    group_name TEXT,
    kind TEXT,
    argv TEXT NOT NULL,
    fingerprint TEXT,
    status TEXT NOT NULL,
    exit_code INTEGER,
    duration_ms INTEGER,
    started_at TEXT,
    finished_at TEXT,
    priority INTEGER NOT NULL DEFAULT 1,
    weight INTEGER NOT NULL DEFAULT 1,
    timeout_ms INTEGER,
    enqueued_ms INTEGER,
    interruptions INTEGER NOT NULL DEFAULT 0,
    output_tail TEXT
);

INSERT INTO verify_runs_new (id, project_id, worktree, sha, scope, origin, origin_id, ordinal,
    requested_by, group_name, kind, argv, fingerprint, status, exit_code, duration_ms,
    started_at, finished_at, output_tail)
SELECT id, project_id, worktree, sha, scope, origin, origin_id, ordinal,
    requested_by, group_name, kind, argv, fingerprint, status, exit_code, duration_ms,
    started_at, finished_at, output_tail FROM verify_runs;

DROP TABLE verify_runs;
ALTER TABLE verify_runs_new RENAME TO verify_runs;

CREATE INDEX verify_runs_project_started ON verify_runs (project_id, started_at);
CREATE INDEX verify_runs_origin ON verify_runs (origin, origin_id);
CREATE INDEX verify_runs_live ON verify_runs (status) WHERE status IN ('queued', 'running');
