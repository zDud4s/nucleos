-- Project-level task claims (spec 2026-10-22 task-claims-design, section 1).
--
-- A task is a problem the daemon (an incident) or an agent (a declared task) found, with ONE owner per
-- project: the first to claim it holds it, and every other worktree of the same repository sees that.
--
-- Time columns are the fixed-width UTC text `execution::wave::stamp` writes, compared as TEXT, and
-- always bound from Rust -- never SQLite's `datetime('now')`, whose spelling does not sort against it.
-- No CHECK constraints (the rule of 0143_knowledge.sql): the allowed values live in task_claims.rs.
--
-- AUTOINCREMENT on task_claims, deliberately: a `probing` row that the proof then discards is deleted,
-- and without it the next insert would reuse the id -- an event log naming two different tasks.
CREATE TABLE task_claims (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id        TEXT NOT NULL,
    task_key          TEXT NOT NULL,             -- canonical, from keys::normalize
    kind              TEXT NOT NULL,             -- 'incident' | 'declared'
    title             TEXT NOT NULL,
    detail            TEXT NOT NULL DEFAULT '{}',  -- JSON object (master-red: red_since_sha, red_tests, ...)
    guard_paths       TEXT NOT NULL DEFAULT '[]',  -- JSON array: the land gate protects these
    hint_paths        TEXT NOT NULL DEFAULT '[]',  -- JSON array: informative only, never blocking
    status            TEXT NOT NULL,             -- 'probing' | 'open' | 'claimed' | 'done' | 'abandoned'
    holder_kind       TEXT,                      -- 'session' | 'run' | 'job' | 'postgate' | 'verify'; NULL when open
    holder_id         TEXT,
    holder_worktree   TEXT,                      -- canonical path; how the land gate recognises the owner
    lease_until       TEXT,                      -- sessions, and rows in 'probing'
    checked_sha       TEXT,                      -- master-red: last master sha the red_tests were proved failing on
    probe_lease_until TEXT,                      -- master-red: single-flight of the re-proof
    created_at        TEXT NOT NULL,
    claimed_at        TEXT,
    closed_at         TEXT,
    close_note        TEXT
);

-- At most one unresolved task per (project, key). Closed tasks leave the index and stay as history.
CREATE UNIQUE INDEX task_claims_one_unresolved_per_key
    ON task_claims (project_id, task_key)
    WHERE status IN ('probing', 'open', 'claimed');

-- The claims a run, a job or a session holds: release_for_owner, touch_session, the startup sweep.
CREATE INDEX task_claims_by_holder
    ON task_claims (holder_kind, holder_id)
    WHERE status = 'claimed';

-- list_live / guarding read a project's unresolved rows.
CREATE INDEX task_claims_by_project_status ON task_claims (project_id, status);

-- Who held what, when. On `taken_over`, `expired_reclaimed` and `done` the holder_* columns hold the
-- PREVIOUS owner (the compare-and-swap overwrites them on the task row), and `note` says who or what
-- came next. That is how a session finds out its claim was lost.
CREATE TABLE task_claim_events (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id         INTEGER NOT NULL REFERENCES task_claims (id),
    event           TEXT NOT NULL,   -- created | claimed | taken_over | released | expired_reclaimed | done | abandoned | override_land
    holder_kind     TEXT,
    holder_id       TEXT,
    holder_worktree TEXT,
    at              TEXT NOT NULL,
    note            TEXT NOT NULL DEFAULT ''
);

CREATE INDEX task_claim_events_by_task ON task_claim_events (task_id, id);
-- take_notices scans the events after a session's cursor.
CREATE INDEX task_claim_events_by_holder ON task_claim_events (holder_id, id);

-- Sessions the hooks have seen: session -> worktree, and how far through the event log it has been told.
-- `notice_cursor` is the id of the last task_claim_events row already delivered.
CREATE TABLE task_sessions (
    session_id    TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL,
    worktree      TEXT NOT NULL,
    last_seen     TEXT NOT NULL,
    notice_cursor INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX task_sessions_by_project ON task_sessions (project_id);
