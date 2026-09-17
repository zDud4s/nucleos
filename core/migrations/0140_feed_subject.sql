-- Every feed line can name what it is about, so a reader can tell a story instead of a list.
--
-- Numbered 0140 for the reason 0139 gives: this branch's own chain stops at 0129 plus 0139, and
-- `master` already carries 0130 through 0138. A gap is free; a duplicate version stops the migrator.
--
-- `subject` is `<kind>:<id>` — `job:57`, `run:900598`, `council:<uuid>`, `team_run:<uuid>`,
-- `vcs:21`, `errand:2` — written by the one writer that knows it, never parsed back out of
-- `summary`. Until now the only way to put "job 57 started" beside "job 57's gate failed" was to
-- read the prose, and prose is the one column every writer is free to reword.
--
-- Nullable, and no backfill. A line with no natural subject (a config write, a digest, a restart)
-- is the ordinary case, not a gap. Old rows stay NULL too: the only exact key they carry is
-- `run_id`, and `run_id` alone cannot say whether that run was a job's node — which is the one
-- distinction a subject exists to make. Guessing it here would write a wrong key that looks right;
-- the reader already falls back to `run_id` for a row that has no subject.
ALTER TABLE feed ADD COLUMN subject TEXT;

-- Partial, because most lines have no subject and an index over NULLs only costs writes. It serves
-- "every line about job 57", which is a subject asking for its own history.
CREATE INDEX feed_by_subject ON feed (subject) WHERE subject IS NOT NULL;
