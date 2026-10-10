-- Tools an agent or a team holds beyond its box's base set, and the trail of how they got there.
--
-- loadout_tools: one row per (owner, tool). A run asks for a tool and the row is born 'proposed'
-- (source 'request'); the owner approves, rejects or later revokes it. Owner-granted rows are
-- born 'active' (source 'owner'). owner_id is an agents.id or a teams.id depending on owner_kind,
-- so there is no foreign key; deleting an owner deletes its rows by hand.
CREATE TABLE loadout_tools (
    id INTEGER PRIMARY KEY,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('agent', 'team')),
    owner_id TEXT NOT NULL,
    tool TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('proposed', 'active', 'rejected', 'revoked')),
    source TEXT NOT NULL CHECK (source IN ('owner', 'request')),
    reason TEXT,
    run_id TEXT,
    created_at TEXT NOT NULL,
    decided_at TEXT,
    UNIQUE (owner_kind, owner_id, tool)
);

-- loadout_tool_events: every status change of a loadout_tools row. No foreign key on
-- tool_row_id on purpose: the events of a row that was merged into another must outlive it.
-- to_status is unchecked because 'merged' is a valid value here and not in loadout_tools.
CREATE TABLE loadout_tool_events (
    id INTEGER PRIMARY KEY,
    tool_row_id INTEGER NOT NULL,
    from_status TEXT,
    to_status TEXT NOT NULL,
    note TEXT,
    at TEXT NOT NULL
);
CREATE INDEX idx_loadout_tool_events_row ON loadout_tool_events (tool_row_id);

-- run_loadout: the tools a run was started with, frozen at launch. Written by the wave-B
-- resolver; tools is a JSON array of bare tool names. It is read by `auth::loadout_admits` (the
-- routes) and the PreToolUse hook (`hooks.rs`) to decide what a run may call,
-- and a missing or unreadable row means no extra tools.
CREATE TABLE run_loadout (
    run_id INTEGER PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
    agent_id TEXT,
    team_id TEXT,
    tools TEXT NOT NULL,
    resolved_at TEXT NOT NULL
);
