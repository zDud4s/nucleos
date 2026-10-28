-- Metric 9's read (spec analytics-devtime §5.1, efficiency by turn class): the stored per-turn stats
-- whose start falls in the window (`turn_stat_window_rows` in devtime/store/turn_stat_window.rs).
-- The only secondary index on `devtime_turn_stats`, 0171's (project_id, turn_class, started_at), cannot serve
-- the global form of that read, which would scan the whole table.
--
-- Keyed by `started_at` for the window's half-open range, then `project_id` for the project form.
CREATE INDEX IF NOT EXISTS idx_devtime_turn_stats_day ON devtime_turn_stats (started_at, project_id);
