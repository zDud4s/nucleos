-- Worktree identity generalises from a run to an (owner_kind, owner_id) pair, so a job can own a
-- worktree that outlives any single one of its runs. `run_id` was INTEGER PRIMARY KEY, and SQLite
-- cannot alter a primary key, so this is a rebuild-and-copy rather than an ALTER. The shape follows
-- 0024_email_server_key.sql, which established that multi-statement rebuilds work through sqlx.
--
-- Every existing row is run-owned by definition: nothing else could create one before this
-- migration. The copy hardcodes 'run', so no row is ambiguous.
--
-- Nothing REFERENCES worktrees, so the DROP fires no cascade. `one_open_worktree_run_per_project`
-- (migration 0009) is an index on `runs`, not on this table, and is therefore untouched.
CREATE TABLE worktrees_new (
    owner_kind   TEXT    NOT NULL CHECK (owner_kind IN ('run', 'job')),
    owner_id     INTEGER NOT NULL,
    project_id   TEXT    NOT NULL,
    project_root TEXT    NOT NULL,
    path         TEXT    NOT NULL,
    branch       TEXT    NOT NULL,
    created_at   TEXT    NOT NULL,
    removed_at   TEXT,
    PRIMARY KEY (owner_kind, owner_id)
);

INSERT INTO worktrees_new
    (owner_kind, owner_id, project_id, project_root, path, branch, created_at, removed_at)
SELECT 'run', run_id, project_id, project_root, path, branch, created_at, removed_at
FROM worktrees;

DROP TABLE worktrees;
ALTER TABLE worktrees_new RENAME TO worktrees;
