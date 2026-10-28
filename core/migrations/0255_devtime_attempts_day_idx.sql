-- Metric 7's read (spec analytics-devtime §5.1, tool failures): the window's tool attempts grouped
-- by tool, program, outcome and failure class (`attempt_failure_rows` in
-- devtime/store/attempt_failures.rs). Only (session_id, started_at) and the ingest lookups of 0181
-- index `devtime_attempts`, so the global form of that read would scan the whole table.
--
-- Keyed by `started_at` for the window's half-open range, then every column the read groups by or
-- derives its class from, and `session_id` for the join to `devtime_sessions`. Covering, so the
-- read never visits the table.
CREATE INDEX IF NOT EXISTS idx_devtime_attempts_day ON devtime_attempts (
    started_at, tool_name, cmd_program, outcome, error_class, bg_status, session_id
);
