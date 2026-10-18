-- Slot occupancy over time (spec 2026-10-17-analytics-pipeline §5.3, §10).
--
-- `project_slots` deletes its row on release, so it can say "in use now" and never "were we at the
-- limit". The sampler in `analytics_pipeline::sampler` writes one row per tick for the house
-- (`project_id` NULL, written every tick: it is the clock) and one per project that holds or is
-- configured for slots, skipping a project that stays idle. Raw rows are kept 8 days; the daily
-- rollup keeps the history.
CREATE TABLE slot_samples (
    sampled_at  TEXT    NOT NULL,   -- RFC 3339 UTC, the same for every row of one tick
    project_id  TEXT,               -- NULL = the house (house_limit)
    in_use      INTEGER NOT NULL,
    slot_limit  INTEGER NOT NULL
);
CREATE INDEX slot_samples_at ON slot_samples (sampled_at);
CREATE INDEX slot_samples_project_at ON slot_samples (project_id, sampled_at);
