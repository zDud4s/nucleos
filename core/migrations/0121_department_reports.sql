-- A department says something in a conversation the owner is reading, and nothing runs.
--
-- **This is deliberately NOT a relay, and the difference is the whole design.** `send_to_chat`
-- hands a message to a conversation and that conversation ANSWERS it: a turn starts, a model
-- thinks, money is spent. Three things make that the wrong shape for a department reporting:
--
--   1. `admit`'s attention brake refuses a relay when the owner is away, which is correct for
--      delegation and wrong for a report. A department that finishes at three in the morning should
--      still leave what it found, to be read at breakfast.
--   2. §13 of the relay design gives a relayed turn `ToolPolicy::McpOnly` — no file system. So the
--      delegation that would have justified spending a turn ("I found a bug, you have the tools,
--      fix it") arrives at the far end DISARMED. The door that made it worth paying for is already
--      shut, by us, on purpose.
--   3. `chain_of` walks `runs.chat_id`, and a team node has none. A team-sourced relay would refuse
--      itself at the chain brake — loudly, which is the safe direction, but it means the relay path
--      does not merely need widening, it needs a table rebuilt.
--
-- So: the words APPEAR. The owner reads them and decides. If they want the conversation to act on
-- it, they say so, which is one sentence and puts the decision to spend with the person rather than
-- with a model.
--
-- **A table of its own and not a `runs` row**, though a `runs` row would have been fewer lines: the
-- transcript is `runs WHERE chat_id = ? AND mode = 'assistant'`, so a completed row with the
-- department's words in `stdout` would render today with no new code at all. It is refused because
-- it would be a lie about what a row is, and this schema is unusually strict about that — `runs` is
-- also the spend ledger (`budget::job_rows` counts by mode and not by intent), the source of a local
-- turn's history (`assistant::recent_exchanges` replays `prompt`/`stdout` pairs), and what
-- `assistant::get_session` reads to decide whether a session may still be resumed. A row that never
-- ran would enter all three, and in the third it would silently rotate a conversation's context.
--
-- Numbered 0121, above this branch's own 0120. The rule from `0084_runs_team_run_id.sql` stands and
-- is the reason to say so here: this branch's migrations move together and ALWAYS TO THE TOP when
-- they land, never into a hole master happens to have. See 0122's header for the two collisions
-- this branch has had — at 0117 and at 0118 — and how both were settled: what has already been
-- APPLIED, or has landed on master, cannot move, whichever file was written first.

-- Where this run reports, or NULL for the vast majority that report nowhere.
--
-- **On the RUN and not on the team**, and that is the governance decision of this feature. A column
-- on `teams` would be a standing address a department writes to for the rest of its life, decided
-- once and then forgotten. On the run it is chosen by whoever starts THAT piece of work — the same
-- shape `team_grants` has, where what a department may do is a person's decision taken before it
-- begins rather than something it discovers it can do.
--
-- The consequence is the point: a department cannot choose who it talks to. It has no tool that
-- takes a destination, no way to list the conversations on this machine, and no way to reach one it
-- was not handed. NULL means it reports nowhere, and the tool says so plainly to the model rather
-- than failing in a way that reads as a bug.
--
-- TEXT and no foreign key, matching `chat_relays.from_chat_id`'s reasoning: the conversation may be
-- archived later, and a run's record of where it was told to report is a historical fact that a
-- later archival must not invalidate or cascade into deleting.
ALTER TABLE team_runs ADD COLUMN report_to_chat_id TEXT;

-- How far this conversation has been read into its notices.
--
-- A SECOND watermark beside `last_seen_turn_id` rather than a shared one, because the two count
-- different tables with independent id sequences and a single number cannot be past both. Written by
-- the same `chats::mark_seen` that writes the other, so there is still exactly one moment at which a
-- conversation becomes read.
--
-- Unlike the turn watermark, this one has no "still landing" case to skip: a notice is written
-- complete or not at all, so `MAX(id)` is always a notice somebody could have seen.
ALTER TABLE chats ADD COLUMN last_seen_notice_id INTEGER;

-- One thing a department said in a conversation.
CREATE TABLE chat_notices (
    id            INTEGER PRIMARY KEY,

    -- Which conversation it appears in. TEXT and unconstrained, as every other reference to
    -- `chats.chat_id` in this schema is, and for the reason `chat_relays` gives: what was said is a
    -- historical fact, and archiving a conversation must not be able to delete it or be blocked by
    -- it.
    chat_id       TEXT    NOT NULL,

    -- Which department said it, and which of its members. `team_run_id` and not `team_id`: two runs
    -- of one department are two pieces of work, and "marketing said" is not as useful as "the run
    -- that was asked to write the launch post said".
    --
    -- No foreign key on any of the three, for `team_notes`' reason: a notice outlives the run it
    -- came from and the agent that wrote it, and `team::delete_team_run` deleting a run must not
    -- reach into a conversation's transcript and remove what the owner already read.
    team_run_id   TEXT    NOT NULL,
    from_agent_id TEXT    NOT NULL,
    -- The node, for the evidence: it holds the prompt and the answer, so it is the join back to what
    -- the department actually knew when it said this.
    from_run_id   INTEGER NOT NULL,

    -- What was said, verbatim. The window quotes it and attributes it, and never presents it as the
    -- conversation's own: a message drawn without a source reads as something the OWNER wrote, which
    -- is the exact failure `fa56de0` fixed on the relay side.
    body          TEXT    NOT NULL,

    created_at    TEXT    NOT NULL
);

-- The transcript read: everything one conversation was told, in order. Not partial — unlike
-- `team_notes`' index there is no pending/delivered split here, because a notice is never claimed;
-- it is simply there from the moment it is written.
CREATE INDEX idx_chat_notices_chat ON chat_notices (chat_id, id);
