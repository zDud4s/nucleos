-- Two ways the embedding backfill fell behind its rows. Design: .ai/specs/2026-10-05-destilador-design.md
-- section 5.4; both were owner follow-ups of the phase B review.
--
-- No CHECK constraints and no foreign key, as in 0172: this is derived data.

-- A row that failed to embed under `model` while other rows embedded fine. `embed::backfill` passes
-- over it, so a row that always fails cannot hold the head of an oldest-first queue. A new model
-- tries it again, and so does new text (the trigger below).
CREATE TABLE knowledge_embed_failures (
  knowledge_id INTEGER PRIMARY KEY,
  model        TEXT NOT NULL,
  failed_at    TEXT NOT NULL
);

-- A row whose text changes loses its vector and its failure mark, so the backfill embeds the new
-- text. `consolidate.rs` rewrites a measurement's title and body in place, and the old vector kept
-- answering similarity for text the row no longer held. A trigger rather than a DELETE beside that
-- one UPDATE, for the reason `knowledge_fts_update` in 0143 is one: every writer that changes the
-- text is covered, including the next one.
CREATE TRIGGER knowledge_embedding_stale AFTER UPDATE OF title, body ON knowledge
WHEN old.title IS NOT new.title OR old.body IS NOT new.body
BEGIN
    DELETE FROM knowledge_embeddings WHERE knowledge_id = new.id;
    DELETE FROM knowledge_embed_failures WHERE knowledge_id = new.id;
END;
