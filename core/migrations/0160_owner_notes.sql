-- The owner's own notes: a second brain that is the person's and not the agent's.
-- `notes.rs` / `job_notes` carry words to a job; `knowledge` is what the AGENT knows. These are
-- neither: nothing the agent reads is written here and nothing here is read into a prompt.
--
-- No CHECK constraints, on purpose and for the house's reason (`0143_knowledge.sql`): the
-- vocabularies (`origin`, `state`, link types) are Rust constants checked before the write.
--
-- `owner_note_links` is polymorphic (`target_kind` + `target_ref`) so it carries no foreign key on the
-- target; the index below answers "what points at this?" without scanning every link.

CREATE TABLE owner_notes (
    id INTEGER PRIMARY KEY,
    note_text TEXT NOT NULL,
    origin TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE owner_note_links (
    id INTEGER PRIMARY KEY,
    note_id INTEGER NOT NULL REFERENCES owner_notes(id),
    link_type TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_ref TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (note_id, link_type, target_kind, target_ref)
);

CREATE INDEX owner_note_links_target ON owner_note_links (target_kind, target_ref);

CREATE TABLE owner_note_events (
    id INTEGER PRIMARY KEY,
    note_id INTEGER NOT NULL REFERENCES owner_notes(id),
    kind TEXT NOT NULL,
    detail TEXT,
    at TEXT NOT NULL
);

-- External-content FTS5 in the shape of 0057_feed_fts.sql. No backfill: the table is new.
CREATE VIRTUAL TABLE IF NOT EXISTS owner_notes_fts USING fts5(
    note_text,
    content='owner_notes',
    content_rowid='id',
    tokenize='unicode61'
);

CREATE TRIGGER IF NOT EXISTS owner_notes_fts_insert AFTER INSERT ON owner_notes BEGIN
    INSERT INTO owner_notes_fts (rowid, note_text) VALUES (new.id, new.note_text);
END;

CREATE TRIGGER IF NOT EXISTS owner_notes_fts_delete AFTER DELETE ON owner_notes BEGIN
    INSERT INTO owner_notes_fts (owner_notes_fts, rowid, note_text) VALUES ('delete', old.id, old.note_text);
END;

CREATE TRIGGER IF NOT EXISTS owner_notes_fts_update AFTER UPDATE ON owner_notes BEGIN
    INSERT INTO owner_notes_fts (owner_notes_fts, rowid, note_text) VALUES ('delete', old.id, old.note_text);
    INSERT INTO owner_notes_fts (rowid, note_text) VALUES (new.id, new.note_text);
END;
