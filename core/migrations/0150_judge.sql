-- Spec A (.ai/specs/2026-09-26-autopilot-modo-juiz-design.md), D2 and D12.
--
-- The judge is an axis of its own, orthogonal to `autopilot_state.mode`: a variant of `mode` would
-- have to be taught to six places that match on it, and a new column avoids rebuilding the CHECK
-- on `mode`. The project's value is COPIED onto each run at launch, so a run's rules do not change
-- halfway through (the reason `permission_mode` is a snapshot too). Nothing is rebuilt; every
-- existing row reads `off`, which is today's behaviour.
ALTER TABLE autopilot_state ADD COLUMN judge TEXT NOT NULL DEFAULT 'off'
    CHECK (judge IN ('off', 'observe', 'enforce'));
ALTER TABLE runs ADD COLUMN judge TEXT NOT NULL DEFAULT 'off'
    CHECK (judge IN ('off', 'observe', 'enforce'));

-- One row per consultation, whatever came of it (D12). `band` is the judge's raw band, so a
-- capped approval still reads `allow` (D11 measures the opinion, not the effect) and `capped`
-- says it did not stand. `band` and the probabilities are NULL when the call failed, and `error`
-- says why (D10). The distinct action is `(tool_name, tool_input_digest)`, the unit
-- `shadow.rs` counts readiness in.
CREATE TABLE judge_verdicts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL,
    shadow_decision_id INTEGER,
    tool_name TEXT NOT NULL,
    tool_input_digest TEXT NOT NULL,
    action_class TEXT NOT NULL,
    classifier_decision TEXT NOT NULL,
    judge TEXT NOT NULL CHECK (judge IN ('observe', 'enforce')),
    model TEXT NOT NULL,
    questions_version INTEGER NOT NULL,
    p_in_scope REAL,
    p_safe REAL,
    p REAL,
    band TEXT CHECK (band IN ('allow', 'middle', 'deny')),
    capped INTEGER NOT NULL DEFAULT 0 CHECK (capped IN (0, 1)),
    final_decision TEXT NOT NULL,
    enforced INTEGER NOT NULL DEFAULT 0 CHECK (enforced IN (0, 1)),
    counted_as_denial INTEGER NOT NULL DEFAULT 0 CHECK (counted_as_denial IN (0, 1)),
    latency_ms INTEGER,
    input_tokens INTEGER,
    cost_usd REAL NOT NULL DEFAULT 0,
    error TEXT,
    human_verdict TEXT CHECK (human_verdict IN ('approve', 'reject')),
    reviewed_at TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX judge_verdicts_by_run ON judge_verdicts (run_id, id);
CREATE INDEX judge_verdicts_by_action ON judge_verdicts (tool_name, tool_input_digest);
CREATE INDEX judge_verdicts_by_time ON judge_verdicts (created_at);
