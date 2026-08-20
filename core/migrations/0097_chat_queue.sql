-- What was said to a conversation while it was already answering.
--
-- Until now a second message was refused: `send_message` takes the chat's one turn slot and answers
-- `a turn is already in progress for this chat`, so the window put a red note under the box and
-- threw the words away. That is the wall this table removes -- you type while it works, the message
-- is kept, and it becomes the next turn the moment the current one lets go of the slot.
--
-- Asked of the CLI first, because the alternative would have been better and is not available: with
-- `--input-format stream-json` and stdin held open, a second user line written mid-answer does NOT
-- interrupt anything. The process finishes the turn it is on, emits `result`, then starts a fresh
-- turn for the new line -- two `init`s, two `result`s, measured. So the CLI queues too, and what
-- this table does is make that queue visible and durable instead of living in a pipe.
--
-- Ordered by `id`, never by `created_at`: two messages typed in the same second must not swap
-- places, and a conversation whose queue reorders itself is one nobody can predict.
--
-- `origin` travels with the text because it decides how the turn is routed -- the shell and the
-- Telegram sidecar are answered differently -- and a message that waited must be sent as the thing
-- it was when it was written, not as whatever the drain happens to assume.
CREATE TABLE chat_queue (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id    TEXT NOT NULL,
    text       TEXT NOT NULL,
    origin     TEXT,
    created_at TEXT NOT NULL
);

-- The drain reads one chat's queue on every turn that ends, so this is the shape of every read.
CREATE INDEX idx_chat_queue_chat ON chat_queue(chat_id, id);
