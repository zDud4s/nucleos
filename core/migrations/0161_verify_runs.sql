-- One row per verification unit the daemon ran or skipped (spec 2026-10-05, "Registo").
--
-- F0 writes only gate rows (`requested_by = 'gate'`, `scope = 'full'`): the job item gate, the
-- run gate and the merge gate. `group_name`, `kind` and `fingerprint` stay NULL until the test
-- map and the executor exist; they are here so the table does not change shape under them.
--
-- No CHECK constraints, for the house's reason (`0143_knowledge.sql`): the vocabularies
-- (`origin`, `status`, `requested_by`, `scope`) are Rust constants in `verify_runs.rs`.
-- `output_tail` goes last, as in `0154_runs_large_columns_last.sql`.

CREATE TABLE verify_runs (
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
    duration_ms INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    output_tail TEXT
);

CREATE INDEX verify_runs_project_started ON verify_runs (project_id, started_at);
CREATE INDEX verify_runs_origin ON verify_runs (origin, origin_id);
