-- The one truth about which (action, rule) pairs are off, in shadow or live. No CHECK constraints:
-- `mode` is 'off' | 'shadow' | 'live' and `changed_by` is 'owner' | 'rollback', both validated in Rust.
CREATE TABLE devtime_action_state (
    action TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    mode TEXT NOT NULL DEFAULT 'shadow',
    since TEXT NOT NULL,
    changed_by TEXT NOT NULL,
    reason TEXT,
    PRIMARY KEY (action, rule_id)
);
