-- The last result of a `command:` schedule rule, kept on the rule's own scheduler_state row.
-- command_outcome is one of passed | failed | errored | refused. All four columns stay NULL
-- until a command rule has fired.
ALTER TABLE scheduler_state ADD COLUMN command_outcome TEXT;
ALTER TABLE scheduler_state ADD COLUMN command_exit_code INTEGER;
ALTER TABLE scheduler_state ADD COLUMN command_output TEXT;
ALTER TABLE scheduler_state ADD COLUMN command_ended_at TEXT;
