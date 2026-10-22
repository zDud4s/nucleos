-- One embedding per owner note (second-brain v2, contract A2). Generic on purpose: the future
-- themes spec reads the same table, so nothing here is specific to "related notes".
--
-- Same shape as `knowledge_embeddings`: `vector` is f32 little-endian (`embed::encode`), `dim` its
-- length. `text_hash` is a hash of the note text the vector was computed from, so a note edited
-- after its embedding is detected and re-embedded without keeping a copy of the text here.
-- Best-effort: a note without a row is simply not matched by embedding, nothing else changes.

CREATE TABLE owner_note_embeddings (
    note_id INTEGER PRIMARY KEY REFERENCES owner_notes(id),
    model TEXT NOT NULL,
    dim INTEGER NOT NULL,
    vector BLOB NOT NULL,
    text_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
);
