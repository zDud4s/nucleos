-- Browser com painel pela página Web: the conversation a session answers to when no run opened it
-- (spec browser-com-painel-pela-pagina-web §3.2). A window opened from the shell's Web page is owned
-- by a chat created on the spot, which has no run yet, so `run_id -> runs.chat_id` cannot name it.
-- NULL for every existing row: their owner, if any, still comes from `run_id`.
--
-- The migration number must be re-checked at merge time: master or another branch may claim it.
ALTER TABLE browser_sessions ADD COLUMN owner_chat_id TEXT;
