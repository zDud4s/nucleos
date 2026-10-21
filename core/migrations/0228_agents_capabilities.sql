-- no-transaction
-- The catalogue stores capabilities, not a tool policy (spec 2026-10-20 §4, R1), and a package
-- agent has no engine of its own, so `engine` becomes nullable.
--
-- Dropping `tool_policy` and relaxing `engine` NOT NULL both need a table rebuild. `agents` is the
-- parent of `teams.director_agent_id`, `team_members.agent_id`, `team_items.agent_id` and
-- `job_items.agent_id`, so this needs `-- no-transaction` and `PRAGMA foreign_keys`, as `0154`
-- does for `runs`: with keys on, `DROP TABLE agents` would delete every row first and the children
-- would refuse it. The children name `agents` by text, so once `agents_new` takes the name they
-- point at it again; ids are copied unchanged. `foreign_key_check` runs before the keys come back.
--
-- `capabilities` is filled from the old policy: `mcp_only` -> ["mcp.nucleos"], `none` -> [].
-- (`unrestricted` was never accepted for an agent; it maps to [] defensively.) Every existing row
-- is a `user` row, and `Agent::tool_policy()` returns for it exactly the value it had.
PRAGMA foreign_keys = OFF;

BEGIN;

CREATE TABLE agents_new (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    speciality   TEXT NOT NULL,
    prompt       TEXT NOT NULL,
    engine       TEXT,
    model        TEXT,
    capabilities TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(capabilities)),
    source       TEXT NOT NULL DEFAULT 'user' CHECK (source IN ('package', 'user')),
    package_ref  TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    CHECK ((source = 'package') = (package_ref IS NOT NULL))
);

INSERT INTO agents_new
    (id, name, speciality, prompt, engine, model, capabilities, source, package_ref,
     created_at, updated_at)
SELECT id, name, speciality, prompt, engine, model,
       CASE tool_policy WHEN 'mcp_only' THEN '["mcp.nucleos"]' ELSE '[]' END,
       'user', NULL, created_at, updated_at
FROM agents;

DROP TABLE agents;
ALTER TABLE agents_new RENAME TO agents;

PRAGMA foreign_key_check;

COMMIT;

PRAGMA foreign_keys = ON;
