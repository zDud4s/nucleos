//! The devtime tables' only SQL (migration 0164): files and offsets, the cwd map, sessions, turns,
//! messages (usage deduped by message id), attempts, spans, markers and the ingest status.
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
             files, edits, reads, parser_version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
            bg_task_id = COALESCE(?, bg_task_id)
         WHERE attempt_id = ?",
    )
    .bind(&result.ended_at)
    .bind(&result.outcome)
    .bind(result.exit_code)
    .bind(&result.error_class)
    .bind(&result.agent_id)
    .bind(&result.model)
    .bind(&result.bg_task_id)
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
                bg_confidence, files, edits, reads, parser_version
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
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
}
