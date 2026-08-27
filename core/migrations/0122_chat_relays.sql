-- Who handed a message to whom, so the chain a relay travels can be bounded.
--
-- One conversation can already act on another's behalf (`assistant.rs`), and `relay::admits`
-- decides, PURELY, whether a chain may take one more hop. That function needs the chain handed to
-- it; this table is where the chain comes FROM.
--
-- A table and not a column, for the same reason `run_untrusted_reads` is one (0109): the
-- relationship is plural. One conversation may relay to several others over its life, and a
-- `chats.last_relay_to` column would only ever hold the most recent one, silently discarding
-- everything before it the next time it was overwritten. And appending is an INSERT, not a
-- read-modify-write — two relays fired by the same conversation in close succession do not race to
-- clobber a shared field.
--
-- The chain itself is DERIVED, never stored. `chain_of` (`relay.rs`) walks it backwards at read
-- time: a run's `from_relay_id`, to the relay's `sending_run_id`, to THAT run's `from_relay_id`,
-- and so on until a run is reached whose `from_relay_id` is NULL — a turn a person wrote, which is
-- where every real chain bottoms out. `depth` below is written for a person skimming this table
-- with a SELECT, nothing more; it is not consulted by the walk and nothing enforces that it agrees
-- with what the walk would find, so it must never become the thing a caller trusts instead of
-- walking. The alternative — copying the growing chain onto every new row so a reader never has to
-- walk — is two representations of the same fact, and the moment code writes one without the other
-- they disagree with no error to say so.
--
-- `delivered_to_run_id` is auditing, not structure, and the distinction is load-bearing. The
-- property `relay::admits` actually protects — that a chain can always be reconstructed and is
-- therefore always boundable — hangs entirely on `runs.from_relay_id`: it is written in the same
-- INSERT that creates the run, so a run born from a relay cannot exist without saying which relay
-- bore it. `delivered_to_run_id` here is the mirror pointer, set afterwards, in a second statement,
-- once whatever spawns the receiving run knows its id. It answers a person's question — "did this
-- relay actually land, and where" — but nothing security-critical reads it, and its being NULL (the
-- relay was written but nothing has claimed it yet, or never will) is not a defect to guard
-- against, the same way an absent `run_untrusted_reads` row is not evidence of a bad read.
--
-- `from_chat_id` / `to_chat_id` are TEXT because `chats.chat_id` is TEXT (0061) and these name the
-- same values; not declared as foreign keys because, as with `run_untrusted_reads.run_id`, a relay
-- is a historical fact about a conversation that a later archival of that conversation must not be
-- able to invalidate or cascade into deleting.
--
-- **Cut as 0117, moved twice, and now 0122.** Two separate collisions rather than one told twice:
-- the first took this file's own number, the second took a sibling's and carried this one up with
-- it. Both are recorded, because the second is what turns the first from bad luck into a pattern.
--
-- **The first, at 0117 — the seventh time this repository has had two files claim one version.**
-- The header this replaces said, truthfully at the time, that 0116 was the
-- highest any branch had claimed. Six and a half hours later `feat/mapa-juncao` committed
-- `0117_map_decisions.sql`, and this is the part every previous collision's header warned about and
-- that no previous one caught in time: **`map decisions` is APPLIED, successfully, on the live
-- database of this machine.** Not proposed on a branch — recorded in `_sqlx_migrations`, version
-- 117, by a daemon the owner runs.
--
-- That is what settles which of the two moves, and it is not seniority. `0084_runs_team_run_id.sql`
-- states the rule as "the BRANCH gives way, master's lineage stands"; the principle underneath it is
-- that what has already been APPLIED cannot move, because moving it means a checksum mismatch on a
-- database somebody depends on. Master is the usual holder of that property and here it is not the
-- only one. This file was chronologically first by six hours and it is still the one that gives way,
-- because the other one is in a table and this one is in a branch.
--
-- The symptom, had this not moved: `sqlx::migrate!` finds version 117 already applied with a
-- different checksum and refuses to start the daemon. There is a backup in the data directory named
-- `pre-117-rollback-2026-08-26` — somebody hit exactly this and undid it by hand.
--
-- **The second, at 0118, and this file lost nothing of its own to it.** While this branch was being
-- merged up to master, `feat/pressao-de-contexto` landed `0118_runs_context_peak.sql` against this
-- branch's `0118_run_origin.sql`. Written first, and still the one that gives way, for the reason
-- above: the other one is on master. Four files move together or the block grows a hole, so all
-- four went up one and this became 0122 without anybody ever having claimed 0121.
--
-- What the pair of them says, plainly: a branch that stays unlanded is renumbered by every branch
-- that lands ahead of it, and the bill is paid twice each time — once in the filenames and once in
-- every comment that names a version. Twenty-two references across nine files, this round. The
-- answer is not a cleverer numbering scheme. It is landing.
--
-- 0122 because 0119, 0120 and 0121 are this branch's own three, sitting directly above master's
-- 0118 with nothing between them. The block is contiguous and leaves no hole for anybody else to
-- fill, which is 0084's rule working rather than an accident: a branch's files go to the TOP, and
-- they go together. Nothing here depends on running before them either — 0119 adds a column to
-- `runs`, 0120 and 0121 build tables of their own, and none of the three touches `chat_relays` or
-- `runs.from_relay_id`.
CREATE TABLE chat_relays (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    from_chat_id        TEXT NOT NULL,
    to_chat_id          TEXT NOT NULL,
    sending_run_id      INTEGER NOT NULL,
    body                TEXT NOT NULL,
    -- Convenience for a person reading this table directly. Not the authority — see header.
    depth               INTEGER NOT NULL,
    created_at          TEXT NOT NULL,
    -- Set once, afterwards, by whatever spawns the run that answers this relay. NULL means
    -- undelivered so far, not lost — see header.
    delivered_to_run_id INTEGER
);

-- Where a run's chain resumes, if it was born from a relay rather than typed by a person. Written
-- in the same statement that creates the run — see header for why that is the property this
-- migration exists to make possible.
ALTER TABLE runs       ADD COLUMN from_relay_id INTEGER;

-- Which relay produced a queued message, carried forward from `chat_relays` so that whatever later
-- spawns a run to drain the queue can stamp that run's own `from_relay_id` without a second lookup.
ALTER TABLE chat_queue  ADD COLUMN relay_id      INTEGER;

CREATE INDEX idx_runs_from_relay ON runs(from_relay_id);
