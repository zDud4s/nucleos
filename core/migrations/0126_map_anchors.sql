-- Numbered 0126 and not 0120: master took 0120 for `team_notes`. See `0124_map_stamps.sql`'s header.
--
-- This one had never been applied anywhere when it moved, which is why it is the cheapest of the
-- three and worth saying so: the live database stopped at 0119. The other two had been applied, and
-- paying for that took two UPDATEs against `_sqlx_migrations` -- the checksum is of the file's
-- CONTENTS, which a rename does not touch.
--
-- Which files are one decision's, remembered rather than re-derived.
--
-- **This table exists because the map had exactly two inputs and both of them rot.** The specs are
-- gitignored working material that gets archived and renamed; that half was cured by reading the
-- document list out of `map_decisions` instead of off the disk. The other half is worse: every
-- anchor in this map is a `§N` written in a comment, recomputed from the working tree on every
-- read, with no memory of ever having been there. A model rewriting a module inside a
-- thousand-line plan drops the comment, and the decision lands in §5.1's *declarado, sem código*
-- indistinguishable from one nobody ever implemented. That is §1 of the design happening to the
-- instrument built against §1, and it is silent.
--
-- **So the comment stops being how an anchor is REMEMBERED and becomes how one is PROPOSED.** The
-- derivation is unchanged and still runs on every read — it is the only signal a decision nobody
-- has looked at has. What changes is that once somebody confirms an anchor, the set of files is
-- written down here, and deleting the comment afterwards no longer erases the association. It
-- produces a specific, named alarm instead: *these files were this decision's, and nothing says so
-- any more*.
--
-- **Two writers, and `source` is what keeps them from being read as one thing.** A stamp records
-- as a side effect, because §5.2's *está como quero* is already the owner looking at a decision and
-- at the code under it — but what it records is the set the COMMENTS produced at that moment, which
-- while §8 is unfixed is a set of guesses about which document a bare `§` meant. The owner pointing
-- at files is a different act with a different standing. Writing them alike would let a guess be
-- read back as a choice, which is the collapse this whole feature exists to refuse.
--
-- **Deliberately NOT written when a decision is approved.** That was the tempting third writer: it
-- would cover every approved decision with no extra click and no new surface. It is also the one
-- shape that manufactures confidence — at approval time nothing has been looked at, and §8 being
-- unfixed means nearly every anchor is a guess, so it would freeze a guess as a record for every
-- decision in the project at once, permanently, and call it memory.
--
-- **Paths and not the `blob path` digest `map_stamps.code_digest` holds**, though the two are one
-- `split_once` apart and reusing the form was the obvious move. They answer different questions and
-- have different lifetimes: that digest is *the code as it stood when I said this was right*, and it
-- exists to STOP BEING TRUE when the code moves (§7.1). This is *which files are this decision's*,
-- and it must survive the code moving — that is its entire purpose. Storing blobs here would make
-- two records differ whenever a file was edited, when nothing about the association changed, and
-- would hand a later reader two plausible ways to ask whether an anchor is current.
--
-- **Append-only, and the absence of `UNIQUE (decision_id)` is the same design `map_stamps` argues
-- for at length.** §9.2: acrescenta uma linha, nunca substitui. The current set is the last row.
-- A constraint here would look like tidiness and would silently delete the record that these files
-- were once this decision's — which is precisely what somebody wants to read when they are trying
-- to work out whether a comment went on purpose.
--
-- **`ON DELETE CASCADE`, following `map_stamps` and `map_triage`.** A decision row that is gone
-- takes its anchors with it: an anchor for a decision nobody can name is a row no reader can ever
-- reach, and it would keep a file looking spoken-for by nothing.
--
-- Numbering: `0126` is the next free number ON THIS BRANCH. Master has moved past it independently
-- and this file will have to move up at merge, exactly as `project_commands` and this branch's
-- other migrations already have. Renumbering is the merge's job and not something to pre-empt here,
-- where the number would then be wrong for everybody working on this branch.
CREATE TABLE map_anchors (
  id          INTEGER PRIMARY KEY,
  decision_id INTEGER NOT NULL REFERENCES map_decisions(id) ON DELETE CASCADE,
  -- The files, one per line, sorted, with forward slashes — the spelling
  -- `project_map::structure` produces and `map_join::Anchored::modules` carries. Sorted because
  -- two records of the same set must compare equal, and the only reader that matters is a set
  -- difference.
  --
  -- **Empty is a legal value and means something.** *These files were this decision's and now none
  -- are* is a fact the owner can assert — a decision whose code was genuinely removed — and it is
  -- not the same as never having recorded anything. NULL is refused for that reason: there is no
  -- third state here, unlike `map_stamps.code_digest` where *could not compute* is real because git
  -- had to be asked. Nothing is asked of git to write this.
  paths       TEXT NOT NULL,
  -- 'stamp' — recorded as a side effect of a verdict, from whatever the comments said at that
  -- moment. 'owner' — pointed at deliberately. See the header: these are not the same claim and a
  -- reader must be able to tell them apart.
  source      TEXT NOT NULL CHECK (source IN ('stamp', 'owner')),
  recorded_at TEXT NOT NULL
);

-- The only read this table has: the latest row per decision, for one project's decisions. Ordered
-- inside the index so the reader never sorts, exactly as `map_stamps_by_decision` is.
CREATE INDEX map_anchors_by_decision ON map_anchors (decision_id, recorded_at DESC);
