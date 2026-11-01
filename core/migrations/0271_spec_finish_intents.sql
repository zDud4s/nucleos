-- A project whose spec backlog still has finishing to do: written when an owner asks for a finish
-- or answers the last open question of a spec, and at boot for a spec a restart cut off. The
-- sweeper runs one pass per intent and deletes it once the pass has nothing left to defer.
-- Re-writing an intent bumps `created_at`, which is how a request that arrives during a pass is
-- told apart from the one the pass served. No foreign keys, ids as TEXT.
CREATE TABLE spec_finish_intents (
    project_id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL
);
