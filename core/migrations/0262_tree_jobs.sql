-- Tree jobs: a daemon-driven tree of work, one row per root node, and the transitions it has applied.
--
-- `tree_jobs` holds the job's manifest as JSON text in `manifest`. `state` is a closed vocabulary
-- validated in Rust, not here, like `devtime_action_state.mode`. `tree_transitions` is the
-- idempotency record: a (node, transition, key) already present is a replay, and its stored `result`
-- is returned instead of applying the transition again. `work_nodes.tree_job_id` links a node to its
-- job; the run behind a node is the existing `work_nodes.run_id` (0216).
-- `created_at` and `updated_at` are RFC 3339 text, the house convention.
CREATE TABLE tree_jobs (
    id TEXT PRIMARY KEY,
    root_node_id TEXT NOT NULL UNIQUE,
    project_id TEXT,
    feature_branch TEXT,
    goal TEXT NOT NULL,
    state TEXT NOT NULL,
    manifest TEXT NOT NULL,
    contract TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE tree_transitions (
    node_id TEXT NOT NULL,
    transition TEXT NOT NULL,
    key TEXT NOT NULL,
    result TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (node_id, transition, key)
);

ALTER TABLE work_nodes ADD COLUMN tree_job_id TEXT;
