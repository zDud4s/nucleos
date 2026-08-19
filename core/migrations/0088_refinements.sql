-- What the agent has learned, kept where a person can read it, revert it, and where a run can be
-- handed it. The gap this closes was named from outside: 87 migrations and not one table for
-- anything the agent learned, so every run started from the same prompt with the same blind spots,
-- for ever.
--
-- WHY A TABLE AND NOT A FILE. The house's standing constraint is that the agent's own working
-- material never enters git — not the tree, not the history. A `memory.md` in the repository is
-- exactly the thing that constraint forbids, and it is also the wrong shape: two runs on two
-- worktrees of the same project would each edit their own copy and the last merge would win. Rows
-- owned by the daemon are per-machine, per-project, and have a status a person can change.
--
-- THE AGENT DECLARES, THE CORE ACTIVATES. Fourth application of the shape `create_calendar_event`
-- established and states in its own words: "The agent never writes the event itself. This row is
-- the whole mechanism: a human approves, and only then does `calendar_events` gain a row." A
-- refinement is proposed by a run, sits `proposed` while a `refinement` proposal waits for a
-- person, and reaches a prompt only once that person has said yes. Nothing a model wrote at 3am
-- changes what the next run is told without somebody having read it.
--
-- The alternative — apply on write, audit in the feed, revert later — was considered and rejected
-- by the owner: it is the only arrangement in which the agent can make itself worse and nobody
-- finds out in time to stop it.

CREATE TABLE refinements (
    id INTEGER PRIMARY KEY,

    -- NULL is machine-wide. Scoped by project because a lesson learned about one repository's
    -- build is a lie about another's, and the cheapest way to poison every future run is to let
    -- one project's blind spot become the house's.
    project_id TEXT,

    -- 'prompt'   — a supplemental instruction, added to a node's brief and never replacing it
    -- 'memory'   — a fact about this project the next run would otherwise rediscover
    -- 'skill'    — how to do a recurring thing here, named so a run can ask for it
    -- 'subagent' — a reusable spec for delegated work
    --
    -- Four kinds and not one free-text blob, because they are read at different moments and a
    -- prompt that pastes all of everything is a prompt nobody can afford.
    kind TEXT NOT NULL CHECK (kind IN ('prompt', 'memory', 'skill', 'subagent')),

    title TEXT NOT NULL,
    body TEXT NOT NULL,

    -- 'proposed'   — declared by a run, waiting for a person
    -- 'active'     — approved, and reaching prompts now
    -- 'rejected'   — the person said no
    -- 'reverted'   — was active, and a person took it back
    -- 'superseded' — a later refinement replaced it
    status TEXT NOT NULL CHECK (status IN ('proposed', 'active', 'rejected', 'reverted', 'superseded')),

    -- The approval that let it in. NULL while proposed, and the audit trail back to the person.
    proposal_id INTEGER,

    -- The refinement this one replaces. What makes the history a chain rather than a pile, and
    -- what makes "roll back to what it said before" answerable.
    supersedes INTEGER REFERENCES refinements(id),

    -- The run that proposed it, so a lesson can be read against the work that taught it.
    origin_run_id INTEGER,

    created_at TEXT NOT NULL,
    activated_at TEXT,
    ended_at TEXT
);

-- The one query on the hot path: what is active for this project, every time a node is spawned.
CREATE INDEX idx_refinements_active ON refinements (project_id, status, kind);

-- Every status change, with who and why. Mirrors `proposal_events`, and for the same reason: the
-- reviewable history the whole feature is for lives here, not in the current value of a column.
CREATE TABLE refinement_events (
    id INTEGER PRIMARY KEY,
    refinement_id INTEGER NOT NULL REFERENCES refinements(id),
    from_status TEXT,
    to_status TEXT NOT NULL,
    note TEXT,
    at TEXT NOT NULL
);

CREATE INDEX idx_refinement_events_refinement ON refinement_events (refinement_id, id);
