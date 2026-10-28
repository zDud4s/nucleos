-- `/mcp`: the MCP servers NucleOS itself knows about, and which conversation has which one on.
-- `mcp_servers` is the registry (one row per stdio server; `env` holds values, which are secrets
-- and never leave the daemon except into the per-turn config file or a child's environment);
-- `chat_mcp_servers` says which of them a conversation has enabled. The fixed `nucleos` server is
-- never a row: it is always on, so the name is refused here. A name is one or more of
-- `[A-Za-z0-9_-]` (the GLOB below refuses any other character; a bare `GLOB '[...]*'` would only
-- constrain the first). Deleting a server or a chat removes its toggles.
-- `chats.ambient_mcp` (migration 0165) is kept, and read only for the card's import hint.
-- `chats::mcp_servers` owns this SQL.
CREATE TABLE mcp_servers (
  name          TEXT PRIMARY KEY
                CHECK (name <> '' AND name NOT GLOB '*[^A-Za-z0-9_-]*' AND lower(name) <> 'nucleos'),
  command       TEXT NOT NULL,
  args          TEXT NOT NULL DEFAULT '[]',
  env           TEXT NOT NULL DEFAULT '{}',
  imported_from TEXT CHECK (imported_from IS NULL
                            OR imported_from IN ('claude-user','claude-project','codex')),
  created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE chat_mcp_servers (
  chat_id    TEXT NOT NULL REFERENCES chats(chat_id) ON DELETE CASCADE,
  name       TEXT NOT NULL REFERENCES mcp_servers(name) ON DELETE CASCADE,
  PRIMARY KEY (chat_id, name)
);
