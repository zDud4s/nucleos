-- A department may start without anybody asking.
--
-- Not a branch in `scheduler_tick`, and the reason is structural rather than stylistic: that loop
-- is `for (project_id, project_root, project_mode) in autopilot_projects(...)`, and everything it
-- does afterwards depends on those three — it loads `.ai/autopilot.yaml` from the root, asks
-- `wip_permits_new_run(project_id)`, keys `scheduler_state` on `(project_id, rule_name)`, and
-- compares `last_head_sha`. A team has no project, no root and no HEAD, and will not get one: a
-- department works over a folder of files, not over a repository. Forcing teams through that loop
-- means inventing a fake project, and from then on half the checks run over a value that means
-- nothing.
--
-- So: `team_trigger.rs`, its own loop, reusing the PURE helpers of `scheduler.rs` (`due_rules`,
-- `next_fire`, `rule_timezone`) and none of the code that needs a project.
--
-- Numbered 0087, having been cut as 0076 and passed through 0084. See
-- `0084_runs_team_run_id.sql` for why all four moved, three times, and always to the top.

-- One rule that starts a department.
CREATE TABLE IF NOT EXISTS team_triggers (
    id          INTEGER PRIMARY KEY,
    team_id     TEXT NOT NULL REFERENCES teams(id),
    name        TEXT NOT NULL,
    -- **Default 0, and that is the decision.** `.ai/autopilot.yaml` says it in its own lines:
    -- writing a rule and arming it are two acts. A rule that armed itself on creation would mean
    -- the moment of writing is the moment of firing, which is the one moment the author is least
    -- sure the text is right.
    enabled     INTEGER NOT NULL DEFAULT 0,
    -- cron | team_finished | email_triaged. Three, and `repo` deliberately absent: a department has
    -- no repository, and one that had would be a job.
    source      TEXT NOT NULL,
    cron        TEXT,
    -- IANA. NULL is UTC, exactly as `ScheduleRule::timezone` means it — and an UNKNOWN name is an
    -- error rather than a fallback, because reading `Europe/Lisbon` as UTC fires an hour off and
    -- looks like it worked.
    timezone    TEXT,
    -- source=team_finished: whose ending starts this one.
    --
    -- A LOGICAL foreign key and not a declared one, for the reason `council_seats.agent_id` gives:
    -- the row it points at is deletable on somebody else's schedule, and here the intended
    -- behaviour is precisely to survive that. Deleting the team a rule watches DISARMS the rule
    -- rather than deleting it — the rule still says what its author wanted and has only lost its
    -- signal — and a declared key makes that inexpressible: the delete would fail, or take the rule
    -- with it. `team_trigger::create` checks the team exists at the one moment the answer is
    -- actionable.
    from_team   TEXT,
    -- source=email_triaged: which triage class.
    email_class TEXT,
    -- **Fixed text, written by the owner.** Not something a model composes: the alternative — team
    -- A's director writing team B's request — is one model instructing another with nobody in
    -- between, and a prompt injected into a web page would stop having to convince an agent to act
    -- and only have to convince it to ASK ANOTHER TEAM to act. Team B reads A's delivery through
    -- `read_team_file`, which is classified `ReadsUntrusted`; the INSTRUCTION comes from the owner.
    --
    -- Two substitutions and not a language: `{{workspace}}` and `{{request}}` of the run that fired
    -- it, for `team_finished` only.
    request     TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (team_id, name)
);

CREATE INDEX IF NOT EXISTS idx_team_triggers_armed ON team_triggers (enabled, source);

-- What each rule has already done. One row per rule, written the first time it is armed.
CREATE TABLE IF NOT EXISTS team_trigger_state (
    trigger_id    INTEGER PRIMARY KEY REFERENCES team_triggers(id),
    -- Kept VERBATIM and never re-parsed, for the reason `scheduler::RuleState` documents: it is the
    -- value the claim compare-and-sets against, and a round trip through `DateTime` can change the
    -- spelling without changing the instant — after which the claim matches nothing.
    last_fired_at TEXT NOT NULL,
    -- source=email_triaged: the newest message this rule has already answered. Initialised to the
    -- current maximum when the rule is ARMED, so arming one does not fire it once per message in
    -- the history of the mailbox.
    last_email_id INTEGER
);

-- What started a run, and what it belongs to.
--
-- `root_id` is `NOT NULL` and a run a person asked for points at ITSELF. A NULL meaning "I am the
-- root" would make every reader write `COALESCE(root_id, id)`, and the day somebody forgets, the
-- tree ceiling reads the wrong run — which is the one place in this design where being wrong costs
-- money without limit.
ALTER TABLE team_runs ADD COLUMN trigger_id INTEGER;
ALTER TABLE team_runs ADD COLUMN parent_id TEXT;
ALTER TABLE team_runs ADD COLUMN root_id TEXT NOT NULL DEFAULT '';
ALTER TABLE team_runs ADD COLUMN depth INTEGER NOT NULL DEFAULT 0;
UPDATE team_runs SET root_id = id WHERE root_id = '';

-- Whether the triggers downstream of this run have already been looked at.
--
-- A MARKER and not a derived condition, which is the lesson migration 0051 bought in `job.rs` and
-- `director_node` repeats one table over: "has this finished run fired its rules" cannot be
-- computed from the run's own state, because the answer has to survive the tick that computes it.
--
-- Every row that exists today is marked served, because they all finished before any rule could
-- exist. Without this line, arming the first `team_finished` rule would fire it once for every run
-- the department has ever completed.
ALTER TABLE team_runs ADD COLUMN triggers_served INTEGER NOT NULL DEFAULT 0;
UPDATE team_runs SET triggers_served = 1;

CREATE INDEX IF NOT EXISTS idx_team_runs_unserved
    ON team_runs (triggers_served, state);
CREATE INDEX IF NOT EXISTS idx_team_runs_root ON team_runs (root_id);

-- How many runs of one department may be in flight at once.
--
-- One by default: a department asked to do the same thing twice at once is usually a rule that
-- fired while the last run was still going, and the answer to that is to SKIP the window rather
-- than queue it. A queue is how a debt accumulates that the machine then tries to pay all at once.
ALTER TABLE teams ADD COLUMN max_live_runs INTEGER NOT NULL DEFAULT 1;
