ALTER TABLE emails ADD COLUMN direction TEXT NOT NULL DEFAULT 'inbound';
ALTER TABLE emails ADD COLUMN to_addrs  TEXT;
CREATE INDEX emails_by_from ON emails (from_addr);

CREATE TABLE contacts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    display_name TEXT,
    created_at   TEXT NOT NULL
);

CREATE TABLE contact_addresses (
    address       TEXT PRIMARY KEY,
    contact_id    INTEGER NOT NULL REFERENCES contacts(id),
    first_seen    TEXT NOT NULL,
    last_seen     TEXT NOT NULL,
    messages_in   INTEGER NOT NULL DEFAULT 0,
    outbound_ever INTEGER NOT NULL DEFAULT 0,
    linked_by     TEXT NOT NULL DEFAULT 'implicit',
    linked_at     TEXT
);

CREATE TABLE contact_overrides (
    contact_id INTEGER PRIMARY KEY REFERENCES contacts(id),
    verdict    TEXT NOT NULL,
    note       TEXT,
    set_at     TEXT NOT NULL
);

CREATE TABLE contact_merge_rejections (
    lower_id    INTEGER NOT NULL,
    higher_id   INTEGER NOT NULL,
    rejected_at TEXT NOT NULL,
    PRIMARY KEY (lower_id, higher_id)
);
