-- Media id -> file in the files folder (section 5.2), kept 7 days.
CREATE TABLE channel_media (
    id         TEXT PRIMARY KEY,
    channel    TEXT NOT NULL,
    path       TEXT NOT NULL,                       -- relative to files_root
    mime       TEXT NOT NULL,
    name       TEXT,
    bytes      INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX channel_media_created ON channel_media (created_at);
