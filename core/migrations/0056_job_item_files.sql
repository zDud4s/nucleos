-- Where the plan node thinks an item lives, carried to the implement node that does it. Nothing of
-- the plan is still in memory by then -- the two are different processes, minutes or hours apart --
-- so the hint travels through the row or not at all. A JSON array of paths, read back best-effort:
-- a malformed value is no hint, never a failed item.
--
-- Nullable, and NULL is the ordinary case rather than the exception. A planner that names no files
-- has not named an empty set of them, and every item written before this column existed looks
-- exactly the same as one written today by a planner that declined to guess. Storing `[]` for both
-- would say "the planner said no files", which is a claim neither of them made.
ALTER TABLE job_items ADD COLUMN files TEXT;
