-- Which Claude Code session (an IDE or terminal one) launched a run or a work node through the MCP
-- tools `create_run`, `tree_launch` and `node_open`. The session's NucleOS hook stamps its id into
-- the tool's `session_id` argument, so the daemon learns it without trusting the model.
--
-- Distinct from `runs.launched_by_chat_id` (a chat turn asked) and from `work_nodes.session_id` (the
-- binding `node_active` looks a tree up by). TEXT and no foreign key: a label for the Fleet view to
-- parent the launch under `claude-code:<session>`, and nothing decides anything on it. NULL when no
-- session launched it.
ALTER TABLE runs ADD COLUMN launching_session_id TEXT;
ALTER TABLE work_nodes ADD COLUMN launching_session_id TEXT;
