-- One-time step-up tickets of a native device. A native app cannot run a passkey ceremony in its
-- webview (the origin is not the gateway's), so it asks for a ticket, opens the gateway's page in the
-- system browser and the person does the ceremony there.
--
-- A ticket belongs to ONE device row, the one that asked: consuming it opens the step-up window of
-- that device and of no other. Only the SHA-256 of the ticket is stored (as for a device session), it
-- lives five minutes and works once. It is not a session: nothing here authenticates a request.
CREATE TABLE remote_stepup_tickets (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ticket_hash TEXT NOT NULL UNIQUE,
    device_id   INTEGER NOT NULL REFERENCES remote_devices(id) ON DELETE CASCADE,
    -- What the person is asked to confirm, as the app worded it (shown on the page).
    label       TEXT NOT NULL,
    -- A short code the app shows too, so the person can match page and app.
    code        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    consumed_at TEXT
);
CREATE INDEX remote_stepup_tickets_device ON remote_stepup_tickets(device_id);
