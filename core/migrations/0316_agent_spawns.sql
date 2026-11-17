-- One row per subagent spawn the Agent-tool hook saw, and later how it ended (`models/agent_spawns.rs`).
--
-- Same convention as `0151_run_route_advice.sql`: every column but the identity ones is nullable and
-- NULL means "not recorded", never "none". A spawn the router never advised, or a transcript that
-- could not be priced, reads as NULL rather than as a zero that would drag an average.

CREATE TABLE agent_spawns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- The Claude Code session that made the call; escalation and redispatch are read per session.
    session_id TEXT NOT NULL,
    subagent_type TEXT,
    description TEXT,
    -- The model the spawn went out on (alias or id as the call carried it). NULL: untouched, unknown.
    model TEXT,
    -- Where that model came from: `explicit`, `router`, `ladder`, `failopen` or `skipped`.
    source TEXT NOT NULL,
    -- The `agents` surface mode the decision was taken under: `shadow` or `apply`.
    mode TEXT NOT NULL,
    -- The model the router advised, whether or not it was used.
    advised_model TEXT,
    -- The router's `decision_id`, which an outcome is later reported against.
    route_decision_id TEXT,
    -- A JSON array of `model[@effort]`: the attempts already failed when this one was routed.
    failed TEXT,
    spawned_at INTEGER NOT NULL,
    ended_at INTEGER,
    -- `ok`, `error` or `lost` (no outcome ever came back; set by `sweep_lost`).
    outcome TEXT,
    cost_usd REAL
);

CREATE INDEX idx_agent_spawns_session ON agent_spawns (session_id, spawned_at);
