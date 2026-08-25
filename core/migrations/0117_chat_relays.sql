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
-- 0117: the highest number any branch in this repository has claimed so far is 0116, and this
-- worktree's own `core/migrations/` only reaches 0109 — see 0109's own header for what it costs to
-- guess low and be wrong.
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
