-- What a run was launched with, and what the local llm-router advised for it (`route_advice.rs`).
--
-- Every column is nullable and NULL means "not recorded", never "none": a run launched while routing
-- was off, a triage run (never routed), and every run older than this file all read the same way.
-- Written in ONE UPDATE by `route_advice::resolve`, so a row never holds half of a decision.

-- The model the run was EFFECTIVELY launched on: the runner's own configured model when the launch
-- named none, so the next retry can name it in `failed` rather than guess.
ALTER TABLE runs ADD COLUMN model TEXT;
-- The reasoning effort the run was launched with. NULL here is the CLI's own default.
ALTER TABLE runs ADD COLUMN effort TEXT;
-- Which agent CLI ran it: `claude` or `codex`.
ALTER TABLE runs ADD COLUMN runner TEXT;
-- The router mode this run was decided under: `shadow` (advice recorded, configured model launched)
-- or `apply` (advice launched when it passed the daemon's checks).
ALTER TABLE runs ADD COLUMN route_mode TEXT;
-- The router's `decision_id`, which an outcome is later reported against. NULL when the router gave
-- no usable answer (down, slow, 400, 422, or an answer outside the set the daemon sent).
ALTER TABLE runs ADD COLUMN route_decision_id TEXT;
-- The runner the router advised, whether or not it was launched.
ALTER TABLE runs ADD COLUMN advised_runner TEXT;
-- The model the router advised, whether or not it was launched.
ALTER TABLE runs ADD COLUMN advised_model TEXT;
-- The effort the router advised, as it answered, before the speed profile's ceiling was applied.
ALTER TABLE runs ADD COLUMN advised_effort TEXT;
-- A JSON array of `model[@effort]`: every attempt that had already failed this run's item when it was
-- routed, exactly as sent in the request's `failed`. The next retry extends it with this run's own.
ALTER TABLE runs ADD COLUMN route_failed TEXT;
