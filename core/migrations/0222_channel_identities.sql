-- Who may talk, where home is, pairing and the primary channel (section 7.2).
CREATE TABLE channel_identities (
    channel           TEXT NOT NULL,
    sender_ref        TEXT NOT NULL,
    sender_name       TEXT,                           -- as given at pairing, not refreshed per event
    conversation_ref  TEXT NOT NULL,
    conversation_kind TEXT NOT NULL CHECK (conversation_kind IN ('private', 'group', 'thread')),
    parent            TEXT,                           -- set iff conversation_kind = 'thread'
    role              TEXT NOT NULL DEFAULT 'owner' CHECK (role IN ('owner')),
    home              INTEGER NOT NULL DEFAULT 0 CHECK (home IN (0, 1)),
    created_at        TEXT NOT NULL,
    PRIMARY KEY (channel, sender_ref, conversation_ref)
);
-- One home per channel, enforced by the schema rather than by every writer.
CREATE UNIQUE INDEX channel_identities_one_home ON channel_identities (channel) WHERE home = 1;

-- A code is hashed (sha256 of channel + code), never stored; the row is kept 1 day after
-- `expires_at` for the states and the failure tally.
CREATE TABLE channel_pairing_codes (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
    channel                 TEXT NOT NULL,
    code_hash               TEXT NOT NULL,
    created_at              TEXT NOT NULL,
    expires_at              TEXT NOT NULL,            -- created_at + 10 min
    attempts                INTEGER NOT NULL DEFAULT 0,   -- tries left = MAX_PAIR_ATTEMPTS - attempts
    superseded_at           TEXT,
    cancelled_at            TEXT,
    paired_at               TEXT,
    paired_sender_ref       TEXT,
    paired_conversation_ref TEXT
);
-- At most one live code per channel; minting a new one supersedes the old one in the same transaction.
CREATE UNIQUE INDEX channel_pairing_codes_one_active ON channel_pairing_codes (channel)
    WHERE superseded_at IS NULL AND cancelled_at IS NULL AND paired_at IS NULL;

-- Where the `channels.primary` setting lives (there is no generic settings table; same pattern as
-- `autopilot_global`).
CREATE TABLE channels_global (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    primary_channel TEXT
);
INSERT INTO channels_global (id, primary_channel) VALUES (1, NULL);
