-- Where a detector remembers what it already said.
--
-- The four efficiency signals are evaluated at the end of every run, which is far more often than
-- anyone wants to be told about them. A single bad run is noise: caches go cold on the first request
-- of a session by design, a context spike can be one large file legitimately read. What deserves a
-- word is a signal that HOLDS. So the write is gated on two things this table exists to remember —
-- how many consecutive evaluations the signal survived, and when it last spoke.
--
-- This matters beyond tidiness because a feed row IS a notification: the Telegram sidecar forwards
-- every row it finds. Suppressing the alert and suppressing the row are the same act, and the state
-- that decides it cannot live in memory — the daemon restarts, and a streak that resets on restart
-- would let a permanent regression stay permanently below the threshold.
--
-- One row per (detector, scope) pair, created on first evaluation and updated in place. This is
-- state, not a log; the log is the feed, and it only receives what survived the gate.
CREATE TABLE IF NOT EXISTS efficiency_signals (
  -- Which detector this row belongs to: `cache_cold`, `context_swelling`, `output_starved`,
  -- `cost_drift`. Stored as text rather than an integer because the feed reads it back and a number
  -- there would need a second table to mean anything.
  signal            TEXT NOT NULL,
  -- `global|<mode>`, or `project:<id>|<mode>`. Two things packed into one key because the streak has
  -- to be counted over exactly the population its baseline is drawn from, and that baseline
  -- partitions by mode as well as project. Project alone would let a signal true of one project
  -- silence it elsewhere; project alone would ALSO make the streak unreachable, since a job's three
  -- nodes are all `worktree` and one interleaved `shadow` run would reset the count every time.
  scope             TEXT NOT NULL,
  -- Consecutive evaluations where the signal held. Reset to zero the moment it does not, which is
  -- what makes a streak mean "still true" rather than "was true this many times ever".
  streak            INTEGER NOT NULL DEFAULT 0,
  -- NULL until this signal has ever alerted. What the silence window is measured from; a NULL here
  -- means the window cannot have elapsed because it never started.
  last_alerted_at   TEXT,
  last_evaluated_at TEXT NOT NULL,
  PRIMARY KEY (signal, scope)
);

-- The baseline reads finished runs of one mode, newest first, and abandons the walk after N.
--
-- Without this it is a scan of the whole runs table at the end of every run — the one moment where
-- added latency is paid by the user waiting for their answer. Partial on `completed_at IS NOT NULL`
-- because a run still in flight has no total to compare against and only makes the index bigger.
--
-- The abandoning is what the index buys, and it only survives if the query's LIMIT applies BEFORE
-- any grouping: ordering by an aggregate would force the whole window to be materialised and sorted
-- first. See `SELECT_BASELINE_TOTALS`, which limits inside a subquery for exactly this reason.
CREATE INDEX runs_by_mode_completed ON runs (mode, completed_at) WHERE completed_at IS NOT NULL;
