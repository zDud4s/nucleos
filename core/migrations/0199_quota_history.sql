-- Every change of a quota reading, so the analytics page can draw how the burn moved (spec
-- 2026-10-17-analytics-fundacao §7). `quota_readings` (0141) keeps only the last figure and says a
-- series would be "a new table with a retention rule, not this one quietly growing": this is it.
--
-- One row per provider window per CHANGE, never one per poll. `quota::record` writes it in the same
-- branch — and the same transaction — that replaces the `quota_readings` row, so a poll that
-- re-reads an identical figure writes neither. `recorded_at` is the provider's `read_at` (RFC3339
-- UTC), which keeps a test's history deterministic. `record` prunes rows older than 180 days, at
-- most once an hour; the prune scans by `recorded_at`, which a table this small affords.
--
-- The history starts when this migration lands. Nothing is reconstructed: if nobody opens the app
-- nothing polls `/quota`, there are no points, and that gap is the truth.
CREATE TABLE quota_history (
    id            INTEGER PRIMARY KEY,
    recorded_at   TEXT NOT NULL,
    provider      TEXT NOT NULL,
    -- "5h" or "7d", the vocabulary of `quota_readings.window_name`.
    window_name   TEXT NOT NULL,
    -- In [0,1], never a percentage: the same CHECK as 0141, for the same reason.
    used_fraction REAL NOT NULL CHECK (used_fraction >= 0 AND used_fraction <= 1),
    resets_at     TEXT,
    fidelity      TEXT NOT NULL CHECK (fidelity IN ('official', 'derived')),
    stale         INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1))
);

-- UNIQUE: concurrent polls, or a stale flip carrying the same `read_at`, must not write the same
-- point twice; `quota::record` upserts on this key.
CREATE UNIQUE INDEX quota_history_series ON quota_history (provider, window_name, recorded_at);

-- The analytics read (`analytics/sessions.rs`) takes finished runs by a `completed_at` range, for
-- every project at once on the global page; without this the read is a full scan of `runs` on
-- every page load.
CREATE INDEX IF NOT EXISTS runs_by_completed ON runs (completed_at);
