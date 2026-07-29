-- The runaway guard survives a restart.
--
-- `DAILY_CAP` was counted in a `HashMap` held by the scheduler loop, so it reset every time the
-- daemon started. The Task Scheduler registration restarts the daemon three times on failure, and
-- a daemon crashing is exactly the situation the guard is named for — so the one scenario where a
-- runaway matters was the one where the cap rearmed itself.
--
-- Counted per (project, rule) alongside the date it counts for, and bumped in the same
-- compare-and-set that claims the window. That makes spending an allowance and claiming a window
-- one act: a run that then fails to start has still spent both, which is the trade the claim
-- already makes on purpose — a missed window is visible and recoverable, a duplicated run is not.
--
-- NULL date for rules armed before this migration; the first fire after it records one.
ALTER TABLE scheduler_state ADD COLUMN fires_date TEXT;
ALTER TABLE scheduler_state ADD COLUMN fires_today INTEGER NOT NULL DEFAULT 0;
