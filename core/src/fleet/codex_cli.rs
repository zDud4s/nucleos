//! `codex_cli`: an `AgentSource` over the Codex CLI rollouts in
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (spec 2026-10-20 §4, item 3).
//!
//! A rollout is an append-only jsonl of `{timestamp, type, payload}` records. This adapter reads it
//! three ways: the first lines (`read_head`: `session_meta`, first prompt), the last ~64 KB
//! (`scan_tail`: is a turn open, latest usage, latest model) and, for lines and the transcript, any
//! range through `map_record`. The format is a third party's and undocumented, so every field is
//! optional and a line that does not parse is skipped.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use serde_json::Value;

use super::limits;
use super::model::{
    Cursor, NodeKey, NodeState, TermKind, TermLine, Timestamp, TranscriptEntry, TranscriptPage,
    clean_label, collapse_ws,
};
use super::source::{
    AgentSource, LinesRead, NodeRef, Offset, Sighting, SourceError, SourceResult, Usage,
};
use super::tail;

/// The longest `detail` kept for one transcript entry (a tool input or output), in chars.
const DETAIL_CHARS: usize = 32_000;

/// One thing a rollout record says, before it is cut to a 200-char terminal line or kept whole for
/// the transcript. `text` is the summary (may hold newlines); `detail` the full tool input/output.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Mapped {
    kind: TermKind,
    text: String,
    detail: Option<String>,
}

impl Mapped {
    fn new(kind: TermKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            detail: None,
        }
    }

    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail: String = detail.into();
        if !detail.is_empty() {
            self.detail = Some(cap_chars(&detail, DETAIL_CHARS));
        }
        self
    }

    /// The terminal line: `TermLine::new` collapses whitespace, redacts secrets and cuts to `limits::LINE_CHARS` (amendment 18).
    fn into_line(self) -> TermLine {
        TermLine::new(self.kind, self.text)
    }

    /// The transcript entry: the summary uncut (whitespace collapsed by `model::collapse_ws`, which
    /// `TermLine::new` also uses), the detail whole.
    fn into_entry(self) -> TranscriptEntry {
        TranscriptEntry {
            kind: self.kind,
            text: collapse_ws(&self.text),
            detail: self.detail,
        }
    }
}

/// At most `max` chars, with a trailing `…` when something was cut.
fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// The `text` of every element of a content array, joined by newlines. Covers `output_text`
/// (assistant), `text` (UserMessage item) and `input_text` (tool output lists).
fn content_text(content: Option<&Value>) -> String {
    content
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| str_of(item, "text"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// What one rollout record contributes to the terminal and the transcript. Reasoning, token
/// counts, turn context, world state and every record kind this adapter does not know map to
/// nothing: an unknown record must never break the stream.
fn map_record(record: &Value) -> Vec<Mapped> {
    let payload = record.get("payload").unwrap_or(&Value::Null);
    let ptype = str_of(payload, "type").unwrap_or("");
    match (str_of(record, "type"), ptype) {
        (Some("response_item"), "message") => assistant_message(payload),
        (Some("event_msg"), "item_completed") => user_item(payload),
        (Some("event_msg"), "user_message") => prompt(str_of(payload, "message").unwrap_or("")),
        (Some("event_msg"), "task_complete") => task_complete(payload),
        (Some("event_msg"), "turn_aborted") => vec![Mapped::new(TermKind::Error, "turn aborted")],
        (Some("response_item"), "function_call") => function_call(payload),
        (Some("response_item"), "custom_tool_call") => custom_call(payload),
        (Some("response_item"), "function_call_output" | "custom_tool_call_output") => {
            output(payload)
        }
        _ => Vec::new(),
    }
}

fn assistant_message(payload: &Value) -> Vec<Mapped> {
    if str_of(payload, "role") != Some("assistant") {
        return Vec::new();
    }
    let text = content_text(payload.get("content"));
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![Mapped::new(TermKind::Text, text)]
    }
}

fn user_item(payload: &Value) -> Vec<Mapped> {
    let Some(item) = payload.get("item") else {
        return Vec::new();
    };
    if str_of(item, "type") != Some("UserMessage") {
        return Vec::new();
    }
    prompt(&content_text(item.get("content")))
}

fn prompt(text: &str) -> Vec<Mapped> {
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![Mapped::new(TermKind::Prompt, text)]
    }
}

/// Tool names whose calls say nothing a reader needs (`wait` polls a running script cell).
const QUIET_TOOLS: [&str; 1] = ["wait"];

/// The first non-blank line of `text`, trimmed.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// The argument most likely to say what a tool call does, from a parsed `arguments` object.
fn argument_preview(arguments: &Value) -> Option<String> {
    [
        "command",
        "cmd",
        "path",
        "file_path",
        "query",
        "patch",
        "explanation",
    ]
    .iter()
    .find_map(|key| str_of(arguments, key))
    .map(first_line)
}

fn tool_line(name: &str, preview: &str) -> String {
    format!("{name} {preview}").trim_end().to_owned()
}

fn function_call(payload: &Value) -> Vec<Mapped> {
    let name = str_of(payload, "name").unwrap_or("tool");
    if QUIET_TOOLS.contains(&name) {
        return Vec::new();
    }
    let arguments = str_of(payload, "arguments").unwrap_or("");
    let preview = serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|parsed| argument_preview(&parsed))
        .unwrap_or_else(|| first_line(arguments));
    vec![Mapped::new(TermKind::Tool, tool_line(name, &preview)).with_detail(arguments)]
}

fn custom_call(payload: &Value) -> Vec<Mapped> {
    let name = str_of(payload, "name").unwrap_or("tool");
    let input = str_of(payload, "input").unwrap_or("");
    vec![Mapped::new(TermKind::Tool, tool_line(name, &first_line(input))).with_detail(input)]
}

/// A tool output is `output` as a string or as a list of `{type, text}` parts.
fn output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(text)) => text.clone(),
        other => content_text(other),
    }
}

/// `exit 0 · 12 lines` when the output carries an `Exit code:` header, else `12 lines`.
fn result_summary(text: &str) -> String {
    let exit = text
        .lines()
        .find_map(|line| line.strip_prefix("Exit code: "))
        .and_then(|code| code.trim().parse::<i32>().ok());
    let lines = text.lines().filter(|line| !line.trim().is_empty()).count();
    match exit {
        Some(code) => format!("exit {code} · {lines} lines"),
        None => format!("{lines} lines"),
    }
}

fn output(payload: &Value) -> Vec<Mapped> {
    let text = output_text(payload.get("output"));
    if text.starts_with("Script running") {
        return Vec::new();
    }
    vec![Mapped::new(TermKind::Result, result_summary(&text)).with_detail(text)]
}

fn task_complete(payload: &Value) -> Vec<Mapped> {
    match str_of(payload, "last_agent_message").filter(|text| !text.trim().is_empty()) {
        Some(text) => vec![Mapped::new(TermKind::Final, text)],
        None => vec![Mapped::new(TermKind::Status, "turn complete")],
    }
}

/// The context size after the latest request: `last_token_usage.total_tokens` (input plus output),
/// falling back to `input_tokens`. Codex reports no price, so `cost_usd` stays `None`. A
/// `token_count` whose `info` is `null` (rate-limit-only update) carries no usage.
fn usage_of(record: &Value) -> Option<Usage> {
    if str_of(record, "type")? != "event_msg"
        || record.pointer("/payload/type")?.as_str()? != "token_count"
    {
        return None;
    }
    let last = record.pointer("/payload/info/last_token_usage")?;
    let tokens = last
        .get("total_tokens")
        .or_else(|| last.get("input_tokens"))?
        .as_i64()?;
    Some(Usage {
        context_tokens: Some(tokens),
        cost_usd: None,
    })
}

