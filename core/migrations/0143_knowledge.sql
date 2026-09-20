-- One store for everything the agent knows, read in four layers, and the layers are the nature of the
-- knowledge rather than four tables behind a facade. A facade could SAY it was one brain without
-- being one, which is today's disease under another name.
--
-- WHY A REBUILD AND NOT `ADD COLUMN`. 0088 put a five-value CHECK on `status` and this design uses
-- nine, and `0123_brain_openrouter.sql:3` states the constraint in its first line: "SQLite cannot
-- alter a CHECK constraint, so widening the set ... means rebuilding the table." `project_id` could
-- not have been dropped either way — it is indexed, and SQLite refuses DROP COLUMN on an indexed
-- column.
--
-- AND NO CHECKs THIS TIME. `0086_team_actions.sql:29` carries the house's reason and the sentence
-- that answers the obvious objection: "The constant is checked before the write, which is the
-- enforcement." The vocabularies live in Rust constants, with the second defence `refine.rs` already
-- practises — `Kind::parse` returns `Option` and the render `filter_map`s — so an unknown value is an
-- INVISIBLE row, never a counted one. Paying for a rebuild and leaving the same problem behind would
-- be paying without taking.

CREATE TABLE knowledge (
    id INTEGER PRIMARY KEY,

    -- The nature of what is known: 'semantic' | 'episodic' | 'procedural' | 'working'.
    layer TEXT NOT NULL,

    -- Scope in two columns so the hot-path index can serve it: 'machine' | 'project' | 'errand' |
    -- 'job', with `scope_id` NULL only for 'machine'. `errand` does NOT hang off `project`:
    -- `0074_errands.sql:19` gives an errand no `project_id` at all — it has `chat_key`, `brain`,
    -- `folder` — so an errand scope inherits from `machine` alone. `scope_id` is TEXT and
    -- polymorphic (a project id is TEXT, a job id is INTEGER), which is why there is no foreign key
    -- here and why something has to close a job's rows by hand.
    scope_kind TEXT NOT NULL,
    scope_id TEXT,

    -- Who knocked at the door: 'owner' | 'run' | 'consolidator'. Never read from the body.
    source TEXT NOT NULL,

    -- Which generator wrote this, for the consolidator only: 'gate' | 'refused-action' | 'shadow' |
    -- 'files' | 'context' | 'successors'. NULL everywhere else. It exists because the Learned view
    -- promises "how many come from each generator" and no other column can answer: `source` says only
    -- that the consolidator wrote it, `layer` is `episodic` on every one of them.
    generator TEXT,

    -- The rows that taught this, as a JSON array of TAGGED objects:
    -- [{"t":"run","id":900449},{"t":"job_item","id":41}]. Tagged because four different sections ask
    -- this column for four different payloads, and there the house posture "a malformed value reads
    -- as ABSENT" (`0063:19`) stops protecting: a WELL-FORMED value of the wrong shape reads as data.
    -- An unknown `t` is an invisible element.
    evidence TEXT,

    -- A SQL COUNT written by the consolidator, and nothing else. NULL is an assertion, a number is a
    -- measurement, and that distinction is what the weakened approval guarantee rests on. A run may
    -- never write it: a number written by a model is not a COUNT, and the fence that makes the
    -- consolidator's number trustworthy — there is no function — does not exist for a run.
    observations INTEGER,

    -- The normalised identity of what the row is ABOUT. One definition, two producers: the
    -- consolidator derives it from the measurement, the door from the normalised title. Two keys, of
    -- different scopes — see the two indexes below.
    fingerprint TEXT,

    -- The pointer, for the procedural layer: it points, it does not copy. `sidecar.rs:568` — "the copy
    -- that drifts is always the one nobody is reading".
    points_at TEXT,

    -- Validity, episodic only. A fact does not become false by being old, so the semantic and the
    -- procedural layers do not expire by time.
    expires_after_runs INTEGER,
    last_confirmed_at TEXT,

    -- The utility signal, durable ON THE KNOWLEDGE ROW and not derived from the trace, so the 90-day
    -- prune of `run_knowledge` can take the explanation of an old briefing without taking a signal.
    --
    -- THREE counters and not two. `runs.rs:1801` writes the gate verdict guarded by
    -- `AND status = 'running'`, so a cancelled run never reaches an outcome at all — and the rows it
    -- was shown would otherwise look failed for ever. So: `shown_count` counts briefings,
    -- `outcome_count` counts briefings whose run reached an outcome, `green_count` counts the green
    -- ones. NOT MEASURED is `outcome_count = 0`, and the rate is undefined when the denominator is.
    shown_count INTEGER NOT NULL DEFAULT 0,
    outcome_count INTEGER NOT NULL DEFAULT 0,
    green_count INTEGER NOT NULL DEFAULT 0,
    last_shown_at TEXT,

    -- 0088's `kind` survives as a sub-type, for the reason `refine.rs:24-25` gives: "an instruction
    -- changes what the node does, a fact changes what it believes".
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,

    -- Nine values, and the ninth is the one that needed arguing:
    --   'proposed' 'active' 'rejected' 'reverted' 'superseded'   (0088's five)
    --   'archived'  — merged into a lesson: out of candidacy, still auditable
    --   'closed'    — a working row whose job ended
    --   'expired'   — the measurement stopped confirming
    --   'live'      — a working row, reaching the next node of ITS OWN job only
    -- `live` exists because both obvious alternatives were worse. `active` means A PERSON APPROVED
    -- THIS, and reusing it here would make the approval test fail the working layer outright.
    -- `proposed` would have the briefing deliver unapproved material, which is precisely the danger
    -- the recall rule names — the rule would stay true to the letter and empty in practice. With
    -- `live`, `active` still means one thing.
    status TEXT NOT NULL,

    proposal_id INTEGER,
    supersedes INTEGER REFERENCES knowledge(id),
    origin_run_id INTEGER,

    created_at TEXT NOT NULL,
    activated_at TEXT,
    ended_at TEXT
);

-- 0088's rows, translated. `kind` becomes the layer it always was: a fact about the project is
-- semantic, the other three are how work is done here. `origin_run_id` decides the source, because
-- 0088 had no column for it and a run is the only thing that could have written a row without a
-- person: non-null is a run, null is the owner.
INSERT INTO knowledge (
    id, layer, scope_kind, scope_id, source, kind, title, body, status,
    proposal_id, supersedes, origin_run_id, created_at, activated_at, ended_at
)
SELECT
    id,
    CASE kind WHEN 'memory' THEN 'semantic' ELSE 'procedural' END,
    CASE WHEN project_id IS NULL THEN 'machine' ELSE 'project' END,
    project_id,
    CASE WHEN origin_run_id IS NULL THEN 'owner' ELSE 'run' END,
    kind, title, body, status,
    proposal_id, supersedes, origin_run_id, created_at, activated_at, ended_at
FROM refinements;

-- The hot path: what is in force for this scope. It is the INDEX's key and not the reading's — the
-- order of the rendered block is by scope, and it is decided elsewhere. Serves the inheritance chain
-- as a search over the scopes of that chain, `scope_id IS NULL` for machine included.
CREATE INDEX idx_knowledge_scope ON knowledge (scope_kind, scope_id, status, layer, kind);

-- The WITHIN-A-SCOPE key: do not repeat, do not resurrect, do not re-measure into a second row. An
-- earlier draft indexed `(fingerprint, status)` and justified it with THESE questions, which belong
-- to another key — leaving one of the two keys with no index at all, and serving instead exactly the
-- cross-project read 0088 warns about: "one project's blind spot become the house's" (0088:28).
CREATE INDEX idx_knowledge_scope_fingerprint ON knowledge (scope_kind, scope_id, fingerprint);

-- The BETWEEN-PROJECTS key, and ONLY the promotion to machine scope. The restriction to the
-- consolidator's own rows lives on the query rather than here: without it, a run declaring the same
-- sentence in three projects would promote itself, and the promotion would stop being measured
-- repetition.
CREATE INDEX idx_knowledge_fingerprint ON knowledge (fingerprint, status);

-- Every status change, with who and why, exactly as `refinement_events` did. Rebuilt rather than
-- renamed because its foreign key names a table that is about to be dropped.
CREATE TABLE knowledge_events (
    id INTEGER PRIMARY KEY,
    knowledge_id INTEGER NOT NULL REFERENCES knowledge(id),
    from_status TEXT,
    to_status TEXT NOT NULL,
    note TEXT,
    at TEXT NOT NULL
);

INSERT INTO knowledge_events (id, knowledge_id, from_status, to_status, note, at)
SELECT id, refinement_id, from_status, to_status, note, at FROM refinement_events;

CREATE INDEX idx_knowledge_events_knowledge ON knowledge_events (knowledge_id, id);

-- What a briefing decided, and why. Five signals in five columns: the debugging view exists to answer
-- WHICH signal elected the row, and one scalar says it won without saying why.
--
-- It is the TRACE and not the signal. If recency and utility lived here, the prune would destroy them
-- at 90 days; they live on the knowledge row. Pruning loses the explanation of an old briefing and
-- loses no signal at all.
--
-- `run_id` is NOT NULL and references `runs`, which is the whole reach of this table: a chat turn, an
-- assistant turn and a voice turn are not rows of `runs`, and have no outcome to credit either,
-- because green is a gate or a run's status and a conversation has neither. In those contexts the
-- briefing is delivered and leaves no trace. A known asymmetry, said out loud rather than hidden.
CREATE TABLE run_knowledge (
    run_id INTEGER NOT NULL REFERENCES runs(id),
    knowledge_id INTEGER NOT NULL REFERENCES knowledge(id),

    -- Whether it made it into the block, or was a candidate that lost.
    shown INTEGER NOT NULL,

    s_fts REAL NOT NULL,
    s_scope REAL NOT NULL,
    s_structure REAL NOT NULL,
    s_recency REAL NOT NULL,
    s_use REAL NOT NULL,

    at TEXT NOT NULL,

    -- What makes crediting idempotent, and it is not the primary key. The PK prevents a second trace
    -- ROW; it does not prevent a second run of `UPDATE knowledge SET shown_count = shown_count + 1`.
    -- The credit pass runs `WHERE credited_at IS NULL` and stamps this.
    credited_at TEXT,

    PRIMARY KEY (run_id, knowledge_id)
);

CREATE INDEX idx_run_knowledge_knowledge ON run_knowledge (knowledge_id, at);

-- Terms only; the text stays in `knowledge`. `knowledge.id` is an INTEGER PRIMARY KEY, hence a rowid
-- alias, so it serves as content_rowid. The fifth index of this kind (0035, 0047, 0057, 0058), and
-- deliberately not a new mechanism.
CREATE VIRTUAL TABLE knowledge_fts USING fts5(
    title,
    body,
    content='knowledge',
    content_rowid='id',
    tokenize='unicode61'
);

-- Backfill every row written before this migration — including the ones translated above, which the
-- triggers below were not yet there to see. Without this the migrated corpus is invisible to the FTS
-- signal, which is the FIRST of the five, and it would be invisible silently.
INSERT INTO knowledge_fts (rowid, title, body) SELECT id, title, body FROM knowledge;

CREATE TRIGGER knowledge_fts_insert AFTER INSERT ON knowledge BEGIN
    INSERT INTO knowledge_fts (rowid, title, body) VALUES (new.id, new.title, new.body);
END;

-- Not a precaution, for the reason `0057:9-12` gives one table over: `project_exit` DELETEs rows of
-- `knowledge` when a project is forgotten, and an index that keeps the terms of a deleted row is that
-- bug again.
CREATE TRIGGER knowledge_fts_delete AFTER DELETE ON knowledge BEGIN
    INSERT INTO knowledge_fts (knowledge_fts, rowid, title, body)
    VALUES ('delete', old.id, old.title, old.body);
END;

CREATE TRIGGER knowledge_fts_update AFTER UPDATE ON knowledge BEGIN
    INSERT INTO knowledge_fts (knowledge_fts, rowid, title, body)
    VALUES ('delete', old.id, old.title, old.body);
    INSERT INTO knowledge_fts (rowid, title, body) VALUES (new.id, new.title, new.body);
END;

DROP INDEX idx_refinement_events_refinement;
DROP TABLE refinement_events;
DROP INDEX idx_refinements_active;
DROP TABLE refinements;
