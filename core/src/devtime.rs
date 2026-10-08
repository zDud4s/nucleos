//! The devtime ingestion cycle: walks the transcripts directory read-only, streams each file from
//! its stored offset, applies the parsed events through `devtime_store`, and rebuilds the lanes of
//! every session it touched, then hands the touched sessions to the rules pass
//! (`devtime_rules::run_rules_pass`). Owns no SQL and no parse rule.
//!
//! **Read-only on the transcripts.** Files are only ever opened for reading; nothing under the
//! projects directory is written, moved or removed.
//!
//! **Incremental.** A file is read from its stored byte offset, in chunks of at most 8 MB. Only
//! newline-terminated lines are consumed, so a line the CLI is still writing is left for the next
//! cycle. Each chunk's events, its offset and its running counters commit in ONE transaction.
//!
//! Choices the spec leaves open:
//! - An unmapped or daemon-worktree file keeps its offset untouched and only gets a status. An
//!   unmapped cwd is looked at again after an hour (`devtime_map`), which is when a roster change
//!   makes its sessions ingestible.
//! - A notification whose `tool-use-id` is empty gives no marker: its lane and its launch cannot be
//!   named, and a guess would put a wait on the wrong lane.
//! - A `bg_ref` is looked for by comparing short string values of a tool call's input against the
//!   background task ids the session already holds. The values are compared, never stored.

use crate::config::DevtimeConfig;
use crate::devtime_lanes;
use crate::devtime_map;
use crate::devtime_parse::{self, Event, EventKind, PARSER_VERSION, ParseVocab, Parsed};
use crate::devtime_rules::{self, AdapterSources};
use crate::devtime_store::{
    self, AttemptResult, AttemptRow, CwdMapping, FileCounters, MarkerRow, MessageRow, SessionRow,
};
use serde_json::{Value, json};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// The most bytes one transaction covers.
const CHUNK_BYTES: u64 = 8 * 1024 * 1024;
/// How far into a transcript the first record carrying a `cwd` is looked for.
const CWD_SCAN_BYTES: u64 = 1024 * 1024;
/// A tool input string longer than this cannot be a task id and is not kept for comparison.
const MAX_INPUT_VALUE: usize = 64;

/// What one cycle saw, for the ingest status row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CycleStats {
    pub files_seen: i64,
    pub files_failed: i64,
    pub lines_read: i64,
    pub lines_failed: i64,
    pub unknown_records: i64,
    pub unmapped_sessions: i64,
    pub skipped_daemon_sessions: i64,
}

enum Fail {
    Io(std::io::Error),
    Db(sqlx::Error),
}

impl From<std::io::Error> for Fail {
    fn from(error: std::io::Error) -> Self {
        Fail::Io(error)
    }
}

impl From<sqlx::Error> for Fail {
    fn from(error: sqlx::Error) -> Self {
        Fail::Db(error)
    }
}

/// The key a transcript is stored under in `devtime_files`.
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn mtime_text(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time)
        .format("%Y-%m-%dT%H:%M:%S%.9fZ")
        .to_string()
}

// ---- walking ---------------------------------------------------------------------------------

struct AgentMeta {
    tool_use_id: Option<String>,
    agent_type: Option<String>,
}

/// One transcript file found under the projects directory.
struct Found {
    path: PathBuf,
    /// The session's main transcript, which exists or not; where the session's cwd is looked for.
    main: PathBuf,
    session: String,
    /// `main` or `agent:<id>`.
    lane: String,
    meta: Option<AgentMeta>,
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(read) => read
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();
    entries
}

fn read_meta(path: &Path) -> Option<AgentMeta> {
    let value: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(AgentMeta {
        tool_use_id: text("toolUseId"),
        agent_type: text("agentType"),
    })
}

/// `projects_dir/*/<session>.jsonl` and `projects_dir/*/<session>/subagents/agent-<id>.jsonl`;
/// everything else is ignored. A session's main file sorts before its agent files.
fn walk(projects_dir: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    for project in sorted_entries(projects_dir) {
        if !project.is_dir() {
            continue;
        }
        for entry in sorted_entries(&project) {
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if entry.is_dir() {
                let subagents = entry.join("subagents");
                let main = project.join(format!("{name}.jsonl"));
                for file in sorted_entries(&subagents) {
                    let Some(id) = file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|n| n.strip_prefix("agent-"))
                        .and_then(|n| n.strip_suffix(".jsonl"))
                    else {
                        continue;
                    };
                    if !file.is_file() {
                        continue;
                    }
                    let meta = read_meta(&subagents.join(format!("agent-{id}.meta.json")));
                    found.push(Found {
                        lane: format!("agent:{id}"),
                        path: file,
                        main: main.clone(),
                        session: name.to_owned(),
                        meta,
                    });
                }
            } else if let Some(session) = name.strip_suffix(".jsonl")
                && entry.is_file()
            {
                found.push(Found {
                    session: session.to_owned(),
                    lane: "main".to_owned(),
                    main: entry.clone(),
                    path: entry,
                    meta: None,
                });
            }
        }
    }
    found.sort_by(|a, b| {
        (&a.main, a.lane != "main", &a.path).cmp(&(&b.main, b.lane != "main", &b.path))
    });
    found
}

/// The first `cwd` in the first megabyte of a transcript.
fn cwd_in(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(CWD_SCAN_BYTES));
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return None;
        }
        let Ok(Value::Object(record)) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if let Some(cwd) = record.get("cwd").and_then(Value::as_str)
            && !cwd.is_empty()
        {
            return Some(cwd.to_owned());
        }
    }
}

fn find_cwd(paths: &[PathBuf]) -> Option<String> {
    paths.iter().find_map(|path| cwd_in(path))
}

// ---- reading ---------------------------------------------------------------------------------

struct Item {
    event: Event,
    /// Per `tool_use` of an assistant record: its id and the short string values of its input.
    inputs: Vec<(String, Vec<String>)>,
}

struct Chunk {
    items: Vec<Item>,
    /// Bytes of complete lines consumed.
    consumed: u64,
    lines_read: i64,
    lines_failed: i64,
    unknown: i64,
    /// The chunk stopped at its size limit, not at the end of the file.
    more: bool,
}

fn tool_inputs(event: &Event, line: &[u8]) -> Vec<(String, Vec<String>)> {
    let EventKind::Assistant { tool_uses, .. } = &event.kind else {
        return Vec::new();
    };
    if tool_uses.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Vec::new();
    };
    let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|block| {
            let id = block.get("id")?.as_str()?.to_owned();
            let values = block
                .get("input")?
                .as_object()?
                .values()
                .filter_map(Value::as_str)
                .filter(|text| text.len() <= MAX_INPUT_VALUE)
                .map(str::to_owned)
                .collect();
            Some((id, values))
        })
        .collect()
}

/// Reads and parses up to one chunk of complete lines from `offset`.
fn read_chunk(path: &Path, offset: u64, vocab: Arc<ParseVocab>) -> std::io::Result<Chunk> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);
    let mut chunk = Chunk {
        items: Vec::new(),
        consumed: 0,
        lines_read: 0,
        lines_failed: 0,
        unknown: 0,
        more: false,
    };
    let mut buf = Vec::new();
    loop {
        if chunk.consumed >= CHUNK_BYTES {
            chunk.more = true;
            break;
        }
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            // End of file, or a torn tail the writer has not finished: not consumed.
            break;
        }
        chunk.consumed += n as u64;
        let mut line = &buf[..n - 1];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        chunk.lines_read += 1;
        match devtime_parse::parse_line_with(line, &vocab) {
            Parsed::Event(event) => {
                let inputs = tool_inputs(&event, line);
                chunk.items.push(Item { event, inputs });
            }
            Parsed::Skipped => {}
            Parsed::Unknown => chunk.unknown += 1,
            Parsed::Failed => chunk.lines_failed += 1,
        }
    }
    Ok(chunk)
}

// ---- applying --------------------------------------------------------------------------------

struct Ctx<'a> {
    session: &'a str,
    /// `main` or `agent:<id>`: the lane the file itself stands for.
    file_lane: &'a str,
    project: &'a str,
    worktree: Option<&'a str>,
}

