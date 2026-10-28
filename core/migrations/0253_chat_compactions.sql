-- `/compact [focus]`: a turn whose answer is a `nucleos-compact v1` handoff document. One row per
-- compaction turn (`run_id`), `pending` while that turn runs, then `done` (the answer is the
-- `summary`), `failed` (no usable answer; the session stays) or `cleared` (a later `/clear` made
-- it stale). Only a `done` row above the chat's `cleared_after_run_id` is ever read back.
-- `chats::compactions` owns this SQL.
CREATE TABLE chat_compactions (
  id             INTEGER PRIMARY KEY,
  chat_id        TEXT NOT NULL REFERENCES chats(chat_id) ON DELETE CASCADE,
  run_id         INTEGER NOT NULL,
  status         TEXT NOT NULL CHECK (status IN ('pending','done','failed','cleared')),
  prompt_version TEXT NOT NULL,
  engine         TEXT NOT NULL,
  model          TEXT,
  focus          TEXT,
  summary        TEXT,
  summary_chars  INTEGER,
  created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX chat_compactions_chat ON chat_compactions(chat_id, run_id);
