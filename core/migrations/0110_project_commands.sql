-- The commands a project declares about ITSELF, and the last thing each of them said.
--
-- Declaration and not detection, which is spec §8's decision and worth the sentence it costs.
-- Reading `package.json`, Cargo aliases and Makefile targets at display time gives a list that
-- changes on its own when somebody edits a file, in an order nobody chose, with no place to mark
-- which one is the gate. Detection belongs at project entry, where it PROPOSES; what survives is a
-- row somebody agreed to.
--
-- A table rather than a block in `.ai/autopilot.yaml`, and the reason is the overlay. §8's model is
-- "the workflow declares, the project overrides" — the same shape already used for models and for
-- nodes, so one concept instead of two — and an overlay needs two sources with a resolution rule
-- between them. A file has one. The rules file also travels with nobody: it is gitignored,
-- per-developer configuration, and a workflow's commands must arrive with the workflow.
--
-- 0110. Free on every branch in this repository today, which is exactly the fact 0109's header says
-- is a fact about a moment — if master moves first, this renumbers, and renumbering an unmerged
-- migration is routine. What is not routine is running one against a database you rely on.
CREATE TABLE project_commands (
  id          INTEGER PRIMARY KEY,
  project_id  TEXT NOT NULL,
  -- What a person types or clicks. Unique per (project, source) so that a project row can SHADOW a
  -- workflow row of the same name, which is the whole of the overlay.
  name        TEXT NOT NULL,
  -- A program and its arguments, split the way `gate::split_command` splits them. NOT a shell line:
  -- `&&` arrives as an ordinary argument, and a caller that wants shell operators names a shell.
  -- This is the same rule `gate_command` in the rules file already follows, and keeping the two
  -- alike is what lets one runner serve both.
  command     TEXT NOT NULL,
  -- Relative to the project root, forward slashes. NULL is the root itself. Checked to resolve
  -- INSIDE the project both when it is declared and again when it is run, because a directory that
  -- existed on Tuesday can be gone on Wednesday.
  cwd         TEXT,
  -- Whether this command's last result is a fact about the project rather than merely the last
  -- thing that happened.
  --
  -- **This is not `.ai/autopilot.yaml`'s `gate_command`, and the two must not be wired together by
  -- somebody who reads only the word.** That one is what judges an autonomous run inside its own
  -- worktree, before the run is allowed to have counted; this is a command a person runs against
  -- the project itself. What this flag buys is a place: a gate belongs in the bar at the foot of
  -- the page because "is it green" is something you want without asking, and everything else
  -- belongs in the palette because it is a verb you go looking for. That rule is the only thing
  -- standing between this feature and a row of fourteen buttons with no owner.
  is_gate     INTEGER NOT NULL DEFAULT 0,
  -- The exit code that counts as passing. 0 for almost everything; a linter that exits 1 for
  -- warnings is why it is a column.
  --
  -- It can never turn a measurement that did not happen into a pass. A process killed by a signal
  -- has no exit code at all -- the OOM killer reaping a suite is the ordinary case -- and no value
  -- here matches it, which is the property `classify_exit` already gives `gate.rs` and which
  -- `project_commands.rs` asserts rather than assumes.
  pass_exit_code INTEGER NOT NULL DEFAULT 0,
  -- `person` or `agent`. Whether an autonomous run may execute this, or only somebody at the
  -- keyboard.
  --
  -- Defaults to `person`, which is the same default-deny direction `auth.rs` takes: a command added
  -- without anybody thinking about this question must not thereby become something a run can
  -- execute on its own.
  runnable_by TEXT NOT NULL DEFAULT 'person',
  -- `project` or `workflow`. Only `project` has a writer today; the library that installs a
  -- workflow's commands is a later slice, and it plugs in here as ROWS rather than as a new shape.
  source      TEXT NOT NULL DEFAULT 'project',
  created_at  TEXT NOT NULL,

  -- The last run, on the row rather than in a history table.
  --
  -- A history of ad-hoc command runs is not a question anything asks: the readings panel already
  -- counts gate verdicts across runs over thirty days, which is the trend, and what a button needs
  -- beside it is whether the thing is green NOW. One row also makes "is it already running" a
  -- lookup instead of a MAX() over a table.
  --
  -- `last_outcome` is one of `running`, `passed`, `failed`, `errored`, or NULL for a command nobody
  -- has run. Five states and not four: never-run is not the same as failed, and `errored` -- the
  -- command could not start -- is not the same as `failed` either, for the reason `GateOutcome`
  -- spells out. A missing binary must not read as broken tests.
  --
  -- `running` is reconciled only by a daemon restart, exactly like `runs.status`. A crash mid-run
  -- otherwise leaves a row claiming work that no process is doing, which is the most confident
  -- possible way to be wrong.
  last_started_at TEXT,
  last_ended_at   TEXT,
  last_outcome    TEXT,
  last_exit_code  INTEGER,
  -- The diagnostic tail, capped by `gate.rs`'s own ceiling before it ever reaches here.
  last_output     TEXT
);

CREATE UNIQUE INDEX project_commands_name ON project_commands (project_id, source, name);
CREATE INDEX project_commands_by_project ON project_commands (project_id);
