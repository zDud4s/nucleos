-- The last quota reading of each provider window, so the notch can draw before the first poll.
--
-- Written by `quota::record` after every successful read of the quota sidecar, and read by
-- `quota::stored` when that sidecar is down. That fallback is the whole reason the table exists:
-- without it, a daemon that has just restarted — or a sidecar that is still starting — draws an
-- empty notch, which is indistinguishable from an untouched quota. Showing the last known figure
-- with the instant it was taken is the honest version of "I do not know yet".
--
-- One row per provider and window, replaced in place. No history: this design has no question that
-- a series answers, and a row per minute per window is ninety thousand rows a month to support a
-- graph nobody asked for. If a burn-rate graph is ever wanted, it is a new table with a retention
-- rule, not this one quietly growing.
CREATE TABLE quota_readings (
    provider      TEXT NOT NULL,
    -- "5h" or "7d" — this design's vocabulary, not the vendor's `five_hour`/`seven_day`. The
    -- sidecar translates at its own edge so a vendor rename never reaches the database.
    --
    -- Deliberately NOT called `window`: that is a reserved word in SQLite's window-function syntax,
    -- and a column by that name works until the first query that needs to quote it.
    window_name   TEXT NOT NULL,
    -- In [0,1]. Never a percentage, and the CHECK is here because this column is the one place the
    -- two providers' different units meet: the Anthropic endpoint reports `utilization` as a
    -- percentage in a field named like a fraction, and reading it as one drew every ring full
    -- (design §1.2.1). The sidecar converts, and this refuses to store the mistake if it ever
    -- stops.
    used_fraction REAL NOT NULL CHECK (used_fraction >= 0 AND used_fraction <= 1),
    -- When this window rolls over, RFC3339. NULL is a real answer and not a defect: the capture of
    -- 2026-09-19 carried a populated window with no reset at all.
    resets_at     TEXT,
    -- `official` or `derived`. `unmeasured` never lands here — a reading nobody measured has no
    -- number, so it has no row, and a brake that reads this table therefore cannot find one to act
    -- on (design D3).
    fidelity      TEXT NOT NULL CHECK (fidelity IN ('official', 'derived')),
    -- 1 when `resets_at` had already passed when the reading was taken: the window has since rolled
    -- over and the figure describes a period that is gone.
    stale         INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
    -- When the sidecar took the reading, RFC3339. The age of the figure is the difference between
    -- this and now, and it is shown rather than hidden — a `derived` reading only moves when the
    -- owner runs something, so a four-hour-old one is normal and must still be legible as old.
    read_at       TEXT NOT NULL,
    PRIMARY KEY (provider, window_name)
);
