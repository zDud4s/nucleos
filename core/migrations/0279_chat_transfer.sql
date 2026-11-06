-- Moving a chat to another machine in the middle of work.
--
-- `moved_to` names the machine that took the chat over: the origin keeps the row (read-only, so the
-- history stays readable and a link points at the destination) and refuses new turns.
--
-- `frozen_for` is the `transfer_id` of the move in progress, and `frozen_at` (unix seconds) is when it
-- was last confirmed alive. A freeze older than 15 minutes is treated as gone when it is read, so an
-- app that vanishes between `begin` and `finish` cannot trap a chat.
ALTER TABLE chats ADD COLUMN moved_to TEXT;
ALTER TABLE chats ADD COLUMN frozen_for TEXT;
ALTER TABLE chats ADD COLUMN frozen_at INTEGER;

-- The transfer ids this machine has already imported, so a repeated import changes nothing.
CREATE TABLE chat_transfers_seen (
    transfer_id TEXT PRIMARY KEY,
    chat_id     TEXT NOT NULL,
    seen_at     INTEGER NOT NULL
);
