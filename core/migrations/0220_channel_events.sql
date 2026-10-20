-- At-least-once dedup (section 5.3): an adapter may redeliver an event, so the nucleo remembers
-- which event ids it has seen for 7 days. `processing` is the claim taken before the work starts.
CREATE TABLE channel_events (
    channel     TEXT NOT NULL,
    event_id    TEXT NOT NULL,
    received_at TEXT NOT NULL,
    outcome     TEXT NOT NULL CHECK (outcome IN ('processing', 'accepted', 'dropped')),
    PRIMARY KEY (channel, event_id)
) WITHOUT ROWID;
CREATE INDEX channel_events_received ON channel_events (received_at);
