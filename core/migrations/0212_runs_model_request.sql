-- Whether a run ever sent a model request, so that $0 is only ever a MEASURED zero (spec §8.5,
-- decision of 2026-07-20: an unmeasured cost is never free). A run with no reported cost is priced
-- by a time estimate; these two stamps let `unmeasured_usd` (governance/budget.rs) price an armed
-- run that never reached the model at nothing.
--
-- `model_watch`: when the watcher was armed. NULL means unknown, and the run keeps the estimate.
-- `model_requested_at`: the first point a model request may have been in flight.
ALTER TABLE runs ADD COLUMN model_watch TEXT;
ALTER TABLE runs ADD COLUMN model_requested_at TEXT;
