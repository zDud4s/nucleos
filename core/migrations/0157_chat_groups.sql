-- User-defined groups for the Chats column. Additive: a chat with no group keeps NULL, and
-- deleting a group sets its chats back to NULL (the daemon also does this explicitly, because it
-- does not rely on the foreign-key pragma).
CREATE TABLE chat_groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    position INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

ALTER TABLE chats ADD COLUMN group_id INTEGER REFERENCES chat_groups(id) ON DELETE SET NULL;
