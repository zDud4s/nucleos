-- Every run a work node ever launched. `work_nodes.run_id` holds only the current one, so a relaunched
-- node dropped its earlier runs out of the tree's cost. This keeps them.
CREATE TABLE work_node_runs (
    node_id TEXT NOT NULL,
    run_id INTEGER NOT NULL,
    PRIMARY KEY (node_id, run_id)
);

INSERT OR IGNORE INTO work_node_runs (node_id, run_id)
SELECT id, CAST(run_id AS INTEGER) FROM work_nodes WHERE run_id IS NOT NULL;

-- A run counts once per tree, whichever node launched it. The union with the current
-- `work_nodes.run_id` keeps a run counted even when no history row was written for it.
CREATE VIEW tree_run_ids AS
SELECT n.root_id AS root_id, CAST(n.run_id AS INTEGER) AS run_id
FROM work_nodes n
WHERE n.run_id IS NOT NULL
UNION
SELECT n.root_id AS root_id, h.run_id AS run_id
FROM work_node_runs h
JOIN work_nodes n ON n.id = h.node_id;

DROP VIEW tree_run_costs;
CREATE VIEW tree_run_costs AS
SELECT
    t.root_id AS root_id,
    COUNT(r.id) AS runs,
    COALESCE(SUM(r.cost_usd), 0.0) AS measured_usd,
    COALESCE(SUM(CASE WHEN r.cost_usd IS NULL THEN 1 ELSE 0 END), 0) AS unmeasured_runs,
    COALESCE(SUM(r.input_tokens), 0) AS input_tokens,
    COALESCE(SUM(r.output_tokens), 0) AS output_tokens,
    COALESCE(SUM(r.cache_read_tokens), 0) AS cache_read_tokens,
    COALESCE(SUM(r.cache_creation_tokens), 0) AS cache_creation_tokens
FROM tree_run_ids t
JOIN runs r ON r.id = t.run_id
GROUP BY t.root_id;
