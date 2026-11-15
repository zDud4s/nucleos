-- Project automations: one row per rule firing, unique per (project, rule, event) so a re-emitted event fires
-- once. No CHECK on `outcome`, for the house reason (0143_knowledge.sql): the enum lives in Rust (automations/firings.rs).
CREATE TABLE automation_firings (
    id INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL,
    rule TEXT NOT NULL,
    event_key TEXT NOT NULL,
    branch TEXT NOT NULL,
    sha TEXT NOT NULL,
    action TEXT NOT NULL,
    outcome TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    at TEXT NOT NULL,
    UNIQUE (project_id, rule, event_key)
);

CREATE INDEX automation_firings_branch ON automation_firings (project_id, branch, action, id);

-- The rule that queued a `push_sha` request; NULL for every other origin.
ALTER TABLE vcs_requests ADD COLUMN automation_rule TEXT;
