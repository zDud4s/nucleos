-- Configuration in the database. Design: .ai/specs/2026-10-11-configuracao-na-base-de-dados-design.md
-- section 2.1. One row per (scope, document name); `scope` is `machine` or `project:<id>`. Every write
-- also appends a settings_history row (a deletion appends the body `null`), and the version never
-- restarts: the next version is max(history, current) + 1.
CREATE TABLE settings (
    scope      TEXT    NOT NULL,
    name       TEXT    NOT NULL,
    body_json  TEXT    NOT NULL,
    version    INTEGER NOT NULL,
    updated_at TEXT    NOT NULL,
    updated_by TEXT    NOT NULL,
    PRIMARY KEY (scope, name)
);

CREATE TABLE settings_history (
    scope      TEXT    NOT NULL,
    name       TEXT    NOT NULL,
    version    INTEGER NOT NULL,
    body_json  TEXT    NOT NULL,
    written_at TEXT    NOT NULL,
    written_by TEXT    NOT NULL,
    PRIMARY KEY (scope, name, version)
);
