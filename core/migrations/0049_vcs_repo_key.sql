-- The lock this module exists to take was on `project_id`, a label, while every command ran in
-- `project_root`, a path. Two projects naming one repository therefore ran against it at the same
-- time -- exactly what vcs.rs's first paragraph says cannot happen. The key moves to the repository
-- itself: git's own canonical common directory, one value for a main checkout and every linked
-- worktree of it.
ALTER TABLE vcs_requests ADD COLUMN repo_key TEXT NOT NULL DEFAULT '';

-- Backfill from `project_id`, NOT from `project_root`: two rows can share a root and differ in
-- project id, and the OLD unique index guaranteed at most one `running` row per `project_id` -- so
-- this is the only value that cannot make the new unique index fail to build.
UPDATE vcs_requests SET repo_key = project_id;

-- ...and then retire every row that is not already terminal. A backfilled key is a LABEL, not a
-- repository, so a surviving `queued` row would be claimable under key 'alpha' while a new request
-- for the same repository holds key '\\?\C:\...\.git' -- two operations against one repository,
-- which is the defect this migration exists to remove. `interrupted` is the status the module
-- already uses for "the daemon cannot say whether this finished".
--
-- `finished_at` is deliberately left alone rather than stamped. Every timestamp in this schema is
-- written by `chrono::Utc::now().to_rfc3339()`; SQLite's `datetime('now')` is a different format
-- (space separator, no offset, second precision) and would be the first value in the table that does
-- not sort lexically with its neighbours. And the daemon does not know when these ended -- an
-- invented timestamp is worse than the absent one NULL already reports.
UPDATE vcs_requests
   SET status = 'interrupted',
       failure_reason = 'the queue changed how it identifies repositories; resubmit if still wanted'
 WHERE status IN ('queued', 'running', 'awaiting_approval');

DROP INDEX one_running_vcs_request_per_repo;
DROP INDEX vcs_requests_queued;

CREATE UNIQUE INDEX one_running_vcs_request_per_repo
    ON vcs_requests (repo_key) WHERE status = 'running';
CREATE INDEX vcs_requests_queued ON vcs_requests (repo_key, id) WHERE status = 'queued';
