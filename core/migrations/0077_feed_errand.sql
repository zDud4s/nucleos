-- The feed learns that a project is not the only thing work can belong to.
--
-- `feed.project_id IS NULL` has always meant "the machine did this, not a project" — the kill
-- switch, the budget, startup recovery. That reading was only ever correct because a project was
-- the one owner there was.
--
-- An errand breaks it. An errand has no project by construction, so an errand's line and a
-- machine-wide line are the same row wearing the same NULL, and with piece 4 firing a rule per
-- errand per day the global feed would fill with them. The view a person opens to see what the
-- machine did overnight would become an errand log, and the question it exists to answer would stop
-- being answerable — silently, because nothing about it looks broken.
--
-- So the same column `proposals` got in 0075, for the same reason and with the same shape: NULL for
-- every row written before this and for every row that belongs to a project, and `Global` is
-- narrowed in `feed.rs` to mean BOTH are NULL. `All` is untouched — a machine-wide view that
-- quietly stopped showing a whole class of work would be this same bug in the other direction.
--
-- No `REFERENCES errands(id)`, which is the opposite of `errand_rules` one migration back. A rule
-- is a live instruction and an orphan one is a cron with no owner; a feed line is a record of
-- something that happened, and it stays true after the errand it happened to is gone. Same argument
-- `errand_artifacts.written_by` makes about pruned runs.
ALTER TABLE feed ADD COLUMN errand_id INTEGER;

-- Partial: the overwhelming majority of feed rows are not an errand's, and this index exists to
-- serve one question — "what has this errand been doing" — that only asks about the rows that are.
CREATE INDEX feed_by_errand ON feed (errand_id) WHERE errand_id IS NOT NULL;
