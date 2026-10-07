-- One capture request per job: the distiller asks the owner what only they know about a job that
-- went wrong, and holds that job's queue rows until the answer or the deadline.
-- Design: .ai/specs/2026-10-07-pedidos-captura-design.md sections 3 and 5.
--
-- No CHECK constraints (the rule of 0143_knowledge.sql): the state vocabulary lives in
-- `capture::STATE_*`. No foreign key to `jobs`: project_exit deletes these rows explicitly, as it
-- deletes distill_queue.

CREATE TABLE capture_requests (
  job_id      INTEGER PRIMARY KEY,   -- one request per job, ever; also the #cap<job_id> mark
  project_id  TEXT NOT NULL,
  causes      TEXT NOT NULL,         -- JSON [{"row", "cause", "fact"}], one per queue row that asked
  prompt_text TEXT NOT NULL,         -- what the owner is shown, redacted before the write
  state       TEXT NOT NULL DEFAULT 'open',  -- open | answered | dismissed | expired
  deadline    TEXT NOT NULL,         -- capture::stamp, compared as text
  note_id     INTEGER,               -- the answer's note, when answered
  created_at  TEXT NOT NULL,
  closed_at   TEXT
);

-- The worker's two questions every tick: which requests are open, and which are past due.
CREATE INDEX capture_requests_open ON capture_requests (state, deadline);
