-- When an errand acts on its own: a cron, a prompt, and the state of both.
--
-- A project's rules live in `.ai/autopilot.yaml` inside its repository, which is exactly why this
-- table has to exist: an errand has no repository. "Every morning check whether new listings
-- appeared" has nowhere to be written down, and a piece of standing work that only moves when
-- somebody types into a topic is not standing work.
--
-- WHY THE STATE IS IN THIS ROW AND NOT IN `scheduler_state`. That table is separate from the rules
-- for one reason: the rules are in a FILE, so there is nothing to hang the state on and the pair
-- (project_id, rule_name) is the only handle available. Here the rule IS a row, so the state
-- belongs to it — one write claims a window instead of two statements against two tables that can
-- disagree about whether a rule still exists. It also means deleting a rule takes its state with
-- it, where the project path leaves an orphan row behind for a rule nobody can find any more.
--
-- What is deliberately NOT here: `last_head_sha`, and everything downstream of it. That column
-- answers "did the repository move under this window", and it is the catch-up re-triage warning in
-- `scheduler.rs` that reads it. An errand has no HEAD, so a catch-up here is late and nothing else.
CREATE TABLE errand_rules (
  id          INTEGER PRIMARY KEY,
  -- ON DELETE CASCADE, unlike `errand_artifacts`' reference to the same table, and for the opposite
  -- reason: an artifact is work that outlives the asking, while a rule is a standing instruction to
  -- start MORE work. An orphan rule is not a record of anything — it is a cron with no owner.
  errand_id   INTEGER NOT NULL REFERENCES errands(id) ON DELETE CASCADE,
  -- What a person calls it, and what they say to delete it. Unique within the errand and not
  -- globally: "manhã" is what everyone calls the morning rule, and two errands both having one is
  -- not an ambiguity anybody experiences.
  name        TEXT NOT NULL,
  -- Validated before it is written (`errands::create_rule`), which is the one thing an errand's
  -- rules can do that a project's cannot. A project's YAML is read long after whoever wrote it has
  -- gone, so an unreadable cron is armed anyway and announced once to the feed; a rule arrives here
  -- over a route, with somebody still holding the keyboard.
  cron        TEXT NOT NULL,
  -- The prompt this fires with. It is the owner's own words, written when the rule was created,
  -- which is why a scheduled turn does not begin tainted: nothing a stranger said is in here. What
  -- taints such a turn is the notebook it is handed and the web it goes and reads.
  prompt      TEXT NOT NULL,
  -- Absent means UTC, exactly as `ScheduleRule.timezone` does. An unknown name never reaches this
  -- column: it is refused at creation rather than silently read as UTC, because a rule that fires
  -- an hour off looks like a rule that worked.
  timezone    TEXT,
  -- The scheduler's window claim, compare-and-set against verbatim. Set to the creation instant so
  -- a new rule is armed rather than owed every window since the epoch — a rule written at 21:30
  -- with `0 8 * * *` must wait for the morning, not fire on the spot.
  last_fired_at TEXT NOT NULL,
  -- The day `fires_today` counts for, so the daily cap resets by comparison instead of by a sweep
  -- somebody has to run at midnight. Same mechanism as migration 0026, same reason.
  fires_date  TEXT,
  fires_today INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  UNIQUE (errand_id, name)
);

-- The scheduler's read is "every rule of every errand still answering", so the index that serves it
-- is on the owner. Small table, and that is not an argument against: it is read twice a minute for
-- as long as the daemon runs.
CREATE INDEX errand_rules_by_errand ON errand_rules (errand_id);
