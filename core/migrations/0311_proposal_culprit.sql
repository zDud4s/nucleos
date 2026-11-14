-- What the classifier said about the line it parked, kept on the proposal so the approval card can
-- show which piece of a compound command stopped it without re-running the classifier.
--
-- All nullable: a row written before this migration has no class, no culprit and no split, and the
-- card renders it as it always did. `segments_json` is a JSON array of strings, the line as the
-- classifier cut it.
ALTER TABLE proposals ADD COLUMN action_class TEXT;
ALTER TABLE proposals ADD COLUMN culprit_segment TEXT;
ALTER TABLE proposals ADD COLUMN culprit_reason TEXT;
ALTER TABLE proposals ADD COLUMN segments_json TEXT;
