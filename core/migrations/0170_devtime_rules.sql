-- Devtime rules (sub-project 2): what the rule catalogue concluded about each session's time.
-- `devtime_store.rs` is the only SQL owner of these tables and columns.
--
-- No CHECK constraints, for the house's reason (0143, 0160, 0164): the vocabularies (waste, lever,
-- level, confidence, verified, feedback verdict and cause, finding scope, bg status) are Rust
-- constants checked before the write.
--
-- Every timestamp is UTC `%Y-%m-%dT%H:%M:%S%.3fZ` TEXT. Rule results are derived data, replaced
-- wholesale per session and stamped with `rules_version` (a fingerprint of the engine, the parser,
-- every rule's version and the rules configuration) so a changed rule recomputes instead of mixing eras.
-- Feedback is NOT derived: it is the owner's ground truth and survives every recompute, keyed by the
-- stable `finding_key`.

ALTER TABLE devtime_sessions ADD COLUMN rules_version TEXT;
ALTER TABLE devtime_sessions ADD COLUMN rules_at TEXT;

ALTER TABLE devtime_attempts ADD COLUMN bg_status TEXT;
ALTER TABLE devtime_attempts ADD COLUMN refs_in TEXT NOT NULL DEFAULT '[]';
ALTER TABLE devtime_attempts ADD COLUMN refs_out TEXT NOT NULL DEFAULT '[]';

ALTER TABLE devtime_spans ADD COLUMN lever TEXT;
ALTER TABLE devtime_spans ADD COLUMN finding_key TEXT;

CREATE TABLE devtime_findings (
    id INTEGER PRIMARY KEY,
    finding_key TEXT NOT NULL,
    project_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    rule_version INTEGER NOT NULL,
    level TEXT NOT NULL,
    waste TEXT NOT NULL,
    lever TEXT NOT NULL,
    confidence TEXT NOT NULL,
    lane TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT NOT NULL,
    cost_ms INTEGER NOT NULL,
    count INTEGER NOT NULL DEFAULT 1,
    attempt_ids TEXT NOT NULL DEFAULT '[]',
    sessions TEXT NOT NULL DEFAULT '[]',
    rules_version TEXT NOT NULL,
    parser_version INTEGER NOT NULL
);

CREATE TABLE devtime_attempt_marks (
    attempt_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    waste TEXT,
    rule_id TEXT,
    lever TEXT,
    finding_key TEXT,
    verified TEXT NOT NULL,
    verified_by TEXT,
    rules_version TEXT NOT NULL
);

CREATE TABLE devtime_feedback (
    id INTEGER PRIMARY KEY,
    finding_key TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    session_id TEXT,
    verdict TEXT NOT NULL,
    cause TEXT,
    rule_version INTEGER,
    marked_at TEXT NOT NULL
);

CREATE TABLE devtime_turn_stats (
    session_id TEXT NOT NULL,
    turn_seq INTEGER NOT NULL,
    project_id TEXT NOT NULL,
    turn_class TEXT NOT NULL,
    started_at TEXT NOT NULL,
    calls INTEGER NOT NULL,
    active_ms INTEGER NOT NULL,
    explained_ms INTEGER NOT NULL,
    rules_version TEXT NOT NULL,
    PRIMARY KEY (session_id, turn_seq)
);

CREATE UNIQUE INDEX idx_devtime_findings_key ON devtime_findings (finding_key);
CREATE INDEX idx_devtime_findings_session ON devtime_findings (session_id, rule_id);
CREATE INDEX idx_devtime_findings_rule ON devtime_findings (project_id, rule_id, started_at);
CREATE INDEX idx_devtime_attempt_marks_session ON devtime_attempt_marks (session_id);
-- One standing mark per case: a re-mark replaces the earlier one.
CREATE UNIQUE INDEX idx_devtime_feedback_key ON devtime_feedback (finding_key);
CREATE INDEX idx_devtime_feedback_rule ON devtime_feedback (rule_id);
CREATE INDEX idx_devtime_turn_stats_project ON devtime_turn_stats (project_id, turn_class, started_at);
CREATE INDEX idx_devtime_sessions_rules ON devtime_sessions (dirty, rules_version);
