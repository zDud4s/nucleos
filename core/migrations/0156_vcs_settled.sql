-- A git request that wants a person, and then stops wanting one, is settled rather than deleted.
--
-- `escalated` and `blocked` are terminal statuses, so before this column a row that had been
-- overtaken by events (the branch merged by hand, the branch deleted, a later run that succeeded
-- on the same arguments) stayed in the Waiting list forever. The status cannot be rewritten --
-- history is the point -- so the fact that nobody needs to look at it any more lives beside it.
--
--   settled_at     NULL -- still wants a person. Anything else: when it stopped.
--   settled_reason -- why: 'superseded', 'resolved', 'merged', 'source-gone' or 'dismissed'.
--
-- Both are NULL on every existing row, which is the safe default: a row is only ever hidden by
-- something that was observed, never by the migration.
ALTER TABLE vcs_requests ADD COLUMN settled_at TEXT;
ALTER TABLE vcs_requests ADD COLUMN settled_reason TEXT;
