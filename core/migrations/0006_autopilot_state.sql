CREATE TABLE autopilot_state (
    project_id TEXT PRIMARY KEY,
    mode TEXT NOT NULL CHECK (mode IN ('off', 'shadow', 'active'))
);

-- This table has exactly one row, seeded by this migration.
CREATE TABLE autopilot_global (
    kill_switch INTEGER NOT NULL DEFAULT 0
);

INSERT INTO autopilot_global (kill_switch) VALUES (0);
