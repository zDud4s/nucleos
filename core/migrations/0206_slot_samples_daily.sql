-- One row per closed UTC day and scope, rolled up from `slot_samples` (spec §10, owner decision
-- 2026-10-17). Kept 400 days. `project_id` '' is the house, so the key never holds a NULL.
-- `samples` counts the rows written for that scope that day: a project skips idle ticks, so its
-- share of time must be read against the house's `samples`, which never skips.
CREATE TABLE slot_samples_daily (
    day               TEXT    NOT NULL,
    project_id        TEXT    NOT NULL DEFAULT '',
    samples           INTEGER NOT NULL,
    avg_in_use        REAL    NOT NULL,
    max_in_use        INTEGER NOT NULL,
    slot_limit        INTEGER NOT NULL,   -- the highest limit seen that day
    at_limit_samples  INTEGER NOT NULL,   -- samples with in_use >= slot_limit and slot_limit > 0
    PRIMARY KEY (day, project_id)
);
