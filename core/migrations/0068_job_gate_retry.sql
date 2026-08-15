-- A red gate spends a retry before it spends the item.
--
-- The gate is the only thing in a job that says no, and today the first time it says no about an
-- item, that item is over. That is the right answer for work that is wrong and the wrong answer for
-- work that is nearly right -- a missing import, a test the node did not know to update -- which is
-- most of what a red gate actually reports. One more implement run, told what the gate said, is
-- cheaper than the item it saves.
--
-- Every column here defaults to the value a job of today already has, which is what makes this
-- migration silent: no job retries anything, no item has ever had a gate go red twice, and nothing
-- reads the output until the code that spends a retry arrives. Nothing needs backfilling.

-- How many EXTRA implement runs this job allows per item after a red gate.
--
-- 0 -- the default, and therefore every row already in this table -- is exactly today's behaviour:
-- the first red gate ends the item. Deliberately NOT defaulted to 1 here, even though
-- `GraphConfig::gate_retries()` defaults new jobs to 1: a default on the column would hand a retry
-- to every job already scheduled, silently, and a retry costs a whole run. The rule file asks; the
-- column only remembers what was asked.
--
-- Per JOB and not per item, because it is a budget and budgets are set by whoever pays for the
-- night. The number arrives from `.ai/autopilot.yaml`, which is gitignored per-developer
-- configuration no review ever sees, so it is cut by `MAX_GATE_RETRIES_CEILING` in code through the
-- same private-field-plus-accessor shape `GraphConfig::max_items` uses -- a caller that read the
-- raw field would honour whatever the file said and leave the ceiling decorative.
ALTER TABLE jobs ADD COLUMN gate_retries INTEGER NOT NULL DEFAULT 0;

-- How many times THIS item's gate has gone red.
--
-- Counted rather than derived, and the alternatives are all worse in the same way. `gate_status`
-- holds the last verdict and not how many there were; the runs table knows how many implement nodes
-- an item owns but not which of them a gate rejected, because a node can be re-run for reasons that
-- have nothing to do with the gate -- an approval supersedes one and starts another. A counter is
-- the same question with the judgement already made, once, by the code that recorded the verdict.
--
-- 0 is "no gate has rejected this item", which is true of every existing row and of every item that
-- passed. Read together with `jobs.gate_retries` and never alone: `item_state_from` maps
-- `gate_failed` to a retriable item while `gate_attempts <= gate_retries` and to a terminal one
-- after, so neither number means anything on its own.
ALTER TABLE job_items ADD COLUMN gate_attempts INTEGER NOT NULL DEFAULT 0;

-- What the gate printed when it last rejected this item.
--
-- NULLABLE, and the nullability is the information: NULL means no gate has rejected this item, which
-- covers every existing row, every passing item, and every job that never retries anything. The
-- retry prompt is built from `Some(tail)` and an item with nothing here gets the brief it has always
-- been given, word for word.
--
-- Stored on the item rather than left in the run's transcript because the node that reads it is a
-- different node, started later, and §5.4 of the design keeps nodes from resuming each other's
-- sessions on purpose. A retry with no account of what failed is a second guess at the same problem,
-- which is a whole run spent to learn what the gate already said.
ALTER TABLE job_items ADD COLUMN gate_output TEXT;
