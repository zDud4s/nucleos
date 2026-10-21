-- Routing state of the telegram channel, written at the switch so the first routing pass of the
-- phase-2 build neither re-announces history nor drops the turns the v1 sidecar had not answered.
--
-- Mirrors `routing::seed_at` (cursors and last states = current values), with one difference:
-- `turn_watermark` is the oldest open Telegram assistant turn minus one, so the replies discovery
-- finds the turns that were open at the switch. Without an open turn it is MAX(runs.id) and nothing
-- is re-sent. The migrations run before `reconcile_orphaned_runs`, so a turn that was 'running'
-- when the daemon stopped is still open here.
--
-- `INSERT OR IGNORE`: `seed_at` is also `INSERT OR IGNORE`, so this row wins over the start-up seed,
-- and a second application changes nothing. A database that never held a Telegram conversation
-- (no prefixed run, queued message or chat token, after 0234) gets no row: the normal seed handles it.
-- `chat_id >= 'telegram:' AND chat_id < 'telegram;'` is the `telegram:` prefix range (`;` follows
-- `:`), the same predicate `replies::discover_channel` uses.
INSERT OR IGNORE INTO channel_routing_state
    (channel, feed_cursor, proposal_cursor, last_kill, last_budget_paused, turn_watermark, seeded_at)
SELECT 'telegram',
       COALESCE((SELECT MAX(id) FROM feed), 0),
       COALESCE((SELECT MAX(id) FROM proposals), 0),
       COALESCE((SELECT kill_switch <> 0 FROM autopilot_global LIMIT 1), 0),
       0,
       COALESCE(
           (SELECT MIN(id) - 1 FROM runs
             WHERE chat_id >= 'telegram:' AND chat_id < 'telegram;'
               AND mode = 'assistant' AND COALESCE(origin, '') <> 'task'
               AND status IN ('running', 'pending')),
           (SELECT COALESCE(MAX(id), 0) FROM runs)),
       strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')
WHERE EXISTS (SELECT 1 FROM runs
               WHERE chat_id >= 'telegram:' AND chat_id < 'telegram;')
   OR EXISTS (SELECT 1 FROM chat_queue
               WHERE chat_id >= 'telegram:' AND chat_id < 'telegram;')
   OR EXISTS (SELECT 1 FROM chat_tokens
               WHERE chat_id >= 'telegram:' AND chat_id < 'telegram;');
