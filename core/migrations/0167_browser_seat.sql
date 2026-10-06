-- Browser volante: who holds the person's seat in a session (spec browser-volante).
--
-- `seat` says WHERE a person drives once the mode is `human`: `shell` (the agent's own headless
-- browser, streamed into the app) or `window` (a real headful window). Rows that existed before this
-- column are today's behaviour, a real window, so they become `window`; new agent rows start NULL,
-- meaning no person drives yet.
--
-- The migration number must be re-checked at merge time: it moved 0161 -> 0162 -> 0167 while this
-- branch was open, each time because master or another branch had claimed it.
ALTER TABLE browser_sessions ADD COLUMN seat TEXT CHECK (seat IS NULL OR seat IN ('shell','window'));
UPDATE browser_sessions SET seat = 'window';
