-- A job is a sequence of runs over one shared worktree, so that autonomous work is not capped by a
-- single context window. It owns what a run cannot: the worktree (keyed by owner since 0035), the
-- item queue, and where in that queue the chain currently is.
CREATE TABLE jobs (
    id           INTEGER PRIMARY KEY,
    project_id   TEXT    NOT NULL,
    project_root TEXT    NOT NULL,
    rule_name    TEXT,
    status       TEXT    NOT NULL,
    stage_cursor INTEGER NOT NULL DEFAULT 0,
    -- The shape the rule asked for, copied onto the job when it starts rather than re-read at each
    -- step. `.ai/autopilot.yaml` can be edited mid-flight, and a job that changed shape between its
    -- own nodes would gate some items and not others with nothing recording why.
    max_items    INTEGER NOT NULL,
    gate_each    INTEGER NOT NULL DEFAULT 1,
    review       INTEGER NOT NULL DEFAULT 1,
    -- The repository HEAD when the job started. Crash recovery resumes from `stage_cursor` only if
    -- HEAD has not moved since: resuming an old plan against a tree that changed underneath it is
    -- the riskiest execution in the system, and the same rule already governs catch-up runs.
    head_sha     TEXT,
    -- `waiting` means two different things — a budget window that will reopen, and an exclusivity
    -- slot another run is holding — and they ask opposite things of a reader. Storing which is what
    -- keeps the feed from saying only "waiting" and leaving the person to guess.
    wait_reason  TEXT,
    created_at   TEXT    NOT NULL,
    completed_at TEXT
);

-- Exclusivity at the storage layer, exactly as 0009 does it for runs: the INSERT is the lock, which
-- removes the in-memory race the scheduler and a manual POST would otherwise share. Enforcing this
-- in job.rs instead would undo a decision already taken and recorded on 2026-07-19.
CREATE UNIQUE INDEX one_live_job_per_project
    ON jobs (project_id)
    WHERE status IN ('planning','implementing','gating','reviewing','awaiting_approval','waiting');

CREATE TABLE job_items (
    job_id      INTEGER NOT NULL REFERENCES jobs(id),
    ordinal     INTEGER NOT NULL,
    description TEXT    NOT NULL,
    status      TEXT    NOT NULL,
    run_id      INTEGER,
    -- The gate is a subprocess, not a CLI invocation: it creates no run and consumes no quota, so
    -- its verdict belongs to the item it measured rather than to a row in `runs`.
    gate_status TEXT,
    PRIMARY KEY (job_id, ordinal)
);

-- Nullable on purpose: every existing caller of `create_run_inner` — the shell, the scheduler,
-- repo_trigger, presets — keeps working untouched. A manual run has no job and never will.
--
-- `stage` holds 'plan' | 'implement' | 'review', and deliberately not 'gate'.
ALTER TABLE runs ADD COLUMN job_id INTEGER REFERENCES jobs(id);
ALTER TABLE runs ADD COLUMN stage TEXT;
