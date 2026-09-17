-- The feed becomes something a person reads by the day, and remembers where they stopped.
--
-- Numbered 0139, not 0130, although this branch's own chain stops at 0129: `master` already carries
-- 0130 through 0138, and two files claiming one version is a migrator that refuses to start the
-- moment this branch lands. A gap in the numbers costs nothing — the migrator applies what it has
-- not applied, in order — and a duplicate costs the daemon.
--
-- The index serves `feed::timeline`, which asks for a window of `created_at` ordered by
-- `created_at, id`. Until now every reader of this table asked for the newest N by `id`, which the
-- primary key answers; a window of days across every scope has nothing to walk but the whole table,
-- and the table holds ninety days of every run, gate and worktree. `id` rides along as the second
-- column because it is the tiebreak the ordering names: two lines written in the same instant still
-- come back in the order they were written, without a sort step on top of the index.
CREATE INDEX feed_by_created_at ON feed (created_at, id);

-- Where the owner stopped reading, as the id of the last line they have seen.
--
-- An id and not a timestamp, because ids are the one order in this table nobody can write out of
-- sequence: `created_at` is text a writer computed, and two writers can disagree about the same
-- instant. "Everything up to line N" is exact; "everything before 14:02" is a guess at the edges.
--
-- One row, and the CHECK says so. `scope` is a column at all so that a per-project marker, if one is
-- ever wanted, is a new permitted value rather than a rebuilt table — and the CHECK is written now
-- for the reason `0129` gives: adding one later means rebuilding the table. Today there is one
-- reader, so there is one marker.
CREATE TABLE feed_seen (
    scope   TEXT PRIMARY KEY CHECK (scope = 'global'),
    -- Monotonic, enforced by the one statement that writes it (`feed::mark_seen`), not here: a
    -- trigger would refuse a lower value, and a lower value is not an error, it is a second window
    -- that had read less than the first.
    through INTEGER NOT NULL,
    seen_at TEXT NOT NULL
);
