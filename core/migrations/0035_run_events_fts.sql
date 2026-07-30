-- Lets a human find a run by what its trajectory SAID, without the index ever returning that text.
-- runs::search matched `prompt LIKE '%q%'` only: a substring on one column, so finding a run meant
-- already knowing the wording, and run_events (0033) — the verbatim trajectory — was unreachable.
-- FTS5 is built into SQLite: no new crate, no embedding model.
--
-- external-content table: the text stays in run_events and the index holds only terms, so a large
-- transcript is never stored twice. run_events.id is an INTEGER PRIMARY KEY, hence a rowid alias,
-- so it can serve as content_rowid.
CREATE VIRTUAL TABLE IF NOT EXISTS run_events_fts USING fts5(
    payload,
    content='run_events',
    content_rowid='id',
    tokenize='unicode61'
);

-- Backfill every trajectory recorded before this migration.
INSERT INTO run_events_fts (rowid, payload) SELECT id, payload FROM run_events;

-- run_events is append-only today (append_run_events only INSERTs); the delete/update triggers
-- keep the index honest if a retention pass ever prunes it.
CREATE TRIGGER IF NOT EXISTS run_events_fts_insert AFTER INSERT ON run_events BEGIN
    INSERT INTO run_events_fts (rowid, payload) VALUES (new.id, new.payload);
END;

CREATE TRIGGER IF NOT EXISTS run_events_fts_delete AFTER DELETE ON run_events BEGIN
    INSERT INTO run_events_fts (run_events_fts, rowid, payload)
    VALUES ('delete', old.id, old.payload);
END;

CREATE TRIGGER IF NOT EXISTS run_events_fts_update AFTER UPDATE ON run_events BEGIN
    INSERT INTO run_events_fts (run_events_fts, rowid, payload)
    VALUES ('delete', old.id, old.payload);
    INSERT INTO run_events_fts (rowid, payload) VALUES (new.id, new.payload);
END;
