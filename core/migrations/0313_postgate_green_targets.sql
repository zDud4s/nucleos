-- The gate targets the last green covered, as a JSON array. NULL = recorded before this column
-- existed (whatever gate_command ran); a red leaves it alone.
ALTER TABLE postgate_state ADD COLUMN green_targets TEXT;
-- The local targets the RUNNING gate's command was composed from, as a JSON array, stored at submit
-- time from the same rules read that composed the command. NULL = the command ran whole (no
-- gate_targets). A green copies it into green_targets, so a rules change mid-run never adds a
-- target the gate did not run. Reset when a gate starts and when it finishes green.
ALTER TABLE postgate_state ADD COLUMN running_targets TEXT;
