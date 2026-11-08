-- The approval a resume run was minted from, for as long as that run has not yet been handed to its
-- CLI. `continue_paused_run` commits the approval (paused run `superseded`, proposal `approved`, the
-- resume row `running`) and only then launches the process; a daemon that dies between the two left
-- a `running` row that no process had ever served, which the startup sweep turned into `interrupted`
-- and so swallowed an approval the owner had given. Set inside the approval's transaction and cleared
-- by the launch just before the CLI starts, so a row that still carries it at startup is exactly a
-- resume that never ran, and `return_unspawned_resumes` hands its approval back to the owner.
-- NULL for every other run, and for every resume once it has launched.
ALTER TABLE runs ADD COLUMN pending_resume_proposal_id INTEGER;
