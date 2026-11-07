-- The tree-job watchdog's record of a running node: when it last proved alive (its lease), how many
-- relaunches of it failed in a row, when the next try may start (the backoff), and the decision that
-- parked it after too many failures. A row is deleted when the node relaunches or the decision is answered.
CREATE TABLE node_watchdog (
    node_id TEXT PRIMARY KEY,
    beat_at TEXT,
    failures INTEGER NOT NULL DEFAULT 0,
    next_at TEXT,
    last_error TEXT,
    decision_id TEXT,
    updated_at TEXT NOT NULL
);
