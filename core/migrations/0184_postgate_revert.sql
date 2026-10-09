-- What the post-merge gate does after it confirms a culprit when `revert_on_red` is on (spec
-- 2026-10-05 §6.2, D4; F3-4): queue a revert of the culprit, follow it, then prepare a `fix/`
-- branch and open one correction run on it. All of it is kept on the project's row so a restart
-- resumes where it stopped, and so the revert and the run are each started exactly once. No CHECK
-- constraints, as in 0180 and 0182.
-- `revert_merge_sha` is the culprit being reverted and is NULL when no revert was started;
-- `revert_request_id` is its vcs ticket, `revert_sha` the commit the queue published, `fix_branch`
-- the branch built on it. `fix_run_id` is NULL until a correction run is claimed, 0 while one is
-- claimed and not yet created, and the run's id afterwards.
ALTER TABLE postgate_state ADD COLUMN revert_merge_sha TEXT;
ALTER TABLE postgate_state ADD COLUMN revert_request_id INTEGER;
ALTER TABLE postgate_state ADD COLUMN revert_sha TEXT;
ALTER TABLE postgate_state ADD COLUMN fix_branch TEXT;
ALTER TABLE postgate_state ADD COLUMN fix_run_id INTEGER;
