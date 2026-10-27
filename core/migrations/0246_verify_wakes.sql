-- Pushing the verdict of a verification to the run that asked for it, instead of the run polling
-- `verify_status`.
--
-- `verify_requests.seen_at` is set when a finished ticket is handed back to its `run:{id}` caller
-- by `POST /verify` or `POST /verify/status`: a ticket the run has seen needs no wake.
--
-- `verify_wakes` has one row per ticket a run was (or is to be) woken for. `pending` is a run that
-- ended with the ticket still open or unseen (its worktree is kept and its judge deferred until the
-- row settles), `delivered` is a verdict sent as a mid-run turn or as a resumed continuation, and
-- `refused` is a wake the brakes stopped. `root_run_id` is the run's lineage root, so the number of
-- wakes of one task can be capped.
--
-- No foreign keys and no CHECK constraints, as in 0170: the vocabularies are Rust constants.
ALTER TABLE verify_requests ADD COLUMN seen_at TEXT;

CREATE TABLE verify_wakes (
    ticket_id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL,
    root_run_id INTEGER NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX verify_wakes_run ON verify_wakes (run_id, state);
CREATE INDEX verify_wakes_root ON verify_wakes (root_run_id);
