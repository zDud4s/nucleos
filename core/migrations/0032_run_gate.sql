-- Records the daemon's verification of a completed worktree run beside the agent's own outcome.
--
-- NULL is the honest value when a project has no gate command: that project has no definition of
-- green for the daemon to measure. A default verdict would invent a check that never happened and
-- make an unconfigured project indistinguishable from one whose configured gate produced a result.
ALTER TABLE runs ADD COLUMN gate_status TEXT;
ALTER TABLE runs ADD COLUMN gate_exit_code INTEGER;
ALTER TABLE runs ADD COLUMN gate_output TEXT;
