-- Spec backlog: a registry of the spec files under `.ai/specs`, their acceptance criteria, the
-- dependencies between specs and the questions put to the owner.
--
-- The registry only reads spec files (`content_sha`); it never writes them. `kind`, `status` and the
-- other closed vocabularies are validated in Rust, not here, like `tree_jobs.state`. Ids are TEXT and
-- there are no foreign keys to the tree-job tables, which land in a parallel phase. `created_at`,
-- `updated_at`, `checked_at` and `answered_at` are RFC 3339 text, the house convention.
CREATE TABLE specs (
    id TEXT PRIMARY KEY,
    project_id TEXT,
    path TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    content_sha TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'unknown',
    status TEXT NOT NULL DEFAULT 'discovered',
    status_reason TEXT,
    tree_id TEXT,
    last_audited_sha TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX specs_project_path ON specs (project_id, path);
CREATE INDEX specs_status ON specs (status);
CREATE INDEX specs_tree ON specs (tree_id);

CREATE TABLE spec_criteria (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    spec_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    text TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'behavior',
    verify_hint TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    evidence TEXT,
    reviewer_run_id TEXT,
    spec_sha TEXT NOT NULL,
    checked_at TEXT
);

CREATE UNIQUE INDEX spec_criteria_spec_ordinal ON spec_criteria (spec_id, ordinal);
CREATE INDEX spec_criteria_status ON spec_criteria (spec_id, status);

CREATE TABLE spec_deps (
    spec_id TEXT NOT NULL,
    depends_on TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'extracted' CHECK (source IN ('extracted', 'owner')),
    PRIMARY KEY (spec_id, depends_on)
);

CREATE INDEX spec_deps_depends_on ON spec_deps (depends_on);

CREATE TABLE spec_questions (
    id TEXT PRIMARY KEY,
    spec_id TEXT NOT NULL,
    question TEXT NOT NULL,
    answer TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    created_at TEXT NOT NULL,
    answered_at TEXT
);

CREATE INDEX spec_questions_spec_status ON spec_questions (spec_id, status);

-- The owner's single list of decisions. SQLite cannot create a view over a table that does not exist
-- yet, and the tree-job `decisions` table is not here, so this view covers `spec_questions` only; the
-- union with `decisions` is done in Rust (`spec_backlog::store`) once that table exists.
CREATE VIEW owner_decisions AS
SELECT
    'spec_question' AS origin,
    id AS id,
    spec_id AS subject,
    question AS question,
    answer AS answer,
    status AS status,
    created_at AS created_at,
    answered_at AS answered_at
FROM spec_questions;
