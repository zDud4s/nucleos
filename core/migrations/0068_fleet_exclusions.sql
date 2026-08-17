-- Two fronts of work somebody asked not to run at the same time.
--
-- `job_low`/`job_high` rather than `job_a`/`job_b`, and the pair is normalised on the way in. The
-- relation is symmetric, so without normalising it the same request drawn the other way round is a
-- second row the unique index below cannot see. Ordering it also settles who waits, with no extra
-- column and no policy: the lower id runs and the higher one holds. That is what makes it IMPOSSIBLE
-- for both to be parked on each other — a tie-break decided at read time could, if the two reads
-- disagreed, and a deadlock between two jobs a person asked to serialise is the one outcome this
-- feature must not be able to produce.
--
-- `proposal_id` is NOT NULL because there is no other way to get a row here. The rule is written
-- when the proposal is approved and never by the request itself, so a row with no proposal would be
-- a rule nobody agreed to — and the id is what lets the screen show WHO agreed and when.
--
-- `paths` is what motivated the request, kept without being used yet. An exclusion dies with its
-- jobs, and a later version that wants to generalise to "these two fronts touch the same code" would
-- otherwise have nothing to generalise FROM. `collision.rs` already measures the overlap; this is
-- where the answer somebody acted on is written down.
--
-- Revoked rather than deleted: `revoked_at` keeps the decision readable after it stops applying,
-- and the partial unique index means a revoked pair can be drawn again.
CREATE TABLE fleet_exclusions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL,
    job_low INTEGER NOT NULL,
    job_high INTEGER NOT NULL,
    proposal_id INTEGER NOT NULL,
    paths TEXT,
    created_at TEXT NOT NULL,
    revoked_at TEXT
);

-- One live rule per pair. Two people approving the same request, or the same person drawing it
-- twice, must not become two rows that both have to be revoked to release the job.
CREATE UNIQUE INDEX one_live_exclusion_per_pair
    ON fleet_exclusions (job_low, job_high) WHERE revoked_at IS NULL;
