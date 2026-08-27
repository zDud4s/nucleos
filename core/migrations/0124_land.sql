-- `core/src/land.rs`'s three columns: the design's decisions #2, #4 and #5.

-- Decision #2. Which branch a landing merges into, declared once and never re-derived from a
-- worktree's HEAD. NULL means "not derived yet" — `land::integration_branch` fills it in on the
-- first landing a project asks for, and every one after that reads the value back rather than
-- asking git again.
ALTER TABLE autopilot_state ADD COLUMN integration_branch TEXT;

-- Decision #4. How many times THIS row has been requeued because the target moved while its merge
-- was being computed. Zero by default, like `from_resolution`'s own default in 0081. The ceiling
-- that reads it lives in `vcs.rs`, not in a CHECK constraint: 3 is a design decision, not a shape
-- the column can be wrong about.
ALTER TABLE vcs_requests ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;

-- Decision #5. The landing that answered THIS row's escalation, once one exists. NULL for almost
-- every row — set only on the original conflicted request, the moment a resolution for it is
-- admitted, so `wait_for` can follow the link instead of handing back a terminal `escalated`
-- status to a session that is still, in every sense that matters to it, waiting.
--
-- No foreign key, matching `resolution_run_id` (0081) and `action_grants.queued_request_id` for
-- the same reason: a row this points at is never pruned out from under it today, but nothing here
-- should depend on that staying true.
ALTER TABLE vcs_requests ADD COLUMN resolved_by INTEGER;
