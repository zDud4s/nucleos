-- A landing that changes the test map waits for the owner.
--
-- `nucleos.tests.yaml` decides which tests guard every merge, so a merge that changes it is the
-- one merge the gate cannot judge: the run under judgement would be choosing its own judge. The
-- queue pauses it in `awaiting_owner` until the owner approves THAT content of the map
-- (spec 2026-10-05 §3.4, defence 1).
--
-- Not terminal: approving puts the row back to `queued`, refusing ends it as `rejected`.
--
--   pending_map_blob  -- the map's blob id in the merge that paused (or 'deleted').
--   approved_map_blob -- the blob the owner approved; the next execution passes only if the
--                        merge it computes carries exactly this one.
--
-- SQLite cannot alter a CHECK, so the table is rebuilt, as 0073 did. The column list is 0073's
-- plus every column added since (0081, 0083, 0127, 0156) plus the two above, and the two indexes
-- are 0073's, unchanged.
CREATE TABLE vcs_requests_new (
    id                INTEGER PRIMARY KEY,
    op                TEXT    NOT NULL,
    args              TEXT    NOT NULL,
    project_id        TEXT    NOT NULL,
    project_root      TEXT    NOT NULL,
    origin            TEXT    NOT NULL CHECK (origin IN ('human', 'run', 'job', 'shell')),
    run_id            INTEGER,
    status            TEXT    NOT NULL CHECK (status IN (
                          'awaiting_approval', 'queued', 'running',
                          'succeeded', 'failed', 'blocked', 'rejected', 'cancelled', 'interrupted',
                          'escalated', 'awaiting_owner'
                      )),
    proposal_id       INTEGER,
    result_sha        TEXT,
    exit_code         INTEGER,
    output_tail       TEXT,
    failure_reason    TEXT,
    created_at        TEXT    NOT NULL,
    started_at        TEXT,
    finished_at       TEXT,
    repo_key          TEXT    NOT NULL DEFAULT '',
    resolution_run_id INTEGER,
    from_resolution   INTEGER NOT NULL DEFAULT 0,
    discarded         TEXT,
    attempts          INTEGER NOT NULL DEFAULT 0,
    resolved_by       INTEGER,
    settled_at        TEXT,
    settled_reason    TEXT,
    pending_map_blob  TEXT,
    approved_map_blob TEXT
);

INSERT INTO vcs_requests_new
SELECT id, op, args, project_id, project_root, origin, run_id, status, proposal_id,
       result_sha, exit_code, output_tail, failure_reason, created_at, started_at,
       finished_at, repo_key, resolution_run_id, from_resolution, discarded, attempts,
       resolved_by, settled_at, settled_reason, NULL, NULL
  FROM vcs_requests;

DROP TABLE vcs_requests;
ALTER TABLE vcs_requests_new RENAME TO vcs_requests;

CREATE UNIQUE INDEX one_running_vcs_request_per_repo
    ON vcs_requests (repo_key) WHERE status = 'running';
CREATE INDEX vcs_requests_queued ON vcs_requests (repo_key, id) WHERE status = 'queued';
