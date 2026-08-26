-- The owner's verdict on one decision, and the moment they gave it.
--
-- This is the half of the map no derivation can produce. Structure says what the code does and the
-- junction says which decision it is standing under; neither can say whether that is what the owner
-- wanted. §5.2's three verdicts are the only place that answer exists, and the whole feature is
-- built to make asking for it cheap and to make the answer stop being true on its own.
--
-- **Append-only, and the absence of `UNIQUE (decision_id)` is the design rather than an oversight.**
-- §9.2: re-carimbar acrescenta uma linha, nunca substitui; o estado corrente é a última linha. A
-- unique constraint here would look like tidiness and would silently be the opposite feature — a
-- *mudei de ideias* would overwrite the *está como quero* that came before it, and the proof that
-- this decision had once been settled would be gone. That proof is exactly what somebody wants to
-- read when the doubt comes back, which is the reason this whole module exists. There is therefore
-- no constraint on this table that any single row can violate by arriving; the reader picks the
-- last one, and the rest are the history.
--
-- **The note is required on `partial` and the table is what requires it.** §7: obligatory on *a
-- meio*, optional on the others. §5.2 makes it the entire point of amber — *falta migrar as páginas
-- de pilar* is worth more than the colour is — so an empty amber is a row that should not exist
-- rather than a row a handler remembers to reject. A handler is a check the second caller forgets,
-- which is the argument `map_store::decide` already makes about `project_id`.
--
-- **`code_digest` has three states, and NULL is one of them.** This is the most important comment
-- in the file, because it is the difference between a green that comes back to ask and one that
-- never does. `''` was going to carry two facts at once: *computed, and this decision has no
-- readable anchor* — permanent, and honestly a stamp that can never expire — and *not computed,
-- because git could not be read just then* — transient, and a fact about this daemon rather than
-- about the code. Written alike, the second silently becomes the first and stays that way: a
-- decision with perfectly good anchors carries a green that never comes back to ask, because git
-- was unreadable for one second, and nobody ever learns why. That is §1's false confidence,
-- manufactured by the feature built to cure it. So NULL is *not computed*, `''` is *computed, and
-- there is nothing readable to watch*, and text is the digest itself.
--
-- **The `CHECK` on `settled` is what makes that distinction cost something.** §7.1: *está como
-- quero* is the only verdict the code moving can falsify, so it is the only one that may not be
-- recorded without knowing what it is anchored to. `partial` and `withdrawn` take NULL freely,
-- because neither expires by code and a missing digest costs them nothing — §7 still asks for the
-- digest whenever it can be had, since it is history worth keeping, so the writer stores NULL only
-- when it truly could not compute one. `POST /projects/{id}/map/stamps` answers `503` rather than
-- write a green it cannot anchor; this CHECK is what keeps that true when the second caller is less
-- careful than the first, which is the argument this file already makes twice above.
--
-- **A foreign key, following the house.** `job_notes.job_id` (0072), `errand_events.errand_id`
-- (0074) and `team_items.agent_id` (0071) all name their parent this way, `storage.rs` opens every
-- pool with `foreign_keys(true)`, and a child row that reaches its project only through its parent
-- has more reason than most to be sure the parent is there. Deliberately WITHOUT `ON DELETE
-- CASCADE`, which the attachment tables do use and this one must not: nothing deletes a decision
-- today — 0117 chose `retired_at` over DELETE precisely so the record survives — and if something
-- ever does, a cascade would quietly take the stamp history with it. Failing the delete loudly is
-- the answer that matches what this table is for.
--
-- 0118, and the number is not negotiable downward. `0116_chats_context_window.sql` and
-- `0117_map_decisions.sql` have both already RUN against the database this machine relies on, so
-- their numbers are spent whatever a spec written earlier says. This is the trap `0115`'s header
-- describes and `0117`'s header describes again; it has now caught this one feature twice.
CREATE TABLE map_stamps (
  id           INTEGER PRIMARY KEY,

  -- Which decision was stamped. **This table has no `project_id`**, and that is on purpose: a
  -- decision already belongs to exactly one project and a second copy of that fact is a second
  -- place for it to be wrong. The consequence is that every read must JOIN `map_decisions` to
  -- discover whose stamp it is, and that JOIN is the only thing standing between two owners' piles.
  decision_id  INTEGER NOT NULL REFERENCES map_decisions(id),

  -- §5.2's three, and nothing else. Portuguese on screen, English in the column — the same split
  -- `map_decisions.kind` makes with `b`/`c`, for the same reason: the window shows *Está como
  -- quero*, *A meio, e eu sei* and *Mudei de ideias*, and a column holding those would tie the
  -- schema to a language choice. The CHECK is what keeps a fourth verdict from being invented by a
  -- caller who is less careful than the first one.
  verdict      TEXT NOT NULL CHECK (verdict IN ('settled', 'partial', 'withdrawn')),

  -- When. Not decoration: §7.1 expires an amber stamp BY TIME and by nothing else, so this column
  -- is the only input to that rule.
  stamped_at   TEXT NOT NULL,

  -- The anchor code as it stood at this instant: one line per anchor file, `<git-blob-sha> <path>`,
  -- sorted by path. The git blob hashes and not a hash of the bytes on disk (§7), so the stamp
  -- expires at the commit rather than at every keystroke; and the canonical TEXT rather than a hash
  -- of it, because §7 requires a lapsed decision to show WHAT moved, and a scalar can only say that
  -- something did.
  --
  -- NULL is *this daemon could not compute one* — no repository, git unavailable, `run_git` failed.
  -- `''` is *computed, and there are no readable anchors to watch*, which is a fact about the
  -- decision and not about the moment, and is what yields a stamp that can never expire. Anything
  -- else is the digest. Nothing may collapse the first two: see the header, and note that an
  -- `unwrap_or_default()` on the way out is all it would take.
  code_digest  TEXT,

  -- What the owner wanted to say. NULL where they said nothing, which is allowed on `settled` and
  -- on `withdrawn` — the first should cost one click, and the second is an assertion the owner may
  -- or may not want to explain.
  note         TEXT,

  -- Amber without a note is not amber. `trim` is given the whitespace it must actually strip: bare
  -- `trim(note)` removes spaces only, so a note of one tab would have satisfied a constraint whose
  -- entire purpose is that the note says something.
  CHECK (
    verdict <> 'partial'
    OR (note IS NOT NULL AND trim(note, ' ' || char(9) || char(10) || char(13)) <> '')
  ),

  -- A green may not be recorded without knowing what it is anchored to (§7.1). The other two
  -- verdicts may, because neither of them expires by the code moving.
  CHECK (verdict <> 'settled' OR code_digest IS NOT NULL)
);

-- Every read of this table is the same question — "the latest stamp for this decision" — so the
-- index is that question written down. `DESC` because the answer is at the newest end.
CREATE INDEX map_stamps_by_decision ON map_stamps (decision_id, stamped_at DESC);
