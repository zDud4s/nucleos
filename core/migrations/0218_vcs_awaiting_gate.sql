-- A push of the integration branch whose sha the post-land gate has not yet judged green waits in
-- `awaiting_gate` instead of pushing (or blocking its caller). Like `awaiting_owner` it is not
-- terminal and not claimable; the post-land gate puts it back to `queued` when a sha turns green.
--
-- SQLite cannot alter a CHECK, so the table is rebuilt, as 0073, 0162 and 0185 did. The column list
-- is 0185's plus `unexecutable` (0208); the copy and the two partial indexes are 0185's. The three
-- read indexes 0209 added are dropped with the old table and so are recreated here.
CREATE TABLE vcs_requests_new (
    id                INTEGER PRIMARY KEY,
    op                TEXT    NOT NULL,
    args              TEXT    NOT NULL,
    project_id        TEXT    NOT NULL,
    project_root      TEXT    NOT NULL,
    origin            TEXT    NOT NULL CHECK (origin IN ('human', 'run', 'job', 'shell', 'daemon')),
    run_id            INTEGER,
    status            TEXT    NOT NULL CHECK (status IN (
                          'awaiting_approval', 'queued', 'running',
                          'succeeded', 'failed', 'blocked', 'rejected', 'cancelled', 'interrupted',
                          'escalated', 'awaiting_owner', 'awaiting_gate'
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
    approved_map_blob TEXT,
    unexecutable      INTEGER NOT NULL DEFAULT 0 CHECK (unexecutable IN (0, 1))
);

INSERT INTO vcs_requests_new
SELECT id, op, args, project_id, project_root, origin, run_id, status, proposal_id,
       result_sha, exit_code, output_tail, failure_reason, created_at, started_at,
       finished_at, repo_key, resolution_run_id, from_resolution, discarded, attempts,
       resolved_by, settled_at, settled_reason, pending_map_blob, approved_map_blob,
       unexecutable
  FROM vcs_requests;

DROP TABLE vcs_requests;
ALTER TABLE vcs_requests_new RENAME TO vcs_requests;

CREATE UNIQUE INDEX one_running_vcs_request_per_repo
    ON vcs_requests (repo_key) WHERE status = 'running';
CREATE INDEX vcs_requests_queued ON vcs_requests (repo_key, id) WHERE status = 'queued';
CREATE INDEX vcs_requests_by_created ON vcs_requests (created_at);
CREATE INDEX vcs_requests_by_ended
    ON vcs_requests (COALESCE(settled_at, finished_at));
CREATE INDEX vcs_requests_by_status ON vcs_requests (status);