fn ts_of(record: &Value) -> Option<Timestamp> {
    parse_ts(str_of(record, "timestamp")?)
}

fn parse_ts(text: &str) -> Option<Timestamp> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// What a batch of raw rollout lines says.
struct Parsed {
    mapped: Vec<Mapped>,
    usage: Option<Usage>,
    last_at: Option<Timestamp>,
}

/// Maps complete lines in order. A line that is not json is skipped, never an error.
fn parse_lines(raw: &[String]) -> Parsed {
    let mut parsed = Parsed {
        mapped: Vec::new(),
        usage: None,
        last_at: None,
    };
    for line in raw {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(at) = ts_of(&record) {
            parsed.last_at = Some(at);
        }
        if let Some(usage) = usage_of(&record) {
            parsed.usage = Some(usage);
        }
        parsed.mapped.extend(map_record(&record));
    }
    parsed
}

/// What the last ~64 KB of a rollout says about the session.
#[derive(Clone, Debug, Default, PartialEq)]
struct TailFacts {
    /// `Some(true)` after `task_started`, `Some(false)` after `task_complete`/`turn_aborted`
    /// (the last marker wins), `None` when the window holds no marker. `None` is "unknown" and
    /// `liveness` reads it as open (Busy while fresh, Lost once quiet, never Done): a finished
    /// session ends with a closing marker inside the tail, so a window without one is mid-turn.
    turn_open: Option<bool>,
    usage: Option<Usage>,
    model: Option<String>,
}

fn scan_tail(raw: &[String]) -> TailFacts {
    let mut facts = TailFacts::default();
    for line in raw {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ptype = record.pointer("/payload/type").and_then(Value::as_str);
        match (str_of(&record, "type"), ptype) {
            (Some("event_msg"), Some("task_started")) => facts.turn_open = Some(true),
            (Some("event_msg"), Some("task_complete" | "turn_aborted")) => {
                facts.turn_open = Some(false)
            }
            (Some("turn_context"), _) => {
                if let Some(model) = record.pointer("/payload/model").and_then(Value::as_str) {
                    facts.model = Some(model.to_owned());
                }
            }
            _ => {}
        }
        if let Some(usage) = usage_of(&record) {
            facts.usage = Some(usage);
        }
    }
    facts
}

/// Spec §3 for a rollout nobody else vouches for (a live daemon run overrides this in fusion):
/// an open (or unknown) turn written to within `CODEX_LIVE_MTIME` is `Busy`; the same turn gone
/// quiet is `Lost` (mid-turn, no process); a closed turn is `Done` whatever its age. There is no
/// `Idle`: a Codex session between turns cannot be told from a dead one without a process table.
fn liveness(turn_open: Option<bool>, idle_for: Duration) -> NodeState {
    let open = turn_open.unwrap_or(true);
    match (open, idle_for <= limits::CODEX_LIVE_MTIME) {
        (true, true) => NodeState::Busy,
        (true, false) => NodeState::Lost,
        (false, _) => NodeState::Done,
    }
}

/// The most bytes and lines read from the start of a rollout looking for `session_meta` and the
/// first prompt. `session_meta` alone is up to ~25 KB (it embeds the base instructions).
const HEAD_MAX_BYTES: u64 = 512 * 1024;
const HEAD_MAX_LINES: usize = 80;

/// What the first lines of a rollout say. Every field is optional.
#[derive(Clone, Debug, Default, PartialEq)]
struct Head {
    id: Option<String>,
    cwd: Option<String>,
    originator: Option<String>,
    cli_version: Option<String>,
    started_at: Option<Timestamp>,
    /// `base_instructions.provenance.model`: only a fallback, the tail's `turn_context` wins.
    model: Option<String>,
    /// `source.subagent.thread_spawn.parent_thread_id`: a Codex subagent's native parent.
    parent: Option<String>,
    nickname: Option<String>,
    agent_role: Option<String>,
    first_prompt: Option<String>,
}

fn parse_head_record(head: &mut Head, record: &Value) {
    let payload = record.get("payload").unwrap_or(&Value::Null);
    if str_of(record, "type") == Some("session_meta") {
        let text = |key: &str| str_of(payload, key).map(str::to_owned);
        // `session_id` can name the root of a fork; `id` is this thread and the file name's uuid.
        head.id = text("id").or_else(|| text("session_id"));
        head.cwd = text("cwd");
        head.originator = text("originator");
        head.cli_version = text("cli_version");
        head.started_at = str_of(payload, "timestamp").and_then(parse_ts);
        head.model = payload
            .pointer("/base_instructions/provenance/model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(spawn) = payload.pointer("/source/subagent/thread_spawn") {
            head.parent = str_of(spawn, "parent_thread_id").map(str::to_owned);
            head.nickname = str_of(spawn, "agent_nickname").map(str::to_owned);
            head.agent_role = str_of(spawn, "agent_role").map(str::to_owned);
        }
    } else if head.first_prompt.is_none() {
        head.first_prompt = map_record(record)
            .into_iter()
            .find(|mapped| mapped.kind == TermKind::Prompt)
            .map(|mapped| mapped.text);
    }
}

/// Reads at most `HEAD_MAX_LINES` lines / `HEAD_MAX_BYTES` bytes from the start, stopping at the
/// first prompt. A line that does not parse (including one cut by the byte budget) is skipped.
fn read_head(path: &Path) -> std::io::Result<Head> {
    let mut reader = BufReader::new(std::fs::File::open(path)?.take(HEAD_MAX_BYTES));
    let mut head = Head::default();
    let mut buf = Vec::new();
    for _ in 0..HEAD_MAX_LINES {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        let Ok(record) = serde_json::from_slice::<Value>(&buf) else {
            continue;
        };
        parse_head_record(&mut head, &record);
        if head.first_prompt.is_some() {
            break;
        }
    }
    Ok(head)
}

/// The harness label and the `NodeKey.harness` of every node this adapter sights.
const HARNESS: &str = "codex-cli";
/// Chars of the nickname or first prompt kept as the node label (passed to `clean_label`).
const LABEL_CHARS: usize = 80;

/// `<root>/YYYY/MM/DD` for today and yesterday, in **local** time (Codex names the directory with
/// the local date) and in UTC (so a clock-zone edge never hides a live session), deduplicated.
/// Older directories are deliberately not scanned (spec §4 "Descoberta incremental"): a session
/// still writing two days after it started is missed.
fn candidate_dirs(root: &Path, now: Timestamp) -> Vec<PathBuf> {
    let local = now.with_timezone(&Local).date_naive();
    let utc = now.date_naive();
    let mut days: Vec<NaiveDate> = [local, utc]
        .into_iter()
        .flat_map(|day| [Some(day), day.pred_opt()])
        .flatten()
        .collect();
    days.sort();
    days.dedup();
    days.into_iter()
        .map(|day| {
            root.join(format!("{:04}", day.year()))
                .join(format!("{:02}", day.month()))
                .join(format!("{:02}", day.day()))
        })
        .collect()
}

