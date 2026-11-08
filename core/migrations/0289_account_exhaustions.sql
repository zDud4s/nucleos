-- Exhaustion marks of the account pool, so a daemon restart neither forgets that an account hit its
-- usage limit nor repeats the limit notice. One row per account and window, like
-- `PoolState::mark_exhausted` (an unnamed window is ''). `resets_at` is the provider's reset time,
-- NULL when it gave none; `until` is when the mark and its notice stop holding (`resets_at`, else
-- the notice fallback). `announced_at` is when the notice was written. Times are RFC 3339 UTC texts
-- in whole seconds, bound from Rust, so `until` compares as text. No CHECK constraints (the rule of
-- 0143).
CREATE TABLE account_exhaustions (
    account_id   TEXT NOT NULL,
    window_name  TEXT NOT NULL,
    resets_at    TEXT,
    until        TEXT NOT NULL,
    announced_at TEXT NOT NULL,
    PRIMARY KEY (account_id, window_name)
);
