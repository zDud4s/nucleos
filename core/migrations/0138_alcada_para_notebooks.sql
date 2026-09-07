-- The third tool that writes files may now be named in a project's write rules.
--
-- `0131` opened this column with `CHECK (tool IN ('', 'Edit', 'Write'))` and said why the list was
-- closed: "o conjunto de ferramentas que escrevem ficheiros é fechado no código que a lê". That
-- sentence was true and stayed true; what changed on 2026-09-08 is the code it points at.
-- `classifier::WRITE_TOOLS` gained `NotebookEdit`, so `write_denied_by_project` can decide a
-- notebook write, and this CHECK became the only thing left refusing to STORE the rule that
-- decision would read. A rule that cannot be written for a tool the classifier now governs is the
-- mirror image of 0131's own worry — not a refusal stored and never applied, but a refusal the
-- owner cannot express at all.
--
-- **Rebuilt rather than altered, because SQLite cannot change a CHECK in place.** The twelve-step
-- procedure, minus the steps that do not apply here: this table has no foreign keys pointing at it
-- and no triggers or views, so the copy is a plain INSERT ... SELECT and the only thing that has to
-- be put back by hand is the index.
--
-- Columns are listed on BOTH sides of the copy rather than relying on `SELECT *`. `tool` arrived by
-- `ALTER TABLE ADD COLUMN` and therefore sits last, which is an ordering nobody chose and nobody
-- should depend on; naming the columns makes this migration correct whatever order the old table
-- happens to be in.
CREATE TABLE project_shell_rules_new (
  id         INTEGER PRIMARY KEY,
  project_id TEXT NOT NULL,
  prefix     TEXT NOT NULL CHECK (prefix <> ''),
  verdict    TEXT NOT NULL CHECK (verdict IN ('allow', 'deny')),
  note       TEXT,
  created_at TEXT NOT NULL,
  -- `''` and never NULL, for the reason 0131 spells out at length: SQLite treats every NULL in a
  -- UNIQUE index as distinct from every other, so a nullable `tool` would let one project hold two
  -- verdicts for the same shell prefix and the `ON CONFLICT` below would never fire.
  tool       TEXT NOT NULL DEFAULT ''
             CHECK (tool IN ('', 'Edit', 'Write', 'NotebookEdit'))
);

INSERT INTO project_shell_rules_new (id, project_id, prefix, verdict, note, created_at, tool)
  SELECT id, project_id, prefix, verdict, note, created_at, tool FROM project_shell_rules;

DROP TABLE project_shell_rules;
ALTER TABLE project_shell_rules_new RENAME TO project_shell_rules;

-- Recreated because `DROP TABLE` took it. Byte-identical to 0131's, deliberately: `project_id` at
-- the head so this index also answers "every rule of this project", and `tool` before `prefix` so
-- the shell rules (`tool = ''`) stay contiguous for the walk a command line does.
CREATE UNIQUE INDEX project_shell_rules_scope ON project_shell_rules (project_id, tool, prefix);
