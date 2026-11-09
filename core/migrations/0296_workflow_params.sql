-- Declared workflow parameters (spec 2026-11-08, phase 1). Values are opaque JSON; the daemon never reads a key's meaning.
CREATE TABLE workflow_param_history (
    project_id TEXT NOT NULL, bundle TEXT NOT NULL, param TEXT NOT NULL, version INTEGER NOT NULL,
    value_json TEXT NOT NULL,
    kind       TEXT NOT NULL,          -- baseline | set | rollback | hand_edit
    actor      TEXT NOT NULL,          -- owner:<who> | tuner:<id> | daemon
    reason     TEXT NOT NULL DEFAULT '',
    rolled_back_to INTEGER,
    written_at TEXT NOT NULL,
    PRIMARY KEY (project_id, bundle, param, version)
);
CREATE TABLE workflow_param_modes (
    scope TEXT NOT NULL,               -- machine | project:<id>  (settings::Scope text)
    bundle TEXT NOT NULL, param TEXT NOT NULL,
    mode TEXT NOT NULL,                -- auto | fixed
    updated_at TEXT NOT NULL, updated_by TEXT NOT NULL,
    PRIMARY KEY (scope, bundle, param)
);
