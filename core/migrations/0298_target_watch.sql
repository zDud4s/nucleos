-- The last tip of each project's integration branch the daemon has accounted for
-- (`task_claims::direct_write`). A tick that finds the branch somewhere else asks whether the queue
-- moved it (every first-parent commit since is a succeeded request's `result_sha`); anything else is a
-- direct write, and it opens the `direct-write:<target>` ticket. The row only advances once the move
-- has been accounted for, so a scan skipped while the queue runs looks again at the same range.
CREATE TABLE target_watch (
    project_id TEXT NOT NULL,
    target     TEXT NOT NULL,
    tip        TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (project_id, target)
);
