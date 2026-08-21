-- Which strangers a turn read, beside the fact THAT it read one.
--
-- `runs.read_untrusted` (0027) answers *whether*, and it is what the barrier reads. This answers
-- *which*, and nothing security-critical reads it. The split is the whole design: the barrier must
-- not come to depend on a record that is allowed to be incomplete.
--
-- **Allowed to be incomplete, on purpose.** `hooks.rs` sets the flag first and refuses the read
-- when the flag cannot be set -- a turn holding a stranger's words with no record of it is the
-- state every refusal downstream depends on not existing. The row here is written afterwards and
-- best-effort, because a provenance able to refuse a read would be a nicety holding a veto over the
-- pillar's main verb. So a run may carry the flag and no rows, and a reader has to say "not
-- recorded" rather than "read nothing".
--
-- A table and not a JSON column on `runs`, for two reasons that both bite. A turn reads many
-- strangers and the plural is the useful answer -- one browsing turn opens, snapshots, acts and
-- snapshots again. And appending to a column is read-modify-write, which two tool calls arriving
-- together silently lose half of; an INSERT is not.
--
-- The arguments are stored raw rather than prettied into a source string. `browser_open` carries
-- the url and `web_read` carries the url, so the deciding fact is already in them; a per-tool
-- extractor would be a second per-tool table beside `TOOL_EFFECTS`, kept in step by hand, which is
-- the drift `core/AGENTS.md` exists to prevent.
--
-- 0105 and not 0099: 0097 and 0098 are claimed twice over, by this branch and by
-- `feat/tab-de-chats`, which holds 0099 through 0101 as well; `master` and `feat/equipa-por-job`
-- hold 0101 through 0104. 0105 is free on every branch this repository currently has.
CREATE TABLE run_untrusted_reads (
  -- A logical reference and not a declared one, for the reason `errand_artifacts.written_by`
  -- gives: `runs` rows are pruned on their own schedule, and what is copied out of here outlives
  -- the run that wrote it.
  run_id     INTEGER NOT NULL,
  tool       TEXT NOT NULL,
  arguments  TEXT,
  at         TEXT NOT NULL
);

CREATE INDEX run_untrusted_reads_by_run ON run_untrusted_reads (run_id);

-- Copied onto the proposal, not joined to it at read time.
--
-- A refused action outlives its run: `runs` rows are pruned, and a record that answers "where did
-- this idea come from" with a dangling id answers nothing. It is the same reason
-- `proposals.tool_input` holds a copy of the arguments instead of pointing at them.
--
-- NULL means nothing was recorded, and that is a legitimate state rather than a defect -- most
-- plainly for the OTHER refusal this kind carries. `ERRAND_MAY_NOT_ACT` fires on whose work it is,
-- not on what the turn read, and an errand's first message has read nothing at all. A reader must
-- therefore not present an absent provenance as contamination.
ALTER TABLE proposals ADD COLUMN read_from TEXT;
