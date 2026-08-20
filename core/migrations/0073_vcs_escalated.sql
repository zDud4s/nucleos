-- A conflicting merge stops being the asker's problem.
--
-- `failed` said the operation did not happen and that was the end of it, so the only actor left
-- holding a conflict was whoever had asked -- which is the agent that had just finished its work,
-- and the one actor the queue exists to spare from other sessions' integration. `escalated` says
-- something different and says it in the row: the operation did not happen, a person now owns it,
-- and there is a proposal to prove it.
--
-- Terminal, like `failed`. Nothing here resumes on its own: the escalation is the end of THIS
-- request, and what a person decides afterwards arrives as a new one.
--
-- SQLite cannot alter a CHECK, so the table is rebuilt. The column list is 0048's plus 0049's
-- `repo_key`, and the two indexes are 0049's -- if either drifts from this, the drift is a bug
-- here and not in them.
CREATE TABLE vcs_requests_new (
    id             INTEGER PRIMARY KEY,
    op             TEXT    NOT NULL,
    args           TEXT    NOT NULL,
    project_id     TEXT    NOT NULL,
    project_root   TEXT    NOT NULL,
    origin         TEXT    NOT NULL CHECK (origin IN ('human', 'run', 'job', 'shell')),
    run_id         INTEGER,
    status         TEXT    NOT NULL CHECK (status IN (
                       'awaiting_approval', 'queued', 'running',
                       'succeeded', 'failed', 'blocked', 'rejected', 'cancelled', 'interrupted',
                       'escalated'
                   )),
    proposal_id    INTEGER,
    result_sha     TEXT,
    exit_code      INTEGER,
    output_tail    TEXT,
    failure_reason TEXT,
    created_at     TEXT    NOT NULL,
    started_at     TEXT,
    finished_at    TEXT,
    repo_key       TEXT    NOT NULL DEFAULT ''
);

INSERT INTO vcs_requests_new
SELECT id, op, args, project_id, project_root, origin, run_id, status, proposal_id,
       result_sha, exit_code, output_tail, failure_reason, created_at, started_at,
       finished_at, repo_key
  FROM vcs_requests;

DROP TABLE vcs_requests;
ALTER TABLE vcs_requests_new RENAME TO vcs_requests;

-- Recreated because the table they indexed is gone. Identical to 0049's, deliberately: this
-- migration adds a status and nothing else, and an index that quietly changed shape here would be
-- the hardest kind of change to find later.
CREATE UNIQUE INDEX one_running_vcs_request_per_repo
    ON vcs_requests (repo_key) WHERE status = 'running';
CREATE INDEX vcs_requests_queued ON vcs_requests (repo_key, id) WHERE status = 'queued';
