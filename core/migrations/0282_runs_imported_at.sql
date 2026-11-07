-- Marks a run row that arrived inside a chat transfer package instead of happening on this machine.
--
-- Moving a chat now carries its run history (prompt, reply, cost, tokens), so the destination can show
-- the conversation and replay it to a model that has no session. Those rows were paid for on the
-- origin: the chat's own total keeps them, but analytics, the fleet board and the token baselines
-- must not count them as spend or work done here. NULL (every row before this migration, and every
-- row a turn here writes) means "happened here"; the value is the import time, RFC3339.
ALTER TABLE runs ADD COLUMN imported_at TEXT;
