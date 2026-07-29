-- Expiring owner-presence heartbeats, scoped globally or to one project.
CREATE TABLE attention_heartbeats (
    scope        TEXT NOT NULL CHECK (scope IN ('global', 'project')),
    project_id   TEXT NOT NULL DEFAULT '',
    last_seen_at TEXT NOT NULL,
    PRIMARY KEY (scope, project_id),
    CHECK (
        (scope = 'global' AND project_id = '')
        OR (scope = 'project' AND project_id <> '')
    )
);
