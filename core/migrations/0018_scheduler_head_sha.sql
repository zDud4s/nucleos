-- The repo HEAD as it stood when a rule's next window was scheduled (spec 8.1/8.3).
--
-- A catch-up run - one recovering a window missed because the machine was off, the daemon was down,
-- or the user was mid-session - is the most dangerous run in the system precisely because nothing
-- about it looks dangerous: an ordinary scheduled task carrying assumptions that may be days stale,
-- on a repo that may have moved underneath it. Recording the sha turns "this plan is old" from a
-- guess into a fact the scheduler can act on.
--
-- NULL for rules armed before this migration; the first fire after it records one.
ALTER TABLE scheduler_state ADD COLUMN last_head_sha TEXT;
