-- The devtime overview's day reads (spec analytics-devtime §9, performance). The span read used to
-- join every span to a recursive list of the window's days, so SQLite paired each span with each
-- day: 20 s for 50,000 spans over 90 days in the debug build, 29 s on a real database of 210,000.
-- These indexes let `span_day_rows` and `message_day_rows` (devtime/store/reads.rs) read rows
-- already in GROUP BY order, so they are summed without a sort.
--
-- Spans that start and end on one UTC day — all but a few hundred — keyed by that day and by the
-- columns the page breaks time down by; `substr(.., 1, 10)` is the day of the one stamp format
-- written. Covering (hence `lane` after its class), so the read never visits the table.
CREATE INDEX IF NOT EXISTS idx_devtime_spans_day ON devtime_spans (
    substr(started_at, 1, 10), kind, waste, rule_id, lever, confidence,
    (CASE WHEN lane = 'main' THEN 'main' ELSE 'other' END), started_at, ended_at, session_id, lane
) WHERE substr(started_at, 1, 10) = substr(ended_at, 1, 10);

-- The rest, which the read cuts at each midnight: a partial index, so finding them is not a scan.
CREATE INDEX IF NOT EXISTS idx_devtime_spans_multiday ON devtime_spans (started_at)
    WHERE substr(started_at, 1, 10) <> substr(ended_at, 1, 10);

-- Messages by the day of `first_at`, model and effort, covering the token columns summed.
CREATE INDEX IF NOT EXISTS idx_devtime_messages_day ON devtime_messages (
    substr(first_at, 1, 10), model, effort, first_at, session_id, input_tokens, cache_read_tokens,
    cache_creation_tokens, output_tokens
);
