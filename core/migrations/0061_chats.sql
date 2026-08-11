-- The conversations the app itself opened.
--
-- The daemon has been multi-chat inside from the start: the turn slot is held per chat
-- (`assistant.rs`), the agent session is per chat, and every run row records which chat it belonged
-- to (0045). What was missing is a place where a conversation exists BEFORE it has answers -- a chat
-- was only real by having runs, so an empty one could not be opened -- and any way to list them.
--
-- This table IS the app's list, and that is the whole point of it. A row is born from
-- `POST /assistant/chats` and from nothing else, so the Telegram sidecar never creates one and its
-- conversations are absent without a filter naming them. That matters: `Origin` exists precisely
-- because guessing the sender from the SHAPE of a chat id would make routing depend on a numbering
-- scheme Telegram owns and can change without telling anyone. The absence of a row is a fact we
-- wrote down; the shape of an id is a guess.
--
-- `title` is nullable on purpose. NULL means nobody has named this yet and the list falls back to
-- the first message. Storing that first message AS the title at creation would be simpler and would
-- be wrong one message later: the conversation moves on and the title lies forever without anyone
-- having written it.
--
-- `archived_at` rather than DELETE, because every turn is a billed run. Deleting the chat would hide
-- money spent from the place that records it. Archiving takes it off the list and leaves the
-- transcript readable.
CREATE TABLE chats (
  chat_id     TEXT PRIMARY KEY,
  title       TEXT,
  brain       TEXT NOT NULL DEFAULT 'cloud' CHECK (brain IN ('cloud', 'local')),
  created_at  TEXT NOT NULL,
  archived_at TEXT
);

CREATE INDEX chats_by_activity ON chats (archived_at, created_at);

-- Which model actually answered a turn.
--
-- The price of being able to change a chat's brain mid-conversation: without a column saying who
-- answered each turn there is nowhere to draw the memory cut, and a note that lives only in the
-- window contradicts this page's own rule -- the transcript belongs to the daemon and is read back
-- on arrival. It also closes a hole that predates this work: today a Telegram turn answered by the
-- local model is indistinguishable from a cloud one in the runs table.
--
-- Nullable, and only written for `mode = 'assistant'`. Rows that predate this keep NULL, which is
-- honest: nothing knows which model produced them.
ALTER TABLE runs ADD COLUMN answered_by TEXT;

-- The one chat that already exists. Everything else in `runs` came from Telegram and deliberately
-- stays without a row.
--
-- The aggregate makes this a single row whether or not the shell has ever been used, which is what
-- we want: the app's own conversation should be openable on a database that has never seen a turn.
INSERT INTO chats (chat_id, title, brain, created_at)
SELECT 'shell', NULL, 'cloud', COALESCE(MIN(created_at), '1970-01-01T00:00:00+00:00')
  FROM runs
 WHERE chat_id = 'shell' AND mode = 'assistant';
