-- The `verify` tool's ticket and the green-unit cache (spec 2026-10-05, "verify" and section 5.3).
--
-- Two tables, because they answer different questions. `verify_requests` is what was asked and
-- what the planner decided (one ticket per call, its plan kept as JSON per unit); the live state of
-- each unit is never copied here, it is read from `verify_runs` by `run_id`, so the two cannot
-- disagree. `verify_cache` maps a unit's fingerprint to the `verify_runs` row that was green for
-- it. Only green rows are ever stored, so a hit is always a reusable pass and never a failure.
--
-- No CHECK constraints, as in `0167_verify_queue.sql`: the vocabularies are Rust constants.

CREATE TABLE verify_requests (
    id INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL,
    worktree TEXT NOT NULL,
    kind TEXT NOT NULL,
    scope TEXT NOT NULL,
    base TEXT,
    priority INTEGER NOT NULL,
    caller TEXT NOT NULL,
    note TEXT,
    unclaimed TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    plan TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE verify_cache (
    project_id TEXT NOT NULL,
    group_name TEXT NOT NULL,
    kind TEXT NOT NULL,
    argv TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    run_id INTEGER NOT NULL,
    duration_ms INTEGER,
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (project_id, group_name, kind, argv, fingerprint)
);

CREATE INDEX verify_cache_created ON verify_cache (created_ms);
