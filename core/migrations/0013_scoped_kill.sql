CREATE TABLE scoped_kill_switches (
    scope_type TEXT NOT NULL,
    scope_id   TEXT NOT NULL,
    engaged    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (scope_type, scope_id)
);
