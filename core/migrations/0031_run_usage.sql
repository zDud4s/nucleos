-- Absence is not zero — a run that reported nothing must read back as unknown, not as a measured
-- zero.
ALTER TABLE runs ADD COLUMN input_tokens INTEGER;
ALTER TABLE runs ADD COLUMN output_tokens INTEGER;
ALTER TABLE runs ADD COLUMN cache_read_tokens INTEGER;
ALTER TABLE runs ADD COLUMN num_turns INTEGER;
