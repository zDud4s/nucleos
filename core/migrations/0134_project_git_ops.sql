-- Which git operations the queue may perform for this project's autonomous runs without stopping
-- to ask a person. Presence is the grant: a row means "declared", and there is no verdict column,
-- because there is nothing to say beyond yes. Absent, the behaviour is what it was — the run stops
-- and a person is asked.
--
-- `op_kind` is `vcs::Op::kind()`'s own spelling and nothing else: merge, push, tag, fetch, rebase,
-- branch-delete. The route refuses anything the catalogue does not name, so a row here is always
-- an operation this build can construct.
CREATE TABLE project_git_ops (
  id         INTEGER PRIMARY KEY,
  project_id TEXT NOT NULL,
  op_kind    TEXT NOT NULL CHECK (op_kind <> ''),
  created_at TEXT NOT NULL
);
-- One row per operation per project, and `project_id` leads so this index also answers "every
-- declaration of this project" — the only other read there is. The three tables of `0128` take
-- this same shape, and the symmetry is the point.
CREATE UNIQUE INDEX project_git_ops_kind ON project_git_ops (project_id, op_kind);