/// The thread uuid at the end of `rollout-<date>T<time>-<uuid>.jsonl`.
fn id_from_file_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let tail = stem.get(stem.len().checked_sub(36)?..)?;
    let shaped = tail.chars().enumerate().all(|(i, c)| {
        if matches!(i, 8 | 13 | 18 | 23) {
            c == '-'
        } else {
            c.is_ascii_hexdigit()
        }
    });
    shaped.then(|| tail.to_owned())
}

/// A subagent's `agent_role`, else the originator without its `codex_`/`codex-` prefix.
fn role_of(agent_role: Option<&str>, originator: Option<&str>) -> Option<String> {
    agent_role.map(str::to_owned).or_else(|| {
        originator.map(|o| {
            o.trim_start_matches("codex_")
                .trim_start_matches("codex-")
                .to_owned()
        })
    })
}

/// One date directory: its mtime when last listed and the rollouts found then.
struct DirEntry {
    mtime: SystemTime,
    files: Vec<PathBuf>,
}

/// What is known about one rollout, valid while (len, mtime) are unchanged. State and liveness are
/// NOT cached: they depend on the clock and are recomputed on every pass from these facts.
struct FileEntry {
    len: u64,
    mtime: SystemTime,
    head: Head,
    facts: TailFacts,
}

/// The adapter's incremental memory (spec §4: "Um nó já conhecido não volta a ser lido do zero").
/// A directory is re-listed only when its mtime changed (new rollouts), but appends to an existing
/// rollout do not touch the directory's mtime, so every known file is `stat`ed on every pass and
/// re-read (tail only) when its (len, mtime) moved.
#[derive(Default)]
struct State {
    dirs: HashMap<PathBuf, DirEntry>,
    files: HashMap<PathBuf, FileEntry>,
    keys: HashMap<NodeKey, PathBuf>,
    /// Directory listings and file refreshes done so far (the tests read these).
    listings: usize,
    refreshes: usize,
}

fn list_rollouts(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "jsonl")
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("rollout-"))
        })
        .collect();
    files.sort();
    files
}

/// Re-reads the parts of one rollout that can have changed. The head is kept while it already has
/// an identity and a first prompt; a half-written file is re-read until it has both.
fn refresh(state: &mut State, path: &Path, len: u64, mtime: SystemTime) {
    state.refreshes += 1;
    let mut head = state
        .files
        .remove(path)
        .map(|entry| entry.head)
        .unwrap_or_default();
    if head.id.is_none() || head.first_prompt.is_none() {
        match read_head(path) {
            Ok(read) => {
                // The CLI version is the only use of `Head::cli_version`: a breadcrumb for the day a
                // format change breaks a mapping (the rollout format is a third party's).
                if head.cli_version.is_none() && read.cli_version.is_some() {
                    tracing::debug!(version = ?read.cli_version, path = %path.display(), "codex_cli: rollout version");
                }
                head = read;
            }
            Err(error) => {
                tracing::debug!(%error, path = %path.display(), "codex_cli: head not readable")
            }
        }
    }
    let facts = match tail::read_complete_lines(path, Offset::TAIL) {
        Ok((lines, _)) => scan_tail(&lines),
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "codex_cli: tail not readable");
            TailFacts::default()
        }
    };
    state.files.insert(
        path.to_owned(),
        FileEntry {
            len,
            mtime,
            head,
            facts,
        },
    );
}

fn sighting(path: &Path, entry: &FileEntry, now: Timestamp) -> Option<Sighting> {
    let head = &entry.head;
    let id = head.id.clone().or_else(|| id_from_file_name(path))?;
    let last_activity_at: Timestamp = entry.mtime.into();
    let idle_for = (now - last_activity_at).to_std().unwrap_or_default();
    let state = liveness(entry.facts.turn_open, idle_for);
    // Amendment 18: every label goes through `clean_label` (collapse, redact, cut), nickname included.
    let label = clean_label(
        &head
            .nickname
            .clone()
            .or_else(|| head.first_prompt.clone())
            .unwrap_or_else(|| format!("codex {}", id.chars().take(8).collect::<String>())),
        LABEL_CHARS,
    );
    Some(Sighting {
        key: NodeKey::new(HARNESS, &id),
        parent: head
            .parent
            .as_deref()
            .map(|parent| NodeKey::new(HARNESS, parent)),
        harness_label: HARNESS.to_owned(),
        model: entry.facts.model.clone().or_else(|| head.model.clone()),
        role: role_of(head.agent_role.as_deref(), head.originator.as_deref()),
        label,
        project: head.cwd.as_deref().and_then(tail::project_of),
        state,
        started_at: head.started_at.unwrap_or(last_activity_at),
        // Discovered already dead: the end is its last activity (spec §3).
        ended_at: (state != NodeState::Busy).then_some(last_activity_at),
        last_activity_at,
        run_id: None,
        can_stop: false,
        has_lines: true,
        has_transcript: true,
        usage: entry.facts.usage.clone(),
    })
}

/// One discovery pass. Synchronous; the async trait method runs it in `spawn_blocking`.
fn scan(state: &Mutex<State>, root: &Path, since: Timestamp, now: Timestamp) -> Vec<Sighting> {
    // Contract amendment 14: `since` only bounds dead nodes. A rollout written to within
    // `CODEX_LIVE_MTIME` can still be `Busy`, so it is never skipped on `since` alone; the dead ones
    // that slip through the cheap mtime test below are dropped once their state is known.
    let since_sys: SystemTime = since.into();
    let live_after: SystemTime = SystemTime::from(now) - limits::CODEX_LIVE_MTIME;
    let keep_after = since_sys.min(live_after);
    let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
    let st: &mut State = &mut guard;
    let mut found = Vec::new();
    for dir in candidate_dirs(root, now) {
        let Ok(meta) = std::fs::metadata(&dir) else {
            st.dirs.remove(&dir);
            continue;
        };
        let dir_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if st.dirs.get(&dir).map(|known| known.mtime) != Some(dir_mtime) {
            st.listings += 1;
            st.dirs.insert(
                dir.clone(),
                DirEntry {
                    mtime: dir_mtime,
                    files: list_rollouts(&dir),
                },
            );
        }
        let files = st
            .dirs
            .get(&dir)
            .map(|known| known.files.clone())
            .unwrap_or_default();
        for path in files {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if mtime < keep_after {
                continue;
            }
            let fresh = st
                .files
                .get(&path)
                .is_some_and(|f| f.len == meta.len() && f.mtime == mtime);
            if !fresh {
                refresh(st, &path, meta.len(), mtime);
            }
            if let Some(found_one) = st
                .files
                .get(&path)
                .and_then(|entry| sighting(&path, entry, now))
            {
                let alive = matches!(found_one.state, NodeState::Busy | NodeState::Idle);
                if !alive && found_one.last_activity_at < since {
                    continue;
                }
                st.keys.insert(found_one.key.clone(), path.clone());
                found.push(found_one);
            }
        }
    }
    found
}

/// The Codex CLI adapter. Cheap to build; all state is behind a mutex because `AgentSource` takes
/// `&self` and the scan runs on the blocking pool.
pub struct CodexCli {
    root: PathBuf,
    // A plain `std` mutex: `scan` holds it for a whole pass (stats plus tail reads of the files that
    // changed), so `node_ref` called from async code can wait out one pass; that is short and
    // bounded, which is why it is not an async lock or a per-file lock.
    state: Arc<Mutex<State>>,
}

