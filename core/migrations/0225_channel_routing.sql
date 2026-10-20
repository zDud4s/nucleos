-- Notifier state and per-channel policy overrides (section 7.4).
CREATE TABLE channel_routing_state (
    channel            TEXT PRIMARY KEY,
    feed_cursor        INTEGER NOT NULL,
    proposal_cursor    INTEGER NOT NULL,
    last_kill          INTEGER NOT NULL CHECK (last_kill IN (0, 1)),
    last_budget_paused INTEGER NOT NULL CHECK (last_budget_paused IN (0, 1)),
    turn_watermark     INTEGER NOT NULL,             -- a cursor that only rises
    seeded_at          TEXT NOT NULL
);

CREATE TABLE channel_notify_policy (
    id         INTEGER PRIMARY KEY,
    channel    TEXT NOT NULL,
    scope      TEXT NOT NULL CHECK (scope IN ('family', 'kind')),
    selector   TEXT NOT NULL,
    enabled    INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    updated_at TEXT NOT NULL,
    UNIQUE (channel, scope, selector)
);
