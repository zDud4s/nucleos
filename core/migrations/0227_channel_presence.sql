-- Last hello per channel: the capabilities that routing and the outbox need before the adapter
-- reconnects after a nucleo restart.
CREATE TABLE channel_presence (
    channel      TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    contract     TEXT NOT NULL,
    capabilities TEXT NOT NULL,                      -- JSON of contract::Capabilities
    hello_at     TEXT NOT NULL
);
