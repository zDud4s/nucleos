-- The conversation stops having two states and starts having five.
--
-- `chats.plan_only` (0106) could say exactly one thing: plan, or don't. Everything the CLI's own
-- permission surface can express — ask about everything, accept edits, judge for yourself, don't
-- ask at all — had nowhere to live. This is where it lives.

-- The remembered position of the selector: what the NEXT message will run under.
--
-- Named `permission_mode` and not `mode` because `runs.mode` already exists, means something else
-- entirely (`assistant`/`shadow`/`worktree`/`real`), and is read three lines away from where this
-- one will be read (`hooks.rs`, the per-tool-call SELECT).
--
-- The CHECK is now or never. Adding one later means REBUILDING `chats`, which is precisely what
-- the header of `0123_brain_openrouter.sql` warns has already gone wrong once here. With it, the
-- reader's "unknown spelling falls back to auto" rule is reachable only by rows older than this
-- migration, never by a write this application made.
ALTER TABLE chats ADD COLUMN permission_mode TEXT NOT NULL DEFAULT 'auto'
  CHECK (permission_mode IN ('manual', 'accept_edits', 'plan', 'auto', 'bypass'));

-- The mode this turn STARTED with. NULL for everything that is not a chat turn run by the CLI —
-- an autopilot run, a shadow run, and every turn answered by the local brain, which never builds a
-- `RunRequest` and never fires a PreToolUse hook. `permission_mode IS NULL` therefore reads as
-- "no CLI was involved", which is a cleaner invariant than a column filled with a value nothing
-- reads.
--
-- No CHECK here, deliberately, and the asymmetry with `chats` above is the point: NULL is
-- MEANINGFUL on this column, and one place writes it.
--
-- It is a snapshot and not a second copy free to disagree: the hook reads it again, many times,
-- long after the turn began — once per tool call. Without it, moving the menu halfway through a
-- turn would change the rules underneath a turn already running.
ALTER TABLE runs ADD COLUMN permission_mode TEXT;

-- Who judges, when the rules recognise nothing.
--
-- Same idiom as `0128_project_alcada.sql`: one table per project, a CHECK on the column that is an
-- enum, and no rows at all meaning the behaviour there has always been.
CREATE TABLE project_judge (
  -- `project_id` is the key, with no surrogate `id`. The three tables in `0128` carry one, plus a
  -- unique index over it, because they are one-to-many — many prefixes, many `op_kind`s, many
  -- branches per project. This one is one-to-one: a project has a judge. Copying the shape would
  -- mint a key nobody uses and force a `UNIQUE INDEX (project_id)` to say what the primary key
  -- already says. `0128` asks for symmetry among its three and is right; this is a different
  -- species, and the divergence is stated here rather than discovered by somebody.
  project_id TEXT PRIMARY KEY,
  -- NULL is the explicit refusal to have a judge on this project: `auto` goes back to being
  -- rules-only. NO ROW at all means the daemon's configured route applies. Three states, and a
  -- nullable column says them in the shape the rest of this codebase already uses to say them.
  --
  -- `cloud` is NOT in the list, and that is not an omission. `assistant_for` refuses `Brain::Cloud`
  -- before doing anything at all: that route answers through the CLI, not through this factory, and
  -- `LocalAssistant` is built from a `LocalChat`. Worse than being inert — a cloud judge would
  -- launch a CLI whose own tool calls would re-enter PreToolUse. It is the single configuration
  -- that would reintroduce reentrancy on a path that otherwise has none.
  brain      TEXT CHECK (brain IS NULL OR brain IN ('local', 'openrouter')),
  model      TEXT,
  created_at TEXT NOT NULL
);

-- The two states `plan_only` could hold, carried onto the column that replaces it. Everything else
-- keeps the column default, `auto` — which is what a rooted conversation has always done: allow
-- what passes the classifier, ask about what does not.
UPDATE chats SET permission_mode = 'plan' WHERE plan_only = 1;

-- `chats.plan_only` is DEAD from here on. It is left standing rather than dropped for the reason
-- the CHECK above is written now: `ALTER TABLE ADD COLUMN` does not rebuild the table, and
-- `DROP COLUMN` does — and `0123`'s header is the record of what rebuilding this particular table
-- costs when the column list is written from memory. Nothing reads it after the migration that
-- follows this one; nothing should start.
