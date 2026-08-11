-- The paths each live worktree has changed since it was born. The **observed** source of the
-- collision warning, and only that one -- the declared source lives in `job_items.files` (0056) and
-- is joined at read time, because copying it here would give a value already in SQLite a staleness
-- window it does not have.
--
-- One row per worktree, and the intersection is computed on READ. Storing the pair instead of the
-- set would mean rewriting N-1 rows every time one of them changed, and there are at most
-- `slots_limit` sets per project (2 by default).
--
-- No CHECK on `owner_kind`, following `project_slots` (0052). Note that `worktrees` (0044) does have
-- one -- 0052's comment says otherwise, which is a mistake that stops here. The reason not to carry
-- one is a different one: this table is deleted and rewritten on every tick from rows `worktrees`
-- has already validated, so the constraint would be checking what the source already guaranteed.
CREATE TABLE worktree_touched_paths (
    owner_kind  TEXT    NOT NULL,
    owner_id    INTEGER NOT NULL,
    -- So grouping needs no JOIN on every read. The screen asks this per project, every 3 seconds.
    project_id  TEXT    NOT NULL,
    -- A JSON array. A malformed value reads as ABSENT, never as an empty set: empty means "I
    -- measured and it touched nothing", which is a claim a read error did not make. The same
    -- posture 0056 itself documents.
    paths       TEXT    NOT NULL,
    measured_at TEXT    NOT NULL,
    PRIMARY KEY (owner_kind, owner_id)
);

CREATE INDEX worktree_touched_paths_by_project ON worktree_touched_paths (project_id);
