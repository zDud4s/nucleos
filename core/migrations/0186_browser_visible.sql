-- Browser com painel: whether a session was opened as a window the person can see, with a chat
-- panel on the right (spec browser-com-painel, decision 1). Existing rows are headless or
-- seat-driven windows as before, so they start at 0.
--
-- The migration number must be re-checked at merge time: master or another branch may claim it.
ALTER TABLE browser_sessions ADD COLUMN visible INTEGER NOT NULL DEFAULT 0 CHECK (visible IN (0,1));
