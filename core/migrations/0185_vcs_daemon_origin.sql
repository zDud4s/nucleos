-- A request the daemon files on its own standing order says so.
--
-- The post-merge gate's revert (`revert_on_red`) used to be recorded as `human`, which is a lie of
-- the kind `shell` exists to avoid: nobody typed it. `daemon` is the daemon acting on the owner's
-- standing instruction (turning the switch on is the consent), so it needs no per-request approval.
--
-- SQLite cannot alter a CHECK, so the table is rebuilt, as 0073 and 0162 did. The column list,
-- the copy and the two indexes are 0162's, unchanged; only 'daemon' joins the origin CHECK.
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
       resolved_by, settled_at, settled_reason, pending_map_blob, approved_map_blob
  FROM vcs_requests;

DROP TABLE vcs_requests;
ALTER TABLE vcs_requests_new RENAME TO vcs_requests;

CREATE UNIQUE INDEX one_running_vcs_request_per_repo
    ON vcs_requests (repo_key) WHERE status = 'running';
CREATE INDEX vcs_requests_queued ON vcs_requests (repo_key, id) WHERE status = 'queued';
