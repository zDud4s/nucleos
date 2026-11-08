-- How many times this item's node run failed on a transient API error (no answer at all, 408, 429,
-- any 5xx, 529 "overloaded" among them) and the item was put back to `pending` for a fresh run, and
-- when the next of those runs may start. On the item for the reason 0273 gives for `interruptions`:
-- `job_nodes.run_id` is overwritten by every new run, and the count has to survive a restart so a
-- service that stays down cannot re-run an item forever. `transient_retry_at` (RFC 3339) is set when
-- the failure is first seen and cleared when the item goes back to `pending`; while it is in the
-- future the item stays `running` on its failed run, which is what holds the backoff.
ALTER TABLE job_nodes ADD COLUMN transient_retries INTEGER NOT NULL DEFAULT 0;
ALTER TABLE job_nodes ADD COLUMN transient_retry_at TEXT;
