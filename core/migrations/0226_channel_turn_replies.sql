-- The durable turn cycle (section 6.6).
CREATE TABLE channel_turn_replies (
    run_id       INTEGER PRIMARY KEY,
    channel      TEXT NOT NULL,
    conversation TEXT NOT NULL,
    acked_at     TEXT,
    pinged_at    TEXT,
    replied_at   TEXT
);
-- Partial: only the (normally 0-3) turns still to answer; looked up by run id.
CREATE INDEX channel_turn_replies_open ON channel_turn_replies (run_id) WHERE replied_at IS NULL;
