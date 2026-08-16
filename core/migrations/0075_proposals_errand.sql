-- Which errand a proposal came from, when it came from one.
--
-- Nullable, and every proposal written before this migration stays NULL: a job's skipped item and a
-- git push waiting for approval belong to a project, not to an errand, and inventing a value for
-- them would be a lie the shell would then render.
--
-- `project_id` beside it is not the same question and does not answer this one. An errand has no
-- project — that is the whole of what an errand is — so every errand proposal carries
-- `project_id IS NULL`, which is also what a machine-wide proposal carries. Without a column of its
-- own there is nothing in the row that says whose work this was.
ALTER TABLE proposals ADD COLUMN errand_id INTEGER;

-- The reading direction of the same fact: what is waiting on one errand. Partial, because the
-- column is NULL for almost every row in this table and an index over those would be paid for on
-- every write to buy nothing.
CREATE INDEX proposals_by_errand
    ON proposals (errand_id)
    WHERE errand_id IS NOT NULL;
