-- The layers of a project's modules (owner decision 2026-10-11, "camadas geradas e congeladas"):
-- derived from the import graph the first time the map is read, frozen as a baseline, and judged
-- against afterwards. Three tables: the frozen layer of every module, the frozen import edges (so
-- a violation can be told apart as new), and the owner's moves, which beat the baseline.
CREATE TABLE map_layer_modules (
    project_id TEXT NOT NULL,
    path       TEXT NOT NULL,
    layer      INTEGER NOT NULL CHECK (layer >= 0),
    frozen_at  TEXT NOT NULL,
    PRIMARY KEY (project_id, path)
);

CREATE TABLE map_layer_imports (
    project_id TEXT NOT NULL,
    from_path  TEXT NOT NULL,
    to_path    TEXT NOT NULL,
    PRIMARY KEY (project_id, from_path, to_path)
);

CREATE TABLE map_layer_overrides (
    project_id TEXT NOT NULL,
    path       TEXT NOT NULL,
    layer      INTEGER NOT NULL CHECK (layer >= 0),
    set_at     TEXT NOT NULL,
    PRIMARY KEY (project_id, path)
);
