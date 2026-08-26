-- Somewhere for one member of a department to leave words for another.
--
-- A department is a director that plans and specialists that answer, and between them there is no
-- channel. What one specialist apurou reaches the next by DISK and by ROUND: the answer is filed
-- into the folder, the folder index goes into the next node's prompt, and the director replans
-- between rounds. So a specialist that discovers, half way through, something that changes what a
-- colleague should be doing has no way to say it — the discovery waits for the folder and for the
-- replan.
--
-- **The steering channel is not the way in, and this is not the same refusal `job_notes` makes.**
-- That one says a job node is one step of a plan and must not be steered mid-flight. Here the
-- refusal is structural: `team::spawn_agent` passes `messages: None`, and with `None` the runner's
-- steering task writes the opening prompt and CLOSES stdin — nothing is listening. Making a
-- specialist listen would be worse than useless: a CLI on `--input-format stream-json` reads until
-- stdin closes, so a node left listening does not finish by having answered, it finishes by
-- TIMEOUT, and is recorded `timed_out` — a failure status — for having been left to listen. And the
-- registry of live channels is a `HashMap` in memory, against a module whose opening line is that
-- everything a run needs to be resumed is in the database and nothing is in memory.
--
-- So the mechanism is `job_notes`' (0072), with the author changed. Text waits on a row and the
-- NEXT node's prompt carries it. That migration's header reserved this exact question:
--
--     This phase writes only the owner here. A run that could leave a note for another run is a
--     separate change with a governance question attached, and this column is the place that
--     question would be answered -- not a reason to consider it answered now.
--
-- This table is that change. The answer is in `core/src/team_notes.rs` and in the effect the note's
-- tool carries: writing one is `WritesOwn`, so a specialist that read the web can still tell a
-- colleague what it found; READING one taints the receiving turn, so the untrusted text travels
-- with the words instead of being refused at the source. `errand_files_write` made exactly this
-- trade for exactly this reason, and its comment says so — as an action it would have been shut by
-- the errand's own first `web_read`.
--
-- Numbered 0119. This worktree holds 0117 and 0118; the highest any branch claims is 0118, also
-- this one. That is not enough on its own and the rule from `0084_runs_team_run_id.sql` stands:
-- this branch's files move together and ALWAYS TO THE TOP when they land, never into a hole master
-- happens to have — the holes belong to branches that have not landed yet, and filling one only
-- moves the collision onto somebody else. There is already one open: `0117_chat_relays.sql` on this
-- branch and `0117_map_decisions.sql` on `feat/mapa-juncao` both claim 0117, which makes seven.
-- `storage.rs::no_two_migrations_claim_the_same_version` is what turns that into a red gate at
-- integration instead of a panic in front of a person.
CREATE TABLE team_notes (
    id                  INTEGER PRIMARY KEY,

    -- Which run of which department the words belong to. A team run and not a team: two runs of one
    -- department are two separate pieces of work, and a note written during last week's run
    -- arriving in tonight's would be a sentence about a request nobody is working on any more.
    --
    -- The foreign key is kept, unlike the agent columns below, and `team::delete_team_run` deletes
    -- these rows the way it already deletes `team_items`. That is deliberate on both counts: a note
    -- is the run's working material rather than a ledger entry, so it goes when the run goes, and
    -- the constraint is what makes forgetting to delete them LOUD instead of silent. `team_actions`
    -- shows the cost of only doing half of that — it carries the same NOT NULL reference and
    -- nothing ever deletes from it.
    team_run_id         TEXT    NOT NULL REFERENCES team_runs(id),

    -- Who wrote it, and which node they were writing from.
    --
    -- **No foreign key on either agent column, and that is the same rule `chat_relays` follows.** A
    -- note is a historical fact about a department, and a later deletion of an agent must not be
    -- able to invalidate it or be blocked by it. `agent::delete` already refuses while an agent is
    -- named by live work, with a 409 that says which side is standing on it; a note from a run that
    -- finished last month is not live work, and a constraint here would turn "delete an agent you
    -- no longer use" into a refusal that can never be satisfied, because the words outlive the
    -- roster. `job_notes.author` is stored rather than derived for the same reason, in its own
    -- words: tokens are revoked and renamed, and the words outlive them.
    --
    -- `from_run_id` is the evidence and `from_agent_id` is the identity. The run holds the prompt
    -- and the answer, so it is the join back to what the sender actually knew when they wrote; the
    -- agent id is what a reader recognises, and it survives the run's own pruning.
    from_agent_id       TEXT    NOT NULL,
    from_run_id         INTEGER NOT NULL,

    -- Who the words are for, by `agents.id`.
    --
    -- **The agent, and deliberately not `team_items.ordinal`.** 0072 addresses the JOB rather than
    -- an item, because an owner speaking to a night should not have to guess an ordinal the plan may
    -- yet renumber. That argument holds here and points the other way: the sender is a colleague who
    -- means a specific person, and `agents.id` is exactly what a specialist can name — it is the
    -- left-hand side of every line `team::roster_lines` prints. An ordinal is a queue position the
    -- replan renumbers; a roster entry is not.
    --
    -- **No broadcast row, and no NULL meaning "everyone".** One sentence multiplied across every
    -- node of the next round is a cost paid by whoever did not need it, and the department already
    -- HAS a broadcast channel: the folder, whose index every prompt carries. The director is
    -- addressable by name like anybody else — `teams.director_agent_id` is an `agents.id` — which is
    -- what covers the most valuable message this table will ever hold: "I found X, it is worth
    -- replanning."
    to_agent_id         TEXT    NOT NULL,

    -- What was said, verbatim. Stored as written and never as a rewritten instruction, exactly as
    -- 0072 stores its own: the prompt is built from this by a function that quotes it, so what a
    -- colleague reads is what a colleague wrote. NOT NULL because a note with no words is an entry
    -- in this queue that delivers nothing and can never be delivered again.
    body                TEXT    NOT NULL,

    created_at          TEXT    NOT NULL,

    -- When the words were carried into a node's prompt. NULLABLE, and the nullability IS the design
    -- rather than an omission — `delivered_at IS NULL` is the queue. There is no status column to
    -- keep in step and therefore no way for two of them to disagree.
    --
    -- Written only AFTER the run that carries it exists, which is 0072's hard-won rule and not a
    -- detail: a node that failed to start never read the words, and a note consumed by it would be
    -- lost in silence — the one failure the owner cannot see and cannot repeat.
    delivered_at        TEXT,

    -- Which run was told. NULL at the same moments and for the same reason as `delivered_at`, and
    -- kept afterwards rather than dropped: "it was delivered" and "it reached THAT node" answer
    -- different questions, and only the second one says whether the words arrived in time to change
    -- anything.
    delivered_to_run_id INTEGER
);

-- The queue, as it is actually read: everything still waiting for one agent of one run. Partial, so
-- the index holds only the pending rows — a department's delivered notes accumulate for the life of
-- the run and none of them is ever looked up this way again.
CREATE INDEX idx_team_notes_pending
    ON team_notes (team_run_id, to_agent_id)
 WHERE delivered_at IS NULL;
