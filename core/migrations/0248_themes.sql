-- Themes above the entities (second-brain abstract, spec 2026-10-22 section 6.1).
--
-- No CHECK constraints (the rule of 0143_knowledge.sql): the vocabularies live in Rust constants
-- (`themes::STATES`, `MemberKind`, `ORIGINS`). No foreign key and no project_id, like
-- `knowledge_embeddings` (0172): a member that disappeared reads `missing`, and `project_exit` clears the
-- derived columns explicitly. `name` never holds text of an owner note (see `themes::provisional_name`).

CREATE TABLE themes (
  id              INTEGER PRIMARY KEY,
  name            TEXT NOT NULL,
  state           TEXT NOT NULL DEFAULT 'proposed',  -- proposed | accepted | refused | merged
  merged_into     INTEGER,                           -- always a live theme (no chains)
  model           TEXT NOT NULL,                     -- embedder the centroid came from
  dim             INTEGER NOT NULL,
  centroid        BLOB,                              -- f32 little-endian; NULL = recompute next run
  synthesis       TEXT,                              -- Phase 6; text derived from notes, owner-only
  synthesis_at    TEXT,
  synthesis_model TEXT,
  synthesis_basis TEXT,                              -- JSON: sorted member ids the synthesis came from
  created_at      TEXT NOT NULL,
  decided_at      TEXT
);
CREATE INDEX themes_by_state ON themes (state, id);

CREATE TABLE theme_members (
  theme_id    INTEGER NOT NULL,
  member_kind TEXT NOT NULL,                         -- note | knowledge
  member_id   INTEGER NOT NULL,
  origin      TEXT NOT NULL DEFAULT 'system',        -- system | owner
  excluded    INTEGER NOT NULL DEFAULT 0,            -- the owner took it out; it does not come back
  added_at    TEXT NOT NULL,
  PRIMARY KEY (theme_id, member_kind, member_id)
);
CREATE INDEX theme_members_by_member ON theme_members (member_kind, member_id);
