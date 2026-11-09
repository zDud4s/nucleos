-- How many acks and renewals the núcleo refused because they echoed a lease generation the row no
-- longer held (contract 1.2; finding rel-channels#1): the late answer of a lease that expired and
-- was handed out again. Until now the ack count lived in memory and restarted at zero with the
-- daemon. One row per channel, created on the first stale call; `last_at` is the most recent one
-- (UTC, second precision, like every outbox stamp).
CREATE TABLE channel_outbox_stale (
    channel      TEXT    PRIMARY KEY,
    stale_acks   INTEGER NOT NULL DEFAULT 0,
    stale_renews INTEGER NOT NULL DEFAULT 0,
    last_at      TEXT    NOT NULL
);
