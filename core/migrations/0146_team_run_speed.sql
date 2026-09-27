-- The speed a department run was started at (speed spec, section 5.2).
--
-- Per run and not per team: the team's `max_parallel` is what `normal` means, and `fast` is a
-- choice about THIS piece of work, made by whoever started it. NULL is `normal` — every run
-- before this column behaves exactly as it did. Validated by `speed.rs`, not by a CHECK: one owner.
ALTER TABLE team_runs ADD COLUMN speed TEXT;
