-- Per-conversation opt-in to the user's ambient MCP servers. Off by default, and only ever read
-- for a rooted chat; an existing chat stays off.
ALTER TABLE chats ADD COLUMN ambient_mcp INTEGER NOT NULL DEFAULT 0;

-- What a notice is: a department's report ('department', every row written so far), or a note from
-- NucleOS itself ('restart' after a daemon restart cut a turn, 'untrusted' when a rooted turn went
-- read-only).
ALTER TABLE chat_notices ADD COLUMN kind TEXT NOT NULL DEFAULT 'department';
