-- Durable idempotency claims for authenticated branch-movement deliveries.
CREATE TABLE webhook_deliveries (
    delivery_key TEXT PRIMARY KEY,
    project_id   TEXT NOT NULL,
    branch       TEXT NOT NULL,
    sha          TEXT NOT NULL,
    received_at  TEXT NOT NULL
);
