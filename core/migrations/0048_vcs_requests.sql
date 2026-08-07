-- Typed intentions over a repository's shared state. The agent says WHAT; the daemon builds the
-- argv. A command string here would put shell parsing back on the attack surface that
-- classifier.rs exists to police.
CREATE TABLE vcs_requests (
    id             INTEGER PRIMARY KEY,
    op             TEXT    NOT NULL,
    args           TEXT    NOT NULL,
    project_id     TEXT    NOT NULL,
    project_root   TEXT    NOT NULL,
    origin         TEXT    NOT NULL CHECK (origin IN ('human', 'run', 'job', 'shell')),
    run_id         INTEGER,
    status         TEXT    NOT NULL CHECK (status IN (
                       'awaiting_approval', 'queued', 'running',
                       'succeeded', 'failed', 'blocked', 'rejected', 'cancelled', 'interrupted'
                   )),
    proposal_id    INTEGER,
    result_sha     TEXT,
    exit_code      INTEGER,
    output_tail    TEXT,
    failure_reason TEXT,
    created_at     TEXT    NOT NULL,
    started_at     TEXT,
    finished_at    TEXT
);

-- The invariant the whole module carries, and a backstop rather than the everyday mechanism: the
-- claim statement's own NOT EXISTS guard is what makes a loser return zero rows instead of
-- erroring. This index is what makes a *bug* in that guard impossible to ship silently — and it is
-- the only half of the pair that survives a process restart. Partial, so only `running` is
-- constrained: any number of requests may sit queued. Same shape as 0009_worktree_exclusivity.sql.
CREATE UNIQUE INDEX one_running_vcs_request_per_repo
    ON vcs_requests (project_id) WHERE status = 'running';

-- The claim query reads exactly this.
CREATE INDEX vcs_requests_queued ON vcs_requests (project_id, id) WHERE status = 'queued';