/// An agent file's records are all its lane; a main file's inline sidechain records carry theirs.
fn lane_of(file_lane: &str, event: &Event) -> String {
    if file_lane != "main" {
        return file_lane.to_owned();
    }
    match (&event.agent_id, event.is_sidechain) {
        (Some(agent), true) => format!("agent:{agent}"),
        _ => "main".to_owned(),
    }
}

fn attempt_id(session: &str, tool_use_id: &str) -> String {
    format!("{session}:{tool_use_id}")
}

/// A path a call writes, as stored: an absolute one relative to the worktree (see [`rel_path`]), and
/// a relative one (a `git restore a.rs` names it from the call's own directory) as written, with its
/// separators normalised.
fn written_path(worktree: Option<&str>, path: &str) -> String {
    let unix = path.replace('\\', "/");
    let absolute = unix.starts_with('/') || unix.as_bytes().get(1) == Some(&b':');
    if absolute {
        rel_path(worktree, path)
    } else {
        unix.strip_prefix("./").unwrap_or(&unix).to_owned()
    }
}

/// A path relative to the session's worktree; outside it, only the basename survives.
fn rel_path(worktree: Option<&str>, path: &str) -> String {
    let path = path.replace('\\', "/");
    if let Some(worktree) = worktree {
        let prefix = format!("{}/", worktree.trim_end_matches('/'));
        let head_matches = path.get(..prefix.len()).is_some_and(|head| {
            if cfg!(windows) {
                head.eq_ignore_ascii_case(&prefix)
            } else {
                head == prefix
            }
        });
        if head_matches && path.len() > prefix.len() {
            return path[prefix.len()..].to_owned();
        }
    }
    path.rsplit('/').next().unwrap_or("").to_owned()
}

