-- The per-project switch that lets an interactive (IDE/terminal) session in a registered
-- worktree receive the `verify` tools (spec 2026-10-05 section 4.7; F2c-1).
--
-- OFF by default, and nothing in this migration turns it on for any project: the owner flips it
-- per project, and with it off a beat from a session is refused and nothing is recorded, so the
-- behaviour is exactly what it was before this column existed.
ALTER TABLE autopilot_state ADD COLUMN ide_verify INTEGER NOT NULL DEFAULT 0;