impl CodexCli {
    /// `home` is the user home; the rollouts live in `<home>/.codex/sessions`.
    pub fn new(home: PathBuf) -> Self {
        Self {
            root: home.join(".codex").join("sessions"),
            state: Arc::default(),
        }
    }
}

fn read_lines_sync(path: &Path, from: Offset) -> SourceResult<LinesRead> {
    let (raw, offset) = tail::read_complete_lines(path, from)?;
    let parsed = parse_lines(&raw);
    Ok(LinesRead {
        lines: parsed.mapped.into_iter().map(Mapped::into_line).collect(),
        offset,
        usage: parsed.usage,
        last_activity_at: parsed.last_at,
    })
}

/// The cursor is the byte offset of the oldest line already shown; anything else means "from the end".
fn transcript_sync(path: &Path, before: Option<Cursor>) -> SourceResult<TranscriptPage> {
    let before = before.and_then(|cursor| cursor.parse::<u64>().ok());
    let (raw, next) = tail::read_lines_before(path, before, limits::TRANSCRIPT_PAGE_BYTES)?;
    let entries = parse_lines(&raw)
        .mapped
        .into_iter()
        .map(Mapped::into_entry)
        .collect();
    Ok(TranscriptPage {
        entries,
        before: next.map(|offset| offset.to_string()),
    })
}

fn path_of(node: &NodeRef) -> SourceResult<PathBuf> {
    node.path.clone().ok_or_else(|| {
        SourceError::msg(format!(
            "codex_cli node {} has no rollout path",
            node.key.id()
        ))
    })
}

#[async_trait]
impl AgentSource for CodexCli {
    fn name(&self) -> &'static str {
        "codex_cli"
    }

    /// The date directories being scanned plus their month directories (a new day appears there).
    fn watch_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = candidate_dirs(&self.root, Utc::now())
            .into_iter()
            .flat_map(|day| [Some(day.clone()), day.parent().map(Path::to_path_buf)])
            .flatten()
            .filter(|path| path.is_dir())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    async fn discover(&self, since: Timestamp) -> SourceResult<Vec<Sighting>> {
        let state = Arc::clone(&self.state);
        let root = self.root.clone();
        Ok(tokio::task::spawn_blocking(move || scan(&state, &root, since, Utc::now())).await?)
    }

    fn node_ref(&self, key: &NodeKey) -> Option<NodeRef> {
        if key.harness != HARNESS {
            return None;
        }
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.keys.get(key).map(|path| NodeRef {
            key: key.clone(),
            path: Some(path.clone()),
        })
    }

    async fn read_lines(&self, node: &NodeRef, from: Offset) -> SourceResult<LinesRead> {
        let path = path_of(node)?;
        tokio::task::spawn_blocking(move || read_lines_sync(&path, from)).await?
    }

    async fn transcript_page(
        &self,
        node: &NodeRef,
        before: Option<Cursor>,
    ) -> SourceResult<TranscriptPage> {
        let path = path_of(node)?;
        tokio::task::spawn_blocking(move || transcript_sync(&path, before)).await?
    }
}

