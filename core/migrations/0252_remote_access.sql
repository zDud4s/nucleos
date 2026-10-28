-- Secure remote access: paired browsers and the one-time codes that pair them.
--
-- Only HASHES are stored. A session secret is the whole credential of a paired browser and a
-- pairing code is the whole credential of a pairing in progress, so a copy of this database (a
-- backup, a support bundle) must not be able to act as either. Both are high-entropy random
-- values, so a plain SHA-256 is enough: there is nothing to brute-force that a salt would help.
CREATE TABLE remote_devices (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT NOT NULL,
    access_level TEXT NOT NULL CHECK (access_level IN ('read-only', 'run-creating', 'admin')),
    session_hash TEXT NOT NULL UNIQUE,
    created_at   TEXT NOT NULL,
    last_seen_at TEXT,
    revoked_at   TEXT
);

CREATE TABLE pairing_codes (
    code_hash    TEXT PRIMARY KEY,
    access_level TEXT NOT NULL CHECK (access_level IN ('read-only', 'run-creating', 'admin')),
    device_name  TEXT NOT NULL,
    expires_at   TEXT NOT NULL,
    used_at      TEXT,
    attempts     INTEGER NOT NULL DEFAULT 0
);
