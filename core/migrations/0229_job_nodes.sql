-- no-transaction
-- `job_items` becomes `job_nodes`: one row per node execution of a job, not only per fan-out item.
--
-- `job_nodes` is the parent of `runs.item_id` and `run_knowledge.item_id`, and project_slots and
-- worktrees hold its `id` by value, so this needs `-- no-transaction` and `PRAGMA foreign_keys`,
-- as `0154` does for `runs`. The rename comes FIRST: SQLite rewrites the REFERENCES clauses of the
-- other tables to the new name (this repo never sets legacy_alter_table), so `runs.item_id` and
-- `run_knowledge.item_id` name `job_nodes` afterwards. The rebuild below exists only to make
-- `ordinal` nullable and to add `node_id`; every column is named on both sides, `id` included,
-- because other tables hold it (0095 could renumber, this one must not).
--
-- The only index the chain ever put on `job_items` was `one_item_per_ordinal_per_job` (0095);
-- it cannot survive, since two fan-outs of one job each count their items from 0.
PRAGMA foreign_keys = OFF;

BEGIN;

ALTER TABLE job_items RENAME TO job_nodes;

CREATE TABLE job_nodes_new (
    id             INTEGER PRIMARY KEY,
    job_id         INTEGER NOT NULL REFERENCES jobs(id),
    node_id        TEXT    NOT NULL,
    ordinal        INTEGER,
    description    TEXT    NOT NULL,
    status         TEXT    NOT NULL,
    run_id         INTEGER,
    gate_status    TEXT,
    checkpoint_sha TEXT,
    round          INTEGER NOT NULL DEFAULT 0,
    files          TEXT,
    gate_attempts  INTEGER NOT NULL DEFAULT 0,
    gate_output    TEXT,
    merge_base_sha TEXT,
    depends_on     TEXT,
    agent_id       TEXT REFERENCES agents(id)
);

INSERT INTO job_nodes_new
    (id, job_id, node_id, ordinal, description, status, run_id, gate_status, checkpoint_sha, round,
     files, gate_attempts, gate_output, merge_base_sha, depends_on, agent_id)
SELECT id, job_id, 'implement', ordinal, description, status, run_id, gate_status, checkpoint_sha, round,
       files, gate_attempts, gate_output, merge_base_sha, depends_on, agent_id
FROM job_nodes
ORDER BY id;

DROP TABLE job_nodes;
ALTER TABLE job_nodes_new RENAME TO job_nodes;

-- NULL ordinal = a node outside a fan-out; otherwise the item index within that fan-out.
CREATE UNIQUE INDEX one_item_per_ordinal_per_node
    ON job_nodes (job_id, node_id, ordinal) WHERE ordinal IS NOT NULL;
CREATE UNIQUE INDEX one_singleton_per_node_per_round
    ON job_nodes (job_id, node_id, round) WHERE ordinal IS NULL;
CREATE INDEX job_nodes_by_job ON job_nodes (job_id, ordinal);

PRAGMA foreign_key_check;

COMMIT;

PRAGMA foreign_keys = ON;
