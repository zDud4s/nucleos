-- The passkeys of an owner's remote device. The WebAuthn ceremony runs in the gateway; the daemon
-- only keeps the credential it produced (public key, sign count, flags) so a later ceremony can be
-- checked, and opens the step-up window when the gateway says one passed.
--
-- A credential belongs to ONE device row: a verify looks up only that device's rows, and a revoked or
-- deleted device takes its passkeys with it. The JSON holds a public key and a counter, no secret,
-- but it is still never logged.
CREATE TABLE remote_passkeys (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    device_id       INTEGER NOT NULL REFERENCES remote_devices(id) ON DELETE CASCADE,
    credential_json TEXT NOT NULL,
    created_at      TEXT NOT NULL
);
CREATE INDEX remote_passkeys_device ON remote_passkeys(device_id);
