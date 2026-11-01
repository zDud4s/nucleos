-- A repair is a daemon-wide hand-off: an unmatched checkout cannot know which project it will
-- become until the roster update has finished. One row therefore represents the one live lease.
CREATE TABLE project_repair_leases (
    singleton     INTEGER PRIMARY KEY CHECK (singleton = 1),
    expires_at_ms INTEGER NOT NULL
);
