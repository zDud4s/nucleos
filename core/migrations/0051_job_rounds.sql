-- A job's queue is emptied in ROUNDS, so a night is not capped by what one planner could see.
--
-- The job-5 dogfood ran its whole queue in four minutes. The depth that is missing does not come
-- from more items per round -- `MAX_ITEMS_CEILING = 5` stays exactly where it is, and for exactly
-- the reason it was written -- it comes from asking again once the queue is empty.
--
-- Every column here defaults to the value a job of today already has, which is what makes this
-- migration silent: a job that never replans is a job of one round, `round = 0`, no dry rounds and
-- no ceiling of its own. Nothing needs backfilling and nothing changes behaviour until the code that
-- reads these columns arrives.

-- Which round the job is on, counting from 0 so that the plan node's items are round 0 and the first
-- replan opens round 1.
ALTER TABLE jobs ADD COLUMN round INTEGER NOT NULL DEFAULT 0;

-- How many consecutive rounds have added no new items.
--
-- Stored rather than derived, and that is the whole design of the "until it dries up" brake. Deriving
-- it means comparing this round's plan against the last one and deciding what "new" means, which is
-- a judgement about text; a counter is the same information with the judgement already made, once,
-- by the node that produced the plan.
--
-- Two dry rounds end the job `completed`. One is not evidence -- a replan can legitimately produce
-- nothing while the previous round's work is still settling -- and counting items by hand never
-- finds the tail, which is why this is a counter and not a target.
ALTER TABLE jobs ADD COLUMN dry_rounds INTEGER NOT NULL DEFAULT 0;

-- The job's own ceiling on rounds, and its own budget. Both NULL-able, and NULL is the information:
-- "only the daemon's ceiling governs" for one, "only the global budget governs" for the other. That
-- preserves exactly what the `graph:` rules do today, which is to say nothing.
--
-- `max_rounds` arrives in an HTTP body that a model may have filled in, so the number stored here is
-- never the number obeyed -- `MAX_ROUNDS_CEILING` cuts it in code, the same private-field-plus-
-- accessor shape `GraphConfig::max_items` uses, because a caller that reads the raw field honours
-- what the model asked for and the ceiling becomes decorative.
ALTER TABLE jobs ADD COLUMN max_rounds INTEGER;
ALTER TABLE jobs ADD COLUMN budget_usd REAL;

-- Whether the replan node that closed the current round said `{"done": true}`.
--
-- On the job and not derived from the run, because a run's status says whether the node finished,
-- never what it concluded. `next_step` is pure and the caller is meant to be dumb, so the verdict has
-- to be visible as state rather than re-read out of a transcript.
--
-- Reset to 0 when a round opens, for the same reason `resume_status` is cleared on the way out: a
-- flag that outlives the thing it describes is read as describing the next one.
ALTER TABLE jobs ADD COLUMN replan_done INTEGER NOT NULL DEFAULT 0;

-- Which round an item belongs to.
--
-- A label, and deliberately NOT a query key -- `load_view` reads the queue unfiltered. Filtering was
-- the obvious first design and it is wrong twice over. A round closes only when every item in it is
-- terminal, so an earlier round's items are all states `next_step` already walks past and the
-- unfiltered queue reads correctly on its own. And `ordinal` is half of this table's primary key:
-- restarting it per round would collide, while filtering would make the position in the loaded queue
-- stop being the ordinal in the table -- which is the number `advance` looks items up by.
--
-- What it IS for: the replan node reading what earlier rounds already tried, and a person reading
-- which round a given item came from.
ALTER TABLE job_items ADD COLUMN round INTEGER NOT NULL DEFAULT 0;
