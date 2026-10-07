-- Local embeddings of knowledge rows, and the similarity a run's knowledge was credited with.
-- Design: .ai/specs/2026-10-05-destilador-design.md sections 3 and 5.4.
--
-- No CHECK constraints (the rule of 0143_knowledge.sql). No foreign key and no project_id on
-- purpose: the vector table is derived data, and project_exit deletes it explicitly.

-- One vector per knowledge row: f32 little-endian, `dim * 4` bytes in `vector`.
CREATE TABLE knowledge_embeddings (
  knowledge_id INTEGER PRIMARY KEY,
  model        TEXT NOT NULL,
  dim          INTEGER NOT NULL,
  vector       BLOB NOT NULL,
  created_at   TEXT NOT NULL
);

ALTER TABLE run_knowledge ADD COLUMN s_sim REAL NOT NULL DEFAULT 0;
