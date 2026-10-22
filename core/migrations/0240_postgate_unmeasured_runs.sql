-- A post-merge gate whose ticket ended without a verdict (a unit errored or timed out) measured
-- nothing, yet `last_attempted_sha` kept the tip, so the same tip was never gated again and a push
-- parked on it waited for the next merge.
-- `unmeasured_runs` counts the gates of `last_attempted_sha` that ended that way. A start for a new
-- tip resets it; the worker runs the same tip once more while it is within the retry budget
-- (`verify_postgate::UNMEASURED_RETRIES`). No CHECK constraints, as in 0180, 0182, 0184 and 0186.
ALTER TABLE postgate_state ADD COLUMN unmeasured_runs INTEGER NOT NULL DEFAULT 0;
