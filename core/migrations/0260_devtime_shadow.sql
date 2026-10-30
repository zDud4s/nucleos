-- Devtime automatic actions, shadow mode: every decision an action (inject, propose) took or would
-- have taken, why, and what the human did in the observation window. No CHECK constraints (house
-- rule of 0143): the vocabularies are Rust constants validated before the write. Timestamps are UTC
-- `%Y-%m-%dT%H:%M:%S%.3fZ`. No free-text message column: `params` holds numbers, ids and hashes only.
CREATE TABLE devtime_shadow (
    id INTEGER PRIMARY KEY,
    action TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    session_id TEXT,
    subject_key TEXT NOT NULL,
    mode TEXT NOT NULL,
    decision TEXT NOT NULL,
    suppress_reason TEXT,
    template_id TEXT NOT NULL,
    params TEXT NOT NULL DEFAULT '{}',
    basis_precision REAL,
    basis_cases INTEGER,
    basis_recent_count INTEGER,
    basis_recent_cost_ms INTEGER,
    expected_saving_ms INTEGER,
    rule_version INTEGER NOT NULL,
    action_version INTEGER NOT NULL,
    thresholds_hash TEXT NOT NULL,
    rules_version TEXT NOT NULL,
    decided_at TEXT NOT NULL,
    window_end_at TEXT NOT NULL,
    outcome_state TEXT NOT NULL DEFAULT 'pending',
    outcome TEXT,
    outcome_finding_key TEXT,
    outcome_cost_ms INTEGER,
    outcome_verified TEXT,
    proposal_id INTEGER,
    human_verdict TEXT,
    human_verdict_at TEXT,
    resolved_at TEXT
);

CREATE UNIQUE INDEX idx_devtime_shadow_decision
    ON devtime_shadow (action, rule_id, subject_key, COALESCE(session_id, ''), action_version, mode);
CREATE INDEX idx_devtime_shadow_pending ON devtime_shadow (outcome_state, window_end_at);
CREATE INDEX idx_devtime_shadow_rule ON devtime_shadow (action, rule_id, mode, decided_at);
