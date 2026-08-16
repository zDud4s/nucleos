-- Which team run paid for this run, in the shape `runs.job_id` already established.
--
-- The teams design said no existing table needed changing, and for four of the five columns it was
-- right. This is the fifth. `team_runs.director_run_id` names the director node IN FLIGHT and is
-- overwritten by the next one, so the plan node, the replan nodes and the delivery node of a
-- finished run are reachable from nothing — and `teams.budget_usd` is a per-run ceiling that would
-- therefore be computed from the specialists alone. A spend ceiling that undercounts is wrong in
-- the expensive direction.
--
-- `job_id` is the precedent and the reason this is a column rather than a join table:
-- `budget::job_rows` filters `WHERE job_id = ?` and deliberately does not filter by mode, because
-- every run carrying the id was started by the thing that owns it. The same is true here.
ALTER TABLE runs ADD COLUMN team_run_id TEXT REFERENCES team_runs(id);

CREATE INDEX idx_runs_team_run_id ON runs(team_run_id) WHERE team_run_id IS NOT NULL;
