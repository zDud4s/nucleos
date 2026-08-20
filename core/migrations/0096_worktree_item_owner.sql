-- A worktree can belong to an item, not only to a run or a job.
--
-- The CHECK is the whole of this migration, and it is a rebuild-and-copy because SQLite cannot add
-- one to an existing table. Same shape as 0044, which is where this CHECK was written; the nine
-- columns below are what that table holds today, `base_sha` having arrived by ALTER in 0062.
--
-- Nothing REFERENCES worktrees, so the DROP fires no cascade — 0044 relied on the same fact and it
-- is still true. `worktree_touched_paths` (0063) names owner pairs but declares no foreign key,
-- deliberately: it is deleted and rewritten on every tick.
--
-- No row changes kind here. Every existing row is a run's or a job's, and stays so; what changes is
-- what a future row is ALLOWED to say. Keeping the constraint tight rather than dropping it is the
-- point of `Owner` being an enum: a caller that invents a fourth kind is refused by the database
-- rather than writing a row nothing will ever collect.
CREATE TABLE worktrees_new (
    owner_kind   TEXT    NOT NULL CHECK (owner_kind IN ('run', 'job', 'item')),
    owner_id     INTEGER NOT NULL,
    project_id   TEXT    NOT NULL,
    project_root TEXT    NOT NULL,
    path         TEXT    NOT NULL,
    branch       TEXT    NOT NULL,
    created_at   TEXT    NOT NULL,
    removed_at   TEXT,
    base_sha     TEXT,
    PRIMARY KEY (owner_kind, owner_id)
);

INSERT INTO worktrees_new
    (owner_kind, owner_id, project_id, project_root, path, branch, created_at, removed_at, base_sha)
SELECT owner_kind, owner_id, project_id, project_root, path, branch, created_at, removed_at, base_sha
FROM worktrees;

DROP TABLE worktrees;
ALTER TABLE worktrees_new RENAME TO worktrees;
