-- Spec B (.ai/specs/2026-09-27-autopilot-juiz-resolve-bloqueios-design.md): D6.1, D11, D12, D13.
--
-- D11: an axis of its own, independent of spec A's `judge`, off by default, and photographed onto
-- each run at launch for the reason `judge` is: a run's rules do not change halfway through. The
-- E4 does NOT read the snapshot; it reads the project's value at the moment it fires (spec B D6).
ALTER TABLE autopilot_state ADD COLUMN judge_resolve TEXT NOT NULL DEFAULT 'off'
    CHECK (judge_resolve IN ('off', 'observe', 'enforce'));
ALTER TABLE runs ADD COLUMN judge_resolve TEXT NOT NULL DEFAULT 'off'
    CHECK (judge_resolve IN ('off', 'observe', 'enforce'));

-- D6.1: NULL means "I am the root", because a run's own id is only known after its INSERT. Every
-- continuation writes COALESCE(lineage_root_id, id) of the run it continues. Every lineage
-- question is asked as `id = ?1 OR lineage_root_id = ?1`, which the primary key and this index
-- answer between them; `COALESCE(lineage_root_id, id) = ?1` would scan the table.
ALTER TABLE runs ADD COLUMN lineage_root_id INTEGER;
CREATE INDEX runs_by_lineage_root ON runs (lineage_root_id) WHERE lineage_root_id IS NOT NULL;

-- D6.1: at most one correction per lineage, and the database is what says so. The first write of
-- the correction's transaction is the INSERT here (D6 "Ordem"), so the UNIQUE both takes the write
-- lock and refuses a concurrent second correction. `correction_run_id` is NULL until the same
-- transaction has created the run. `project_id` is here so the daily ceiling counts without a
-- join. `end_reported_at` is how the sweep of `judge/correction.rs` says each correction's
-- non-`completed` end exactly once.
CREATE TABLE judge_corrections (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    root_run_id INTEGER NOT NULL UNIQUE,
    origin_run_id INTEGER NOT NULL,
    correction_run_id INTEGER,
    project_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    end_reported_at TEXT
);
CREATE INDEX judge_corrections_by_project ON judge_corrections (project_id, created_at);
CREATE INDEX judge_corrections_by_correction ON judge_corrections (correction_run_id);

-- D12: an action a person declined, for the rest of that task. The hook reads this on every
-- `pending_approval`, so it is a table with an index and not a column on `proposals`: a column
-- would need a join through the lineage and a hash per proposal on the hook's path. It lives
-- outside the proposal's lifecycle: a proposal that expires (spec A D14) or is pruned does not
-- take the person's answer with it.
CREATE TABLE declined_actions (
    lineage_root_id INTEGER NOT NULL,
    tool_input_hash TEXT NOT NULL,
    proposal_id INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (lineage_root_id, tool_input_hash)
);

-- D13: one row per question put to the judge about a block, whatever came of it. The outcomes are
-- D3's, plus `moot`: a park question whose answer no longer mattered because spec A's judge decided
-- the call first. No FOREIGN KEY to `runs`, for the reason `judge_verdicts` has none.
-- `tool_input_digest` is the review unit (D11): the distinct action for E1/E3, and the literal
-- 'gate_failed' for E4, whose unit is (lineage, failed gate).
CREATE TABLE judge_resolutions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL,
    lineage_root_id INTEGER NOT NULL,
    event TEXT NOT NULL CHECK (event IN ('hard_deny', 'park', 'gate_failed')),
    event_ref INTEGER,
    tool_input_digest TEXT NOT NULL,
    p_off_task REAL,
    p_needed REAL,
    p_avoidable REAL,
    p_fixable REAL,
    default_outcome TEXT NOT NULL
        CHECK (default_outcome IN ('deny', 'warn', 'stop', 'park', 'explain', 'owner', 'correction')),
    judge_outcome TEXT
        CHECK (judge_outcome IN ('deny', 'warn', 'stop', 'park', 'explain', 'owner', 'correction')),
    final_outcome TEXT NOT NULL
        CHECK (final_outcome IN ('deny', 'warn', 'stop', 'park', 'explain', 'owner', 'correction', 'moot')),
    enforced INTEGER NOT NULL DEFAULT 0 CHECK (enforced IN (0, 1)),
    correction_run_id INTEGER,
    human_outcome TEXT
        CHECK (human_outcome IN ('deny', 'warn', 'stop', 'park', 'explain', 'owner', 'correction')),
    reviewed_at TEXT,
    input_tokens INTEGER,
    cost_usd REAL NOT NULL DEFAULT 0,
    latency_ms INTEGER,
    error TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX judge_resolutions_by_lineage ON judge_resolutions (lineage_root_id, event);
CREATE INDEX judge_resolutions_by_run ON judge_resolutions (run_id, id);
CREATE INDEX judge_resolutions_by_time ON judge_resolutions (created_at);
