-- The IDE verify switch is ON by default (plan 2026-10-17, F1): every session in a rostered
-- project's worktrees, the main checkout included, gets the `verify` box without anybody flipping
-- anything, because the final verification of every task now goes through it.
--
-- SQLite cannot change a column's default, so the switch is stored inverted: a new row reads 0,
-- which is on. Every existing project is switched on, deliberately, including one somebody had
-- left off; the owner switches a project off again from the same panel.
ALTER TABLE autopilot_state ADD COLUMN ide_verify_off INTEGER NOT NULL DEFAULT 0;
ALTER TABLE autopilot_state DROP COLUMN ide_verify;
