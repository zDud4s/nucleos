-- Numbered concurrency slots (night-jobs spec §7.1): one project may have N pieces of autonomous
-- work in flight instead of exactly one.
--
-- This replaces what two partial unique indexes do today — `one_open_worktree_run_per_project`
-- (0009) and `one_live_job_per_project` (0042) — WITHOUT moving where the property lives. Those
-- indexes never gave "one at a time" as such; what they gave is that the INSERT *is* the lock, so a
-- scheduler tick and a manual POST racing for the same project cannot both pass a check and then
-- both proceed. The primary key `(project_id, slot)` does the same work with a number in place of a
-- boolean: claiming is an INSERT at slot 0, then 1, up to the ceiling, and whoever loses the race
-- loses it on the key rather than on a read.
--
-- Count-then-insert in a transaction would also work, and is rejected: it would need a disciplined
-- BEGIN IMMEDIATE at every caller, moving the property out of the database and into the correctness
-- of whoever writes the next caller.
--
-- The old indexes are NOT dropped here. They come out only once the slots are actually holding the
-- property — dropping them in this migration would open a window, between it and the last converted
-- caller, in which nothing holds anything at all. That window is two worktrees in one repository.
CREATE TABLE project_slots (
    project_id TEXT    NOT NULL,
    slot       INTEGER NOT NULL,
    -- 'run' | 'job'. No CHECK constraint, deliberately: `worktrees` (0044) already carries this
    -- exact pair without one, and inventing a rule here that its sibling does not have is
    -- divergence for no gain.
    owner_kind TEXT    NOT NULL,
    owner_id   INTEGER NOT NULL,
    claimed_at TEXT    NOT NULL,
    PRIMARY KEY (project_id, slot)
);

-- Releasing looks a slot up by its OWNER, never by its number — a run that just finished knows what
-- it is, not which slot it got. Without this, every ending in the daemon is a table scan.
CREATE INDEX project_slots_by_owner ON project_slots (owner_kind, owner_id);

-- Two ceilings, because they bound different resources. This one stops a single project from
-- monopolising the machine.
--
-- Default 2, and the number is measured rather than modest: the gate for this repository is
-- `scripts/gates.sh core` — fmt, clippy and the whole suite — and every worktree carries its own
-- `target/`. Two simultaneous gates are two cold Rust builds.
ALTER TABLE autopilot_global ADD COLUMN max_concurrent_slots INTEGER DEFAULT 2;

-- And this one stops N projects from saturating it together. A per-project ceiling alone does not
-- bound the house: three projects at two slots each is six cold builds.
ALTER TABLE autopilot_global ADD COLUMN max_concurrent_total INTEGER DEFAULT 3;

-- Per-project override. NULL inherits the global default. As with `wip_limit` (0017) there is
-- deliberately no per-project "off" — and unlike the WIP brake there is no global off either, since
-- a concurrency ceiling switched off is not a state worth being able to express.
ALTER TABLE autopilot_state ADD COLUMN max_concurrent_slots INTEGER;
