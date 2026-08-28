-- What the triager thought of one decision, why, and what it was looking at when it thought it.
--
-- The model enters this feature twice and is a compressor both times (§6). Before, it turns a
-- thousand line spec into a dozen lines of decision. After — here — it looks at one node with the
-- mechanical proof beside it and answers a single question: *isto merece o olhar dele?* This table
-- exists because the alternative is §10's grey wall: three hundred and fifty rows nobody has
-- stamped, in no order anybody can act on, which is the fastest way to make a map nobody opens
-- twice.
--
-- **`verdict` admits two words and there is no third, and this is the most important sentence in
-- the file.** §6's table names exactly one thing the triager is forbidden to do — **"Aprovar"** —
-- and a prohibition written in a comment, in a handler and in a Rust enum is a prohibition with
-- three ways round it. The CHECK is the fourth copy and the only one that stays true for a caller
-- who reaches for none of the other three, which is every caller after the first: **a table that
-- COULD hold the word is a table somebody will eventually write it into**, at two in the morning,
-- from a psql prompt, to unstick something. `flagged` and `silenced`, and nothing else — not
-- `approved`, and not `map_stamps`' own `settled`, which is the more dangerous of the two because
-- it is the word that actually turns something green in this codebase.
--
-- **Silenced is not a statement about the code.** §5.1 spells it: *sem sinal de problema*, and
-- **ninguém olhou**. §6.1 says what collapsing that into a stamp would cost — *"se colapsassem, a
-- autoridade que foi retirada ao modelo era-lhe devolvida pela porta da renderização — e o mapa
-- passava a ser a falsa confiança de novo, agora com autoridade de semáforo"*. Two things keep the
-- two apart structurally rather than by convention: this is its own table and not a column on
-- `map_stamps`, so no default and no `LEFT JOIN` can turn a silence into a verdict; and the two
-- CHECKs admit disjoint vocabularies, so a word borrowed from one table is refused by the other
-- instead of quietly meaning something there. The columns share the NAME `verdict` because §9.2
-- names them alike; they can never share a VALUE, which is the half that matters.
--
-- **`reason` is required on both verdicts, and the table is what requires it.** §6.2 keeps the
-- silenced pile readable *"com a razão de cada silenciamento e o modelo que o produziu"*, because
-- *"um triador que silencia o que não devia é um bug do triador, e um bug só é corrigível se for
-- visível."* §13 rates that bug a **real** residual risk and names this pile as its only mitigation
-- — so a silence carrying nothing to read is the mitigation deleted one row at a time. Required on
-- a flag too, and not for symmetry: a flag with no reason is a nag the owner cannot answer, and a
-- nag nobody can answer is one they stop reading, which costs the same trust from the other side.
--
-- **The `trim` is given the whitespace it must actually strip.** Bare `trim(reason)` in SQLite
-- removes spaces and nothing else, so a reason of one tab would satisfy a constraint whose entire
-- purpose is that the text says something. `0118`'s note CHECK already argues this; it is owed here
-- three times over.
--
-- **`inputs_digest` is a scalar hash, and that is the OPPOSITE of what `0118` chose one file back.**
-- The obvious inference — *slice 4 kept its digest as readable text, so this one should too* — is
-- wrong, and the reason the two differ is the reason each exists. `map_stamps.code_digest` is text
-- because §7 requires a lapsed decision to show **what moved**: paths added, changed and gone, which
-- a scalar can never say. Nothing shows a diff of these inputs to anybody. The only question ever
-- asked of this column is *is this answer still about the same thing?*, that question is answered by
-- an equality test, and a hash is the cheapest honest way to answer it — one column of fixed width
-- against a blob that would otherwise carry every anchor path of every decision, times the number of
-- times the triager has run.
--
-- **What the hash must cover is everything whose change would make the answer wrong, and nothing
-- else:** the decision's `text` and `section`, because the model read them; the anchor set —
-- `map_join::Anchored::modules` and `::foreign`, sorted, because an order that varies would lapse a
-- judgement nothing happened to; the anchor `code_digest`, because §6 has the model answer *"com a
-- prova mecânica ao lado"* and the proof is therefore part of what it looked at; and the `Anchor`
-- variant, because *declared* and *guessed* are different evidence about the same paths.
--
-- **The three states of the anchor digest have to survive being hashed.** `0118` spends its longest
-- comment keeping *nobody could compute one* — transient, a fact about this daemon — apart from
-- *computed, and there is nothing to watch* — permanent, a fact about the decision. An
-- `unwrap_or_default()` on the way into this hash collapses them again one table over, and the row
-- it mints is the familiar one wearing a new hat: a judgement made in a minute when git would not
-- answer hashes identically to one made over a decision that genuinely has no anchors, so it stays
-- "current" for ever, about anchors it never saw.
--
-- **`Standing` is deliberately NOT covered.** §10 gives the triager one scope — a decision *"se
-- nunca foi vista"* — so a decision that leaves *never seen* leaves triage entirely. Its judgement
-- does not go stale; it becomes irrelevant, and the reader drops it on the standing rather than on
-- the digest. A digest that moved when somebody stamped a decision would send a model back over
-- rows the owner has already answered, which spends real money to learn nothing.
--
-- **Refused blank, and no narrower than that.** All three of `reason`, `model` and `inputs_digest`
-- must say something; none is constrained to a length or an alphabet. A `CHECK` for sixty-four hex
-- characters would tie the table to today's hash and refuse tomorrow's, and it would be guarding the
-- wrong failure: a wrong hash compares unequal and costs one extra model call, while an EMPTY hash
-- compares equal to the next empty one and presents a stale judgement as current. That is the whole
-- of what this constraint is for.
--
-- **Append-only, and the absence of `UNIQUE (decision_id)` is the design.** §9.2 makes re-answering
-- add a row, never replace one, and here that is not tidiness deferred — it is §13's mitigation
-- itself. A triager that silenced something it should have shown is a bug, and the row recording
-- that it did so is the only way anybody finds it. A unique constraint would let a later silence
-- erase the flag that preceded it, which is precisely the sequence somebody wants to read.
--
-- **A foreign key without `ON DELETE CASCADE`, which is what `0118` settled on and this follows.**
-- `job_notes.job_id` (0072) and `errand_events.errand_id` (0074) name their parent the same way,
-- `storage.rs` opens every pool with `foreign_keys(true)`, and a child row that reaches its project
-- only through its parent has more reason than most to be sure the parent is there. No cascade:
-- nothing deletes a decision today — `0117` chose `retired_at` over DELETE precisely so the record
-- survives — and if something ever does, a cascade would quietly take the triage history with it.
-- Failing the delete loudly is the answer that matches what this table is for.
--
-- 0119, and the number is not negotiable downward. Checked on 2026-08-26 against
-- `%LOCALAPPDATA%\nucleos\NucleOS\data\nucleos.db`, whose `_sqlx_migrations` is applied through
-- **117**: `0118` is this feature's own and has never run, and `0119` is free. `sqlx::migrate!`
-- checksums every file byte for byte and an applied one that changed panics the daemon at startup
-- with `Migrate(VersionMismatch)` — the trap `0115`'s header describes, `0117`'s describes again,
-- and which has now caught this one feature twice on numbering alone. **Never edit `0117` or
-- anything below it, and check before editing any migration rather than inferring from the fact
-- that `0118` was amended after it was written.**
CREATE TABLE map_triage (
  id            INTEGER PRIMARY KEY,

  -- Which decision was judged. **This table has no `project_id`**, on purpose and for `0118`'s
  -- reason: a decision already belongs to exactly one project, and a second copy of that fact is a
  -- second place for it to be wrong. Every read must therefore JOIN `map_decisions` to discover
  -- whose judgement it is, and that JOIN is the only thing standing between two owners' piles.
  decision_id   INTEGER NOT NULL REFERENCES map_decisions(id),

  -- §6's two, and no third. Read the header: this line is why the file is long.
  verdict       TEXT NOT NULL CHECK (verdict IN ('flagged', 'silenced')),

  -- Why, in the triager's own words. The mitigation §13 names, and half of what §6.2 asks the
  -- silenced pile to carry.
  reason        TEXT NOT NULL,

  -- Which brain answered. The other half of §6.2, and the same argument `map_decisions.brain` makes
  -- one table back: a list the owner found useless is worth being able to attribute before they
  -- conclude the feature is useless. The repair for a triager that silences too much is to stop
  -- using that triager, and that is not a decision anybody can take about a pile that will not say
  -- who filled it. `model` and not `brain` because §9.2 named it so and because a brain resolves to
  -- a model — the pile is more useful naming the one that actually answered.
  model         TEXT NOT NULL,

  -- When. Not decoration and not expiry: nothing here ages out by time, the way an amber note does
  -- (§7.1). It exists so the pile can be read newest-first and so two judgements about one decision
  -- have an order — and because a silence whose reason nobody can date is a silence nobody can
  -- argue with.
  computed_at   TEXT NOT NULL,

  -- What the triager looked at, hashed. See the header for what goes in, what stays out, and why
  -- this is a scalar where `map_stamps.code_digest` is text.
  inputs_digest TEXT NOT NULL,

  -- The three columns that must actually say something. `NOT NULL` alone would admit `''`, and each
  -- of the three has its own cost for that: an unreadable reason deletes §13's mitigation, an
  -- unnamed model makes the pile unattributable, and an empty digest compares EQUAL to the next
  -- empty one and so presents a stale judgement as current. `trim` is given the characters it must
  -- see through, because the bare form strips spaces only.
  CHECK (trim(reason, ' ' || char(9) || char(10) || char(13)) <> ''),
  CHECK (trim(model, ' ' || char(9) || char(10) || char(13)) <> ''),
  CHECK (trim(inputs_digest, ' ' || char(9) || char(10) || char(13)) <> '')
);

-- Two questions are asked of this table and they are the same question: "the latest judgement for
-- this decision", once per decision for the map and once filtered to `silenced` for §6.2's pile.
-- `DESC` because the answer is at the newest end.
CREATE INDEX map_triage_by_decision ON map_triage (decision_id, computed_at DESC);
