-- no-transaction
-- `runs` keeps its large text columns last.
--
-- SQLite stores a value too big for its page in a chain of overflow pages, and to read any column
-- declared AFTER that value it has to walk the whole chain. `stdout` was the seventh column of 56
-- -- `0002` created it there and every column since arrived by `ALTER TABLE ADD COLUMN`, which can
-- only append -- so `mode`, `created_at`, `cost_usd`, `job_id`, `gate_status` and the rest all sat
-- behind every run's full output. A query that only wanted `mode` read the transcript to get it.
-- Measured on the owner's 229 MB database (54 MB of it `stdout`) before and after this rebuild:
-- `budget::autonomous_rows` 118 ms -> 2 ms, the roster's last gate verdict 51 ms -> 0.3 ms, the
-- `/runs` list 152 ms -> 0.8 ms, a job tick's `job_id`/`stage` lookup 47 ms -> 0.2 ms. The idle
-- daemon was reading 40-80 MB/s off disk doing that, several times every three seconds.
--
-- Columns move; nothing else may. The list below is the table AS OF 0153, read off a database
-- built by running the whole chain, not off `0002` -- see `0123`'s warning about rebuilding from
-- the first `CREATE TABLE` you find. Every name, type, default, CHECK and REFERENCES is as it was,
-- the five indexes are recreated verbatim, and nothing in the code depends on column position
-- (there is no `SELECT *` from `runs` and no `INSERT INTO runs` without a column list).
-- `storage::tests::migration_0154_moves_the_large_runs_columns_last_and_keeps_everything_else`
-- holds all of that, row for row.
--
-- Unlike `0123` this DOES need `-- no-transaction` and `PRAGMA foreign_keys`: `runs` is the
-- parent of `team_runs.director_run_id`, `team_items.run_id`, `run_knowledge.run_id` and its own
-- `successor_run_id`. With foreign keys on, `DROP TABLE runs` deletes every row first and those
-- children refuse it. This is SQLite's own procedure for a referenced table (lang_altertable.html,
-- "Making Other Kinds Of Table Schema Changes"): keys off, which only takes effect outside a
-- transaction, then the rebuild inside one, then keys back on. The children name `runs` by text,
-- so once `runs_new` takes the name they point at it again. The rows are copied unchanged, so
-- every reference that held before still holds; the test runs `foreign_key_check` to prove it.
--
-- The AUTOINCREMENT high-water mark is copied across before the old table goes: `DROP TABLE`
-- deletes its `sqlite_sequence` row, and re-deriving it from the surviving rows would hand out
-- again an id that a pruned run once had.
PRAGMA foreign_keys = OFF;

BEGIN;

