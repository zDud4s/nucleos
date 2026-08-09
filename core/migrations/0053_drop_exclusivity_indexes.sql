-- The one-at-a-time indexes come out, now that `project_slots` (0052) holds the same property with
-- a number instead of a boolean.
--
-- Last, and not in 0052, on purpose. Between creating the table and converting the final caller
-- there was a window in which neither mechanism was complete; dropping these there would have left
-- nothing holding anything, and what that permits is two worktrees writing one repository.
--
-- What actually changes for a reader: a run stranded at `running` or `awaiting_approval` no longer
-- blocks its whole project. It consumes one slot instead — less catastrophic, and much quieter,
-- which is why `concurrency::reconcile_orphaned_slots` runs on the job tick as well as at startup.
-- The `core/AGENTS.md` paragraph that explained cancellation safety through `0009` is rewritten in
-- the same commit as this file, because it describes a mechanism that stops existing here.
DROP INDEX one_open_worktree_run_per_project;
DROP INDEX one_live_job_per_project;
