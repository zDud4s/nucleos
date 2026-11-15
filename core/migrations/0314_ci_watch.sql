-- The CI watch: one row per (project, sha, workflow) an automation pushed, so the watch is
-- idempotent across daemon restarts.
CREATE TABLE ci_watch (
  project_id TEXT NOT NULL, branch TEXT NOT NULL, sha TEXT NOT NULL, workflow TEXT NOT NULL,
  pushed_at TEXT NOT NULL,                 -- the firing's `at`: the 15-minute clock starts here
  state TEXT NOT NULL DEFAULT 'watching',  -- watching | rerun_waiting | done | superseded | gave_up
  run_id INTEGER, run_created_at TEXT,
  rerun_from_attempt INTEGER,              -- attempt number at the moment the rerun was requested
  outcome TEXT,                            -- NULL | ci_unavailable | no_run | stuck | superseded | finished
  warned_at TEXT,                          -- one feed line per (sha, reason)
  PRIMARY KEY (project_id, sha, workflow)
);
