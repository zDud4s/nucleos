-- Reports on a task (spec 2026-11-06-tickets-globais). A problem found by any session is REPORTED
-- first: the same key adds one more row here and never a second ticket. At most 50 reports are kept
-- per ticket (the oldest are deleted and counted in task_claims.detail.reports_dropped); `report_count`
-- is the all-time total. `blocking` marks a reporter that is waiting for the fix and must hear when
-- the ticket closes. `linked_to` points at another ticket of the same project; it is a plain pointer
-- and never copies guard paths. No CHECK constraints (the rule of 0143): the allowed values of
-- reporter_kind (session|run|job|daemon) live in Rust.
ALTER TABLE task_claims ADD COLUMN linked_to INTEGER;
ALTER TABLE task_claims ADD COLUMN report_count INTEGER NOT NULL DEFAULT 0;

CREATE TABLE task_claim_reports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id INTEGER NOT NULL REFERENCES task_claims(id),
    reporter_kind TEXT NOT NULL,
    reporter_id TEXT NOT NULL,
    reporter_worktree TEXT,
    at TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    paths TEXT NOT NULL DEFAULT '[]',
    tests TEXT NOT NULL DEFAULT '[]',
    blocking INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_task_claim_reports_task ON task_claim_reports (task_id, id);
