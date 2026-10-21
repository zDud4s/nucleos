-- no-transaction
-- `channel_outbox.class` admits `progress`: the best-effort edits of a live turn's progress
-- message (dropped, never retried, never converted to a send). SQLite cannot alter a CHECK, so the
-- table is rebuilt: the same columns, defaults and CHECKs as `0223`, with only the `class` list
-- widened, the two partial indexes recreated verbatim, and every row copied with its id so the
-- self-reference `replaced_by` keeps pointing at the same row.
--
-- Same procedure as `0154`: `-- no-transaction` and `PRAGMA foreign_keys = OFF` (which only takes
-- effect outside a transaction), then the rebuild inside one, then keys back on. Nothing but the
-- table itself references `channel_outbox`. No later migration altered the table, so the column
-- set below is `0223`'s.
--
-- The AUTOINCREMENT high-water mark is copied across before the old table goes: `DROP TABLE`
-- deletes its `sqlite_sequence` row, and an id handed out once must not be handed out again.
PRAGMA foreign_keys = OFF;

BEGIN;

CREATE TABLE channel_outbox_new (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    channel       TEXT NOT NULL,
    conversation  TEXT NOT NULL,                    -- the ref, without the channel prefix
    lane          TEXT NOT NULL CHECK (lane IN ('ordered', 'immediate')),
    action        TEXT NOT NULL CHECK (action IN ('send', 'edit', 'answer_action')),
    payload       TEXT NOT NULL,                    -- JSON: parts, buttons, silent, reply_to, target, action_ref, text
    class         TEXT NOT NULL CHECK (class IN ('turn', 'approval', 'notification', 'command', 'progress')),
    subject       TEXT,                             -- 'capture:<job>' | 'proposal:<id>' | 'run:<id>'
    state         TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'leased', 'done', 'failed')),
    due_at        TEXT NOT NULL,
    lease_until   TEXT,                             -- set on lease, extended by renew; expired = pending again
    expires_at    TEXT,                             -- notification +24 h, answer_action +60 s, progress +10 min, else NULL
    attempts      INTEGER NOT NULL DEFAULT 0,
    platform_refs TEXT,                             -- JSON array, set on ok
    fail_reason   TEXT,                             -- 'expired' | 'revoked' | 'attempts_exhausted' | adapter reason
    replaced_by   INTEGER REFERENCES channel_outbox (id),
    created_at    TEXT NOT NULL,
    settled_at    TEXT
);

INSERT INTO channel_outbox_new
    (id, channel, conversation, lane, action, payload, class, subject, state, due_at, lease_until,
     expires_at, attempts, platform_refs, fail_reason, replaced_by, created_at, settled_at)
SELECT id, channel, conversation, lane, action, payload, class, subject, state, due_at, lease_until,
       expires_at, attempts, platform_refs, fail_reason, replaced_by, created_at, settled_at
FROM channel_outbox;

UPDATE sqlite_sequence
   SET seq = (SELECT seq FROM sqlite_sequence WHERE name = 'channel_outbox')
 WHERE name = 'channel_outbox_new'
   AND EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'channel_outbox');

DROP TABLE channel_outbox;
ALTER TABLE channel_outbox_new RENAME TO channel_outbox;

-- Partial: only open rows, i.e. what the lease query and `outbox::counts` (pending/leased and
-- `oldest_due_at`) read. `failed` counts and pruning use `channel_outbox_settled` instead.
CREATE INDEX channel_outbox_due ON channel_outbox (channel, state, due_at)
    WHERE state IN ('pending', 'leased');
CREATE INDEX channel_outbox_settled ON channel_outbox (settled_at) WHERE settled_at IS NOT NULL;

COMMIT;

PRAGMA foreign_keys = ON;