/// Test support for the edge sources (`work_nodes_edges`, `run_launch`): a one-line rollout for `id`
/// under `<home>/.codex/sessions/<local today>/`, exactly where `candidate_dirs` looks.
#[cfg(test)]
pub(super) fn write_minimal_rollout(home: &Path, id: &str) {
    let day = Local::now();
    let dir = home
        .join(".codex")
        .join("sessions")
        .join(day.format("%Y/%m/%d").to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let meta = serde_json::json!({
        "timestamp": Utc::now().to_rfc3339(), "type": "session_meta",
        "payload": {"id": id, "cwd": "/w/x", "originator": "codex_exec"}
    });
    let name = format!("rollout-{}T10-00-00-{id}.jsonl", day.format("%Y-%m-%d"));
    std::fs::write(dir.join(name), format!("{meta}\n")).unwrap();
}

#[cfg(test)]
mod tests {
    // `super::*` brings in every name above (private `use` items included), so the tests import only
    // what the module itself does not: the `json!` macro.
    use super::*;
    use serde_json::json;

    /// One rollout line as the CLI writes it.
    fn rec(kind: &str, payload: Value) -> String {
        json!({"timestamp": "2026-10-20T10:00:00.000Z", "type": kind, "payload": payload})
            .to_string()
    }

    fn mapped(line: &str) -> Vec<Mapped> {
        map_record(&serde_json::from_str(line).expect("fixture is valid json"))
    }

    #[test]
    fn an_assistant_message_is_a_text_line() {
        let line = rec(
            "response_item",
            json!({"type": "message", "role": "assistant",
                   "content": [{"type": "output_text", "text": "Done:\n  two files changed"}]}),
        );
        let got = mapped(&line);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, TermKind::Text);
        assert_eq!(got[0].clone().into_line().text, "Done: two files changed");
    }

    #[test]
    fn injected_context_and_reasoning_never_become_lines() {
        for payload in [
            json!({"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "<permissions>"}]}),
            json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>"}]}),
            json!({"type": "reasoning", "summary": [], "encrypted_content": "gAAAA"}),
        ] {
            assert!(mapped(&rec("response_item", payload)).is_empty());
        }
        // Other record kinds are not lines either.
        assert!(mapped(&rec("turn_context", json!({"model": "gpt-5"}))).is_empty());
        assert!(mapped(&rec("world_state", json!({"full": true}))).is_empty());
    }

    #[test]
    fn a_user_message_is_a_prompt_line_in_both_known_encodings() {
        let item = rec(
            "event_msg",
            json!({"type": "item_completed",
                   "item": {"type": "UserMessage", "content": [{"type": "text", "text": "fix the build"}]}}),
        );
        let event = rec(
            "event_msg",
            json!({"type": "user_message", "message": "fix the build"}),
        );
        for line in [item, event] {
            let got = mapped(&line);
            assert_eq!(got.len(), 1, "{line}");
            assert_eq!(got[0].kind, TermKind::Prompt);
            assert_eq!(got[0].text, "fix the build");
        }
        // The other item kinds of item_completed duplicate records we map elsewhere.
        let agent = rec(
            "event_msg",
            json!({"type": "item_completed", "item": {"type": "AgentMessage", "content": [{"type": "text", "text": "x"}]}}),
        );
        assert!(mapped(&agent).is_empty());
        // event_msg/agent_message duplicates response_item/message (0.160).
        assert!(
            mapped(&rec(
                "event_msg",
                json!({"type": "agent_message", "message": "x"})
            ))
            .is_empty()
        );
    }

    #[test]
    fn a_function_call_is_a_tool_line_with_the_command_as_preview() {
        let line = rec(
            "response_item",
            json!({"type": "function_call", "name": "shell_command", "call_id": "c1",
                   "arguments": "{\"command\":\"Get-ChildItem -Recurse\\nSelect-Object -First 3\",\"workdir\":\"C:\\\\x\"}"}),
        );
        let got = mapped(&line);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, TermKind::Tool);
        assert_eq!(got[0].text, "shell_command Get-ChildItem -Recurse");
        assert!(got[0].detail.as_deref().unwrap().contains("Select-Object"));
    }

    #[test]
    fn a_function_call_with_unparseable_arguments_still_shows_its_name() {
        let line = rec(
            "response_item",
            json!({"type": "function_call", "name": "update_plan", "arguments": "{not json"}),
        );
        let got = mapped(&line);
        assert_eq!(got[0].text, "update_plan {not json");
    }

    #[test]
    fn a_custom_tool_call_shows_the_first_line_of_its_input() {
        let line = rec(
            "response_item",
            json!({"type": "custom_tool_call", "name": "exec", "call_id": "c2",
                   "input": "\nconst r = await tools.exec_command({cmd: \"ls\"});\ntext(r);"}),
        );
        let got = mapped(&line);
        assert_eq!(got[0].kind, TermKind::Tool);
        assert_eq!(
            got[0].text,
            "exec const r = await tools.exec_command({cmd: \"ls\"});"
        );
        assert!(got[0].detail.as_deref().unwrap().ends_with("text(r);"));
    }

    #[test]
    fn the_wait_poll_and_its_yield_stub_are_noise() {
        let call = rec(
            "response_item",
            json!({"type": "function_call", "name": "wait", "arguments": "{\"cell_id\":\"10\"}"}),
        );
        assert!(mapped(&call).is_empty());
        let stub = rec(
            "response_item",
            json!({"type": "function_call_output", "call_id": "c",
                   "output": "Script running with cell ID 10\nWall time 0.0 seconds"}),
        );
        assert!(mapped(&stub).is_empty());
    }

    #[test]
    fn a_tool_output_is_one_summary_line_in_either_encoding() {
        let as_string = rec(
            "response_item",
            json!({"type": "function_call_output", "call_id": "c",
                   "output": "Exit code: 0\nWall time: 1.0 seconds\nOutput:\na\nb\n"}),
        );
        let as_list = rec(
            "response_item",
            json!({"type": "custom_tool_call_output", "call_id": "c",
                   "output": [{"type": "input_text", "text": "Script completed"},
                              {"type": "input_text", "text": "x\ny\nz"}]}),
        );
        let got = mapped(&as_string);
        assert_eq!(got[0].kind, TermKind::Result);
        assert_eq!(got[0].text, "exit 0 · 5 lines");
        assert!(got[0].detail.as_deref().unwrap().contains("Wall time"));
        let got = mapped(&as_list);
        assert_eq!(got[0].text, "4 lines");
        // A tool output with neither shape degrades to an empty summary rather than failing.
        let odd = rec(
            "response_item",
            json!({"type": "function_call_output", "output": {"a": 1}}),
        );
        assert_eq!(mapped(&odd)[0].text, "0 lines");
    }

    #[test]
    fn a_huge_tool_output_keeps_a_capped_detail() {
        let big = "x".repeat(DETAIL_CHARS * 2);
        let line = rec(
            "response_item",
            json!({"type": "function_call_output", "output": big}),
        );
        let detail = mapped(&line)[0].detail.clone().unwrap();
        assert_eq!(detail.chars().count(), DETAIL_CHARS + 1);
        assert!(detail.ends_with('…'));
    }

    #[test]
    fn task_complete_is_the_final_line_or_a_status() {
        let with = rec(
            "event_msg",
            json!({"type": "task_complete", "last_agent_message": "All green."}),
        );
        let got = mapped(&with);
        assert_eq!(
            (got[0].kind, got[0].text.as_str()),
            (TermKind::Final, "All green.")
        );
        let without = rec(
            "event_msg",
            json!({"type": "task_complete", "last_agent_message": null}),
        );
        assert_eq!(mapped(&without)[0].kind, TermKind::Status);
        let aborted = rec("event_msg", json!({"type": "turn_aborted"}));
        assert_eq!(mapped(&aborted)[0].kind, TermKind::Error);
    }

    #[test]
    fn usage_comes_from_the_last_token_usage_and_tolerates_a_null_info() {
        let line = |info: Value| rec("event_msg", json!({"type": "token_count", "info": info}));
        let got = usage_of(&serde_json::from_str(&line(json!({"last_token_usage": {"input_tokens": 100, "output_tokens": 20, "total_tokens": 120}}))).unwrap());
        assert_eq!(
            got,
            Some(Usage {
                context_tokens: Some(120),
                cost_usd: None
            })
        );
        let fallback = usage_of(
            &serde_json::from_str(&line(json!({"last_token_usage": {"input_tokens": 100}})))
                .unwrap(),
        );
        assert_eq!(fallback.unwrap().context_tokens, Some(100));
        assert_eq!(
            usage_of(&serde_json::from_str(&line(Value::Null)).unwrap()),
            None
        );
        let other = rec("event_msg", json!({"type": "task_started"}));
        assert_eq!(usage_of(&serde_json::from_str(&other).unwrap()), None);
    }

    #[test]
    fn parse_lines_skips_garbage_and_keeps_the_latest_usage_and_timestamp() {
        let raw = [
            "{broken".to_owned(),
            String::new(),
            rec("event_msg", json!({"type": "token_count", "info": {"last_token_usage": {"total_tokens": 10}}})),
            rec("response_item", json!({"type": "message", "role": "assistant", "content": [{"text": "hi"}]})),
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":30}}},"timestamp":"2026-10-20T10:05:00Z"}"#.to_owned(),
        ];
        let parsed = parse_lines(&raw);
        assert_eq!(parsed.mapped.len(), 1);
        assert_eq!(parsed.usage.unwrap().context_tokens, Some(30));
        assert_eq!(
            parsed.last_at.unwrap().to_rfc3339(),
            "2026-10-20T10:05:00+00:00"
        );
    }

    fn lines(kinds: &[(&str, Value)]) -> Vec<String> {
        kinds
            .iter()
            .map(|(kind, payload)| rec(kind, payload.clone()))
            .collect()
    }

    #[test]
    fn the_last_turn_marker_decides_whether_a_turn_is_open() {
        let open = scan_tail(&lines(&[
            ("event_msg", json!({"type": "task_started"})),
            ("event_msg", json!({"type": "task_complete"})),
            ("event_msg", json!({"type": "task_started"})),
            (
                "response_item",
                json!({"type": "function_call", "name": "exec", "input": "x"}),
            ),
        ]));
        assert_eq!(open.turn_open, Some(true));
        let closed = scan_tail(&lines(&[
            ("event_msg", json!({"type": "task_started"})),
            ("event_msg", json!({"type": "task_complete"})),
            (
                "event_msg",
                json!({"type": "token_count", "info": {"last_token_usage": {"total_tokens": 7}}}),
            ),
        ]));
        assert_eq!(closed.turn_open, Some(false));
        assert_eq!(closed.usage.unwrap().context_tokens, Some(7));
        let aborted = scan_tail(&lines(&[("event_msg", json!({"type": "turn_aborted"}))]));
        assert_eq!(aborted.turn_open, Some(false));
        // No marker in the tail window at all: unknown, which `liveness` reads as open.
        assert_eq!(
            scan_tail(&lines(&[("response_item", json!({"type": "reasoning"}))])).turn_open,
            None
        );
    }

    /// Documents a deliberate limit: a tail window with no turn marker at all (a turn so long that its
    /// `task_started` scrolled out of the last 64 KB, or a rollout that never wrote one) is "unknown",
    /// and unknown is read as OPEN. So it is `Busy` while the file is fresh and `Lost` once it goes
    /// quiet, and never `Done`: a closed session always ends with `task_complete`/`turn_aborted`
    /// inside the tail, so "no marker" cannot be a finished turn.
    #[test]
    fn a_tail_without_any_turn_marker_is_busy_then_lost_never_done() {
        let facts = scan_tail(&lines(&[
            (
                "response_item",
                json!({"type": "function_call", "name": "exec", "input": "x"}),
            ),
            (
                "response_item",
                json!({"type": "function_call_output", "output": "ok"}),
            ),
        ]));
        assert_eq!(facts.turn_open, None);
        assert_eq!(
            liveness(facts.turn_open, Duration::from_secs(5)),
            NodeState::Busy
        );
        let stale = limits::CODEX_LIVE_MTIME + Duration::from_secs(1);
        assert_eq!(liveness(facts.turn_open, stale), NodeState::Lost);
    }

    #[test]
    fn the_latest_turn_context_names_the_model() {
        let facts = scan_tail(&lines(&[
            ("turn_context", json!({"model": "gpt-5.5"})),
            ("turn_context", json!({"model": "gpt-5.6-luna"})),
            ("turn_context", json!({"effort": "high"})),
        ]));
        assert_eq!(facts.model.as_deref(), Some("gpt-5.6-luna"));
    }

    #[test]
    fn liveness_follows_the_spec_table() {
        let fresh = Duration::from_secs(5);
        let stale = limits::CODEX_LIVE_MTIME + Duration::from_secs(1);
        assert_eq!(liveness(Some(true), fresh), NodeState::Busy);
        assert_eq!(liveness(None, fresh), NodeState::Busy);
        assert_eq!(liveness(Some(true), stale), NodeState::Lost);
        assert_eq!(liveness(Some(false), fresh), NodeState::Done);
        assert_eq!(liveness(Some(false), stale), NodeState::Done);
    }

    const ID_A: &str = "01a14120-24a4-7bc3-9946-a787ff71b89f";
    const ID_B: &str = "01a14121-0000-7000-8000-000000000001";
    const PARENT: &str = "019d4af2-8aa1-7bb3-a122-17d7b76f7766";

    fn meta_line(id: &str, cwd: &str, originator: &str) -> String {
        rec(
            "session_meta",
            json!({"session_id": id, "id": id, "timestamp": "2026-10-20T09:00:00.000Z", "cwd": cwd,
                   "originator": originator, "cli_version": "0.162.1", "source": "exec",
                   "base_instructions": {"text": "x".repeat(2000), "provenance": {"type": "model", "model": "gpt-from-provenance"}}}),
        )
    }

    fn prompt_line(text: &str) -> String {
        rec(
            "event_msg",
            json!({"type": "item_completed", "item": {"type": "UserMessage", "content": [{"type": "text", "text": text}]}}),
        )
    }

    fn write_file(path: &Path, lines: &[String]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn the_head_gives_identity_cwd_and_the_first_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        write_file(
            &path,
            &[
                meta_line(ID_A, "C:\\Projects\\nucleos", "codex_exec"),
                rec(
                    "response_item",
                    json!({"type": "message", "role": "developer", "content": []}),
                ),
                prompt_line("refactor the parser"),
                prompt_line("a second prompt"),
            ],
        );
        let head = read_head(&path).unwrap();
        assert_eq!(head.id.as_deref(), Some(ID_A));
        assert_eq!(head.cwd.as_deref(), Some("C:\\Projects\\nucleos"));
        assert_eq!(head.originator.as_deref(), Some("codex_exec"));
        assert_eq!(head.model.as_deref(), Some("gpt-from-provenance"));
        assert_eq!(head.first_prompt.as_deref(), Some("refactor the parser"));
        assert_eq!(
            head.started_at.unwrap().to_rfc3339(),
            "2026-10-20T09:00:00+00:00"
        );
        assert!(head.parent.is_none());
    }

    #[test]
    fn a_subagent_head_names_its_parent_thread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let meta = rec(
            "session_meta",
            json!({"id": ID_B, "cwd": "/w/x", "originator": "codex_vscode",
                   "source": {"subagent": {"thread_spawn": {"parent_thread_id": PARENT, "depth": 1,
                              "agent_nickname": "Copernicus", "agent_role": "worker"}}}}),
        );
        write_file(&path, &[meta]);
        let head = read_head(&path).unwrap();
        assert_eq!(head.parent.as_deref(), Some(PARENT));
        assert_eq!(head.nickname.as_deref(), Some("Copernicus"));
        assert_eq!(head.agent_role.as_deref(), Some("worker"));
    }

    #[test]
    fn a_head_with_a_malformed_first_line_degrades_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        write_file(&path, &["{not json".to_owned(), prompt_line("still found")]);
        let head = read_head(&path).unwrap();
        assert_eq!(head.id, None);
        assert_eq!(head.first_prompt.as_deref(), Some("still found"));
        // Not even a file: an io error the caller turns into a skipped node.
        assert!(read_head(&dir.path().join("missing.jsonl")).is_err());
    }

    #[test]
    fn candidate_dirs_cover_local_and_utc_today_and_yesterday_once() {
        let root = Path::new("/h/.codex/sessions");
        let now: Timestamp = "2026-10-20T23:30:00Z".parse().unwrap();
        let dirs = candidate_dirs(root, now);
        // UTC today/yesterday are always there; the local pair adds at most two more.
        assert!(dirs.contains(&root.join("2026").join("10").join("20")));
        assert!(dirs.contains(&root.join("2026").join("10").join("19")));
        let mut unique = dirs.clone();
        unique.dedup();
        assert_eq!(unique, dirs);
        assert!(dirs.len() >= 2 && dirs.len() <= 4);
        // Month boundary.
        let first: Timestamp = "2026-11-01T00:30:00Z".parse().unwrap();
        assert!(candidate_dirs(root, first).contains(&root.join("2026").join("10").join("31")));
    }

    #[test]
    fn the_id_falls_back_to_the_uuid_in_the_file_name() {
        let name = format!("rollout-2026-10-15T20-53-08-{ID_A}.jsonl");
        assert_eq!(id_from_file_name(Path::new(&name)).as_deref(), Some(ID_A));
        assert_eq!(
            id_from_file_name(Path::new("rollout-2026-10-15.jsonl")),
            None
        );
        assert_eq!(id_from_file_name(Path::new("short.jsonl")), None);
    }

    #[test]
    fn short_roles_come_from_the_originator() {
        assert_eq!(role_of(None, Some("codex_exec")).as_deref(), Some("exec"));
        assert_eq!(
            role_of(None, Some("codex_vscode")).as_deref(),
            Some("vscode")
        );
        assert_eq!(
            role_of(Some("worker"), Some("codex_vscode")).as_deref(),
            Some("worker")
        );
        assert_eq!(role_of(None, None), None);
    }

    /// `<home>/.codex/sessions/Y/M/D/rollout-<stamp>-<id>.jsonl` for the local "today".
    fn rollout_path(sessions: &Path, id: &str) -> PathBuf {
        let day = Local::now().date_naive();
        sessions
            .join(format!("{:04}", day.year()))
            .join(format!("{:02}", day.month()))
            .join(format!("{:02}", day.day()))
            .join(format!("rollout-{day}T10-00-00-{id}.jsonl"))
    }

    fn age(path: &Path, by: Duration) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - by).unwrap();
    }

    fn since_10_min() -> Timestamp {
        Utc::now() - chrono::Duration::minutes(10)
    }

    fn open_turn_lines(id: &str) -> Vec<String> {
        vec![
            meta_line(id, "C:\\Projects\\nucleos", "codex_exec"),
            prompt_line("do the thing\nin two lines"),
            rec("event_msg", json!({"type": "task_started"})),
            rec("turn_context", json!({"model": "gpt-5.6-luna"})),
            rec(
                "event_msg",
                json!({"type": "token_count", "info": {"last_token_usage": {"total_tokens": 4242}}}),
            ),
        ]
    }

    fn closed_turn_lines(id: &str) -> Vec<String> {
        let mut lines = open_turn_lines(id);
        lines.push(rec(
            "event_msg",
            json!({"type": "task_complete", "last_agent_message": "ok"}),
        ));
        lines
    }

    fn scan_now(state: &Mutex<State>, sessions: &Path) -> Vec<Sighting> {
        scan(state, sessions, since_10_min(), Utc::now())
    }

    #[test]
    fn a_fresh_open_turn_is_busy_and_carries_the_session_facts() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("s");
        let path = rollout_path(&sessions, ID_A);
        write_file(&path, &open_turn_lines(ID_A));
        let state = Mutex::new(State::default());
        let found = scan_now(&state, &sessions);
        assert_eq!(found.len(), 1);
        let s = &found[0];
        assert_eq!(s.key, NodeKey::new("codex-cli", ID_A));
        assert_eq!(s.harness_label, "codex-cli");
        assert_eq!(s.state, NodeState::Busy);
        assert_eq!(s.ended_at, None);
        assert_eq!(s.model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(s.role.as_deref(), Some("exec"));
        assert_eq!(s.project.as_deref(), Some("nucleos"));
        assert_eq!(s.label, "do the thing in two lines");
        assert_eq!(s.usage.as_ref().unwrap().context_tokens, Some(4242));
        assert!(s.has_lines && s.has_transcript && !s.can_stop);
        assert_eq!((s.run_id, s.parent.clone()), (None, None));
    }

    #[test]
    fn a_quiet_open_turn_is_lost_and_a_closed_one_is_done_with_an_end() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("s");
        let lost = rollout_path(&sessions, ID_A);
        let done = rollout_path(&sessions, ID_B);
        write_file(&lost, &open_turn_lines(ID_A));
        write_file(&done, &closed_turn_lines(ID_B));
        for path in [&lost, &done] {
            age(path, limits::CODEX_LIVE_MTIME + Duration::from_secs(60));
        }
        let found = scan_now(&Mutex::new(State::default()), &sessions);
        let by_id = |id: &str| found.iter().find(|s| s.key.native_id == id).unwrap();
        assert_eq!(by_id(ID_A).state, NodeState::Lost);
        assert_eq!(by_id(ID_B).state, NodeState::Done);
        for id in [ID_A, ID_B] {
            assert_eq!(by_id(id).ended_at, Some(by_id(id).last_activity_at));
        }
    }

    #[test]
    fn a_subagent_rollout_is_a_child_of_its_parent_thread() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("s");
        let meta = rec(
            "session_meta",
            json!({"id": ID_B, "cwd": "/w/x", "originator": "codex_vscode",
                   "source": {"subagent": {"thread_spawn": {"parent_thread_id": PARENT, "agent_nickname": "Pasteur", "agent_role": "worker"}}}}),
        );
        write_file(
            &rollout_path(&sessions, ID_B),
            &[meta, rec("event_msg", json!({"type": "task_started"}))],
        );
        let found = scan_now(&Mutex::new(State::default()), &sessions);
        assert_eq!(found[0].parent, Some(NodeKey::new("codex-cli", PARENT)));
        assert_eq!(found[0].label, "Pasteur");
        assert_eq!(found[0].role.as_deref(), Some("worker"));
    }

    #[test]
    fn a_secret_in_a_label_is_redacted_by_clean_label() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("s");
        let token = format!("ghp_{}", "a".repeat(36));
        let meta = rec(
            "session_meta",
            json!({"id": ID_B, "cwd": "/w/x", "originator": "codex_vscode",
                   "source": {"subagent": {"thread_spawn": {"parent_thread_id": PARENT, "agent_nickname": format!("push {token}"), "agent_role": "worker"}}}}),
        );
        write_file(
            &rollout_path(&sessions, ID_B),
            &[meta, rec("event_msg", json!({"type": "task_started"}))],
        );
        let found = scan_now(&Mutex::new(State::default()), &sessions);
        assert!(
            found[0].label.contains("[SECRET:github]"),
            "{}",
            found[0].label
        );
        assert!(!found[0].label.contains(&token), "{}", found[0].label);
    }

    #[test]
    fn a_missing_session_meta_degrades_to_the_file_name_id_and_no_model() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("s");
        write_file(
            &rollout_path(&sessions, ID_A),
            &[rec("event_msg", json!({"type": "task_started"}))],
        );
        let found = scan_now(&Mutex::new(State::default()), &sessions);
        assert_eq!(found[0].key.native_id, ID_A);
        assert_eq!(
            (found[0].model.clone(), found[0].project.clone()),
            (None, None)
        );
        assert!(found[0].label.starts_with("codex 01a14120"));
    }

    #[test]
    fn a_session_with_activity_before_since_is_not_discovered() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("s");
        let path = rollout_path(&sessions, ID_A);
        write_file(&path, &closed_turn_lines(ID_A));
        age(&path, Duration::from_secs(3600));
        assert!(scan_now(&Mutex::new(State::default()), &sessions).is_empty());
    }

    #[test]
    fn a_live_root_older_than_since_is_still_sighted() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("s");
        let live = rollout_path(&sessions, ID_A);
        let dead = rollout_path(&sessions, ID_B);
        write_file(&live, &open_turn_lines(ID_A));
        write_file(&dead, &closed_turn_lines(ID_B));
        for path in [&live, &dead] {
            age(path, Duration::from_secs(30));
        }
        // `since` is newer than both files, yet the open turn is still inside CODEX_LIVE_MTIME.
        let since = Utc::now() - chrono::Duration::seconds(10);
        let found = scan(&Mutex::new(State::default()), &sessions, since, Utc::now());
        assert_eq!(found.len(), 1, "only the dead rollout is bounded by since");
        assert_eq!(
            (found[0].key.native_id.as_str(), found[0].state),
            (ID_A, NodeState::Busy)
        );
    }

    #[test]
    fn unchanged_directories_and_files_are_not_read_again() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("s");
        let path = rollout_path(&sessions, ID_A);
        write_file(&path, &open_turn_lines(ID_A));
        let state = Mutex::new(State::default());
        scan_now(&state, &sessions);
        let first = counters(&state);
        assert!(first.0 >= 1 && first.1 == 1);
        // Same directory mtime, same file (len, mtime): neither a re-list nor a re-read.
        let again = scan_now(&state, &sessions);
        assert_eq!(again.len(), 1);
        assert_eq!(counters(&state), first);
        // A growing file is re-read, but the directory is not re-listed.
        let mut lines = open_turn_lines(ID_A);
        lines.push(rec("event_msg", json!({"type": "task_complete"})));
        write_file(&path, &lines);
        let grown = scan_now(&state, &sessions);
        assert_eq!(grown[0].state, NodeState::Done);
        assert_eq!(counters(&state).1, first.1 + 1);
        // A directory whose mtime moved is re-listed and sees the new file.
        forget_dir_mtimes(&state);
        write_file(&rollout_path(&sessions, ID_B), &open_turn_lines(ID_B));
        assert_eq!(scan_now(&state, &sessions).len(), 2);
        assert!(counters(&state).0 > first.0);
    }

    fn counters(state: &Mutex<State>) -> (usize, usize) {
        let state = state.lock().unwrap();
        (state.listings, state.refreshes)
    }

    /// Simulates a directory whose mtime changed since the last pass.
    fn forget_dir_mtimes(state: &Mutex<State>) {
        for dir in state.lock().unwrap().dirs.values_mut() {
            dir.mtime = SystemTime::UNIX_EPOCH;
        }
    }

    #[tokio::test]
    async fn the_source_discovers_resolves_refs_and_reads_lines_incrementally() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex").join("sessions");
        let path = rollout_path(&sessions, ID_A);
        write_file(&path, &open_turn_lines(ID_A));
        let source = CodexCli::new(home.path().to_path_buf());
        assert_eq!(source.name(), "codex_cli");

        let found = source.discover(since_10_min()).await.unwrap();
        assert_eq!(found.len(), 1);
        let node = source
            .node_ref(&found[0].key)
            .expect("a sighted key resolves");
        assert_eq!(node.path.as_deref(), Some(path.as_path()));
        assert!(
            source
                .node_ref(&NodeKey::new("codex-cli", "nope"))
                .is_none()
        );
        assert!(
            source
                .node_ref(&NodeKey::new("claude-code", ID_A))
                .is_none()
        );

        // First read starts from the tail and returns every complete line's summary.
        let first = source.read_lines(&node, Offset::TAIL).await.unwrap();
        assert_eq!(first.lines.first().unwrap().kind, TermKind::Prompt);
        assert_eq!(first.usage.unwrap().context_tokens, Some(4242));
        assert!(first.last_activity_at.is_some());

        // Appending one assistant line (and a half-written one) yields only the complete line.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(&rec(
            "response_item",
            json!({"type": "message", "role": "assistant", "content": [{"text": "working"}]}),
        ));
        text.push('\n');
        text.push_str("{\"timestamp\":\"2026-10-20T10:00:00Z\",\"type\":\"resp");
        std::fs::write(&path, &text).unwrap();
        let second = source.read_lines(&node, first.offset).await.unwrap();
        assert_eq!(second.lines.len(), 1);
        assert_eq!(second.lines[0].text, "working");
        assert!(
            second.offset.0 < text.len() as u64,
            "the offset stays before the partial line"
        );
        assert_eq!(second.usage, None);

        // Nothing new: empty read, same offset.
        let third = source.read_lines(&node, second.offset).await.unwrap();
        assert!(third.lines.is_empty());
        assert_eq!(third.offset, second.offset);
    }

    #[tokio::test]
    async fn a_truncated_file_is_read_again_from_the_tail() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex").join("sessions");
        let path = rollout_path(&sessions, ID_A);
        write_file(&path, &closed_turn_lines(ID_A));
        let source = CodexCli::new(home.path().to_path_buf());
        let key = source.discover(since_10_min()).await.unwrap().remove(0).key;
        let node = source.node_ref(&key).unwrap();
        let stale = Offset(1_000_000);
        write_file(&path, &[prompt_line("after rotation")]);
        let read = source.read_lines(&node, stale).await.unwrap();
        assert_eq!(read.lines.len(), 1);
        assert_eq!(read.lines[0].text, "after rotation");
    }

    #[tokio::test]
    async fn the_transcript_pages_backwards_and_keeps_tool_detail() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex").join("sessions");
        let path = rollout_path(&sessions, ID_A);
        let mut lines = vec![meta_line(ID_A, "/w/x", "codex_exec")];
        lines.push(prompt_line("first"));
        lines.push(rec(
            "response_item",
            json!({"type": "function_call", "name": "shell_command",
            "arguments": "{\"command\":\"ls -la\\nmore\"}"}),
        ));
        lines.push(rec(
            "response_item",
            json!({"type": "function_call_output", "output": "Exit code: 0\nWall time: 1 s\nOutput:\nx"}),
        ));
        lines.push(rec(
            "response_item",
            json!({"type": "message", "role": "assistant", "content": [{"text": "line one\nline two"}]}),
        ));
        write_file(&path, &lines);
        let source = CodexCli::new(home.path().to_path_buf());
        let key = source.discover(since_10_min()).await.unwrap().remove(0).key;
        let node = source.node_ref(&key).unwrap();

        let page = source.transcript_page(&node, None).await.unwrap();
        let kinds: Vec<TermKind> = page.entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TermKind::Prompt,
                TermKind::Tool,
                TermKind::Result,
                TermKind::Text
            ]
        );
        assert_eq!(page.entries[1].text, "shell_command ls -la");
        assert!(page.entries[1].detail.as_deref().unwrap().contains("more"));
        assert_eq!(page.entries[3].text, "line one line two");
        assert_eq!(page.before, None, "the whole file fit in one page");
    }

    #[tokio::test]
    async fn a_cursor_pages_back_to_the_start_of_the_file_and_then_stops() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex").join("sessions");
        let path = rollout_path(&sessions, ID_A);
        let lines = [
            prompt_line("one"),
            prompt_line("two"),
            prompt_line("three"),
            prompt_line("four"),
        ];
        write_file(&path, &lines);
        let source = CodexCli::new(home.path().to_path_buf());
        let key = source.discover(since_10_min()).await.unwrap().remove(0).key;
        let node = source.node_ref(&key).unwrap();
        let texts = |page: &TranscriptPage| {
            page.entries
                .iter()
                .map(|e| e.text.clone())
                .collect::<Vec<_>>()
        };

        // The cursor is the byte offset of the third line: both earlier lines and their newlines.
        let third = (lines[0].len() + 1 + lines[1].len() + 1) as u64;
        let page = source
            .transcript_page(&node, Some(third.to_string()))
            .await
            .unwrap();
        assert_eq!(
            texts(&page),
            ["one", "two"],
            "only entries from lines before the cursor come back"
        );
        assert_eq!(
            page.before, None,
            "byte 0 was reached, so there is no earlier page"
        );

        // The cursor is the offset of the second line: only the first line is earlier.
        let second = (lines[0].len() + 1) as u64;
        let page = source
            .transcript_page(&node, Some(second.to_string()))
            .await
            .unwrap();
        assert_eq!(texts(&page), ["one"]);
        assert_eq!(page.before, None);

        // A cursor that is not a number means "from the end".
        let page = source
            .transcript_page(&node, Some("garbage".to_owned()))
            .await
            .unwrap();
        assert_eq!(texts(&page), ["one", "two", "three", "four"]);
    }

    /// The key a daemon codex run gets from `daemon_runs` is `codex-cli:<runs.session_id>` and
    /// `runs.session_id` is the `thread.started` thread id. The adapter must key by the rollout's
    /// `session_meta.id`, which is also the uuid in the file name. If a real run ever shows a
    /// thread id that differs from both, this is the assumption to revisit (plan: Formats observed).
    #[test]
    fn the_node_key_is_the_session_meta_id_which_is_the_file_name_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("rollout-2026-10-20T10-00-00-{ID_A}.jsonl");
        let path = dir.path().join(&name);
        // A fork: `session_id` names the fork's root, `id` the thread. The key must be the thread.
        let meta = rec(
            "session_meta",
            json!({"session_id": ID_B, "id": ID_A, "cwd": "/w"}),
        );
        write_file(&path, &[meta]);
        let head = read_head(&path).unwrap();
        assert_eq!(head.id.as_deref(), Some(ID_A));
        assert_eq!(id_from_file_name(&path).as_deref(), Some(ID_A));
    }
}
