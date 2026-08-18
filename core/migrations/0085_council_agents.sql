-- A council seat may be filled by an agent of the house catalogue, not only by a model name.
--
-- Two columns and no table. What an agent brings to a seat — a prompt, a name, a speciality, a tool
-- policy — already has a row in `agents`; a table here would be the second place the same truth is
-- declared, and the design says so at length.
--
-- Numbered 0085, having been cut as 0074 and passed through 0082. `0084_runs_team_run_id.sql`
-- carries the whole account: three cedings, and why these four always go above master's highest
-- rather than into a hole some branch has already written into.

-- The agent that took this seat, or NULL for a seat declared the old way — `{ kind, ref }`, which
-- stays valid forever. Not a declared foreign key, for the reason `chairman_run_id` gives one line
-- up in `0065_council.sql`: the rows it points at are deletable on somebody else's schedule, and a
-- council whose agent has since been deleted is still worth reading.
ALTER TABLE council_seats ADD COLUMN agent_id TEXT;

-- The same, for whoever presided.
ALTER TABLE council_runs ADD COLUMN chairman_agent_id TEXT;

-- `model_ref` and `chairman_ref` keep their meaning and their NOT NULL: they say WHICH MODEL
-- ANSWERED, and an agent seat fills them with what its agent resolved to at the moment the council
-- convened. That is not redundancy — it is the same argument `0065_council.sql` already makes for
-- copying the roster onto the row instead of reading it back from configuration. An agent can be
-- edited or deleted the afternoon after it deliberated, and a March council has to keep saying what
-- actually spoke.
--
-- It is also why an agent that names no model cannot take a seat: `council::start` refuses it
-- rather than write the blank that the column would otherwise have to hold.
