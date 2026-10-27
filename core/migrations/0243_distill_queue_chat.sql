-- A conversation batch has no job: the row names its chat. Spec 2026-10-22 4.1.
-- The unique index of 0163 already keeps one row per (cause, last included run): NULL job and item
-- go through IFNULL there, and a run belongs to exactly one chat.
ALTER TABLE distill_queue ADD COLUMN chat_id TEXT;
CREATE INDEX distill_queue_chat ON distill_queue (cause, chat_id) WHERE chat_id IS NOT NULL;
-- P8: chat history is never distilled automatically. The automatic path only sees turns after this
-- cut; older chats come in by the owner's explicit ingestion. INSERT OR IGNORE: a re-run keeps it.
INSERT OR IGNORE INTO schema_meta (key, value)
SELECT 'distiller.chat_floor_run_id', CAST(COALESCE(MAX(id), 0) AS TEXT) FROM runs;
