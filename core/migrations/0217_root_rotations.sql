-- Invisible root rotation: when a root chat's context grows past the soft threshold, the daemon
-- ends the CLI session at the end of a turn and stores the rehydration prefix here. The next turn
-- takes the newest undelivered row (marking it delivered) and prepends it to its prompt.
--
-- `tainted`: some run of the chat read untrusted text, so the prefix (brief/summary) may carry it
-- and the turn it is delivered to must be treated as having read untrusted text too.
CREATE TABLE root_rotations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id TEXT NOT NULL,
    root_id TEXT NOT NULL,
    from_run_id INTEGER,
    prefix TEXT NOT NULL,
    tainted INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    delivered_run_id INTEGER
);

CREATE INDEX idx_root_rotations_chat_delivered ON root_rotations (chat_id, delivered_run_id);
