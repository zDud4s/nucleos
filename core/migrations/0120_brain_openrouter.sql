-- The third route joins the two `chats` already has.
--
-- SQLite cannot alter a CHECK constraint, so widening the set of brains a conversation may name
-- means rebuilding the table, the same way `0073_vcs_escalated.sql` and `0086_team_actions.sql`
-- rebuilt theirs to widen a `status`. This one only touches `brain`.
--
-- The column list below is the SCHEMA AS OF THIS MIGRATION, not `0061_chats.sql`'s. `chats` was
-- created there with six columns and has since been altered by ten further migrations —
-- `0091` (`handover`), `0106` (`plan_only`), `0110` (`model`, `effort`), `0111` (`extra_dirs`,
-- `turn_budget_usd`, `fallback_model`), `0112` (`agents`), `0113` (`system_prompt`,
-- `denied_tools`), `0114` (`cleared_after_run_id`), `0116` (`context_window`), plus whichever two
-- put `last_seen_turn_id`, `cwd` and `ide_session_id` on the row before them. A rebuild written
-- from `0061`'s six columns would silently drop the other fourteen, and everyone's data with
-- them. Whoever writes the migration after this one and needs to rebuild `chats` again: read the
-- table's ACTUAL columns off a fresh database first, the way this one did, and do not trust the
-- column list of whichever migration you find first with `CREATE TABLE chats` in it.
--
-- No `-- no-transaction` and no `PRAGMA foreign_keys` here, unlike a rebuild that has to survive
-- one. Nothing in this schema declares `REFERENCES chats`, so there is nothing for the pragma to
-- protect against and no foreign key to drop and lose. Reaching for either on a table nothing
-- points at is how a safe rebuild turns into a dangerous one.
CREATE TABLE chats_new (
  chat_id              TEXT PRIMARY KEY,
  title                TEXT,
  brain                TEXT NOT NULL DEFAULT 'cloud' CHECK (brain IN ('cloud', 'local', 'openrouter')),
  created_at           TEXT NOT NULL,
  archived_at          TEXT,
  last_seen_turn_id    INTEGER,
  cwd                  TEXT,
  ide_session_id       TEXT,
  handover             TEXT,
  plan_only            INTEGER NOT NULL DEFAULT 0,
  model                TEXT,
  effort               TEXT,
  extra_dirs           TEXT,
  turn_budget_usd      REAL,
  fallback_model       TEXT,
  agents               TEXT,
  system_prompt        TEXT,
  denied_tools         TEXT,
  cleared_after_run_id INTEGER,
  context_window       INTEGER
);

INSERT INTO chats_new
SELECT chat_id, title, brain, created_at, archived_at, last_seen_turn_id, cwd, ide_session_id,
       handover, plan_only, model, effort, extra_dirs, turn_budget_usd, fallback_model, agents,
       system_prompt, denied_tools, cleared_after_run_id, context_window
  FROM chats;

DROP TABLE chats;
ALTER TABLE chats_new RENAME TO chats;

-- Recreated because the table it indexed is gone. Identical to the one this replaces, deliberately
-- — this migration widens `brain` and nothing else, and an index that quietly changed shape here
-- would be the hardest kind of change to find later.
CREATE INDEX chats_by_activity ON chats (archived_at, created_at);
