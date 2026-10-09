-- Context files an agent or a team carries (equipamento spec §4.3).
--
-- The CHECKs below are deliberate here, unlike in most tables of this schema: the spec states the
-- two closed vocabularies (owner_kind agent|team, kind file|dir) and the uniqueness of a path per
-- owner as part of the contract, so the schema is the last line of defence for what the Rust side
-- already validates.
CREATE TABLE context_refs (
    id INTEGER PRIMARY KEY,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('agent', 'team')),
    owner_id TEXT NOT NULL,
    path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('file', 'dir')),
    note TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (owner_kind, owner_id, path)
);
