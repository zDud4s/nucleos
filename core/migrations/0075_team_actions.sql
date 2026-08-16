-- A department may ask the core to DO something, and the core does it after a human says yes.
--
-- The obvious design — put `send_email` in `TEAM_TOOLS`, `POST /email/send` in `TEAM_ROUTES`, and a
-- per-team list of which are on — fails three ways at once, and the design spec argues each. The
-- short version: `auth::permits` is a pure function over `(Scope, Method, path)` with no database
-- and must stay one; an `action-approval` pauses a run and `approve` resumes it, and a team item is
-- not resumable — it would sit `working` for days holding `max_parallel`; and the surface would
-- grow one route, one tool and one `TOOL_EFFECTS` entry per action forever.
--
-- What the house already had written, in `proposals::create_calendar_event`: "The agent never
-- writes the event itself. This row is the whole mechanism: a human approves, and only then does
-- `calendar_events` gain a row." So the agent DECLARES and the core EXECUTES. `TEAM_TOOLS` gains
-- `propose_action` and stays read-only forever; `TEAM_ROUTES` gains `POST /team-actions`.
--
-- Numbered 0075 against a branch tip of 0074 and a master tip of 0072. Re-check before merging.

-- What one team is allowed to ask for, and whether a human sees it first.
--
-- A table and not a file, unlike the council's roster and autopilot's rules, because of where the
-- object comes from: a team is created in the UI, `teams.id` is a slug derived there, and an
-- alçada in a file would make the owner create the team on screen and then open a text editor to
-- say what it may do — in two places with different naming rules, one of them gitignored. The
-- precedent is `autopilot_state.wip_limit`: a ceiling the interface moves lives in a column.
CREATE TABLE IF NOT EXISTS team_grants (
    team_id TEXT NOT NULL REFERENCES teams(id),
    -- One of `team::GRANTABLE_ACTIONS`. Not a CHECK constraint: the list is a house decision that
    -- changes with the code, and a constraint would need a migration to add an action while the
    -- constant needs a line. The constant is checked before the write, which is the enforcement.
    kind    TEXT NOT NULL,
    -- propose | allow. There is deliberately no `deny`: the ABSENCE of a row is the denial, and two
    -- ways of saying no is where they eventually disagree.
    mode    TEXT NOT NULL,
    PRIMARY KEY (team_id, kind)
);

-- One thing a department asked for, and what became of it.
CREATE TABLE IF NOT EXISTS team_actions (
    id           INTEGER PRIMARY KEY,
    team_run_id  TEXT NOT NULL REFERENCES team_runs(id),
    -- Which item of the run asked, or NULL when the director did. Resolved from the calling node's
    -- run id — see `team::calling_node` — and never from anything the model supplies.
    ordinal      INTEGER,
    kind         TEXT NOT NULL,
    -- JSON, validated by kind AT THE MOMENT IT IS WRITTEN and not at execution. A `send_email` with
    -- no recipient has to be refused to the agent, which is still in its turn and can still fix it,
    -- rather than to a human three hours later who can fix nothing.
    payload      TEXT NOT NULL,
    -- What the agent wrote to justify it. This is the sentence a person reads before deciding, so
    -- it is required rather than optional: an approval queue whose entries do not say why is a
    -- queue that gets approved unread.
    why          TEXT NOT NULL,
    -- The proposal a human will answer, or NULL when the grant was `allow` and there is nobody in
    -- the middle. A logical foreign key, like every other reference to `proposals` in this schema.
    proposal_id  INTEGER,
    -- pending | done | failed. Deliberately NOT the same field as `proposals.status`: that one says
    -- what the human decided, this one says what the world answered. Merged, `failed` would read as
    -- "the human refused" and `approved` would be a lie about an email that never left.
    state        TEXT NOT NULL,
    error        TEXT,
    created_at   TEXT NOT NULL,
    executed_at  TEXT
);

-- The tick reads exactly this: what is still pending, oldest first.
CREATE INDEX IF NOT EXISTS idx_team_actions_pending ON team_actions (state, id);
CREATE INDEX IF NOT EXISTS idx_team_actions_run ON team_actions (team_run_id, id);

-- No ON DELETE CASCADE and no trigger, unlike `council_seats`: an action OUTLIVES the run that
-- asked for it. A run may finish with actions still undecided, and it must, because the
-- alternative is a department sitting in `working` until somebody opens a laptop — holding a
-- `max_parallel` slot and counting against the four-hour ceiling the whole time.

-- `ordinal` is NOT the model naming itself, which `team::post_read_file` refuses on principle one
-- route over. It comes from `NUCLEOS_RUN_ID`, which the daemon puts in the node's environment and
-- its own MCP client puts on the wire — the model never builds the request and cannot touch it. It
-- is exactly as trustworthy as `NUCLEOS_DAEMON_TOKEN`, which travels the same way: anything able to
-- forge one already has the other. The daemon still checks that the node named belongs to the team
-- run the key authenticated, so a value from somewhere else names nothing.

-- How many actions one team may leave waiting for a decision.
--
-- `wip.rs` does not serve here and the reason is worth the column. `wip_permits_new_run` counts
-- `proposals WHERE project_id = ?1` plus that project's shadow decisions — it is per PROJECT, and a
-- team has none: `team_runs` has no `project_id` and will not get one, because a department does
-- not work on a repository. A `team-action` proposal is written with `project_id = NULL` and so
-- never reaches that count, exactly as `contact-merge` and `calendar-event` already do not.
--
-- Same idea, different axis, and self-clearing for the same reason wip is: it frees the moment
-- somebody decides.
ALTER TABLE teams ADD COLUMN max_open_actions INTEGER NOT NULL DEFAULT 5;
