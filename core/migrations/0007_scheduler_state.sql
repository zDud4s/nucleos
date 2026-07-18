CREATE TABLE scheduler_state (
    project_id TEXT NOT NULL,
    rule_name TEXT NOT NULL,
    last_fired_at TEXT NOT NULL,
    PRIMARY KEY (project_id, rule_name)
);

ALTER TABLE autopilot_state ADD COLUMN project_root TEXT;
