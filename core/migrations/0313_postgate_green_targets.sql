-- The gate targets the last green covered, as a JSON array. NULL = recorded before this column
-- existed (whatever gate_command ran); a red leaves it alone.
ALTER TABLE postgate_state ADD COLUMN green_targets TEXT;