fn marker(ctx: &Ctx<'_>, lane: &str, ts: &str, kind: &str, reference: Option<&str>) -> MarkerRow {
    MarkerRow {
        id: 0,
        session_id: ctx.session.to_owned(),
        lane: lane.to_owned(),
        ts: ts.to_owned(),
        kind: kind.to_owned(),
        r#ref: reference.map(str::to_owned),
        parser_version: PARSER_VERSION,
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

async fn apply_event(
    conn: &mut SqliteConnection,
    ctx: &Ctx<'_>,
    item: &Item,
    turn: &mut Option<i64>,
) -> sqlx::Result<()> {
    let event = &item.event;
    let lane = lane_of(ctx.file_lane, event);
    match &event.kind {
        EventKind::HumanPrompt { correction, .. } => {
            if lane == "main" {
                let seq = devtime_store::open_turn(
                    conn,
                    ctx.session,
                    &event.ts,
                    *correction,
                    PARSER_VERSION,
                )
                .await?;
                *turn = Some(seq);
            }
        }
        EventKind::Assistant {
            message_id,
            model,
            effort,
            usage,
            tool_uses,
            ..
        } => {
            devtime_store::upsert_message(
                conn,
                &MessageRow {
                    session_id: ctx.session.to_owned(),
                    lane: lane.clone(),
                    message_id: message_id.clone(),
                    first_at: event.ts.clone(),
                    last_at: event.ts.clone(),
                    model: non_empty(model),
                    effort: effort.clone(),
                    input_tokens: usage[0],
                    cache_read_tokens: usage[1],
                    cache_creation_tokens: usage[2],
                    output_tokens: usage[3],
                    has_tool_use: i64::from(!tool_uses.is_empty()),
                    parser_version: PARSER_VERSION,
                },
            )
            .await?;
            for tool in tool_uses.iter().filter(|tool| !tool.id.is_empty()) {
                let agent = matches!(tool.name.as_str(), "Agent" | "Task");
                let files: Vec<String> = tool
                    .files
                    .iter()
                    .map(|p| written_path(ctx.worktree, p))
                    .collect();
                let edits: Vec<Value> = tool
                    .edits
                    .iter()
                    .map(|(p, before, after)| {
                        json!({"path": rel_path(ctx.worktree, p), "before": before, "after": after})
                    })
                    .collect();
                let reads: Vec<Value> = tool
                    .reads
                    .iter()
                    .map(|(p, offset)| json!({"path": rel_path(ctx.worktree, p), "offset": offset}))
                    .collect();
                devtime_store::upsert_attempt_launch(
                    conn,
                    &AttemptRow {
                        attempt_id: attempt_id(ctx.session, &tool.id),
                        session_id: ctx.session.to_owned(),
                        lane: lane.clone(),
                        message_id: Some(message_id.clone()),
                        tool_use_id: tool.id.clone(),
                        kind: if agent { "agent" } else { "tool" }.to_owned(),
                        tool_name: tool.name.clone(),
                        model: non_empty(model),
                        effort: effort.clone(),
                        started_at: event.ts.clone(),
                        outcome: "unknown".to_owned(),
                        cmd_program: tool.cmd_program.clone(),
                        cmd_hash: tool.cmd_hash.clone(),
                        timeout_ms: tool.timeout_ms,
                        background: i64::from(tool.background),
                        files: serde_json::to_string(&files).unwrap_or_default(),
                        edits: serde_json::to_string(&edits).unwrap_or_default(),
                        reads: serde_json::to_string(&reads).unwrap_or_default(),
                        parser_version: PARSER_VERSION,
                        refs_in: serde_json::to_string(&tool.refs).unwrap_or_default(),
                        ..Default::default()
                    },
                )
                .await?;
            }
            // A tool call that names a background task the session knows is a reference to it.
            if item.inputs.iter().any(|(_, values)| !values.is_empty()) {
                let known = devtime_store::known_bg_task_ids(conn, ctx.session).await?;
                for (_, values) in &item.inputs {
                    for task in known.iter().filter(|task| values.contains(task)) {
                        devtime_store::insert_marker(
                            conn,
                            &marker(ctx, &lane, &event.ts, "bg_ref", Some(task)),
                        )
                        .await?;
                    }
                }
            }
            if let Some(seq) = *turn {
                devtime_store::touch_turn(conn, ctx.session, seq, &event.ts).await?;
            }
        }
        EventKind::ToolResults(results) => {
            for result in results.iter().filter(|r| !r.tool_use_id.is_empty()) {
                let id = attempt_id(ctx.session, &result.tool_use_id);
                devtime_store::complete_attempt(
                    conn,
                    &id,
                    &AttemptResult {
                        ended_at: Some(event.ts.clone()),
                        outcome: result.outcome.to_owned(),
                        exit_code: result.exit_code,
                        error_class: result.error_class.map(str::to_owned),
                        agent_id: result.agent_id.clone(),
                        model: result.resolved_model.clone(),
                        bg_task_id: result.bg_task_id.clone(),
                        refs_out: serde_json::to_string(&result.refs).unwrap_or_default(),
                    },
                )
                .await?;
                if result.outcome == "launched" {
                    devtime_store::set_background(conn, &id).await?;
                }
            }
            if let Some(seq) = *turn {
                devtime_store::touch_turn(conn, ctx.session, seq, &event.ts).await?;
            }
        }
        EventKind::Interrupt => {
            devtime_store::insert_marker(conn, &marker(ctx, &lane, &event.ts, "interrupt", None))
                .await?;
            if lane == "main"
                && let Some(seq) = *turn
            {
                devtime_store::mark_turn_interrupted(conn, ctx.session, seq).await?;
            }
        }
        EventKind::Notification {
            tool_use_id,
            status,
            exit_code,
            ..
        } => {
            if tool_use_id.is_empty() {
                return Ok(());
            }
            let launch = devtime_store::attempt_by_tool_use(conn, ctx.session, tool_use_id).await?;
            let marker_lane = launch.as_ref().map_or(lane.as_str(), |a| a.lane.as_str());
            devtime_store::insert_marker(
                conn,
                &marker(
                    ctx,
                    marker_lane,
                    &event.ts,
                    "bg_notification",
                    Some(tool_use_id),
                ),
            )
            .await?;
            // How it ended, for any launch kind: a status outside the closed vocabulary is left out.
            if let Some(launch) = &launch
                && devtime_store::BG_STATUSES.contains(&status.as_str())
            {
                devtime_store::set_bg_status(conn, &launch.attempt_id, status, *exit_code).await?;
            }
            // A background Bash really ended when it said so; the earliest sighting is that moment.
            if let Some(launch) = &launch
                && launch.kind != "agent"
                && launch.background != 0
                && launch
                    .bg_ended_at
                    .as_deref()
                    .is_none_or(|known| event.ts.as_str() < known)
            {
                devtime_store::set_bg_end(conn, &launch.attempt_id, &event.ts, "exact").await?;
            }
        }
        EventKind::Compact { .. } => {
            devtime_store::insert_marker(
                conn,
                &marker(ctx, &lane, &event.ts, "compact_boundary", None),
            )
            .await?;
        }
    }
    Ok(())
}

async fn apply_chunk(
    conn: &mut SqliteConnection,
    ctx: &Ctx<'_>,
    items: &[Item],
    turn: &mut Option<i64>,
) -> sqlx::Result<()> {
    for item in items {
        apply_event(conn, ctx, item, turn).await?;
    }
    let (Some(first), Some(last)) = (
        items.iter().map(|i| i.event.ts.as_str()).min(),
        items.iter().map(|i| i.event.ts.as_str()).max(),
    ) else {
        return Ok(());
    };
    devtime_store::upsert_session(
        conn,
        &SessionRow {
            session_id: ctx.session.to_owned(),
            project_id: ctx.project.to_owned(),
            worktree: ctx.worktree.map(str::to_owned),
            chat_id: None,
            started_at: Some(first.to_owned()),
            ended_at: Some(last.to_owned()),
            source: items.iter().find_map(|i| i.event.entrypoint.clone()),
            dirty: 1,
            parser_version: PARSER_VERSION,
            updated_at: String::new(),
        },
    )
    .await
}

// ---- one file --------------------------------------------------------------------------------

async fn set_status(pool: &SqlitePool, key: &str, status: &str) {
    let result: Result<(), Fail> = async {
        let known = devtime_store::file_state(pool, key).await?;
        if known.is_some_and(|state| state.status == status) {
            return Ok(());
        }
        let mut conn = pool.acquire().await?;
        devtime_store::mark_file_status(&mut conn, key, status).await?;
        Ok(())
    }
    .await;
    if let Err(Fail::Db(error)) = result {
        tracing::warn!(%error, file = key, "devtime: could not record a file status");
    }
}

async fn ingest_file(
    pool: &SqlitePool,
    file: &Found,
    project: &str,
    worktree: Option<&str>,
    vocab: &Arc<ParseVocab>,
    stats: &mut CycleStats,
    touched: &mut BTreeSet<String>,
) -> Result<(), Fail> {
    let key = path_key(&file.path);
    let meta = std::fs::metadata(&file.path)?;
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let mtime = mtime_text(meta.modified()?);

    // An agent file names the call that launched it; that link can arrive after the main file.
    if let (Some(agent), Some(link)) = (file.lane.strip_prefix("agent:"), file.meta.as_ref())
        && let Some(tool_use_id) = link.tool_use_id.as_deref()
    {
        let mut conn = pool.acquire().await?;
        let changed = devtime_store::link_agent(
            &mut conn,
            &file.session,
            tool_use_id,
            agent,
            link.agent_type.as_deref(),
        )
        .await?;
        if changed > 0 {
            touched.insert(file.session.clone());
        }
    }

    let state = devtime_store::file_state(pool, &key).await?;
    if let Some(state) = &state {
        if state.status == "ok" && state.size == size && state.mtime.as_deref() == Some(&mtime) {
            return Ok(());
        }
        if size < state.offset {
            if state.status != "truncated" {
                set_status(pool, &key, "truncated").await;
            }
            stats.files_failed += 1;
            return Ok(());
        }
    }

    let mut offset = state.as_ref().map_or(0, |s| s.offset);
    let mut counters = state
        .as_ref()
        .map_or_else(FileCounters::default, |s| FileCounters {
            lines_read: s.lines_read,
            lines_failed: s.lines_failed,
            unknown_records: s.unknown_records,
        });
    let needs_ok = state.as_ref().is_some_and(|s| s.status != "ok");
    let ctx = Ctx {
        session: &file.session,
        file_lane: &file.lane,
        project,
        worktree,
    };
    let mut turn: Option<i64> = None;
    let mut turn_loaded = false;
    loop {
        let path = file.path.clone();
        let from = u64::try_from(offset).unwrap_or(0);
        let vocab = Arc::clone(vocab);
        let chunk = tokio::task::spawn_blocking(move || read_chunk(&path, from, vocab))
            .await
            .map_err(|error| Fail::Io(std::io::Error::other(error)))??;

        let mut tx = devtime_store::begin_chunk(pool).await?;
        if !turn_loaded {
            turn = devtime_store::last_turn(&mut tx, &file.session).await?;
            turn_loaded = true;
        }
        apply_chunk(&mut tx, &ctx, &chunk.items, &mut turn).await?;
        offset += i64::try_from(chunk.consumed).unwrap_or(0);
        counters.lines_read += chunk.lines_read;
        counters.lines_failed += chunk.lines_failed;
        counters.unknown_records += chunk.unknown;
        devtime_store::save_offset(&mut tx, &key, offset, size, Some(&mtime), &counters).await?;
        devtime_store::set_file_identity(&mut tx, &key, &file.session, &file.lane).await?;
        if needs_ok {
            devtime_store::mark_file_status(&mut tx, &key, "ok").await?;
        }
        tx.commit().await?;

        stats.lines_read += chunk.lines_read;
        stats.lines_failed += chunk.lines_failed;
        stats.unknown_records += chunk.unknown;
        if !chunk.items.is_empty() {
            touched.insert(file.session.clone());
        }
        if !chunk.more {
            return Ok(());
        }
    }
}

// ---- the cycle -------------------------------------------------------------------------------

/// One pass over the projects directory. Never panics on input and never fails as a whole: a file
/// that cannot be read is marked and counted, and the rest go on.
pub async fn ingest_cycle(
    pool: &SqlitePool,
    cfg: &DevtimeConfig,
    projects_dir: &Path,
) -> CycleStats {
    let mut stats = CycleStats::default();
    let roster: Vec<(String, String)> = match crate::autopilot::project_roster(pool).await {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| row.project_root.map(|root| (row.project_id, root)))
            .collect(),
        Err(error) => {
            tracing::warn!(%error, "devtime: could not read the project roster");
            Vec::new()
        }
    };

    let dir = projects_dir.to_path_buf();
    let found = tokio::task::spawn_blocking(move || walk(&dir))
        .await
        .unwrap_or_default();
    stats.files_seen = i64::try_from(found.len()).unwrap_or(i64::MAX);

    let mut root_keys = HashMap::new();
    let mut mappings: HashMap<PathBuf, Option<CwdMapping>> = HashMap::new();
    let mut counted: HashSet<PathBuf> = HashSet::new();
    let mut touched: BTreeSet<String> = BTreeSet::new();
    let vocab = Arc::new(ParseVocab::from_config(&cfg.rules.vocab));

    for file in &found {
        let mapping = match mappings.get(&file.main) {
            Some(known) => known.clone(),
            None => {
                let candidates = [file.main.clone(), file.path.clone()];
                let cwd = tokio::task::spawn_blocking(move || find_cwd(&candidates))
                    .await
                    .unwrap_or(None);
                let resolved = match cwd {
                    Some(cwd) => {
                        Some(devtime_map::resolve_with(pool, &cwd, &roster, &mut root_keys).await)
                    }
                    None => None,
                };
                mappings.insert(file.main.clone(), resolved.clone());
                resolved
            }
        };
        let key = path_key(&file.path);
        match mapping.as_ref() {
            Some(m) if m.kind == "daemon_worktree" => {
                if counted.insert(file.main.clone()) {
                    stats.skipped_daemon_sessions += 1;
                }
                set_status(pool, &key, "skipped_daemon").await;
            }
            Some(CwdMapping {
                kind,
                project_id: Some(project),
                worktree,
                ..
            }) if kind == "project" || kind == "worktree" => {
                match ingest_file(
                    pool,
                    file,
                    project,
                    worktree.as_deref(),
                    &vocab,
                    &mut stats,
                    &mut touched,
                )
                .await
                {
                    Ok(()) => {}
                    Err(Fail::Io(error)) => {
                        tracing::warn!(%error, file = %key, "devtime: a transcript could not be read");
                        stats.files_failed += 1;
                        set_status(pool, &key, "unreadable").await;
                    }
                    Err(Fail::Db(error)) => {
                        tracing::warn!(%error, file = %key, "devtime: a transcript could not be stored");
                        stats.files_failed += 1;
                    }
                }
            }
            _ => {
                if counted.insert(file.main.clone()) {
                    stats.unmapped_sessions += 1;
                }
                set_status(pool, &key, "unmapped").await;
            }
        }
    }

    let idle = Duration::from_secs(cfg.idle_minutes.saturating_mul(60));
    let mut lanes_failed: BTreeSet<String> = BTreeSet::new();
    for session in &touched {
        let rebuilt = async {
            let rows = devtime_store::session_rows(pool, session).await?;
            let spans = devtime_lanes::build_spans(&rows, idle);
            devtime_store::replace_spans(pool, session, &spans).await
        }
        .await;
        if let Err(error) = rebuilt {
            tracing::warn!(%error, session = %session, "devtime: could not rebuild a session's lanes");
            lanes_failed.insert(session.clone());
        }
    }
    // The rules read the spans just rebuilt, so a session whose lanes failed waits for the next cycle.
    if cfg.rules.enabled {
        let pass = devtime_rules::run_rules_pass(
            pool,
            &cfg.rules,
            AdapterSources::detect(&cfg.rules.adapters),
            &touched,
            &lanes_failed,
        )
        .await;
        tracing::debug!(
            sessions_ruled = pass.sessions_ruled,
            sessions_failed = pass.sessions_failed,
            families_panicked = pass.families_panicked,
            "devtime: rules pass"
        );
    }
    stats
}

// ---- the loop --------------------------------------------------------------------------------

fn status_row(
    enabled: bool,
    cfg: &DevtimeConfig,
    stats: &CycleStats,
    totals: &CycleStats,
) -> devtime_store::IngestStatus {
    devtime_store::IngestStatus {
        enabled,
        cycle_at: mtime_text(SystemTime::now()),
        files_seen: stats.files_seen,
        files_failed: stats.files_failed,
        lines_read: totals.lines_read,
        lines_failed: totals.lines_failed,
        unknown_records: totals.unknown_records,
        unmapped_sessions: stats.unmapped_sessions,
        skipped_daemon_sessions: stats.skipped_daemon_sessions,
        failure_amber_rate: cfg.parse_failure_amber_rate,
        failure_min_lines: i64::try_from(cfg.parse_failure_min_lines).unwrap_or(i64::MAX),
    }
}

/// Folds one cycle into the running totals. `files_failed` is not among them: see [`run_ingest_loop`].
fn accumulate(totals: &mut CycleStats, stats: &CycleStats) {
    totals.lines_read += stats.lines_read;
    totals.lines_failed += stats.lines_failed;
    totals.unknown_records += stats.unknown_records;
}

/// Runs [`ingest_cycle`] every `cycle_seconds` for as long as the daemon lives, and writes the
/// status row the health readout reads after each one.
///
/// Counters that only grow (lines, line failures, unknown records) are totals for this daemon
/// process; what a cycle can only observe afresh (files seen, files failed, unmapped and daemon
/// sessions, which are looked at again every cycle: a file that stays truncated fails again in each
/// one, and a sum would count it sixty times an hour) is that cycle's own figure. Each chunk of a transcript is
/// one transaction, so dropping the task mid-cycle leaves nothing half-written.
pub async fn run_ingest_loop(pool: SqlitePool, cfg: DevtimeConfig, projects_dir: Option<PathBuf>) {
    let Some(projects_dir) = projects_dir.filter(|_| cfg.enabled) else {
        let off = status_row(false, &cfg, &CycleStats::default(), &CycleStats::default());
        if let Err(error) = devtime_store::write_ingest_status(&pool, &off).await {
            tracing::warn!(%error, "devtime: could not write the ingest status");
        }
        return;
    };
    let mut ticker = tokio::time::interval(Duration::from_secs(cfg.cycle_seconds.max(1)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut totals = CycleStats::default();
    loop {
        ticker.tick().await;
        let stats = ingest_cycle(&pool, &cfg, &projects_dir).await;
        accumulate(&mut totals, &stats);
        let row = status_row(true, &cfg, &stats, &totals);
        if let Err(error) = devtime_store::write_ingest_status(&pool, &row).await {
            tracing::warn!(%error, "devtime: could not write the ingest status");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DevtimeConfig;
    use crate::devtime_store;
    use sha2::{Digest, Sha256};
    use sqlx::SqlitePool;

    use std::collections::BTreeMap;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    const ROOT: &str = "C:/fixture/proj";

    async fn test_pool() -> SqlitePool {
        let pool = crate::testdb::fresh_pool().await;
        // The roster is autopilot's table and its public writers demand an onboarded project with a
        // wired hook, which a fixture has no use for: the row is written directly, in the test only.
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('p1', 'shadow', ?)",
        )
        .bind(ROOT)
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    struct Harness {
        pool: SqlitePool,
        _dir: tempfile::TempDir,
        projects: PathBuf,
    }

    async fn harness(cases: &[&str]) -> Harness {
        let dir = tempfile::TempDir::new().unwrap();
        let projects = dir.path().join("projects");
        for case in cases {
            let src = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata")
                .join("devtime")
                .join(case);
            copy_dir(&src, &projects);
        }
        Harness {
            pool: test_pool().await,
            _dir: dir,
            projects,
        }
    }

    fn cycle_config() -> DevtimeConfig {
        DevtimeConfig::default()
    }

    async fn count(pool: &SqlitePool, table: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn proj_file(h: &Harness, name: &str) -> PathBuf {
        h.projects.join("c--fixture-proj").join(name)
    }

    #[tokio::test]
    async fn incremental_cycle_reads_only_appended_bytes() {
        let h = harness(&["parallel"]).await;
        let cfg = cycle_config();
        let first = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        assert_eq!(first.files_seen, 1);
        assert_eq!(first.lines_failed, 0);
        assert_eq!(
            count(&h.pool, "devtime_messages").await,
            2,
            "m1 dedupes, <synthetic> is skipped"
        );
        assert_eq!(count(&h.pool, "devtime_attempts").await, 3);
        let output: i64 = sqlx::query_scalar(
            "SELECT output_tokens FROM devtime_messages WHERE message_id = 'm1'",
        )
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(output, 40);

        let file = proj_file(&h, "s-par.jsonl");
        let len = std::fs::metadata(&file).unwrap().len() as i64;
        let state = devtime_store::file_state(&h.pool, &path_key(&file))
            .await
            .unwrap()
            .expect("the file has a row");
        assert_eq!(state.offset, len);

        let appended = "{\"type\":\"assistant\",\"timestamp\":\"2026-10-04T10:00:20.000Z\",\"sessionId\":\"s-par\",\"message\":{\"id\":\"m3\",\"model\":\"claude-x\",\"usage\":{\"output_tokens\":3},\"content\":[{\"type\":\"text\",\"text\":\"more\"}]}}\n";
        std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(appended.as_bytes())
            .unwrap();

        let second = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        assert_eq!(second.lines_read, 1, "only the appended line is read");
        assert_eq!(count(&h.pool, "devtime_messages").await, 3);
        assert_eq!(count(&h.pool, "devtime_attempts").await, 3);
        let grown = devtime_store::file_state(&h.pool, &path_key(&file))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(grown.offset, len + appended.len() as i64);
        assert!(grown.offset > state.offset);
        assert_eq!(
            grown.lines_read,
            state.lines_read + 1,
            "counters are running totals"
        );
    }

    #[tokio::test]
    async fn torn_last_line_is_left_for_the_next_cycle() {
        let h = harness(&["torn"]).await;
        let cfg = cycle_config();
        let file = proj_file(&h, "s-torn.jsonl");
        let len = std::fs::metadata(&file).unwrap().len() as i64;

        let first = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        assert_eq!(first.lines_read, 4, "the torn line is not read");
        assert_eq!(first.lines_failed, 1, "the malformed middle line");
        assert_eq!(
            first.unknown_records, 1,
            "the unknown type is counted, not failed"
        );
        assert_eq!(count(&h.pool, "devtime_messages").await, 1);
        let state = devtime_store::file_state(&h.pool, &path_key(&file))
            .await
            .unwrap()
            .unwrap();
        assert!(state.offset < len, "the offset stops before the torn tail");

        std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        let second = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        assert_eq!(second.lines_read, 1);
        assert_eq!(second.lines_failed, 0);
        assert_eq!(
            count(&h.pool, "devtime_messages").await,
            2,
            "the completed line is ingested once"
        );
        let done = devtime_store::file_state(&h.pool, &path_key(&file))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.offset, len + 1);
    }

    #[tokio::test]
    async fn truncated_file_is_marked_and_skipped() {
        let h = harness(&["parallel"]).await;
        let cfg = cycle_config();
        ingest_cycle(&h.pool, &cfg, &h.projects).await;
        let messages = count(&h.pool, "devtime_messages").await;

        let file = proj_file(&h, "s-par.jsonl");
        let text = std::fs::read_to_string(&file).unwrap();
        let first_line = text.lines().next().unwrap();
        std::fs::write(&file, format!("{first_line}\n")).unwrap();

        let second = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        assert_eq!(second.files_failed, 1);
        assert_eq!(second.lines_read, 0);
        let state = devtime_store::file_state(&h.pool, &path_key(&file))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.status, "truncated");
        assert_eq!(count(&h.pool, "devtime_messages").await, messages);
    }

    #[tokio::test]
    async fn files_failed_does_not_accumulate_across_cycles() {
        let h = harness(&["parallel"]).await;
        let cfg = cycle_config();
        ingest_cycle(&h.pool, &cfg, &h.projects).await;
        let file = proj_file(&h, "s-par.jsonl");
        let text = std::fs::read_to_string(&file).unwrap();
        let first_line = text.lines().next().unwrap();
        std::fs::write(
            &file,
            format!(
                "{first_line}
"
            ),
        )
        .unwrap();

        let mut totals = CycleStats::default();
        let first = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        accumulate(&mut totals, &first);
        let after_first = status_row(true, &cfg, &first, &totals);
        let second = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        accumulate(&mut totals, &second);
        let after_second = status_row(true, &cfg, &second, &totals);

        assert_eq!(after_first.files_failed, 1);
        assert_eq!(after_second.files_failed, after_first.files_failed);
    }

    async fn agent_lane(case: &str) -> (Vec<(String, String, String, String)>, Option<String>) {
        let h = harness(&[case]).await;
        ingest_cycle(&h.pool, &cycle_config(), &h.projects).await;
        let spans: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT kind, started_at, ended_at, confidence FROM devtime_spans
             WHERE lane = 'agent:a1' ORDER BY started_at, id",
        )
        .fetch_all(&h.pool)
        .await
        .unwrap();
        let agent: Option<String> = sqlx::query_scalar(
            "SELECT agent_id FROM devtime_attempts WHERE tool_use_id = 'toolu_ag1'",
        )
        .fetch_one(&h.pool)
        .await
        .unwrap();
        (spans, agent)
    }

    #[tokio::test]
    async fn both_subagent_layouts_produce_the_same_agent_lane() {
        let (from_files, files_agent) = agent_lane("background").await;
        let (inline, inline_agent) = agent_lane("inline_sidechain").await;
        assert_eq!(
            from_files.len(),
            2,
            "a tool group, then the closing message: {from_files:?}"
        );
        assert_eq!(from_files, inline);
        assert_eq!(files_agent.as_deref(), Some("a1"));
        assert_eq!(inline_agent.as_deref(), Some("a1"));
    }

    async fn dump(pool: &SqlitePool) -> Vec<String> {
        const TABLES: [&str; 11] = [
            "devtime_files",
            "devtime_sessions",
            "devtime_turns",
            "devtime_messages",
            "devtime_attempts",
            "devtime_spans",
            "devtime_markers",
            "devtime_cwd_map",
            "devtime_findings",
            "devtime_attempt_marks",
            "devtime_turn_stats",
        ];
        const CLOCK: [&str; 5] = ["updated_at", "resolved_at", "cycle_at", "id", "rules_at"];
        let mut out = Vec::new();
        for table in TABLES {
            let columns: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT name FROM pragma_table_info('{table}')"
            )))
            .fetch_all(pool)
            .await
            .unwrap();
            let expr = columns
                .iter()
                .filter(|c| !CLOCK.contains(&c.as_str()))
                .map(|c| format!("COALESCE(CAST(\"{c}\" AS TEXT), '~')"))
                .collect::<Vec<_>>()
                .join(" || '|' || ");
            let lines: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT {expr} AS line FROM {table} ORDER BY line"
            )))
            .fetch_all(pool)
            .await
            .unwrap();
            out.extend(lines.into_iter().map(|line| format!("{table}: {line}")));
        }
        out
    }

    #[tokio::test]
    async fn cycle_is_idempotent() {
        let h = harness(&["background", "parallel", "inline_sidechain", "compaction"]).await;
        let cfg = cycle_config();
        ingest_cycle(&h.pool, &cfg, &h.projects).await;
        let first = dump(&h.pool).await;
        assert!(first.iter().any(|l| l.starts_with("devtime_spans")));
        let markers: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devtime_markers WHERE kind = 'bg_notification'",
        )
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(markers, 2, "the bash and the agent notification, each once");
        let bg_end: Option<String> = sqlx::query_scalar(
            "SELECT bg_ended_at FROM devtime_attempts WHERE tool_use_id = 'toolu_b1'",
        )
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(bg_end.as_deref(), Some("2026-10-04T10:00:30.000Z"));
        let compactions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devtime_markers WHERE kind = 'compact_boundary'",
        )
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(compactions, 1);

        let second = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        assert_eq!(second.lines_read, 0);
        assert_eq!(dump(&h.pool).await, first);
    }

    #[tokio::test]
    async fn unmapped_and_daemon_sessions_are_counted_not_ingested() {
        let h = harness(&["daemon", "unmapped"]).await;
        let stats = ingest_cycle(&h.pool, &cycle_config(), &h.projects).await;
        assert_eq!(stats.files_seen, 2);
        assert_eq!(stats.skipped_daemon_sessions, 1);
        assert_eq!(stats.unmapped_sessions, 1);
        assert_eq!(stats.lines_read, 0);
        for table in [
            "devtime_sessions",
            "devtime_turns",
            "devtime_messages",
            "devtime_attempts",
            "devtime_spans",
        ] {
            assert_eq!(count(&h.pool, table).await, 0, "{table}");
        }
        let rows: Vec<(String, String, i64)> =
            sqlx::query_as("SELECT path, status, offset FROM devtime_files ORDER BY path")
                .fetch_all(&h.pool)
                .await
                .unwrap();
        let statuses: Vec<&str> = rows.iter().map(|(_, status, _)| status.as_str()).collect();
        assert_eq!(statuses, ["skipped_daemon", "unmapped"], "{rows:?}");
        assert!(
            rows.iter().all(|(_, _, offset)| *offset == 0),
            "the offset stays untouched"
        );
    }

    fn fingerprint(root: &Path) -> BTreeMap<String, String> {
        fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if entry.file_type().unwrap().is_dir() {
                    out.insert(rel, "dir".to_string());
                    walk(&path, root, out);
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                    let digest: String = Sha256::digest(&bytes)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    out.insert(rel, format!("{digest} {modified:?}"));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    #[tokio::test]
    async fn the_projects_tree_is_never_written() {
        let h = harness(&[
            "parallel",
            "background",
            "inline_sidechain",
            "torn",
            "compaction",
            "daemon",
            "unmapped",
        ])
        .await;
        let before = fingerprint(&h.projects);
        assert!(before.len() >= 8);
        let stats = ingest_cycle(&h.pool, &cycle_config(), &h.projects).await;
        assert!(stats.lines_read > 0);
        ingest_cycle(&h.pool, &cycle_config(), &h.projects).await;
        assert_eq!(fingerprint(&h.projects), before);
    }

    // ---- the replay of a stripped real transcript --------------------------------------------

    /// Writes the stripped replay fixture from a real main transcript named by
    /// `DEVTIME_STRIP_SOURCE`. Ignored: it reads a file outside the repository, once, by hand.
    #[test]
    #[ignore = "reads DEVTIME_STRIP_SOURCE; regenerates core/testdata/devtime/replay by hand"]
    fn regenerate_replay_fixture() {
        let source = std::env::var("DEVTIME_STRIP_SOURCE")
            .expect("DEVTIME_STRIP_SOURCE must name a real main transcript");
        let source = PathBuf::from(source);
        let name = source.file_name().expect("a file name").to_owned();
        let bytes = std::fs::read(&source).unwrap();
        let mut out = String::new();
        for line in bytes.split(|b| *b == b'\n') {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if let Some(stripped) = devtime_parse::strip_to_structure(line, ROOT) {
                out.push_str(&stripped);
                out.push('\n');
            }
        }
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("devtime")
            .join("replay")
            .join("projects")
            .join("c--fixture-proj");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join(name), out).unwrap();
    }

    fn millis(text: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(text)
            .unwrap()
            .timestamp_millis()
    }

    /// A lane's spans touch end to start and none is empty.
    fn assert_lane_partitions(lane: &str, spans: &[(String, String, String, String)]) {
        assert!(!spans.is_empty(), "lane {lane} has no spans");
        for (kind, start, end, _) in spans {
            assert!(start < end, "lane {lane}: empty {kind} span {start}..{end}");
        }
        for pair in spans.windows(2) {
            assert_eq!(pair[0].2, pair[1].1, "lane {lane}: gap or overlap {pair:?}");
        }
    }

    async fn render(pool: &SqlitePool) -> String {
        let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT lane, kind, started_at, ended_at, confidence FROM devtime_spans
             ORDER BY lane, started_at, id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        let mut lanes: BTreeMap<String, Vec<(String, String, String, String)>> = BTreeMap::new();
        for (lane, kind, start, end, confidence) in rows {
            lanes
                .entry(lane)
                .or_default()
                .push((kind, start, end, confidence));
        }
        let mut out = String::new();
        for (lane, spans) in &lanes {
            assert_lane_partitions(lane, spans);
            let origin = millis(&spans[0].1);
            out.push_str(&format!("lane {lane}\n"));
            for (kind, start, end, confidence) in spans {
                let from = millis(start) - origin;
                let length = millis(end) - millis(start);
                out.push_str(&format!("  {kind} +{from} {length} {confidence}\n"));
            }
        }
        let outcomes: Vec<(String, i64)> = sqlx::query_as(
            "SELECT outcome, COUNT(*) FROM devtime_attempts GROUP BY outcome ORDER BY outcome",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        out.push_str("attempts\n");
        for (outcome, n) in outcomes {
            out.push_str(&format!("  {outcome} {n}\n"));
        }
        let kinds: Vec<(String, i64)> = sqlx::query_as(
            "SELECT kind, COUNT(*) FROM devtime_markers GROUP BY kind ORDER BY kind",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        out.push_str("markers\n");
        for (kind, n) in kinds {
            out.push_str(&format!("  {kind} {n}\n"));
        }
        out
    }

    #[tokio::test]
    async fn replay_of_a_stripped_real_transcript_matches_golden() {
        let h = harness(&["replay/projects"]).await;
        let stats = ingest_cycle(&h.pool, &cycle_config(), &h.projects).await;
        assert!(stats.lines_read > 0, "the replay fixture holds lines");
        assert_eq!(stats.files_failed, 0);
        assert_eq!(
            stats.unmapped_sessions, 0,
            "the replay maps onto the fixture root"
        );
        let rendered = render(&h.pool).await;
        if std::env::var("DEVTIME_BLESS").as_deref() == Ok("1") {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata")
                .join("devtime")
                .join("replay")
                .join("golden.txt");
            std::fs::write(path, &rendered).unwrap();
            return;
        }
        let golden = include_str!("../testdata/devtime/replay/golden.txt").replace("\r\n", "\n");
        assert_eq!(rendered, golden);
    }

    /// The 8-hex reference a token is stored as, written out here so the test does not borrow the parser's.
    fn ref8(token: &str) -> String {
        Sha256::digest(token.as_bytes())
            .iter()
            .take(4)
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    async fn attempt_refs(pool: &SqlitePool, tool_use_id: &str) -> (Vec<String>, Vec<String>) {
        let (refs_in, refs_out): (String, String) =
            sqlx::query_as("SELECT refs_in, refs_out FROM devtime_attempts WHERE tool_use_id = ?")
                .bind(tool_use_id)
                .fetch_one(pool)
                .await
                .unwrap();
        (
            serde_json::from_str(&refs_in).unwrap(),
            serde_json::from_str(&refs_out).unwrap(),
        )
    }

    #[tokio::test]
    async fn v2_rows_store_correction_refs_bg_status_and_git_paths() {
        let h = harness(&["parallel"]).await;
        let stamp = |n: u32| format!("2026-10-04T12:00:{n:02}.000Z");
        let lines = [
            serde_json::json!({"type": "user", "timestamp": stamp(0), "sessionId": "s-v2",
                "cwd": ROOT, "entrypoint": "cli", "origin": {"kind": "human"},
                "message": {"role": "user", "content": "Não era isso, volta"}}),
            serde_json::json!({"type": "assistant", "timestamp": stamp(2), "sessionId": "s-v2",
            "cwd": ROOT, "message": {"id": "m-v2", "model": "claude-x",
            "usage": {"input_tokens": 1, "output_tokens": 2},
            "content": [
                {"type": "tool_use", "id": "toolu_r", "name": "Read",
                 "input": {"file_path": format!("{ROOT}/src/a.rs")}},
                {"type": "tool_use", "id": "toolu_g", "name": "Grep",
                 "input": {"pattern": "fn main", "path": ROOT}},
                {"type": "tool_use", "id": "toolu_b", "name": "Bash",
                 "input": {"command": "cargo build", "run_in_background": true}},
                {"type": "tool_use", "id": "toolu_gr", "name": "Bash",
                 "input": {"command": "git restore a.rs"}}
            ]}}),
            serde_json::json!({"type": "user", "timestamp": stamp(4), "sessionId": "s-v2",
            "cwd": ROOT, "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_r", "content": "file body"},
                {"type": "tool_result", "tool_use_id": "toolu_g",
                 "content": "src/a.rs:12: fn main() {}"},
                {"type": "tool_result", "tool_use_id": "toolu_gr", "content": ""}
            ]}}),
            serde_json::json!({"type": "user", "timestamp": stamp(5), "sessionId": "s-v2",
            "cwd": ROOT, "toolUseResult": {"backgroundTaskId": "bV2"},
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_b", "content": "started"}
            ]}}),
            serde_json::json!({"type": "queue-operation", "operation": "enqueue",
                "timestamp": stamp(9), "sessionId": "s-v2",
                "content": "<task-notification>\n<task-id>bV2</task-id>\n<tool-use-id>toolu_b</tool-use-id>\n\
                            <status>completed</status>\n<summary>Background command \"cargo build\" completed (exit code 2)</summary>\n</task-notification>"}),
        ];
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(proj_file(&h, "s-v2.jsonl"), body).unwrap();

        let stats = ingest_cycle(&h.pool, &cycle_config(), &h.projects).await;
        assert_eq!(stats.lines_failed, 0);

        let corrected: Option<i64> = sqlx::query_scalar(
            "SELECT opens_with_correction FROM devtime_turns WHERE session_id = 's-v2'",
        )
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(
            corrected,
            Some(1),
            "the prompt opens with a correction opener"
        );

        let (read_in, read_out) = attempt_refs(&h.pool, "toolu_r").await;
        let (grep_in, grep_out) = attempt_refs(&h.pool, "toolu_g").await;
        assert_eq!(read_in, vec![ref8("a.rs")]);
        assert!(read_out.is_empty(), "{read_out:?}");
        assert_eq!(grep_in, vec![ref8("proj")]);
        assert_eq!(grep_out, vec![ref8("a.rs")]);
        assert!(
            read_in.iter().any(|r| grep_out.contains(r)),
            "the read used what the grep returned"
        );

        let (status, exit_code, task): (Option<String>, Option<i64>, Option<String>) =
            sqlx::query_as(
                "SELECT bg_status, exit_code, bg_task_id FROM devtime_attempts
                 WHERE tool_use_id = 'toolu_b'",
            )
            .fetch_one(&h.pool)
            .await
            .unwrap();
        assert_eq!(status.as_deref(), Some("completed"));
        assert_eq!(exit_code, Some(2));
        assert_eq!(task.as_deref(), Some("bV2"));

        let files: String =
            sqlx::query_scalar("SELECT files FROM devtime_attempts WHERE tool_use_id = 'toolu_gr'")
                .fetch_one(&h.pool)
                .await
                .unwrap();
        assert_eq!(files, r#"["a.rs"]"#);

        let versions: Vec<i64> = sqlx::query_scalar(
            "SELECT DISTINCT parser_version FROM devtime_attempts WHERE session_id = 's-v2'",
        )
        .fetch_all(&h.pool)
        .await
        .unwrap();
        assert_eq!(versions, vec![PARSER_VERSION]);
    }

    const SENTINEL: &str = "SECRET-TEXT-42";

    #[tokio::test]
    async fn no_derived_row_holds_message_text() {
        let h = harness(&[
            "parallel",
            "background",
            "inline_sidechain",
            "torn",
            "compaction",
            "daemon",
            "unmapped",
        ])
        .await;
        let stamp = |n: u32| format!("2026-10-04T11:00:{n:02}.000Z");
        let lines = [
            serde_json::json!({"type": "user", "timestamp": stamp(0), "sessionId": "s-secret",
                "cwd": ROOT, "entrypoint": "cli", "origin": {"kind": "human"},
                "message": {"role": "user", "content": format!("prompt {SENTINEL}")}}),
            serde_json::json!({"type": "assistant", "timestamp": stamp(2), "sessionId": "s-secret",
            "cwd": ROOT, "message": {"id": "m-sec", "model": "claude-x",
            "usage": {"input_tokens": 1, "output_tokens": 2},
            "content": [
                {"type": "thinking", "thinking": format!("thinking {SENTINEL}")},
                {"type": "text", "text": format!("reply {SENTINEL}")},
                {"type": "tool_use", "id": "toolu_sec1", "name": "Bash",
                 "input": {"command": format!("echo {SENTINEL} && ls"), "description": SENTINEL}},
                {"type": "tool_use", "id": "toolu_sec2", "name": "Grep",
                 "input": {"pattern": SENTINEL, "path": ROOT}}
            ]}}),
            serde_json::json!({"type": "user", "timestamp": stamp(5), "sessionId": "s-secret",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_sec1", "is_error": true,
                 "content": format!("error: {SENTINEL}")},
                {"type": "tool_result", "tool_use_id": "toolu_sec2",
                 "content": format!("match {SENTINEL}")}
            ]}}),
        ];
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(proj_file(&h, "s-secret.jsonl"), body).unwrap();

        ingest_cycle(&h.pool, &cycle_config(), &h.projects).await;
        let secret_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devtime_attempts WHERE session_id = 's-secret'",
        )
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(secret_rows, 2, "the sentinel session was ingested");
        // The sentinel looks like an id, so it was tokenized: what is stored is its hash.
        let refs_in: String = sqlx::query_scalar(
            "SELECT refs_in FROM devtime_attempts WHERE tool_use_id = 'toolu_sec1'",
        )
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_ne!(
            refs_in, "[]",
            "the sentinel was read, and kept only as a hash"
        );
        // The prompt was evaluated against the openers, and only the flag was kept.
        let corrected: Option<i64> = sqlx::query_scalar(
            "SELECT opens_with_correction FROM devtime_turns WHERE session_id = 's-secret'",
        )
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(corrected, Some(0));

        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'devtime\\_%' ESCAPE '\\'",
        )
        .fetch_all(&h.pool)
        .await
        .unwrap();
        assert!(tables.len() >= 9, "{tables:?}");
        for table in [
            "devtime_findings",
            "devtime_attempt_marks",
            "devtime_turn_stats",
            "devtime_feedback",
        ] {
            assert!(
                tables.iter().any(|t| t == table),
                "{table} is part of the scan: {tables:?}"
            );
        }
        for table in tables {
            let columns: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT name FROM pragma_table_info('{table}') WHERE upper(type) = 'TEXT'"
            )))
            .fetch_all(&h.pool)
            .await
            .unwrap();
            for column in columns {
                let hits: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT COUNT(*) FROM {table} WHERE instr(\"{column}\", '{SENTINEL}') > 0"
                )))
                .fetch_one(&h.pool)
                .await
                .unwrap();
                assert_eq!(hits, 0, "{table}.{column} holds message text");
            }
        }
    }

    // ---- rules end to end (sub-project 2) -------------------------------------------------------
    //
    // The transcripts below are built in code and carry structure only: placeholder words, no
    // project text. One session is a failing check, an edit, the same check passing (A1), then a
    // human prompt that opens with a correction opener (C2 on the turn before it).

    fn tool_id(name: &str, sid: &str) -> String {
        format!("toolu_{name}_{sid}")
    }

    /// The records of one synthetic session, in file order. `hour` keeps two sessions apart in time.
    fn rules_lines(sid: &str, hour: u32) -> Vec<serde_json::Value> {
        let at = |second: u32| format!("2026-10-05T{hour:02}:00:{second:02}.000Z");
        let assistant = |second: u32, mid: &str, content: serde_json::Value| {
            serde_json::json!({"type": "assistant", "timestamp": at(second), "sessionId": sid,
                "cwd": ROOT, "entrypoint": "cli",
                "message": {"id": format!("{mid}-{sid}"), "model": "claude-x",
                    "usage": {"input_tokens": 10, "output_tokens": 5},
                    "content": content}})
        };
        let result = |second: u32, name: &str, is_error: bool, text: &str| {
            serde_json::json!({"type": "user", "timestamp": at(second), "sessionId": sid,
                "cwd": ROOT,
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": tool_id(name, sid),
                     "is_error": is_error, "content": text}]}})
        };
        let prompt = |second: u32, text: &str| {
            serde_json::json!({"type": "user", "timestamp": at(second), "sessionId": sid,
                "cwd": ROOT, "entrypoint": "cli", "origin": {"kind": "human"},
                "message": {"role": "user", "content": text}})
        };
        let test_call = |name: &str| {
            serde_json::json!([{"type": "tool_use", "id": tool_id(name, sid), "name": "Bash",
                "input": {"command": "cargo test -p fixture"}}])
        };
        vec![
            prompt(0, "make the failing check pass"),
            assistant(1, "m1", test_call("t1")),
            result(10, "t1", true, "Exit code 101"),
            assistant(
                11,
                "m2",
                serde_json::json!([{"type": "tool_use", "id": tool_id("edit", sid), "name": "Edit",
                    "input": {"file_path": format!("{ROOT}/src/a.rs"),
                              "old_string": "alpha", "new_string": "beta"}}]),
            ),
            result(12, "edit", false, "edited"),
            assistant(13, "m3", test_call("t3")),
            result(20, "t3", false, "ok"),
            assistant(
                21,
                "m4",
                serde_json::json!([{"type": "text", "text": "done"}]),
            ),
            prompt(30, "That is not what I asked"),
            assistant(
                32,
                "m5",
                serde_json::json!([{"type": "text", "text": "redone"}]),
            ),
        ]
    }

    /// Writes one synthetic session into the harness's project directory and returns its file.
    fn write_rules_session(h: &Harness, sid: &str, hour: u32) -> PathBuf {
        let dir = h.projects.join("c--fixture-proj");
        std::fs::create_dir_all(&dir).unwrap();
        let body: String = rules_lines(sid, hour)
            .iter()
            .map(|line| format!("{line}\n"))
            .collect();
        let file = dir.join(format!("{sid}.jsonl"));
        std::fs::write(&file, body).unwrap();
        file
    }

    /// The confidence of a session's finding for a rule, `None` when the rule did not fire.
    async fn finding_confidence(pool: &SqlitePool, sid: &str, rule: &str) -> Option<String> {
        sqlx::query_scalar(
            "SELECT confidence FROM devtime_findings WHERE session_id = ? AND rule_id = ?",
        )
        .bind(sid)
        .bind(rule)
        .fetch_optional(pool)
        .await
        .unwrap()
    }

    /// `(rules_version, rules_at, dirty)` of a session row.
    async fn rules_state(pool: &SqlitePool, sid: &str) -> (Option<String>, Option<String>, i64) {
        sqlx::query_as(
            "SELECT rules_version, rules_at, dirty FROM devtime_sessions WHERE session_id = ?",
        )
        .bind(sid)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn cycle_runs_rules_and_annotates_spans() {
        let h = harness(&[]).await;
        let sid = "s-rules-a";
        write_rules_session(&h, sid, 10);
        let stats = ingest_cycle(&h.pool, &cycle_config(), &h.projects).await;
        assert_eq!(stats.lines_failed, 0);
        assert_eq!(stats.files_failed, 0);

        let flags: Vec<Option<i64>> = sqlx::query_scalar(
            "SELECT opens_with_correction FROM devtime_turns WHERE session_id = ? ORDER BY seq",
        )
        .bind(sid)
        .fetch_all(&h.pool)
        .await
        .unwrap();
        assert_eq!(
            flags,
            vec![Some(0), Some(1)],
            "only the second prompt corrects"
        );

        assert_eq!(
            finding_confidence(&h.pool, sid, "A1").await.as_deref(),
            Some("exact"),
            "fail, edit, same check passes"
        );
        assert_eq!(
            finding_confidence(&h.pool, sid, "C2").await.as_deref(),
            Some("inferred"),
            "the turn before the correction"
        );

        // Every work span is annotated: the rules left nothing NULL where a verdict is owed.
        let unannotated: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devtime_spans WHERE session_id = ?
               AND kind IN ('model', 'tool', 'subagent', 'wait_background', 'wait_machine')
               AND waste IS NULL",
        )
        .bind(sid)
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(unannotated, 0, "every work span carries a waste class");

        // The edit that ended the failing loop is claimed by A1 (registry order beats C2).
        let edit_span: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT waste, rule_id FROM devtime_spans
             WHERE session_id = ? AND attempt_id =
                (SELECT attempt_id FROM devtime_attempts WHERE tool_use_id = ?)",
        )
        .bind(sid)
        .bind(tool_id("edit", sid))
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(edit_span.0.as_deref(), Some("rework"));
        assert_eq!(edit_span.1.as_deref(), Some("A1"));

        // The answer to the correction is nobody's finding: its span is useful.
        let last_model: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT waste, rule_id FROM devtime_spans
             WHERE session_id = ? AND lane = 'main' AND kind = 'model'
             ORDER BY started_at DESC LIMIT 1",
        )
        .bind(sid)
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(last_model.0.as_deref(), Some("useful"));
        assert_eq!(last_model.1, None, "a useful span names no rule");

        let (version, at, dirty) = rules_state(&h.pool, sid).await;
        assert!(
            version.as_deref().is_some_and(|v| !v.is_empty()),
            "{version:?}"
        );
        assert!(at.is_some(), "the pass is stamped");
        assert_eq!(dirty, 0, "the pass clears the pending flag");
    }

    #[tokio::test]
    async fn cycle_with_rules_is_idempotent() {
        let h = harness(&["background", "parallel", "inline_sidechain", "compaction"]).await;
        write_rules_session(&h, "s-rules-a", 10);
        write_rules_session(&h, "s-rules-b", 11);
        let cfg = cycle_config();
        ingest_cycle(&h.pool, &cfg, &h.projects).await;
        let first = dump(&h.pool).await;
        for table in [
            "devtime_findings",
            "devtime_attempt_marks",
            "devtime_turn_stats",
        ] {
            assert!(
                first
                    .iter()
                    .any(|line| line.starts_with(&format!("{table}: "))),
                "the first cycle wrote {table}"
            );
        }
        assert!(count(&h.pool, "devtime_findings").await > 0);

        let second = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        assert_eq!(second.lines_read, 0, "nothing new to read");
        assert_eq!(dump(&h.pool).await, first, "a second cycle changes nothing");

        let third = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        assert_eq!(third.lines_read, 0);
        assert_eq!(dump(&h.pool).await, first, "and neither does a third");
    }

    #[tokio::test]
    async fn appended_bytes_recompute_only_that_session() {
        let h = harness(&[]).await;
        let (a, b) = ("s-rules-a", "s-rules-b");
        let file_a = write_rules_session(&h, a, 10);
        write_rules_session(&h, b, 11);
        let cfg = cycle_config();
        ingest_cycle(&h.pool, &cfg, &h.projects).await;

        let (version_a, at_a, dirty_a) = rules_state(&h.pool, a).await;
        let (version_b, at_b, dirty_b) = rules_state(&h.pool, b).await;
        assert!(at_a.is_some() && at_b.is_some(), "both sessions were ruled");
        assert!(
            version_a.is_some() && version_a == version_b,
            "one fingerprint, one config"
        );
        assert_eq!((dirty_a, dirty_b), (0, 0));

        // A marker no pass would write: whichever session still holds it afterwards was not recomputed.
        sqlx::query("UPDATE devtime_sessions SET rules_at = 'sentinel'")
            .execute(&h.pool)
            .await
            .unwrap();

        let appended = serde_json::json!({"type": "user", "timestamp": "2026-10-05T10:00:40.000Z",
            "sessionId": a, "cwd": ROOT, "origin": {"kind": "human"},
            "message": {"role": "user", "content": "all good, thanks"}});
        let mut line = appended.to_string();
        line.push('\n');
        std::fs::OpenOptions::new()
            .append(true)
            .open(&file_a)
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();

        let second = ingest_cycle(&h.pool, &cfg, &h.projects).await;
        assert_eq!(second.lines_read, 1, "only the appended line is read");

        let (version_a2, at_a2, dirty_a2) = rules_state(&h.pool, a).await;
        let (version_b2, at_b2, dirty_b2) = rules_state(&h.pool, b).await;
        assert_ne!(
            at_a2.as_deref(),
            Some("sentinel"),
            "the grown session was recomputed"
        );
        assert_eq!(
            at_b2.as_deref(),
            Some("sentinel"),
            "the untouched session was not"
        );
        assert_eq!(version_a2, version_a);
        assert_eq!(version_b2, version_b);
        assert_eq!((dirty_a2, dirty_b2), (0, 0));

        let turns_a: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM devtime_turns WHERE session_id = ?")
                .bind(a)
                .fetch_one(&h.pool)
                .await
                .unwrap();
        assert_eq!(turns_a, 3, "the appended prompt opened a third turn");
    }

    #[tokio::test]
    async fn rules_disabled_leaves_spans_unannotated() {
        let h = harness(&[]).await;
        let sid = "s-rules-a";
        write_rules_session(&h, sid, 10);
        let mut cfg = cycle_config();
        cfg.rules.enabled = false;
        ingest_cycle(&h.pool, &cfg, &h.projects).await;

        assert!(
            count(&h.pool, "devtime_spans").await > 0,
            "the lanes are still built"
        );
        for table in [
            "devtime_findings",
            "devtime_attempt_marks",
            "devtime_turn_stats",
        ] {
            assert_eq!(count(&h.pool, table).await, 0, "{table}");
        }
        let annotated: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devtime_spans
             WHERE waste IS NOT NULL OR rule_id IS NOT NULL OR lever IS NOT NULL
                OR finding_key IS NOT NULL",
        )
        .fetch_one(&h.pool)
        .await
        .unwrap();
        assert_eq!(annotated, 0, "no span carries an annotation");
        let (version, at, dirty) = rules_state(&h.pool, sid).await;
        assert_eq!((version, at), (None, None), "the session was never ruled");
        assert_eq!(dirty, 1, "and stays pending");

        // Switching the rules on later finds the pending session on its own, with no new bytes.
        let on = ingest_cycle(&h.pool, &cycle_config(), &h.projects).await;
        assert_eq!(on.lines_read, 0);
        assert!(finding_confidence(&h.pool, sid, "A1").await.is_some());
        let (version, _, dirty) = rules_state(&h.pool, sid).await;
        assert!(version.is_some());
        assert_eq!(dirty, 0);
    }
}
