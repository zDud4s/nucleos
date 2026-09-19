-- Which quota warning has already been said, so it is said once (design D11).
--
-- The notch polls, so `quota::warn` runs against the same reading over and over for as long as the
-- window stays over the line. A feed row IS a notification — the Telegram sidecar forwards every
-- row it finds — so "write a line when the window is above 80%" would ping the owner's phone every
-- minute of the afternoon. What deserves one word is the CROSSING, and this table is what
-- remembers that it happened.
--
-- The same shape, and for the same reason, as `efficiency_signals` (0067): state, not a log. The
-- log is the feed, and it only receives what survived the gate. In memory it would not survive a
-- daemon restart, and a restart during a burned window would warn a second time about the same
-- crossing.
--
-- One row per provider and window, replaced in place.
CREATE TABLE quota_warnings (
    provider         TEXT NOT NULL,
    -- "5h" or "7d", the same vocabulary as `quota_readings`.
    window_name      TEXT NOT NULL,
    -- WHICH window instance this claim belongs to: the reset instant the reading carried when the
    -- warning went out, RFC3339.
    --
    -- This column is what re-arms the thresholds. A claim is not "claude's 5h window has warned",
    -- it is "the 5h window that ends at 16:40 has warned" — so when a reading arrives with a later
    -- reset, it is a different window and it gets its own word. Without it, the first burn past
    -- 80% would be the last warning this machine ever gave.
    --
    -- NULL is a real answer: the capture of 2026-09-19 carried a populated window with no reset at
    -- all. A window with no reset instant can never be told apart from its successor, so its claim
    -- simply holds — which errs towards silence rather than towards a repeated ping.
    window_resets_at TEXT,
    -- The highest threshold already announced for this window instance, in per cent. Percent and
    -- not a fraction because the thresholds are the owner's numbers (`warn_at_percent`, D9) and
    -- they are written as 80 and 100 everywhere a human reads them.
    alerted_percent  INTEGER NOT NULL CHECK (alerted_percent > 0 AND alerted_percent <= 100),
    -- When it was announced, RFC3339. Not read by the gate — the claim is decided by the threshold
    -- and the window instance, not by elapsed time — and kept because the first question about an
    -- unexpected ping is when the last one went out.
    last_alerted_at  TEXT NOT NULL,
    PRIMARY KEY (provider, window_name)
);
