-- The context-fit card of a closed work node (spec 2026-11-12, section 4): what the node was given,
-- what it used, and the verdicts (`under`, `over`, `split`; an empty list is `fit`) with the
-- evidence behind each. One row per node, rewritten when a node closes again or when
-- `rules_version` moves. `verdicts`, `evidence`, `given` and `used` are JSON text.
CREATE TABLE node_context_fit (
    node_id TEXT PRIMARY KEY,
    tree_id TEXT NOT NULL,
    layer TEXT NOT NULL,
    verdicts TEXT NOT NULL DEFAULT '[]',
    evidence TEXT NOT NULL DEFAULT '[]',
    given TEXT NOT NULL DEFAULT '{}',
    used TEXT NOT NULL DEFAULT '{}',
    rules_version INTEGER NOT NULL,
    computed_at TEXT NOT NULL
);

CREATE INDEX idx_node_context_fit_tree ON node_context_fit (tree_id);
CREATE INDEX idx_node_context_fit_computed ON node_context_fit (computed_at);
