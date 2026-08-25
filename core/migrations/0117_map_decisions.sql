-- The decisions a spec fixed, as a model proposed them and as their owner answered.
--
-- **The map does not read a decision table; it provokes one.** The convention of a numbered
-- decision table exists in 2 of this repository's 38 specs, so reading one is not a strategy. A
-- model reads the document and proposes a short list; the owner approves it line by line; nothing
-- reaches the map unapproved. That is spec §4, and this table is where the proposal waits.
--
-- `approved_at IS NULL` is the whole of "not looked at yet", and the pile of those is counted
-- SEPARATELY from real decisions. A map that mixed them would be claiming a project decided things
-- nobody has read.
--
-- **No row for §4.1's type A.** A decision about scope, process, or the document itself can be
-- implemented by no code and asks nothing of anybody. It is dropped when the extraction is parsed
-- and never becomes a row — `kind` admits only `b` and `c`, and the CHECK is what keeps that true
-- when a later caller is less careful than the first one.
--
-- **`retired_at` rather than DELETE.** A decision the owner rejected must not be proposed again for
-- that spec, and a row that is gone cannot say so. It is also the only record that somebody once
-- looked at this line and said no, which is exactly what is worth being able to see when the doubt
-- comes back.
--
-- 0117 and not 0116. The spec said 0116; `0116_chats_context_window.sql` landed on master first
-- (aa73c57) and had already RUN against the database this machine relies on, so the free number was
-- gone and the applied one could not be argued with. This is the same trap `0115`'s own header
-- describes, and it caught this feature the second time in a row.
CREATE TABLE map_decisions (
  id          INTEGER PRIMARY KEY,
  project_id  TEXT NOT NULL,
  -- The document's filename without its extension, as `map_intent::spec_slug` computes it. Not the
  -- path: two projects that keep their specs in different folders must name one document alike.
  spec_slug   TEXT NOT NULL,
  -- The heading it came from, verbatim. This is the anchor slice 3 joins to code by, and a row
  -- without one was already dropped before reaching here.
  section     TEXT NOT NULL,
  -- Its place in the list the owner read, 1-based, per extraction.
  ordinal     INTEGER NOT NULL,
  text        TEXT NOT NULL,
  kind        TEXT NOT NULL CHECK (kind IN ('b', 'c')),
  -- Which brain answered. Kept because the owner chooses it, and a list they found useless is worth
  -- being able to attribute before they conclude the feature is useless.
  brain       TEXT NOT NULL,
  extracted_at TEXT NOT NULL,
  -- NULL until somebody looked. The two piles are counted apart, always.
  approved_at TEXT,
  -- Set when the owner says no, or when a later extraction supersedes this one.
  retired_at  TEXT,
  UNIQUE (project_id, spec_slug, ordinal, extracted_at)
);

-- The two questions asked of this table: "what is waiting for me in this project" and "what did
-- this spec decide". Both are filtered by project, so it leads.
CREATE INDEX map_decisions_by_project ON map_decisions (project_id, spec_slug, retired_at);
