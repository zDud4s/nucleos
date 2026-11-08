-- Retention for `shadow_decisions` (review finding scale#0): the table was written on every
-- governed tool call and never pruned, so it grew for as long as the daemon was ever used.
--
-- The owner's decision: raw rows older than `shadow::rollup::RAW_RETENTION_DAYS` are deleted, and
-- before they go their counts are folded into the two tables below, which are kept forever. The
-- fold and the delete happen in ONE transaction over the same rows (`shadow::rollup::prune`), so a
-- row is counted in a rollup exactly when it stops being counted raw: re-running a day finds no row
-- left to fold and adds nothing.
--
-- Rows still waiting on a person are never pruned, whatever their age: an unreviewed decision of a
-- `shadow`-mode run (the review queue and the WIP brake count it), and a decision an open judge or
-- resolver review item points at (those queues join the row for its `tool_input`). Such a row is
-- folded on the sweep after it leaves the queue, into the day it was created, which is why every
-- counter below is ADDED to on conflict rather than replaced.

-- One row per (day, project, run mode, action class, policy digest): every additive count
-- `shadow::scoreboard`, `shadow::shadow_readiness` and the analytics classifier block read.
--
-- `day` is the UTC date of `created_at` (`YYYY-MM-DD`). `project_id` and `mode` are copied from
-- the run at fold time, '' when the run row no longer exists (the analytics block still counts
-- such a decision globally, as its LEFT JOIN does). `policy_digest` is '' for a NULL digest: a key
-- column, and SQLite treats NULLs as distinct in a key, which would defeat the upsert.
-- `oldest_unreviewed_at` combines by MIN, every other counter by sum.
CREATE TABLE IF NOT EXISTS shadow_decision_days (
    day TEXT NOT NULL,
    project_id TEXT NOT NULL,
    mode TEXT NOT NULL,
    action_class TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    total INTEGER NOT NULL DEFAULT 0,
    would_allow INTEGER NOT NULL DEFAULT 0,
    would_pend INTEGER NOT NULL DEFAULT 0,
    would_deny INTEGER NOT NULL DEFAULT 0,
    reviewed INTEGER NOT NULL DEFAULT 0,
    agree INTEGER NOT NULL DEFAULT 0,
    disagree INTEGER NOT NULL DEFAULT 0,
    stricter INTEGER NOT NULL DEFAULT 0,
    looser INTEGER NOT NULL DEFAULT 0,
    unreviewed INTEGER NOT NULL DEFAULT 0,
    oldest_unreviewed_at TEXT,
    reviewed_allow INTEGER NOT NULL DEFAULT 0,
    reviewed_pend INTEGER NOT NULL DEFAULT 0,
    reviewed_deny INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, project_id, mode, action_class, policy_digest)
);

-- The scoreboard and readiness read the whole history of a project, not a window of days.
CREATE INDEX IF NOT EXISTS shadow_decision_days_by_project
    ON shadow_decision_days (project_id, mode, action_class);

-- The part of readiness a daily count cannot hold: it counts DISTINCT reviewed actions
-- (a SHA-256 of `tool_name || 0x1f || tool_input`), and a distinct count over two days is not the sum of
-- the two. So every reviewed action of a `shadow`-mode run that is pruned leaves its HASH here (never the raw tool_input, which must not outlive the retention window), once
-- per (project, class), with whether a person EVER disagreed about it (combined by MAX, the same
-- "an action anyone disagreed about never counts as agreement" rule `shadow_readiness` applies).
-- Bounded by how many distinct actions people have reviewed, not by how many tool calls ran.
CREATE TABLE IF NOT EXISTS shadow_reviewed_actions (
    project_id TEXT NOT NULL,
    action_class TEXT NOT NULL,
    action_hash TEXT NOT NULL,
    disagreed INTEGER NOT NULL DEFAULT 0 CHECK (disagreed IN (0, 1)),
    PRIMARY KEY (project_id, action_class, action_hash)
);
