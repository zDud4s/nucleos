-- Tree cost: a per-tree spending budget, the record of cache-wait decisions, the wait policy
-- settings, and a view that sums what a tree's runs cost.
--
-- `tree_budgets.tree_id` is the root node id; there is no foreign key to the tree-job tables, which
-- land in a parallel phase. A NULL `limit_usd` means no cost ceiling. `created_at`, `updated_at`,
-- `decided_at` and `returned_at` are RFC 3339 text, the house convention.
CREATE TABLE tree_budgets (
    tree_id TEXT PRIMARY KEY,
    limit_usd REAL,
    soft_pct REAL NOT NULL DEFAULT 80,
    action TEXT NOT NULL DEFAULT 'park',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- One row per wait decision, closed later with what actually happened, so the policy can grade itself.
CREATE TABLE cache_waits (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT,
    context_tokens INTEGER,
    expected_wait_s INTEGER NOT NULL,
    ttl_s INTEGER,
    action TEXT NOT NULL,
    decided_at TEXT NOT NULL,
    returned INTEGER,
    returned_at TEXT
);

CREATE INDEX cache_waits_session ON cache_waits (session_id, decided_at);

CREATE TABLE wait_policy (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT '1970-01-01T00:00:00+00:00'
);

-- Conservative starting values: rotate a big context before a wait longer than the cache TTL, and
-- keep-warm off until the cache report authorises it.
INSERT INTO wait_policy (key, value) VALUES
    ('rotate_context_tokens', '60000'),
    ('rotate_min_wait_over_ttl', '1'),
    ('keep_warm_enabled', 'false'),
    ('resume_probability', '0.5');

-- Cost and tokens per tree. `work_nodes.run_id` is TEXT and `runs.id` is INTEGER, hence the CAST.
-- `measured_usd` sums only runs whose CLI reported a cost; `unmeasured_runs` counts the others so the
-- caller can price them with `governance::budget::unmeasured_usd` instead of reading them as zero.
CREATE VIEW tree_run_costs AS
SELECT
    n.root_id AS root_id,
    COUNT(r.id) AS runs,
    COALESCE(SUM(r.cost_usd), 0.0) AS measured_usd,
    COALESCE(SUM(CASE WHEN r.cost_usd IS NULL THEN 1 ELSE 0 END), 0) AS unmeasured_runs,
    COALESCE(SUM(r.input_tokens), 0) AS input_tokens,
    COALESCE(SUM(r.output_tokens), 0) AS output_tokens,
    COALESCE(SUM(r.cache_read_tokens), 0) AS cache_read_tokens,
    COALESCE(SUM(r.cache_creation_tokens), 0) AS cache_creation_tokens
FROM work_nodes n
JOIN runs r ON r.id = CAST(n.run_id AS INTEGER)
GROUP BY n.root_id;
