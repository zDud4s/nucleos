-- What a message was billed as, beyond its four token counts (spec devtime §4.4): a request is
-- priced by its prompt-length tier, its `speed` and its `inference_geo`, and its cache writes by
-- their duration. Transcripts carry all of it in `message.usage`; the parser read only the counts.
--
-- `cache_creation_1h_tokens`: the part of `cache_creation_tokens` written with the 1-hour TTL
-- (`usage.cache_creation.ephemeral_1h_input_tokens`); `cache_creation_tokens` stays the total, so the
-- 5-minute part is the difference. A 1-hour write costs 1.6x a 5-minute one, and Claude Code writes
-- 1-hour cache, so pricing every write at the 5-minute rate understated that line.
-- `speed`: `standard` or `fast`. `inference_geo`: where the request ran (`us` costs more).
--
-- All three NULL means unknown: every row ingested before this migration, which nothing re-reads.
-- The cost read prices an unknown cache split as a range rather than guessing one side silently.
ALTER TABLE devtime_messages ADD COLUMN cache_creation_1h_tokens INTEGER;
ALTER TABLE devtime_messages ADD COLUMN speed TEXT;
ALTER TABLE devtime_messages ADD COLUMN inference_geo TEXT;

-- 0211's day index, rebuilt with the billing mode after effort and the 1-hour count among the
-- covered columns: the day read groups by (day, model, effort, speed, inference_geo) and still
-- takes its rows in that order straight from the index, without a sort and without the table.
DROP INDEX IF EXISTS idx_devtime_messages_day;
CREATE INDEX idx_devtime_messages_day ON devtime_messages (
    substr(first_at, 1, 10), model, effort, speed, inference_geo, first_at, session_id,
    input_tokens, cache_read_tokens, cache_creation_tokens, cache_creation_1h_tokens, output_tokens
);
