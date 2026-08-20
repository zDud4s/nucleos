-- An item of a job gets an identity of its own, so that something can belong to it.
--
-- `(job_id, ordinal)` was the primary key, and it is not a name a worktree can be keyed on: the
-- pair is stable, but everything that wanted to point AT an item pointed at `run_id` instead, and
-- `spawn_node` rewrites that column on every start. A tree named after a run is renamed on the
-- first retry, and the retry then cannot find the tree it was supposed to continue in.
--
-- SQLite cannot alter a primary key, so this is a rebuild-and-copy in the shape 0044 established.
-- Every column is named on both sides of the INSERT: five of these eleven arrived by ALTER TABLE
-- after the original CREATE, so the schema in `0042_jobs.sql` is not the schema on disk, and a
-- column left out of the SELECT is data deleted with no error anywhere.
--
-- `(job_id, ordinal)` survives as a UNIQUE INDEX, and that is not tidying: it was half the primary
-- key, `advance` looks items up by that number, and without the index a replan could write two
-- items with the same ordinal into one job.
--
-- Nothing REFERENCES job_items yet, so the DROP fires no cascade — which is exactly why
-- `runs.item_id` is added BELOW rather than in a migration of its own that could be renumbered
-- above this one. Inside one file the order cannot be broken.
CREATE TABLE job_items_new (
    id             INTEGER PRIMARY KEY,
    job_id         INTEGER NOT NULL REFERENCES jobs(id),
    ordinal        INTEGER NOT NULL,
    description    TEXT    NOT NULL,
    status         TEXT    NOT NULL,
    run_id         INTEGER,
    -- The gate is a subprocess, not a CLI invocation: it creates no run and consumes no quota, so
    -- its verdict belongs to the item it measured rather than to a row in `runs`.
    gate_status    TEXT,
    checkpoint_sha TEXT,
    round          INTEGER NOT NULL DEFAULT 0,
    files          TEXT,
    gate_attempts  INTEGER NOT NULL DEFAULT 0,
    gate_output    TEXT
);

-- `id` is deliberately absent from the column list: it is the rowid alias, so SQLite assigns
-- 1..N in the order the rows arrive. The ORDER BY is what makes that order meaningful — ids climb
-- with the queue, which is what a person reading this table at three in the morning expects.
INSERT INTO job_items_new
    (job_id, ordinal, description, status, run_id, gate_status,
     checkpoint_sha, round, files, gate_attempts, gate_output)
SELECT job_id, ordinal, description, status, run_id, gate_status,
       checkpoint_sha, round, files, gate_attempts, gate_output
FROM job_items
ORDER BY job_id, ordinal;

DROP TABLE job_items;
ALTER TABLE job_items_new RENAME TO job_items;

CREATE UNIQUE INDEX one_item_per_ordinal_per_job ON job_items (job_id, ordinal);

-- Which item a run is working on, and NULL is the information: "this run works in its job's tree,
-- or in its own, the way every run before this column did".
--
-- Not derivable from `job_items.run_id`, which is the direction that already exists: that column
-- says which run is working on an item NOW and is rewritten on every start, so it cannot answer
-- "which item did this finished run belong to" — and both successor paths need exactly that,
-- after the predecessor has stopped being the current one.
--
-- ADD COLUMN with a REFERENCES clause is legal here because the default is NULL; SQLite rejects it
-- only when an added foreign-key column carries a non-null default.
ALTER TABLE runs ADD COLUMN item_id INTEGER REFERENCES job_items(id);
