-- Structural marker for a vcs request that could not be executed (owner decision 2026-10-18,
-- analytics pipeline plan Task 0.2). Written by `Outcome::Unexecutable` and by nothing else.
--
-- `output_tail IS NULL` used to carry this meaning, but `vcs::prune_output_tails` nulls the tail
-- of every finished row after 30 days, so past that age a NULL tail says nothing about the row.
-- A column is not pruned. Rows written before this migration default to 0: they carry no marker
-- and are read as ordinary failures. No backfill, on purpose.
ALTER TABLE vcs_requests ADD COLUMN unexecutable INTEGER NOT NULL DEFAULT 0 CHECK (unexecutable IN (0, 1));
