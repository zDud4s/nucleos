-- Which chat an assistant turn belonged to.
--
-- A turn IS a run, and the run row already carries the session — but a session is not a
-- conversation. `assistant.rs` deliberately drops a chat's session whenever a turn read
-- third-party text, so the thread continues under a new one, and past turns become unreachable
-- from the chat that produced them. Nothing anywhere recorded who a turn was talking to.
--
-- The consequence was that the shell's assistant transcript lived only in the window that made it:
-- reopening the app showed an empty conversation whose turns were all still in the database, and
-- reconstructing it from `/runs?mode=assistant` was not available either — that returns every
-- chat's turns, the Telegram sidecar's included, with nothing on the row to tell them apart.
--
-- Nullable, and written only for `mode = 'assistant'`: every other run has no chat, and backfilling
-- one would be inventing a fact rather than recording it. Turns that predate this column keep NULL
-- and are simply not listed, which is honest — nothing knows which conversation they were part of.
ALTER TABLE runs ADD COLUMN chat_id TEXT;

CREATE INDEX runs_assistant_by_chat ON runs (chat_id, id) WHERE chat_id IS NOT NULL;
