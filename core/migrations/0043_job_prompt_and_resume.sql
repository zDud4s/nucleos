-- The rule's prompt, copied onto the job at creation for the same reason `gate_each` and `review`
-- are: `.ai/autopilot.yaml` can be edited mid-flight, and a job's plan node can run long after the
-- job started — after a crash resume, hours later. Re-reading the file at spawn time would plan a
-- task nobody asked this job to do, with nothing recording the substitution.
ALTER TABLE jobs ADD COLUMN prompt TEXT;

-- Where a parked job goes back to.
--
-- `waiting` is a status like any other, so it overwrites the one underneath it — and exactly one of
-- those carries meaning the job cannot reconstruct: `planning` is the only status that says the
-- item queue does not exist yet. A job parked for budget while still planning and then resumed
-- would read as planned-with-an-empty-queue, which is the honest "there was no work" night, and it
-- would report itself complete without ever having planned anything.
ALTER TABLE jobs ADD COLUMN resume_status TEXT;
