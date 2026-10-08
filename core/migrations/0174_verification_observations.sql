-- What an unattended worktree run verified by itself (spec 2026-10-05 section 9, metric 7; F2b step 1).
--
-- One row per verification command a run executed on its own, where the `verify` tool exists to do
-- that job. Step 1 only observes: the row never blocks the command, and `verify_observe::deviation`
-- reads these rows against the `verify` units of the same project to say how often the tool was
-- bypassed. `kind`, `name` and `segment` are what `verify_guard` found in the command line.
--
-- No CHECK constraints and no foreign keys, as in `0170_verify_requests.sql`: the vocabularies are
-- Rust constants, and an observation outlives the run and decision it points at.

CREATE TABLE verification_observations (
    id INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL,
    run_id INTEGER NOT NULL,
    shadow_decision_id INTEGER,
    tool_name TEXT NOT NULL,
    command TEXT NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    segment TEXT NOT NULL,
    decision TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX verification_observations_project ON verification_observations (project_id, created_at);
