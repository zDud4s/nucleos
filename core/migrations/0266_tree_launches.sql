-- Tree launches: the prompt behind each `launch` of a tree node, kept so a boot or an answered
-- decision can start the node's agent again.
--
-- `(node_id, key)` is the key of the `launch` row of `tree_transitions`, and the first write per key
-- wins, as a launch itself is once per key. `seq` orders the writes, so that "the latest launch of a
-- node" does not depend on a clock or on how keys sort. `project_id`, `cwd` and `mode` are what
-- `create_run_inner` was given. Neither column is a foreign key: the row may outlive its node.
CREATE TABLE tree_launches (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id TEXT NOT NULL,
    key TEXT NOT NULL,
    prompt TEXT NOT NULL,
    project_id TEXT,
    cwd TEXT,
    mode TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (node_id, key)
);
