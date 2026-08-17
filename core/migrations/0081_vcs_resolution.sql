-- The two columns the conflict resolver needs, and they answer different questions.

-- **"Has anyone looked at this conflict?"** NULL means nobody has; a value is the record that it
-- had its one attempt, and which run made it.
--
-- One attempt, never a second: repeating is where an agent burns budget insisting on the same wall,
-- and whoever reads an escalation should find one attempt to read rather than seven. Without the
-- column at all, a tick every few seconds would launch a fresh agent per tick against the same
-- conflict — a failure that does not degrade but explodes, into a fleet of runs editing the same two
-- branches, discovered by the budget rather than by anything watching.
--
-- No foreign key, matching `action_grants.queued_request_id` and for its reason: `runs` rows are
-- pruned on their own schedule, and the record of having been attempted must not vanish because the
-- attempt aged out.
ALTER TABLE vcs_requests ADD COLUMN resolution_run_id INTEGER;

-- **"Did this request come OUT of a resolution?"** — and it is the loop brake, not bookkeeping.
--
-- A resolution run finishes by landing, which is a new request. A new request that conflicts is
-- eligible for resolution, which lands, which can conflict... one agent per turn, for ever. So a
-- request a resolution created is marked, and marked requests never spawn a resolution: they
-- escalate straight to a person.
--
-- It is also where the queue knows to verify before merging. A marked source branch must carry a
-- two-parent tip and no conflict markers, and neither is true of an ordinary landing, so the check
-- has to know which kind it is holding.
ALTER TABLE vcs_requests ADD COLUMN from_resolution INTEGER NOT NULL DEFAULT 0;
