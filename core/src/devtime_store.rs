//! The devtime tables' only SQL (migrations 0164 and 0171): files and offsets, the cwd map, sessions,
//! turns, messages (usage deduped by message id), attempts, spans, markers and the ingest status, and
//! what the rule engine writes: findings, attempt marks, turn stats and the owner's feedback.
//!
//! Knows no transcript format and no lane rule: it stores what a parser hands it and answers what a
//! lane builder asks. The vocabularies below are checked by callers before the write, because the
//! migration carries no CHECK constraints (the house rule of 0143).
//!
//! Everything a chunk of a transcript produces goes through a `&mut SqliteConnection` taken from
//! [`begin_chunk`], so rows and the file's offset commit together or not at all: a crash between
//! them would otherwise re-read a chunk against rows that already hold it.

use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

/// The parser generation a file row is stamped with when this module creates it.
pub use crate::devtime_parse::PARSER_VERSION;

#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
pub const FILE_STATUSES: [&str; 5] = [
    "ok",
    "truncated",
    "unreadable",
    "unmapped",
    "skipped_daemon",
];
#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
pub const CWD_KINDS: [&str; 4] = ["project", "worktree", "daemon_worktree", "unmapped"];
#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
pub const SPAN_KINDS: [&str; 7] = [
    "model",
    "tool",
    "subagent",
    "wait_background",
    "wait_human",
    "wait_machine",
    "idle",
];
#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
pub const MARKER_KINDS: [&str; 8] = [
    "compact_boundary",
    "clear",
    "commit",
    "land",
    "chat_start",
    "interrupt",
    "bg_notification",
    "bg_ref",
];
#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
pub const OUTCOMES: [&str; 5] = ["ok", "error", "interrupted", "launched", "unknown"];
#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
pub const CONFIDENCE: [&str; 2] = ["exact", "inferred"];

// The rule engine's closed vocabularies (migration 0171). The tables carry no CHECK constraints, so
// the engine and `devtime_precision` check against these before every write.
#[cfg_attr(not(test), allow(dead_code))]
pub const WASTES: [&str; 3] = ["useful", "rework", "avoidable"];
#[cfg_attr(not(test), allow(dead_code))]
pub const LEVERS: [&str; 19] = [
    "model",
    "spec",
    "clarity",
    "verification",
    "dispatch_scope",
    "machine",
    "precision",
    "project_knowledge",
    "environment_knowledge",
    "estimation",
    "permissions",
    "discipline",
    "context",
    "delegation",
    "parallelism",
    "waiting",
    "autonomy",
    "script_skill",
    "gotcha",
];
#[allow(dead_code)] // vocabulary kept for the SP4 endpoint
pub const LEVELS: [&str; 3] = ["base", "adapter", "deferred"];
#[allow(dead_code)] // vocabulary kept for the SP4 endpoint
pub const VERIFIED: [&str; 3] = ["passed", "failed", "not_measured"];
#[allow(dead_code)] // vocabulary kept for the SP4 endpoint
pub const FINDING_SCOPES: [&str; 2] = ["session", "cross"];
#[cfg_attr(not(test), allow(dead_code))]
pub const FEEDBACK_VERDICTS: [&str; 2] = ["not_rework", "confirmed"];
/// A cause may also be any of [`LEVERS`].
#[cfg_attr(not(test), allow(dead_code))]
pub const FEEDBACK_CAUSES: [&str; 5] = ["spec_gap", "scope_creep", "gate_skip", "infra", "taste"];
#[cfg_attr(not(test), allow(dead_code))]
pub const BG_STATUSES: [&str; 3] = ["completed", "failed", "killed"];

fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
pub struct FileState {
    pub path: String,
    pub session_id: Option<String>,
    pub lane: Option<String>,
    pub offset: i64,
    pub size: i64,
    pub mtime: Option<String>,
    pub status: String,
    pub lines_read: i64,
    pub lines_failed: i64,
    pub unknown_records: i64,
    pub parser_version: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct CwdMapping {
    pub cwd: String,
    pub project_id: Option<String>,
    pub worktree: Option<String>,
    pub kind: String,
    pub resolved_at: String,
}

#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct SessionRow {
    pub session_id: String,
    pub project_id: String,
    pub worktree: Option<String>,
    pub chat_id: Option<i64>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub source: Option<String>,
    pub dirty: i64,
    pub parser_version: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
pub struct TurnRow {
    pub session_id: String,
    pub seq: i64,
    pub started_at: String,
    pub ended_at: String,
    pub interrupted: i64,
    pub opens_with_correction: Option<i64>,
    pub parser_version: i64,
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct MessageRow {
    pub session_id: String,
    pub lane: String,
    pub message_id: String,
    pub first_at: String,
    pub last_at: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub input_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub output_tokens: i64,
    pub has_tool_use: i64,
    pub parser_version: i64,
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct AttemptRow {
    pub attempt_id: String,
    pub session_id: String,
    pub lane: String,
    pub message_id: Option<String>,
    pub tool_use_id: String,
    pub kind: String,
    pub tool_name: String,
    pub agent_type: Option<String>,
    pub agent_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub outcome: String,
    pub exit_code: Option<i64>,
    pub error_class: Option<String>,
    pub cmd_program: Option<String>,
    pub cmd_hash: Option<String>,
    pub timeout_ms: Option<i64>,
    pub background: i64,
    pub bg_task_id: Option<String>,
    pub bg_ended_at: Option<String>,
    pub bg_confidence: Option<String>,
    /// JSON arrays stored as TEXT; empty is written as `[]`.
    pub files: String,
    pub edits: String,
    pub reads: String,
    pub parser_version: i64,
    /// `completed|failed|killed` ([`BG_STATUSES`]) once a background task's notification said so.
    pub bg_status: Option<String>,
    /// JSON arrays of 8-hex reference hashes (see the plan's D4/D5 rows); empty is written as `[]`.
    pub refs_in: String,
    pub refs_out: String,
}

/// What a tool result adds to an attempt that was launched earlier.
#[derive(Debug, Clone, Default)]
pub struct AttemptResult {
    pub ended_at: Option<String>,
    pub outcome: String,
    pub exit_code: Option<i64>,
    pub error_class: Option<String>,
    pub agent_id: Option<String>,
    pub model: Option<String>,
    pub bg_task_id: Option<String>,
    /// A JSON array of reference hashes taken from the result; empty is written as `[]`.
    pub refs_out: String,
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
pub struct SpanRow {
    /// Assigned by the database; ignored on write.
    pub id: i64,
    pub session_id: String,
    pub lane: String,
    pub kind: String,
    pub started_at: String,
    pub ended_at: String,
    pub attempt_id: Option<String>,
    /// A JSON array stored as TEXT; empty is written as `[]`.
    pub attempt_ids: String,
    pub context_tokens: Option<i64>,
    pub waste: Option<String>,
    pub rule_id: Option<String>,
    pub confidence: String,
    pub parser_version: i64,
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
#[allow(dead_code)] // a table column or closed vocabulary that no reader asks for yet
pub struct MarkerRow {
    /// Assigned by the database; ignored on write.
    pub id: i64,
    pub session_id: String,
    pub lane: String,
    pub ts: String,
    pub kind: String,
    pub r#ref: Option<String>,
    pub parser_version: i64,
}

/// A file's running totals so far, written as they are: the caller keeps the sum across chunks.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileCounters {
    pub lines_read: i64,
    pub lines_failed: i64,
    pub unknown_records: i64,
}

#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct IngestStatus {
    pub enabled: bool,
    pub cycle_at: String,
    pub files_seen: i64,
    pub files_failed: i64,
    pub lines_read: i64,
    pub lines_failed: i64,
    pub unknown_records: i64,
    pub unmapped_sessions: i64,
    pub skipped_daemon_sessions: i64,
    pub failure_amber_rate: f64,
    pub failure_min_lines: i64,
}

/// Everything a lane builder needs about one session.
#[derive(Debug, Clone, Default)]
pub struct SessionRows {
    pub turns: Vec<TurnRow>,
    pub messages: Vec<MessageRow>,
    pub attempts: Vec<AttemptRow>,
    pub markers: Vec<MarkerRow>,
}

fn json_or_empty(text: &str) -> &str {
    if text.is_empty() { "[]" } else { text }
}

pub async fn file_state(pool: &SqlitePool, path: &str) -> sqlx::Result<Option<FileState>> {
    sqlx::query_as::<_, FileState>(
        "SELECT path, session_id, lane, offset, size, mtime, status, lines_read, lines_failed,
                unknown_records, parser_version, updated_at
         FROM devtime_files WHERE path = ?",
    )
    .bind(path)
    .fetch_optional(pool)
    .await
}

/// Opens the transaction one chunk's rows and its offset share. Dropping it uncommitted rolls all
/// of it back.
pub async fn begin_chunk(pool: &SqlitePool) -> sqlx::Result<Transaction<'_, Sqlite>> {
    pool.begin().await
}

pub async fn upsert_session(conn: &mut SqliteConnection, row: &SessionRow) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO devtime_sessions
            (session_id, project_id, worktree, chat_id, started_at, ended_at, source, dirty,
             parser_version, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(session_id) DO UPDATE SET
            project_id = excluded.project_id,
            worktree = excluded.worktree,
            chat_id = COALESCE(excluded.chat_id, chat_id),
            started_at = CASE WHEN started_at IS NULL OR excluded.started_at < started_at
                              THEN excluded.started_at ELSE started_at END,
            ended_at = CASE WHEN ended_at IS NULL OR excluded.ended_at > ended_at
                            THEN excluded.ended_at ELSE ended_at END,
            source = COALESCE(excluded.source, source),
            dirty = excluded.dirty,
            parser_version = excluded.parser_version,
            updated_at = excluded.updated_at",
    )
    .bind(&row.session_id)
    .bind(&row.project_id)
    .bind(&row.worktree)
    .bind(row.chat_id)
    .bind(&row.started_at)
    .bind(&row.ended_at)
    .bind(&row.source)
    .bind(row.dirty)
    .bind(row.parser_version)
    .bind(now())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Opens the next turn of a session (`seq` = MAX + 1) and returns its `seq`.
pub async fn open_turn(
    conn: &mut SqliteConnection,
    session_id: &str,
    started_at: &str,
    opens_with_correction: Option<bool>,
    parser_version: i64,
) -> sqlx::Result<i64> {
    let seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM devtime_turns WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO devtime_turns
            (session_id, seq, started_at, ended_at, interrupted, opens_with_correction, parser_version)
         VALUES (?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(session_id)
    .bind(seq)
    .bind(started_at)
    .bind(started_at)
    .bind(opens_with_correction.map(i64::from))
    .bind(parser_version)
    .execute(&mut *conn)
    .await?;
    Ok(seq)
}

/// Moves a turn's `ended_at` forward to `ts`; never backwards.
pub async fn touch_turn(
    conn: &mut SqliteConnection,
    session_id: &str,
    seq: i64,
    ts: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE devtime_turns SET ended_at = MAX(ended_at, ?) WHERE session_id = ? AND seq = ?",
    )
    .bind(ts)
    .bind(session_id)
    .bind(seq)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn mark_turn_interrupted(
    conn: &mut SqliteConnection,
    session_id: &str,
    seq: i64,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE devtime_turns SET interrupted = 1 WHERE session_id = ? AND seq = ?")
        .bind(session_id)
        .bind(seq)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// One row per message id. A message streams as several snapshots whose usage only grows, so the row
/// keeps the snapshot with the largest four-field usage sum (ties keep what is there), the earliest
/// `first_at`, the latest `last_at`, and `has_tool_use` as an OR.
pub async fn upsert_message(conn: &mut SqliteConnection, row: &MessageRow) -> sqlx::Result<()> {
    const BIGGER: &str = "(excluded.input_tokens + excluded.cache_read_tokens
         + excluded.cache_creation_tokens + excluded.output_tokens)
         > (input_tokens + cache_read_tokens + cache_creation_tokens + output_tokens)";
    let sql = format!(
        "INSERT INTO devtime_messages
            (session_id, lane, message_id, first_at, last_at, model, effort, input_tokens,
             cache_read_tokens, cache_creation_tokens, output_tokens, has_tool_use, parser_version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(session_id, message_id) DO UPDATE SET
            first_at = MIN(first_at, excluded.first_at),
            last_at = MAX(last_at, excluded.last_at),
            has_tool_use = MAX(has_tool_use, excluded.has_tool_use),
            model = CASE WHEN {BIGGER} THEN COALESCE(excluded.model, model) ELSE model END,
            effort = CASE WHEN {BIGGER} THEN COALESCE(excluded.effort, effort) ELSE effort END,
            input_tokens = CASE WHEN {BIGGER} THEN excluded.input_tokens ELSE input_tokens END,
            cache_read_tokens = CASE WHEN {BIGGER} THEN excluded.cache_read_tokens ELSE cache_read_tokens END,
            cache_creation_tokens = CASE WHEN {BIGGER} THEN excluded.cache_creation_tokens ELSE cache_creation_tokens END,
            output_tokens = CASE WHEN {BIGGER} THEN excluded.output_tokens ELSE output_tokens END,
            parser_version = excluded.parser_version"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&row.session_id)
        .bind(&row.lane)
        .bind(&row.message_id)
        .bind(&row.first_at)
        .bind(&row.last_at)
        .bind(&row.model)
        .bind(&row.effort)
        .bind(row.input_tokens)
        .bind(row.cache_read_tokens)
        .bind(row.cache_creation_tokens)
        .bind(row.output_tokens)
        .bind(row.has_tool_use)
        .bind(row.parser_version)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Records a launch. A repeat of the same launch line changes nothing.
pub async fn upsert_attempt_launch(
    conn: &mut SqliteConnection,
    row: &AttemptRow,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO devtime_attempts
            (attempt_id, session_id, lane, message_id, tool_use_id, kind, tool_name, agent_type,
             agent_id, model, effort, started_at, ended_at, outcome, exit_code, error_class,
             cmd_program, cmd_hash, timeout_ms, background, bg_task_id, bg_ended_at, bg_confidence,
             files, edits, reads, parser_version, bg_status, refs_in, refs_out)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 ?, ?, ?)",
    )
    .bind(&row.attempt_id)
    .bind(&row.session_id)
    .bind(&row.lane)
    .bind(&row.message_id)
    .bind(&row.tool_use_id)
    .bind(&row.kind)
    .bind(&row.tool_name)
    .bind(&row.agent_type)
    .bind(&row.agent_id)
    .bind(&row.model)
    .bind(&row.effort)
    .bind(&row.started_at)
    .bind(&row.ended_at)
    .bind(&row.outcome)
    .bind(row.exit_code)
    .bind(&row.error_class)
    .bind(&row.cmd_program)
    .bind(&row.cmd_hash)
    .bind(row.timeout_ms)
    .bind(row.background)
    .bind(&row.bg_task_id)
    .bind(&row.bg_ended_at)
    .bind(&row.bg_confidence)
    .bind(json_or_empty(&row.files))
    .bind(json_or_empty(&row.edits))
    .bind(json_or_empty(&row.reads))
    .bind(row.parser_version)
    .bind(&row.bg_status)
    .bind(json_or_empty(&row.refs_in))
    .bind(json_or_empty(&row.refs_out))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Fills in what the tool result says. `agent_id`, `model` and `bg_task_id` only fill, never blank,
/// what the launch already knew.
pub async fn complete_attempt(
    conn: &mut SqliteConnection,
    attempt_id: &str,
    result: &AttemptResult,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE devtime_attempts SET
            ended_at = ?, outcome = ?, exit_code = ?, error_class = ?,
            agent_id = COALESCE(?, agent_id), model = COALESCE(?, model),
            bg_task_id = COALESCE(?, bg_task_id), refs_out = ?
         WHERE attempt_id = ?",
    )
    .bind(&result.ended_at)
    .bind(&result.outcome)
    .bind(result.exit_code)
    .bind(&result.error_class)
    .bind(&result.agent_id)
    .bind(&result.model)
    .bind(&result.bg_task_id)
    .bind(json_or_empty(&result.refs_out))
    .bind(attempt_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Records how a background attempt ended, from its notification. The exit code only fills a NULL: a
/// foreground result that already carried one is not overwritten by a later notification.
pub async fn set_bg_status(
    conn: &mut SqliteConnection,
    attempt_id: &str,
    status: &str,
    exit_code: Option<i64>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE devtime_attempts SET bg_status = ?, exit_code = COALESCE(exit_code, ?)
         WHERE attempt_id = ?",
    )
    .bind(status)
    .bind(exit_code)
    .bind(attempt_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Records when a background attempt really finished, and how sure that is.
pub async fn set_bg_end(
    conn: &mut SqliteConnection,
    attempt_id: &str,
    bg_ended_at: &str,
    confidence: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE devtime_attempts SET bg_ended_at = ?, bg_confidence = ? WHERE attempt_id = ?",
    )
    .bind(bg_ended_at)
    .bind(confidence)
    .bind(attempt_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// One marker per `(session, kind, ref)` when it has a `ref`. A background notification can be seen
/// more than once, and the earliest sighting is when it arrived, so that kind keeps the minimum
/// `ts`, a `bg_ref` keeps the maximum (its last reference); any other kind keeps what it first wrote.
pub async fn insert_marker(conn: &mut SqliteConnection, row: &MarkerRow) -> sqlx::Result<()> {
    let conflict = if row.r#ref.is_none() {
        ""
    } else if row.kind == "bg_notification" {
        " ON CONFLICT(session_id, kind, ref) WHERE ref IS NOT NULL
          DO UPDATE SET ts = MIN(ts, excluded.ts)"
    } else if row.kind == "bg_ref" {
        // The inferred end of a background task is its LAST reference.
        " ON CONFLICT(session_id, kind, ref) WHERE ref IS NOT NULL
          DO UPDATE SET ts = MAX(ts, excluded.ts)"
    } else {
        " ON CONFLICT(session_id, kind, ref) WHERE ref IS NOT NULL DO NOTHING"
    };
    let sql = format!(
        "INSERT INTO devtime_markers (session_id, lane, ts, kind, ref, parser_version)
         VALUES (?, ?, ?, ?, ?, ?){conflict}"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&row.session_id)
        .bind(&row.lane)
        .bind(&row.ts)
        .bind(&row.kind)
        .bind(&row.r#ref)
        .bind(row.parser_version)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The newest turn of a session, if it has one.
pub async fn last_turn(conn: &mut SqliteConnection, session_id: &str) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT MAX(seq) FROM devtime_turns WHERE session_id = ?")
        .bind(session_id)
        .fetch_one(&mut *conn)
        .await
}

/// What a notification or a bg reference needs to know about the attempt that launched it.
#[derive(Debug, Clone)]
pub struct AttemptRef {
    pub attempt_id: String,
    pub lane: String,
    pub kind: String,
    pub background: i64,
    pub bg_ended_at: Option<String>,
}

pub async fn attempt_by_tool_use(
    conn: &mut SqliteConnection,
    session_id: &str,
    tool_use_id: &str,
) -> sqlx::Result<Option<AttemptRef>> {
    let row: Option<(String, String, String, i64, Option<String>)> = sqlx::query_as(
        "SELECT attempt_id, lane, kind, background, bg_ended_at FROM devtime_attempts
         WHERE session_id = ? AND tool_use_id = ?",
    )
    .bind(session_id)
    .bind(tool_use_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(
        |(attempt_id, lane, kind, background, bg_ended_at)| AttemptRef {
            attempt_id,
            lane,
            kind,
            background,
            bg_ended_at,
        },
    ))
}

/// The background task ids a session's attempts have been given so far.
pub async fn known_bg_task_ids(
    conn: &mut SqliteConnection,
    session_id: &str,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT DISTINCT bg_task_id FROM devtime_attempts
         WHERE session_id = ? AND bg_task_id IS NOT NULL",
    )
    .bind(session_id)
    .fetch_all(&mut *conn)
    .await
}

/// A result that says "launched" makes the attempt a background one, whatever its input said.
pub async fn set_background(conn: &mut SqliteConnection, attempt_id: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE devtime_attempts SET background = 1 WHERE attempt_id = ?")
        .bind(attempt_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Ties an agent attempt to its transcript file (`agent-<id>.meta.json` names the launching
/// `tool_use`). Only fills what is missing; returns how many rows actually changed.
pub async fn link_agent(
    conn: &mut SqliteConnection,
    session_id: &str,
    tool_use_id: &str,
    agent_id: &str,
    agent_type: Option<&str>,
) -> sqlx::Result<u64> {
    let done = sqlx::query(
        "UPDATE devtime_attempts SET
            agent_id = COALESCE(agent_id, ?), agent_type = COALESCE(agent_type, ?)
         WHERE session_id = ? AND tool_use_id = ?
           AND (agent_id IS NULL OR (agent_type IS NULL AND ? IS NOT NULL))",
    )
    .bind(agent_id)
    .bind(agent_type)
    .bind(session_id)
    .bind(tool_use_id)
    .bind(agent_type)
    .execute(&mut *conn)
    .await?;
    Ok(done.rows_affected())
}

/// Records which session and lane a file belongs to; `save_offset` knows neither.
pub async fn set_file_identity(
    conn: &mut SqliteConnection,
    path: &str,
    session_id: &str,
    lane: &str,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE devtime_files SET session_id = ?, lane = ? WHERE path = ?")
        .bind(session_id)
        .bind(lane)
        .bind(path)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Saves how far a file has been read, with the file's running counters. Called inside the chunk's
/// transaction so the offset moves only if the rows it covers do.
pub async fn save_offset(
    conn: &mut SqliteConnection,
    path: &str,
    offset: i64,
    size: i64,
    mtime: Option<&str>,
    counters: &FileCounters,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO devtime_files
            (path, offset, size, mtime, status, lines_read, lines_failed, unknown_records,
             parser_version, updated_at)
         VALUES (?, ?, ?, ?, 'ok', ?, ?, ?, ?, ?)
         ON CONFLICT(path) DO UPDATE SET
            offset = excluded.offset, size = excluded.size, mtime = excluded.mtime,
            lines_read = excluded.lines_read, lines_failed = excluded.lines_failed,
            unknown_records = excluded.unknown_records, updated_at = excluded.updated_at",
    )
    .bind(path)
    .bind(offset)
    .bind(size)
    .bind(mtime)
    .bind(counters.lines_read)
    .bind(counters.lines_failed)
    .bind(counters.unknown_records)
    .bind(PARSER_VERSION)
    .bind(now())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn mark_file_status(
    conn: &mut SqliteConnection,
    path: &str,
    status: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO devtime_files (path, status, parser_version, updated_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(path) DO UPDATE SET status = excluded.status, updated_at = excluded.updated_at",
    )
    .bind(path)
    .bind(status)
    .bind(PARSER_VERSION)
    .bind(now())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn cwd_mapping(pool: &SqlitePool, cwd: &str) -> sqlx::Result<Option<CwdMapping>> {
    sqlx::query_as::<_, CwdMapping>(
        "SELECT cwd, project_id, worktree, kind, resolved_at FROM devtime_cwd_map WHERE cwd = ?",
    )
    .bind(cwd)
    .fetch_optional(pool)
    .await
}

pub async fn save_cwd_mapping(pool: &SqlitePool, row: &CwdMapping) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO devtime_cwd_map (cwd, project_id, worktree, kind, resolved_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(cwd) DO UPDATE SET
            project_id = excluded.project_id, worktree = excluded.worktree,
            kind = excluded.kind, resolved_at = excluded.resolved_at",
    )
    .bind(&row.cwd)
    .bind(&row.project_id)
    .bind(&row.worktree)
    .bind(&row.kind)
    .bind(&row.resolved_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// The worktree paths already mapped to a project.
pub async fn known_worktrees(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT DISTINCT worktree FROM devtime_cwd_map
         WHERE project_id = ? AND worktree IS NOT NULL ORDER BY worktree",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// A session's turns, messages, attempts and markers, each in time order.
pub async fn session_rows(pool: &SqlitePool, session_id: &str) -> sqlx::Result<SessionRows> {
    let turns = sqlx::query_as::<_, TurnRow>(
        "SELECT session_id, seq, started_at, ended_at, interrupted, opens_with_correction,
                parser_version
         FROM devtime_turns WHERE session_id = ? ORDER BY seq",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    let messages = sqlx::query_as::<_, MessageRow>(
        "SELECT session_id, lane, message_id, first_at, last_at, model, effort, input_tokens,
                cache_read_tokens, cache_creation_tokens, output_tokens, has_tool_use,
                parser_version
         FROM devtime_messages WHERE session_id = ? ORDER BY first_at, message_id",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    let attempts = sqlx::query_as::<_, AttemptRow>(
        "SELECT attempt_id, session_id, lane, message_id, tool_use_id, kind, tool_name, agent_type,
                agent_id, model, effort, started_at, ended_at, outcome, exit_code, error_class,
                cmd_program, cmd_hash, timeout_ms, background, bg_task_id, bg_ended_at,
                bg_confidence, files, edits, reads, parser_version, bg_status, refs_in, refs_out
         FROM devtime_attempts WHERE session_id = ? ORDER BY started_at, attempt_id",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    let markers = sqlx::query_as::<_, MarkerRow>(
        "SELECT id, session_id, lane, ts, kind, ref, parser_version
         FROM devtime_markers WHERE session_id = ? ORDER BY ts, id",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    Ok(SessionRows {
        turns,
        messages,
        attempts,
        markers,
    })
}

/// Replaces a session's spans wholesale, in one transaction: the lane builder derives them from
/// scratch, so a partial write would leave a lane that never existed.
pub async fn replace_spans(
    pool: &SqlitePool,
    session_id: &str,
    spans: &[SpanRow],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM devtime_spans WHERE session_id = ?")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    for span in spans {
        sqlx::query(
            "INSERT INTO devtime_spans
                (session_id, lane, kind, started_at, ended_at, attempt_id, attempt_ids,
                 context_tokens, waste, rule_id, confidence, parser_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(&span.lane)
        .bind(&span.kind)
        .bind(&span.started_at)
        .bind(&span.ended_at)
        .bind(&span.attempt_id)
        .bind(json_or_empty(&span.attempt_ids))
        .bind(span.context_tokens)
        .bind(&span.waste)
        .bind(&span.rule_id)
        .bind(&span.confidence)
        .bind(span.parser_version)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// The one-row summary of the latest ingestion cycle (`id = 1`).
pub async fn write_ingest_status(pool: &SqlitePool, status: &IngestStatus) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO devtime_ingest_status
            (id, enabled, cycle_at, files_seen, files_failed, lines_read, lines_failed,
             unknown_records, unmapped_sessions, skipped_daemon_sessions, failure_amber_rate,
             failure_min_lines)
         VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
            enabled = excluded.enabled, cycle_at = excluded.cycle_at,
            files_seen = excluded.files_seen, files_failed = excluded.files_failed,
            lines_read = excluded.lines_read, lines_failed = excluded.lines_failed,
            unknown_records = excluded.unknown_records,
            unmapped_sessions = excluded.unmapped_sessions,
            skipped_daemon_sessions = excluded.skipped_daemon_sessions,
            failure_amber_rate = excluded.failure_amber_rate,
            failure_min_lines = excluded.failure_min_lines",
    )
    .bind(status.enabled)
    .bind(&status.cycle_at)
    .bind(status.files_seen)
    .bind(status.files_failed)
    .bind(status.lines_read)
    .bind(status.lines_failed)
    .bind(status.unknown_records)
    .bind(status.unmapped_sessions)
    .bind(status.skipped_daemon_sessions)
    .bind(status.failure_amber_rate)
    .bind(status.failure_min_lines)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn read_ingest_status(pool: &SqlitePool) -> sqlx::Result<Option<IngestStatus>> {
    sqlx::query_as::<_, IngestStatus>(
        "SELECT enabled, cycle_at, files_seen, files_failed, lines_read, lines_failed,
                unknown_records, unmapped_sessions, skipped_daemon_sessions, failure_amber_rate,
                failure_min_lines
         FROM devtime_ingest_status WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
}

/// What the rules pass needs to know about a session before it reads the rest: its project, its
/// span of time, and the two stamps that decide whether the rules still owe it work.
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct SessionHead {
    pub session_id: String,
    pub project_id: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub updated_at: String,
    /// 1 until `write_rule_results` has seen the rows as they are now.
    pub dirty: i64,
    pub rules_version: Option<String>,
}

pub async fn session_head(
    pool: &SqlitePool,
    session_id: &str,
) -> sqlx::Result<Option<SessionHead>> {
    sqlx::query_as::<_, SessionHead>(
        "SELECT session_id, project_id, started_at, ended_at, updated_at, dirty, rules_version
         FROM devtime_sessions WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
}

/// A session's spans, with their ids, in lane and time order.
pub async fn session_spans(pool: &SqlitePool, session_id: &str) -> sqlx::Result<Vec<SpanRow>> {
    sqlx::query_as::<_, SpanRow>(
        "SELECT id, session_id, lane, kind, started_at, ended_at, attempt_id, attempt_ids,
                context_tokens, waste, rule_id, confidence, parser_version
         FROM devtime_spans WHERE session_id = ? ORDER BY lane, started_at, id",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
}

/// The sessions whose rule results are missing or out of date, newest first: dirty ones, ones never
/// ruled, and ones ruled under another `rules_version` fingerprint.
pub async fn sessions_needing_rules(
    pool: &SqlitePool,
    fingerprint: &str,
    limit: u32,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT session_id FROM devtime_sessions
         WHERE dirty = 1 OR rules_version IS NULL OR rules_version <> ?
         ORDER BY updated_at DESC, session_id LIMIT ?",
    )
    .bind(fingerprint)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
}

/// One rule finding, session-scoped or cross-session. The two version columns are the engine's
/// stamps, carried on the row so a cross finding (written without a session pass) has them too.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct FindingRow {
    pub finding_key: String,
    pub project_id: String,
    pub session_id: String,
    /// One of [`FINDING_SCOPES`].
    pub scope: String,
    pub rule_id: String,
    pub rule_version: i64,
    /// One of [`LEVELS`].
    pub level: String,
    /// One of [`WASTES`].
    pub waste: String,
    /// One of [`LEVERS`]: the rule's first lever.
    pub lever: String,
    /// One of [`CONFIDENCE`].
    pub confidence: String,
    pub lane: String,
    pub started_at: String,
    pub ended_at: String,
    pub cost_ms: i64,
    pub count: i64,
    /// JSON array; empty is written as `[]`.
    pub attempt_ids: String,
    /// JSON array of the sessions a cross finding spans; empty is written as `[]`.
    pub sessions: String,
    pub rules_version: String,
    pub parser_version: i64,
}

/// The winning annotation of one span.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpanMark {
    pub span_id: i64,
    pub waste: Option<String>,
    pub rule_id: Option<String>,
    pub lever: Option<String>,
    pub finding_key: Option<String>,
}

/// One attempt's winning annotation and its three-valued verification result.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct AttemptMarkRow {
    pub attempt_id: String,
    pub session_id: String,
    pub waste: Option<String>,
    pub rule_id: Option<String>,
    pub lever: Option<String>,
    pub finding_key: Option<String>,
    /// One of [`VERIFIED`].
    pub verified: String,
    /// The rule that set `verified`, when one did.
    pub verified_by: Option<String>,
}

/// One main-lane turn's time, and how much of it a rule explains.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct TurnStatRow {
    pub session_id: String,
    pub turn_seq: i64,
    pub project_id: String,
    pub turn_class: String,
    pub started_at: String,
    pub calls: i64,
    pub active_ms: i64,
    pub explained_ms: i64,
}

/// Everything one session's rules pass writes, in one transaction.
#[derive(Debug, Clone, Default)]
pub struct RuleWrite {
    pub findings: Vec<FindingRow>,
    pub span_marks: Vec<SpanMark>,
    pub attempt_marks: Vec<AttemptMarkRow>,
    pub turn_stats: Vec<TurnStatRow>,
}

async fn insert_finding(conn: &mut SqliteConnection, row: &FindingRow) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO devtime_findings
            (finding_key, project_id, session_id, scope, rule_id, rule_version, level, waste, lever,
             confidence, lane, started_at, ended_at, cost_ms, count, attempt_ids, sessions,
             rules_version, parser_version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(finding_key) DO NOTHING",
    )
    .bind(&row.finding_key)
    .bind(&row.project_id)
    .bind(&row.session_id)
    .bind(&row.scope)
    .bind(&row.rule_id)
    .bind(row.rule_version)
    .bind(&row.level)
    .bind(&row.waste)
    .bind(&row.lever)
    .bind(&row.confidence)
    .bind(&row.lane)
    .bind(&row.started_at)
    .bind(&row.ended_at)
    .bind(row.cost_ms)
    .bind(row.count)
    .bind(json_or_empty(&row.attempt_ids))
    .bind(json_or_empty(&row.sessions))
    .bind(&row.rules_version)
    .bind(row.parser_version)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Replaces what the rules concluded about one session, wholesale and in one transaction: its
/// session-scoped findings, the annotations on its spans, its attempt marks and its turn stats.
/// Then stamps the session with `fingerprint` and clears `dirty`, but only if `updated_at` is still
/// what `head` read: rows ingested while the rules ran leave it dirty, so the next cycle runs again.
pub async fn write_rule_results(
    pool: &SqlitePool,
    head: &SessionHead,
    write: &RuleWrite,
    fingerprint: &str,
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM devtime_findings WHERE session_id = ? AND scope = 'session'")
        .bind(&head.session_id)
        .execute(&mut *tx)
        .await?;
    for finding in &write.findings {
        insert_finding(&mut tx, finding).await?;
    }
    sqlx::query(
        "UPDATE devtime_spans SET waste = NULL, rule_id = NULL, lever = NULL, finding_key = NULL
         WHERE session_id = ?",
    )
    .bind(&head.session_id)
    .execute(&mut *tx)
    .await?;
    for mark in &write.span_marks {
        sqlx::query(
            "UPDATE devtime_spans SET waste = ?, rule_id = ?, lever = ?, finding_key = ?
             WHERE id = ? AND session_id = ?",
        )
        .bind(&mark.waste)
        .bind(&mark.rule_id)
        .bind(&mark.lever)
        .bind(&mark.finding_key)
        .bind(mark.span_id)
        .bind(&head.session_id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM devtime_attempt_marks WHERE session_id = ?")
        .bind(&head.session_id)
        .execute(&mut *tx)
        .await?;
    for mark in &write.attempt_marks {
        sqlx::query(
            "INSERT OR REPLACE INTO devtime_attempt_marks
                (attempt_id, session_id, waste, rule_id, lever, finding_key, verified, verified_by,
                 rules_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&mark.attempt_id)
        .bind(&mark.session_id)
        .bind(&mark.waste)
        .bind(&mark.rule_id)
        .bind(&mark.lever)
        .bind(&mark.finding_key)
        .bind(&mark.verified)
        .bind(&mark.verified_by)
        .bind(fingerprint)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM devtime_turn_stats WHERE session_id = ?")
        .bind(&head.session_id)
        .execute(&mut *tx)
        .await?;
    for stat in &write.turn_stats {
        sqlx::query(
            "INSERT OR REPLACE INTO devtime_turn_stats
                (session_id, turn_seq, project_id, turn_class, started_at, calls, active_ms,
                 explained_ms, rules_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&stat.session_id)
        .bind(stat.turn_seq)
        .bind(&stat.project_id)
        .bind(&stat.turn_class)
        .bind(&stat.started_at)
        .bind(stat.calls)
        .bind(stat.active_ms)
        .bind(stat.explained_ms)
        .bind(fingerprint)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE devtime_sessions SET rules_version = ?, rules_at = ?,
            dirty = CASE WHEN updated_at = ? THEN 0 ELSE dirty END
         WHERE session_id = ?",
    )
    .bind(fingerprint)
    .bind(now())
    .bind(&head.updated_at)
    .bind(&head.session_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Replaces a project's cross-session findings for the given rules. Session-scoped findings and
/// other rules' cross findings are untouched.
pub async fn replace_cross_findings(
    pool: &SqlitePool,
    project_id: &str,
    rule_ids: &[&str],
    rows: &[FindingRow],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    for rule_id in rule_ids {
        sqlx::query(
            "DELETE FROM devtime_findings WHERE project_id = ? AND scope = 'cross' AND rule_id = ?",
        )
        .bind(project_id)
        .bind(*rule_id)
        .execute(&mut *tx)
        .await?;
    }
    for row in rows {
        insert_finding(&mut tx, row).await?;
    }
    tx.commit().await
}

/// What the cross-session rules (F) read of one attempt.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct CrossAttempt {
    pub session_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub tool_name: String,
    pub cmd_program: Option<String>,
    pub cmd_hash: Option<String>,
    pub outcome: String,
    pub error_class: Option<String>,
    /// A JSON array of `{path, offset}`.
    pub reads: String,
}

/// The main-lane attempts of a project's sessions since `since` (a UTC timestamp), in session and
/// time order.
pub async fn cross_session_rows(
    pool: &SqlitePool,
    project_id: &str,
    since: &str,
) -> sqlx::Result<Vec<CrossAttempt>> {
    sqlx::query_as::<_, CrossAttempt>(
        "SELECT a.session_id, a.started_at, a.ended_at, a.tool_name, a.cmd_program, a.cmd_hash,
                a.outcome, a.error_class, a.reads
         FROM devtime_attempts a
         JOIN devtime_sessions s ON s.session_id = a.session_id
         WHERE s.project_id = ? AND a.lane = 'main' AND a.started_at >= ?
         ORDER BY a.session_id, a.started_at, a.attempt_id",
    )
    .bind(project_id)
    .bind(since)
    .fetch_all(pool)
    .await
}

pub async fn finding_by_key(pool: &SqlitePool, key: &str) -> sqlx::Result<Option<FindingRow>> {
    sqlx::query_as::<_, FindingRow>(
        "SELECT finding_key, project_id, session_id, scope, rule_id, rule_version, level, waste,
                lever, confidence, lane, started_at, ended_at, cost_ms, count, attempt_ids, sessions,
                rules_version, parser_version
         FROM devtime_findings WHERE finding_key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
}

/// The owner's standing mark on one finding.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct FeedbackRow {
    pub finding_key: String,
    pub rule_id: String,
    pub session_id: Option<String>,
    /// One of [`FEEDBACK_VERDICTS`].
    pub verdict: String,
    /// One of [`FEEDBACK_CAUSES`] or [`LEVERS`].
    pub cause: Option<String>,
    pub rule_version: Option<i64>,
    /// Empty on write means now.
    pub marked_at: String,
}

/// One standing mark per finding: marking again replaces the earlier mark.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn upsert_feedback(pool: &SqlitePool, row: &FeedbackRow) -> sqlx::Result<()> {
    let marked_at = if row.marked_at.is_empty() {
        now()
    } else {
        row.marked_at.clone()
    };
    sqlx::query(
        "INSERT INTO devtime_feedback
            (finding_key, rule_id, session_id, verdict, cause, rule_version, marked_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(finding_key) DO UPDATE SET
            rule_id = excluded.rule_id, session_id = excluded.session_id,
            verdict = excluded.verdict, cause = excluded.cause,
            rule_version = excluded.rule_version, marked_at = excluded.marked_at",
    )
    .bind(&row.finding_key)
    .bind(&row.rule_id)
    .bind(&row.session_id)
    .bind(&row.verdict)
    .bind(&row.cause)
    .bind(row.rule_version)
    .bind(marked_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// How many current findings of a rule exist and how many of them the owner has judged.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct RuleCaseCounts {
    pub rule_id: String,
    /// Findings whose confidence is `exact`.
    pub exact: i64,
    /// Findings that are not `exact` but carry a mark.
    pub inferred_marked: i64,
    /// Findings marked `not_rework`.
    pub not_rework: i64,
    pub total: i64,
}

/// Per rule, over the findings that exist now: a mark whose finding was recomputed away is kept in
/// `devtime_feedback` for history but is no longer a case, so the join runs from the findings.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn rule_case_counts(
    pool: &SqlitePool,
    since: Option<&str>,
) -> sqlx::Result<Vec<RuleCaseCounts>> {
    sqlx::query_as::<_, RuleCaseCounts>(
        "SELECT f.rule_id AS rule_id,
                COALESCE(SUM(CASE WHEN f.confidence = 'exact' THEN 1 ELSE 0 END), 0) AS exact,
                COALESCE(SUM(CASE WHEN f.confidence <> 'exact' AND b.finding_key IS NOT NULL
                                  THEN 1 ELSE 0 END), 0) AS inferred_marked,
                COALESCE(SUM(CASE WHEN b.verdict = 'not_rework' THEN 1 ELSE 0 END), 0)
                    AS not_rework,
                COUNT(*) AS total
         FROM devtime_findings f
         LEFT JOIN devtime_feedback b ON b.finding_key = f.finding_key
         WHERE (? IS NULL OR f.started_at >= ?)
         GROUP BY f.rule_id
         ORDER BY f.rule_id",
    )
    .bind(since)
    .bind(since)
    .fetch_all(pool)
    .await
}

/// How many sessions fired each rule, out of the sessions that have at least one attempt (a session
/// with none could not have fired anything, so it is not the denominator). Cross findings belong to
/// no single session's run and are left out.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn rule_base_rates(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: Option<&str>,
) -> sqlx::Result<(i64, Vec<(String, i64)>)> {
    let with_tools: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM devtime_sessions s
         WHERE EXISTS (SELECT 1 FROM devtime_attempts a WHERE a.session_id = s.session_id)
           AND (? IS NULL OR s.project_id = ?)
           AND (? IS NULL OR s.started_at >= ?)",
    )
    .bind(project_id)
    .bind(project_id)
    .bind(since)
    .bind(since)
    .fetch_one(pool)
    .await?;
    let fired: Vec<(String, i64)> = sqlx::query_as(
        "SELECT f.rule_id, COUNT(DISTINCT f.session_id)
         FROM devtime_findings f
         JOIN devtime_sessions s ON s.session_id = f.session_id
         WHERE f.scope = 'session'
           AND EXISTS (SELECT 1 FROM devtime_attempts a WHERE a.session_id = s.session_id)
           AND (? IS NULL OR s.project_id = ?)
           AND (? IS NULL OR s.started_at >= ?)
         GROUP BY f.rule_id
         ORDER BY f.rule_id",
    )
    .bind(project_id)
    .bind(project_id)
    .bind(since)
    .bind(since)
    .fetch_all(pool)
    .await?;
    Ok((with_tools, fired))
}

/// Stored per-turn stats, for the unexplained listing.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn turn_stats_rows(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: Option<&str>,
) -> sqlx::Result<Vec<TurnStatRow>> {
    sqlx::query_as::<_, TurnStatRow>(
        "SELECT session_id, turn_seq, project_id, turn_class, started_at, calls, active_ms,
                explained_ms
         FROM devtime_turn_stats
         WHERE (? IS NULL OR project_id = ?) AND (? IS NULL OR started_at >= ?)
         ORDER BY session_id, turn_seq",
    )
    .bind(project_id)
    .bind(project_id)
    .bind(since)
    .bind(since)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    async fn test_pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn columns(pool: &sqlx::SqlitePool, table: &str) -> Vec<String> {
        sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA table_info({table})")))
            .fetch_all(pool)
            .await
            .unwrap()
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect()
    }

    fn attempt(id: &str) -> AttemptRow {
        AttemptRow {
            attempt_id: id.to_string(),
            session_id: "s1".to_string(),
            lane: "main".to_string(),
            tool_use_id: "toolu_1".to_string(),
            kind: "tool".to_string(),
            tool_name: "Bash".to_string(),
            started_at: "2026-10-04T10:00:00.000Z".to_string(),
            outcome: "launched".to_string(),
            parser_version: 1,
            ..Default::default()
        }
    }

    fn message(id: &str, first: &str, last: &str, output: i64, tool_use: i64) -> MessageRow {
        MessageRow {
            session_id: "s1".to_string(),
            lane: "main".to_string(),
            message_id: id.to_string(),
            first_at: first.to_string(),
            last_at: last.to_string(),
            output_tokens: output,
            has_tool_use: tool_use,
            parser_version: 1,
            ..Default::default()
        }
    }

    fn marker(kind: &str, reference: &str, ts: &str) -> MarkerRow {
        MarkerRow {
            session_id: "s1".to_string(),
            lane: "main".to_string(),
            ts: ts.to_string(),
            kind: kind.to_string(),
            r#ref: Some(reference.to_string()),
            parser_version: 1,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn migration_creates_every_devtime_table_and_column() {
        let pool = test_pool().await;
        let expected: &[(&str, &[&str])] = &[
            (
                "devtime_files",
                &[
                    "path",
                    "session_id",
                    "lane",
                    "offset",
                    "size",
                    "mtime",
                    "status",
                    "lines_read",
                    "lines_failed",
                    "unknown_records",
                    "parser_version",
                    "updated_at",
                ],
            ),
            (
                "devtime_cwd_map",
                &["cwd", "project_id", "worktree", "kind", "resolved_at"],
            ),
            (
                "devtime_sessions",
                &[
                    "session_id",
                    "project_id",
                    "worktree",
                    "chat_id",
                    "started_at",
                    "ended_at",
                    "source",
                    "dirty",
                    "parser_version",
                    "updated_at",
                    "rules_version",
                    "rules_at",
                ],
            ),
            (
                "devtime_turns",
                &[
                    "session_id",
                    "seq",
                    "started_at",
                    "ended_at",
                    "interrupted",
                    "opens_with_correction",
                    "parser_version",
                ],
            ),
            (
                "devtime_messages",
                &[
                    "session_id",
                    "lane",
                    "message_id",
                    "first_at",
                    "last_at",
                    "model",
                    "effort",
                    "input_tokens",
                    "cache_read_tokens",
                    "cache_creation_tokens",
                    "output_tokens",
                    "has_tool_use",
                    "parser_version",
                ],
            ),
            (
                "devtime_attempts",
                &[
                    "attempt_id",
                    "session_id",
                    "lane",
                    "message_id",
                    "tool_use_id",
                    "kind",
                    "tool_name",
                    "agent_type",
                    "agent_id",
                    "model",
                    "effort",
                    "started_at",
                    "ended_at",
                    "outcome",
                    "exit_code",
                    "error_class",
                    "cmd_program",
                    "cmd_hash",
                    "timeout_ms",
                    "background",
                    "bg_task_id",
                    "bg_ended_at",
                    "bg_confidence",
                    "files",
                    "edits",
                    "reads",
                    "parser_version",
                    "bg_status",
                    "refs_in",
                    "refs_out",
                ],
            ),
            (
                "devtime_spans",
                &[
                    "id",
                    "session_id",
                    "lane",
                    "kind",
                    "started_at",
                    "ended_at",
                    "attempt_id",
                    "attempt_ids",
                    "context_tokens",
                    "waste",
                    "rule_id",
                    "confidence",
                    "parser_version",
                    "lever",
                    "finding_key",
                ],
            ),
            (
                "devtime_markers",
                &[
                    "id",
                    "session_id",
                    "lane",
                    "ts",
                    "kind",
                    "ref",
                    "parser_version",
                ],
            ),
            (
                "devtime_ingest_status",
                &[
                    "id",
                    "enabled",
                    "cycle_at",
                    "files_seen",
                    "files_failed",
                    "lines_read",
                    "lines_failed",
                    "unknown_records",
                    "unmapped_sessions",
                    "skipped_daemon_sessions",
                    "failure_amber_rate",
                    "failure_min_lines",
                ],
            ),
        ];
        for (table, want) in expected {
            let got = columns(&pool, table).await;
            assert!(!got.is_empty(), "table {table} is missing");
            for column in *want {
                assert!(
                    got.iter().any(|c| c == column),
                    "{table}.{column} is missing; has {got:?}"
                );
            }
            assert_eq!(
                got.len(),
                want.len(),
                "{table} has columns the spec does not list: {got:?}"
            );
        }
    }

    #[tokio::test]
    async fn attempt_upsert_keeps_launch_then_fills_result() {
        let pool = test_pool().await;
        let mut tx = begin_chunk(&pool).await.unwrap();
        upsert_attempt_launch(&mut tx, &attempt("a1"))
            .await
            .unwrap();
        // A re-read of the same launch line must not overwrite what is there.
        let mut again = attempt("a1");
        again.tool_name = "Other".to_string();
        upsert_attempt_launch(&mut tx, &again).await.unwrap();
        complete_attempt(
            &mut tx,
            "a1",
            &AttemptResult {
                ended_at: Some("2026-10-04T10:00:05.000Z".to_string()),
                outcome: "ok".to_string(),
                exit_code: Some(0),
                error_class: None,
                agent_id: None,
                model: Some("sonnet".to_string()),
                bg_task_id: None,
                refs_out: String::new(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let rows = session_rows(&pool, "s1").await.unwrap();
        assert_eq!(rows.attempts.len(), 1);
        let row = &rows.attempts[0];
        assert_eq!(
            row.tool_name, "Bash",
            "the launch row wins over a repeat launch"
        );
        assert_eq!(row.outcome, "ok");
        assert_eq!(row.exit_code, Some(0));
        assert_eq!(row.ended_at.as_deref(), Some("2026-10-04T10:00:05.000Z"));
        assert_eq!(row.started_at, "2026-10-04T10:00:00.000Z");
    }

    #[tokio::test]
    async fn message_upsert_keeps_the_largest_usage() {
        let pool = test_pool().await;
        let mut tx = begin_chunk(&pool).await.unwrap();
        upsert_message(
            &mut tx,
            &message(
                "m1",
                "2026-10-04T10:00:02.000Z",
                "2026-10-04T10:00:02.000Z",
                10,
                0,
            ),
        )
        .await
        .unwrap();
        upsert_message(
            &mut tx,
            &message(
                "m1",
                "2026-10-04T10:00:01.000Z",
                "2026-10-04T10:00:01.500Z",
                80,
                1,
            ),
        )
        .await
        .unwrap();
        // A later, smaller streaming snapshot must not shrink the usage.
        upsert_message(
            &mut tx,
            &message(
                "m1",
                "2026-10-04T10:00:03.000Z",
                "2026-10-04T10:00:03.000Z",
                40,
                0,
            ),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let rows = session_rows(&pool, "s1").await.unwrap();
        assert_eq!(rows.messages.len(), 1);
        let row = &rows.messages[0];
        assert_eq!(row.output_tokens, 80);
        assert_eq!(row.first_at, "2026-10-04T10:00:01.000Z");
        assert_eq!(row.last_at, "2026-10-04T10:00:03.000Z");
        assert_eq!(row.has_tool_use, 1, "has_tool_use is an OR");
    }

    #[tokio::test]
    async fn chunk_rows_and_offset_commit_together() {
        let pool = test_pool().await;
        let counters = FileCounters {
            lines_read: 3,
            lines_failed: 1,
            unknown_records: 0,
        };
        let mtime = Some("2026-10-04T10:00:00.000Z");

        let mut tx = begin_chunk(&pool).await.unwrap();
        upsert_attempt_launch(&mut tx, &attempt("a1"))
            .await
            .unwrap();
        save_offset(&mut tx, "C:/x/s1.jsonl", 120, 120, mtime, &counters)
            .await
            .unwrap();
        drop(tx);
        assert!(
            file_state(&pool, "C:/x/s1.jsonl").await.unwrap().is_none(),
            "an uncommitted offset must not persist"
        );
        assert!(
            session_rows(&pool, "s1").await.unwrap().attempts.is_empty(),
            "uncommitted rows must not persist"
        );

        let mut tx = begin_chunk(&pool).await.unwrap();
        upsert_attempt_launch(&mut tx, &attempt("a1"))
            .await
            .unwrap();
        save_offset(&mut tx, "C:/x/s1.jsonl", 120, 120, mtime, &counters)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let state = file_state(&pool, "C:/x/s1.jsonl")
            .await
            .unwrap()
            .expect("committed");
        assert_eq!(state.offset, 120);
        assert_eq!(state.lines_read, 3);
        assert_eq!(state.lines_failed, 1);
        assert_eq!(session_rows(&pool, "s1").await.unwrap().attempts.len(), 1);
    }

    #[tokio::test]
    async fn bg_notification_marker_dedupes_to_earliest() {
        let pool = test_pool().await;
        let mut tx = begin_chunk(&pool).await.unwrap();
        for (reference, ts) in [
            ("task-1", "2026-10-04T10:00:09.000Z"),
            ("task-1", "2026-10-04T10:00:04.000Z"),
            ("task-1", "2026-10-04T10:00:20.000Z"),
            ("task-2", "2026-10-04T10:00:30.000Z"),
        ] {
            insert_marker(&mut tx, &marker("bg_notification", reference, ts))
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let rows = session_rows(&pool, "s1").await.unwrap();
        assert_eq!(rows.markers.len(), 2);
        let one = rows
            .markers
            .iter()
            .find(|m| m.r#ref.as_deref() == Some("task-1"))
            .unwrap();
        assert_eq!(one.ts, "2026-10-04T10:00:04.000Z");
    }

    const T0: &str = "2026-10-04T10:00:00.000Z";

    fn session_row(id: &str, project: &str, dirty: i64) -> SessionRow {
        SessionRow {
            session_id: id.to_string(),
            project_id: project.to_string(),
            started_at: Some(T0.to_string()),
            ended_at: Some("2026-10-04T11:00:00.000Z".to_string()),
            dirty,
            parser_version: 1,
            ..Default::default()
        }
    }

    async fn add_session(pool: &SqlitePool, id: &str, project: &str, dirty: i64) {
        let mut tx = begin_chunk(pool).await.unwrap();
        upsert_session(&mut tx, &session_row(id, project, dirty))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn add_attempt(pool: &SqlitePool, session: &str, id: &str, lane: &str, started: &str) {
        let mut row = attempt(id);
        row.session_id = session.to_string();
        row.lane = lane.to_string();
        row.started_at = started.to_string();
        let mut tx = begin_chunk(pool).await.unwrap();
        upsert_attempt_launch(&mut tx, &row).await.unwrap();
        tx.commit().await.unwrap();
    }

    fn span(session: &str, start: &str, end: &str) -> SpanRow {
        SpanRow {
            session_id: session.to_string(),
            lane: "main".to_string(),
            kind: "tool".to_string(),
            started_at: start.to_string(),
            ended_at: end.to_string(),
            confidence: "exact".to_string(),
            parser_version: 1,
            ..Default::default()
        }
    }

    fn finding(key: &str, rule: &str, session: &str, scope: &str, confidence: &str) -> FindingRow {
        FindingRow {
            finding_key: key.to_string(),
            project_id: "p1".to_string(),
            session_id: session.to_string(),
            scope: scope.to_string(),
            rule_id: rule.to_string(),
            rule_version: 1,
            level: "base".to_string(),
            waste: "rework".to_string(),
            lever: "model".to_string(),
            confidence: confidence.to_string(),
            lane: "main".to_string(),
            started_at: T0.to_string(),
            ended_at: "2026-10-04T10:05:00.000Z".to_string(),
            cost_ms: 300_000,
            count: 1,
            attempt_ids: "[\"a1\"]".to_string(),
            sessions: String::new(),
            rules_version: "fp".to_string(),
            parser_version: 2,
        }
    }

    fn span_mark(id: i64, rule: &str, key: &str) -> SpanMark {
        SpanMark {
            span_id: id,
            waste: Some("rework".to_string()),
            rule_id: Some(rule.to_string()),
            lever: Some("model".to_string()),
            finding_key: Some(key.to_string()),
        }
    }

    /// Writes `findings` as session `session`'s rule results, reading the head just before.
    async fn rule_session(pool: &SqlitePool, session: &str, findings: Vec<FindingRow>) {
        let head = session_head(pool, session).await.unwrap().unwrap();
        write_rule_results(
            pool,
            &head,
            &RuleWrite {
                findings,
                ..Default::default()
            },
            "fp",
        )
        .await
        .unwrap();
    }

    async fn count(pool: &SqlitePool, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }

    #[tokio::test]
    async fn migration_0171_adds_rule_columns_and_tables() {
        let pool = test_pool().await;
        let added: &[(&str, &[&str])] = &[
            ("devtime_sessions", &["rules_version", "rules_at"]),
            ("devtime_attempts", &["bg_status", "refs_in", "refs_out"]),
            ("devtime_spans", &["lever", "finding_key"]),
            (
                "devtime_findings",
                &[
                    "id",
                    "finding_key",
                    "project_id",
                    "session_id",
                    "scope",
                    "rule_id",
                    "rule_version",
                    "level",
                    "waste",
                    "lever",
                    "confidence",
                    "lane",
                    "started_at",
                    "ended_at",
                    "cost_ms",
                    "count",
                    "attempt_ids",
                    "sessions",
                    "rules_version",
                    "parser_version",
                ],
            ),
            (
                "devtime_attempt_marks",
                &[
                    "attempt_id",
                    "session_id",
                    "waste",
                    "rule_id",
                    "lever",
                    "finding_key",
                    "verified",
                    "verified_by",
                    "rules_version",
                ],
            ),
            (
                "devtime_feedback",
                &[
                    "id",
                    "finding_key",
                    "rule_id",
                    "session_id",
                    "verdict",
                    "cause",
                    "rule_version",
                    "marked_at",
                ],
            ),
            (
                "devtime_turn_stats",
                &[
                    "session_id",
                    "turn_seq",
                    "project_id",
                    "turn_class",
                    "started_at",
                    "calls",
                    "active_ms",
                    "explained_ms",
                    "rules_version",
                ],
            ),
        ];
        for (table, want) in added {
            let got = columns(&pool, table).await;
            for column in *want {
                assert!(
                    got.iter().any(|c| c == column),
                    "{table}.{column} is missing; has {got:?}"
                );
            }
        }
        for table in [
            "devtime_findings",
            "devtime_attempt_marks",
            "devtime_feedback",
            "devtime_turn_stats",
        ] {
            let want = added.iter().find(|(name, _)| *name == table).unwrap().1;
            assert_eq!(columns(&pool, table).await.len(), want.len(), "{table}");
        }
    }

    #[tokio::test]
    async fn write_rule_results_replaces_session_scope_and_clears_dirty() {
        let pool = test_pool().await;
        add_session(&pool, "s1", "p1", 1).await;
        add_attempt(&pool, "s1", "a1", "main", T0).await;
        replace_spans(
            &pool,
            "s1",
            &[
                span("s1", T0, "2026-10-04T10:01:00.000Z"),
                span("s1", "2026-10-04T10:01:00.000Z", "2026-10-04T10:02:00.000Z"),
            ],
        )
        .await
        .unwrap();
        let ids: Vec<i64> = session_spans(&pool, "s1")
            .await
            .unwrap()
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids.len(), 2);
        // A cross finding of the same project must survive both writes.
        replace_cross_findings(
            &pool,
            "p1",
            &["F1"],
            &[finding("kc", "F1", "s1", "cross", "inferred")],
        )
        .await
        .unwrap();

        let head = session_head(&pool, "s1").await.unwrap().unwrap();
        assert_eq!(head.dirty, 1);
        assert_eq!(head.rules_version, None);
        write_rule_results(
            &pool,
            &head,
            &RuleWrite {
                findings: vec![finding("k1", "A1", "s1", "session", "exact")],
                span_marks: vec![span_mark(ids[0], "A1", "k1")],
                attempt_marks: vec![AttemptMarkRow {
                    attempt_id: "a1".to_string(),
                    session_id: "s1".to_string(),
                    waste: Some("rework".to_string()),
                    rule_id: Some("A1".to_string()),
                    lever: Some("model".to_string()),
                    finding_key: Some("k1".to_string()),
                    verified: "failed".to_string(),
                    verified_by: Some("A1".to_string()),
                }],
                turn_stats: vec![TurnStatRow {
                    session_id: "s1".to_string(),
                    turn_seq: 1,
                    project_id: "p1".to_string(),
                    turn_class: "c1".to_string(),
                    started_at: T0.to_string(),
                    calls: 3,
                    active_ms: 60_000,
                    explained_ms: 60_000,
                }],
            },
            "fp1",
        )
        .await
        .unwrap();
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM devtime_findings WHERE scope = 'session'"
            )
            .await,
            1
        );
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM devtime_turn_stats").await,
            1
        );
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM devtime_attempt_marks").await,
            1
        );

        // The second write has a different finding on the other span: the first one is replaced.
        let head = session_head(&pool, "s1").await.unwrap().unwrap();
        assert_eq!(head.dirty, 0, "the first write cleared dirty");
        assert_eq!(head.rules_version.as_deref(), Some("fp1"));
        write_rule_results(
            &pool,
            &head,
            &RuleWrite {
                findings: vec![finding("k2", "B1", "s1", "session", "exact")],
                span_marks: vec![span_mark(ids[1], "B1", "k2")],
                ..Default::default()
            },
            "fp2",
        )
        .await
        .unwrap();
        let keys: Vec<String> =
            sqlx::query_scalar("SELECT finding_key FROM devtime_findings WHERE scope = 'session'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(keys, ["k2"]);
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM devtime_findings WHERE scope = 'cross'"
            )
            .await,
            1,
            "the cross finding is untouched"
        );
        let spans = session_spans(&pool, "s1").await.unwrap();
        assert_eq!(spans[0].rule_id, None, "the earlier annotation was reset");
        assert_eq!(spans[0].waste, None);
        assert_eq!(spans[1].rule_id.as_deref(), Some("B1"));
        let lever: Option<String> =
            sqlx::query_scalar("SELECT lever FROM devtime_spans WHERE id = ?")
                .bind(ids[1])
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(lever.as_deref(), Some("model"));
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM devtime_attempt_marks").await,
            0
        );
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM devtime_turn_stats").await,
            0
        );
        let head = session_head(&pool, "s1").await.unwrap().unwrap();
        assert_eq!(head.rules_version.as_deref(), Some("fp2"));
        assert_eq!(head.dirty, 0);
    }

    #[tokio::test]
    async fn dirty_survives_a_write_racing_new_rows() {
        let pool = test_pool().await;
        add_session(&pool, "s1", "p1", 1).await;
        let head = session_head(&pool, "s1").await.unwrap().unwrap();
        // Ingestion touches the session after the head was read.
        sqlx::query("UPDATE devtime_sessions SET updated_at = '2999-01-01T00:00:00.000Z'")
            .execute(&pool)
            .await
            .unwrap();
        write_rule_results(&pool, &head, &RuleWrite::default(), "fp")
            .await
            .unwrap();
        let after = session_head(&pool, "s1").await.unwrap().unwrap();
        assert_eq!(
            after.dirty, 1,
            "rows arrived mid-run, so the rules still owe a pass"
        );
        assert_eq!(after.rules_version.as_deref(), Some("fp"));
    }

    #[tokio::test]
    async fn sessions_needing_rules_selects_dirty_null_and_stale() {
        let pool = test_pool().await;
        for id in ["dirty", "current", "stale", "never"] {
            add_session(&pool, id, "p1", i64::from(id == "dirty")).await;
        }
        sqlx::query("UPDATE devtime_sessions SET rules_version = 'fp' WHERE session_id IN ('dirty', 'current')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE devtime_sessions SET rules_version = 'old' WHERE session_id = 'stale'")
            .execute(&pool)
            .await
            .unwrap();
        let mut got = sessions_needing_rules(&pool, "fp", 50).await.unwrap();
        got.sort();
        assert_eq!(got, ["dirty", "never", "stale"]);
        assert_eq!(
            sessions_needing_rules(&pool, "fp", 2).await.unwrap().len(),
            2
        );
    }

    fn feedback(key: &str, rule: &str, verdict: &str) -> FeedbackRow {
        FeedbackRow {
            finding_key: key.to_string(),
            rule_id: rule.to_string(),
            session_id: Some("s1".to_string()),
            verdict: verdict.to_string(),
            rule_version: Some(1),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn feedback_upsert_keeps_one_mark_per_key_and_case_counts_join_current_findings() {
        let pool = test_pool().await;
        add_session(&pool, "s1", "p1", 1).await;
        rule_session(
            &pool,
            "s1",
            vec![
                finding("k1", "A1", "s1", "session", "exact"),
                finding("k2", "A1", "s1", "session", "inferred"),
                finding("k3", "A1", "s1", "session", "inferred"),
            ],
        )
        .await;
        upsert_feedback(&pool, &feedback("k1", "A1", "not_rework"))
            .await
            .unwrap();
        upsert_feedback(&pool, &feedback("k2", "A1", "not_rework"))
            .await
            .unwrap();
        // The latest mark wins: k2 is re-marked as confirmed.
        let mut again = feedback("k2", "A1", "confirmed");
        again.cause = Some("spec_gap".to_string());
        upsert_feedback(&pool, &again).await.unwrap();
        // A mark whose finding no longer exists is kept but is not a case.
        upsert_feedback(&pool, &feedback("ghost", "A1", "not_rework"))
            .await
            .unwrap();

        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM devtime_feedback").await,
            3
        );
        let verdict: String =
            sqlx::query_scalar("SELECT verdict FROM devtime_feedback WHERE finding_key = 'k2'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(verdict, "confirmed");
        let counts = rule_case_counts(&pool, None).await.unwrap();
        assert_eq!(
            counts,
            vec![RuleCaseCounts {
                rule_id: "A1".to_string(),
                exact: 1,
                inferred_marked: 1,
                not_rework: 1,
                total: 3,
            }]
        );
        assert!(
            rule_case_counts(&pool, Some("2999-01-01T00:00:00.000Z"))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            finding_by_key(&pool, "k3").await.unwrap().unwrap().rule_id,
            "A1"
        );
        assert!(finding_by_key(&pool, "ghost").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn base_rates_exclude_sessions_without_attempts() {
        let pool = test_pool().await;
        for id in ["s1", "s2", "s3"] {
            add_session(&pool, id, "p1", 1).await;
        }
        add_session(&pool, "s4", "p2", 1).await;
        add_attempt(&pool, "s1", "a1", "main", T0).await;
        add_attempt(&pool, "s3", "a3", "main", T0).await;
        add_attempt(&pool, "s4", "a4", "main", T0).await;
        rule_session(
            &pool,
            "s1",
            vec![finding("k1", "A1", "s1", "session", "exact")],
        )
        .await;
        // s2 has no attempt: its finding cannot count, and neither can the session.
        rule_session(
            &pool,
            "s2",
            vec![finding("k2", "A1", "s2", "session", "exact")],
        )
        .await;

        let (sessions, fired) = rule_base_rates(&pool, Some("p1"), None).await.unwrap();
        assert_eq!(sessions, 2);
        assert_eq!(fired, [("A1".to_string(), 1)]);
        let (all, _) = rule_base_rates(&pool, None, None).await.unwrap();
        assert_eq!(all, 3);
        let (none, fired) = rule_base_rates(&pool, Some("nobody"), None).await.unwrap();
        assert_eq!(none, 0);
        assert!(fired.is_empty());
    }

    #[tokio::test]
    async fn attempt_refs_and_bg_status_round_trip() {
        let pool = test_pool().await;
        let mut first = attempt("a1");
        first.refs_in = "[\"aaaaaaaa\"]".to_string();
        let mut second = attempt("a2");
        second.tool_use_id = "toolu_2".to_string();
        let mut tx = begin_chunk(&pool).await.unwrap();
        upsert_attempt_launch(&mut tx, &first).await.unwrap();
        upsert_attempt_launch(&mut tx, &second).await.unwrap();
        complete_attempt(
            &mut tx,
            "a1",
            &AttemptResult {
                outcome: "launched".to_string(),
                refs_out: "[\"bbbbbbbb\",\"cccccccc\"]".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        complete_attempt(
            &mut tx,
            "a2",
            &AttemptResult {
                outcome: "ok".to_string(),
                exit_code: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // A NULL exit code is filled; one the result already carried is kept.
        set_bg_status(&mut tx, "a1", "failed", Some(2))
            .await
            .unwrap();
        set_bg_status(&mut tx, "a2", "failed", Some(9))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let rows = session_rows(&pool, "s1").await.unwrap();
        let a1 = rows.attempts.iter().find(|a| a.attempt_id == "a1").unwrap();
        assert_eq!(a1.refs_in, "[\"aaaaaaaa\"]");
        assert_eq!(a1.refs_out, "[\"bbbbbbbb\",\"cccccccc\"]");
        assert_eq!(a1.bg_status.as_deref(), Some("failed"));
        assert_eq!(a1.exit_code, Some(2));
        let a2 = rows.attempts.iter().find(|a| a.attempt_id == "a2").unwrap();
        assert_eq!(
            a2.refs_in, "[]",
            "an empty launch holds an empty JSON array"
        );
        assert_eq!(a2.refs_out, "[]");
        assert_eq!(a2.bg_status.as_deref(), Some("failed"));
        assert_eq!(a2.exit_code, Some(0));
    }

    #[tokio::test]
    async fn cross_session_rows_are_main_lane_and_windowed() {
        let pool = test_pool().await;
        add_session(&pool, "s1", "p1", 1).await;
        add_session(&pool, "s2", "p1", 1).await;
        add_session(&pool, "s3", "p2", 1).await;
        add_attempt(&pool, "s1", "old", "main", "2026-09-01T10:00:00.000Z").await;
        add_attempt(&pool, "s1", "in1", "main", "2026-10-04T10:00:00.000Z").await;
        add_attempt(&pool, "s1", "side", "agent:x", "2026-10-04T10:00:01.000Z").await;
        add_attempt(&pool, "s2", "in2", "main", "2026-10-04T09:00:00.000Z").await;
        add_attempt(&pool, "s3", "other", "main", "2026-10-04T10:00:00.000Z").await;

        let rows = cross_session_rows(&pool, "p1", "2026-10-01T00:00:00.000Z")
            .await
            .unwrap();
        let seen: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.session_id.as_str(), r.started_at.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                ("s1", "2026-10-04T10:00:00.000Z"),
                ("s2", "2026-10-04T09:00:00.000Z"),
            ],
            "main lane only, this project only, inside the window, in session then time order"
        );
        assert_eq!(rows[0].tool_name, "Bash");
    }
}
