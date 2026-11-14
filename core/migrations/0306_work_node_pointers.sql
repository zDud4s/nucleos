-- Files a parent points a node at to start from (spec 2026-11-12-encaixe-de-contexto-por-no-design 3.2):
-- a JSON list of paths, set when the node is opened.
ALTER TABLE work_nodes ADD COLUMN pointers TEXT NOT NULL DEFAULT '[]';
