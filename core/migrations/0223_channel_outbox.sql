-- The leased outbound queue (sections 6.1-6.2).
CREATE TABLE channel_outbox (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    channel       TEXT NOT NULL,
    conversation  TEXT NOT NULL,                    -- the ref, without the channel prefix
    lane          TEXT NOT NULL CHECK (lane IN ('ordered', 'immediate')),
    action        TEXT NOT NULL CHECK (action IN ('send', 'edit', 'answer_action')),
    payload       TEXT NOT NULL,                    -- JSON: parts, buttons, silent, reply_to, target, action_ref, text
    class         TEXT NOT NULL CHECK (class IN ('turn', 'approval', 'notification', 'command')),
    subject       TEXT,                             -- 'capture:<job>' | 'proposal:<id>' | 'run:<id>'
    state         TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'leased', 'done', 'failed')),
    due_at        TEXT NOT NULL,
    lease_until   TEXT,                             -- set on lease, extended by renew; expired = pending again
    expires_at    TEXT,                             -- notification +24 h, answer_action +60 s, else NULL
    attempts      INTEGER NOT NULL DEFAULT 0,
    platform_refs TEXT,                             -- JSON array, set on ok
    fail_reason   TEXT,                             -- 'expired' | 'revoked' | 'attempts_exhausted' | adapter reason
    replaced_by   INTEGER REFERENCES channel_outbox (id),
    created_at    TEXT NOT NULL,
    settled_at    TEXT
);
-- Partial: only open rows, i.e. what the lease query and `outbox::counts` (pending/leased and
-- `oldest_due_at`) read. `failed` counts and pruning use `channel_outbox_settled` instead.
CREATE INDEX channel_outbox_due ON channel_outbox (channel, state, due_at)
    WHERE state IN ('pending', 'leased');
CREATE INDEX channel_outbox_settled ON channel_outbox (settled_at) WHERE settled_at IS NOT NULL;
