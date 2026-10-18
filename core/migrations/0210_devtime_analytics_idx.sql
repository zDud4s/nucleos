-- The project-scoped devtime analytics reads (spec analytics-devtime §6.5): find a project's sessions
-- in a window without scanning every session. Added because EXPLAIN QUERY PLAN showed a scan (see the
-- test `the_project_span_read_reaches_spans_through_an_index`). Not duplicated: spans and messages are
-- already indexed by session (0164), sessions by (dirty, rules_version) (0171).
CREATE INDEX IF NOT EXISTS idx_devtime_sessions_project ON devtime_sessions (project_id, started_at);