CREATE TABLE runs_new (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id            TEXT,
    cwd                   TEXT,
    -- status is free-form TEXT (no CHECK): running -> completed | failed | timed_out | cancelled
    -- | awaiting_approval (§8.4 active-termination) | interrupted (§3.2 startup reconciliation).
    status                TEXT NOT NULL,
    exit_code             INTEGER,
    session_id            TEXT,
    cost_usd              REAL,
    created_at            TEXT NOT NULL,
    completed_at          TEXT,
    mode                  TEXT NOT NULL DEFAULT 'real',
    attempt               INTEGER NOT NULL DEFAULT 1,
    token                 TEXT,
    denials               INTEGER NOT NULL DEFAULT 0,
    read_untrusted        INTEGER NOT NULL DEFAULT 0,
    input_tokens          INTEGER,
    output_tokens         INTEGER,
    cache_read_tokens     INTEGER,
    num_turns             INTEGER,
    gate_status           TEXT,
    gate_exit_code        INTEGER,
    steerable             INTEGER NOT NULL DEFAULT 0,
    context_fill          INTEGER,
    successor_run_id      INTEGER REFERENCES runs(id),
    job_id                INTEGER REFERENCES jobs(id),
    stage                 TEXT,
    chat_id               TEXT,
    answered_by           TEXT,
    cache_creation_tokens INTEGER,
    team_run_id           TEXT REFERENCES team_runs(id),
    thought_tokens        INTEGER,
    item_id               INTEGER REFERENCES job_items(id),
    compacted             INTEGER NOT NULL DEFAULT 0,
    context_peak          INTEGER,
    origin                TEXT,
    from_relay_id         INTEGER,
    permission_mode       TEXT,
    authored_prompt_chars INTEGER,
    judge                 TEXT NOT NULL DEFAULT 'off'
        CHECK (judge IN ('off', 'observe', 'enforce')),
    model                 TEXT,
    effort                TEXT,
    runner                TEXT,
    route_mode            TEXT,
    route_decision_id     TEXT,
    advised_runner        TEXT,
    advised_model         TEXT,
    advised_effort        TEXT,
    route_failed          TEXT,
    judge_resolve         TEXT NOT NULL DEFAULT 'off'
        CHECK (judge_resolve IN ('off', 'observe', 'enforce')),
    lineage_root_id       INTEGER,
    -- Large, and last: smallest first, so reading `prompt` never walks `stdout`'s chain.
    tools_used            TEXT,
    prompt_images         TEXT,
    thought               TEXT,
    prompt                TEXT NOT NULL,
    gate_output           TEXT,
    stderr                TEXT,
    stdout                TEXT
);

INSERT INTO runs_new (
    id, project_id, cwd, status, exit_code, session_id, cost_usd, created_at, completed_at, mode,
    attempt, token, denials, read_untrusted, input_tokens, output_tokens, cache_read_tokens,
    num_turns, gate_status, gate_exit_code, steerable, context_fill, successor_run_id, job_id,
    stage, chat_id, answered_by, cache_creation_tokens, team_run_id, thought_tokens, item_id,
    compacted, context_peak, origin, from_relay_id, permission_mode, authored_prompt_chars, judge,
    model, effort, runner, route_mode, route_decision_id, advised_runner, advised_model,
    advised_effort, route_failed, judge_resolve, lineage_root_id,
    tools_used, prompt_images, thought, prompt, gate_output, stderr, stdout
)
SELECT
    id, project_id, cwd, status, exit_code, session_id, cost_usd, created_at, completed_at, mode,
    attempt, token, denials, read_untrusted, input_tokens, output_tokens, cache_read_tokens,
    num_turns, gate_status, gate_exit_code, steerable, context_fill, successor_run_id, job_id,
    stage, chat_id, answered_by, cache_creation_tokens, team_run_id, thought_tokens, item_id,
    compacted, context_peak, origin, from_relay_id, permission_mode, authored_prompt_chars, judge,
    model, effort, runner, route_mode, route_decision_id, advised_runner, advised_model,
    advised_effort, route_failed, judge_resolve, lineage_root_id,
    tools_used, prompt_images, thought, prompt, gate_output, stderr, stdout
FROM runs;

UPDATE sqlite_sequence
   SET seq = (SELECT seq FROM sqlite_sequence WHERE name = 'runs')
 WHERE name = 'runs_new'
   AND EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'runs');

DROP TABLE runs;
ALTER TABLE runs_new RENAME TO runs;

CREATE INDEX runs_assistant_by_chat ON runs (chat_id, id) WHERE chat_id IS NOT NULL;
CREATE INDEX runs_by_mode_completed ON runs (mode, completed_at) WHERE completed_at IS NOT NULL;
CREATE INDEX idx_runs_team_run_id ON runs(team_run_id) WHERE team_run_id IS NOT NULL;
CREATE INDEX idx_runs_from_relay ON runs(from_relay_id);
CREATE INDEX runs_by_lineage_root ON runs (lineage_root_id) WHERE lineage_root_id IS NOT NULL;

COMMIT;

PRAGMA foreign_keys = ON;
