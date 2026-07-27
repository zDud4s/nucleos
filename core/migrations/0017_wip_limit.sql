-- WIP limit on open proposals (spec 8.4/8.7): the scarce resource is the user's attention, not
-- tokens, so autonomy stops generating new work for a project once that project's approval queue is
-- full. Same block-new-starts semantics as the kill switch and the budget: in-flight runs finish.
--
-- Default 3, matching the shell's SWAMPED_THRESHOLD: the queue is capped exactly where the UI would
-- otherwise start calling it swamped, so the state is prevented rather than merely reported.
ALTER TABLE autopilot_global ADD COLUMN wip_limit INTEGER DEFAULT 3;

-- Per-project override. NULL inherits the global default; there is deliberately no per-project
-- "off", because turning the brake off for one project is not a thing worth making easy.
ALTER TABLE autopilot_state ADD COLUMN wip_limit INTEGER;
