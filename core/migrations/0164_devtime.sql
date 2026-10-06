-- Devtime ingestion: what the coding agent's transcripts say about how the owner's time and tokens go.
-- `devtime_store.rs` is the only SQL owner of these tables.
--
-- No CHECK constraints, on purpose and for the house's reason (`0143_knowledge.sql`, `0160`): the
-- vocabularies (file status, cwd kind, span kind, marker kind, outcome, confidence) are Rust
-- constants checked before the write.
--
-- Every timestamp is UTC `%Y-%m-%dT%H:%M:%S%.3fZ` TEXT, so string order is time order and MIN/MAX on
-- the column are right.

CREATE TABLE devtime_files (
    path TEXT PRIMARY KEY,
    session_id TEXT,
    lane TEXT,
    offset INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0,
    mtime TEXT,
    status TEXT NOT NULL DEFAULT 'ok',
    lines_read INTEGER NOT NULL DEFAULT 0,
    lines_failed INTEGER NOT NULL DEFAULT 0,
    unknown_records INTEGER NOT NULL DEFAULT 0,
    parser_version INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE devtime_cwd_map (
    cwd TEXT PRIMARY KEY,
    project_id TEXT,
    worktree TEXT,
    kind TEXT NOT NULL,
    resolved_at TEXT NOT NULL
);

CREATE TABLE devtime_sessions (
    session_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    worktree TEXT,
    chat_id INTEGER,
    started_at TEXT,
    ended_at TEXT,
    source TEXT,
    dirty INTEGER NOT NULL DEFAULT 1,
    parser_version INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE devtime_turns (
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT NOT NULL,
    interrupted INTEGER NOT NULL DEFAULT 0,
    opens_with_correction INTEGER,
    parser_version INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
);

CREATE TABLE devtime_messages (
    session_id TEXT NOT NULL,
    lane TEXT NOT NULL,
    message_id TEXT NOT NULL,
    first_at TEXT NOT NULL,
    last_at TEXT NOT NULL,
    model TEXT,
    effort TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    has_tool_use INTEGER NOT NULL DEFAULT 0,
    parser_version INTEGER NOT NULL,
    PRIMARY KEY (session_id, message_id)
);

CREATE TABLE devtime_attempts (
    attempt_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    lane TEXT NOT NULL,
    message_id TEXT,
    tool_use_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    agent_type TEXT,
    agent_id TEXT,
    model TEXT,
    effort TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT NOT NULL,
    exit_code INTEGER,
    error_class TEXT,
    cmd_program TEXT,
    cmd_hash TEXT,
    timeout_ms INTEGER,
    background INTEGER NOT NULL DEFAULT 0,
    bg_task_id TEXT,
    bg_ended_at TEXT,
    bg_confidence TEXT,
    files TEXT NOT NULL DEFAULT '[]',
    edits TEXT NOT NULL DEFAULT '[]',
    reads TEXT NOT NULL DEFAULT '[]',
    parser_version INTEGER NOT NULL
);

CREATE TABLE devtime_spans (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL,
    lane TEXT NOT NULL,
    kind TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT NOT NULL,
    attempt_id TEXT,
    attempt_ids TEXT NOT NULL DEFAULT '[]',
    context_tokens INTEGER,
    waste TEXT,
    rule_id TEXT,
    confidence TEXT NOT NULL,
    parser_version INTEGER NOT NULL
);

CREATE TABLE devtime_markers (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL,
    lane TEXT NOT NULL,
    ts TEXT NOT NULL,
    kind TEXT NOT NULL,
    ref TEXT,
    parser_version INTEGER NOT NULL
);

CREATE TABLE devtime_ingest_status (
    id INTEGER PRIMARY KEY,
    enabled INTEGER NOT NULL,
    cycle_at TEXT NOT NULL,
    files_seen INTEGER NOT NULL,
    files_failed INTEGER NOT NULL,
    lines_read INTEGER NOT NULL,
    lines_failed INTEGER NOT NULL,
    unknown_records INTEGER NOT NULL,
    unmapped_sessions INTEGER NOT NULL,
    skipped_daemon_sessions INTEGER NOT NULL,
    failure_amber_rate REAL NOT NULL,
    failure_min_lines INTEGER NOT NULL
);

CREATE INDEX idx_devtime_spans_session ON devtime_spans (session_id, lane, started_at);
CREATE INDEX idx_devtime_attempts_session ON devtime_attempts (session_id, started_at);
CREATE INDEX idx_devtime_messages_session ON devtime_messages (session_id, lane, first_at);
CREATE INDEX idx_devtime_markers_session ON devtime_markers (session_id, ts);
-- One marker per (session, kind, ref): a notification seen twice is one event.
CREATE UNIQUE INDEX idx_devtime_markers_ref
    ON devtime_markers (session_id, kind, ref) WHERE ref IS NOT NULL;
