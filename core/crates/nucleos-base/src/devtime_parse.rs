//! PURE: one transcript line becomes a typed devtime event, with no I/O and no stored text.
//!
//! Also strips a line down to its structure, for fixtures.
//!
//! **What it keeps.** Timestamps, ids, token counts, tool names, a program token (`cargo test`) and a
//! sha256 prefix of the whitespace-collapsed command, closed outcome and error tokens, and the hashes
//! of edited text. **What it never keeps** is message text, thinking, file contents, error text or the
//! arguments of a command: the privacy line of the devtime spec (§3.1/§8) is drawn here, before
//! anything reaches a row. Paths stay absolute and are relativised by the ingestion packet.
//!
//! The parser is total: a line it cannot read is [`Parsed::Failed`], an unfamiliar `type` is
//! [`Parsed::Unknown`], and neither panics. It builds on `serde_json::Value` rather than a typed
//! struct on purpose, because the transcript format is the CLI's to change and a missing field must
//! cost one record, never the file.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::command_reader::{self, Shell};

/// The parser generation. Derived rows carry it so a parser fix can re-derive what it changed.
pub const PARSER_VERSION: i64 = 1;

const INTERRUPT_MARK: &str = "[Request interrupted by user";
const DENIAL_MARKS: [&str; 3] = ["doesn't want to proceed", "User rejected", "user declined"];
const NOTIFICATION_TAG: &str = "<task-notification>";

/// The closed vocabulary of `ToolResult::error_class`.
#[cfg_attr(not(test), allow(dead_code))]
pub const ERROR_CLASSES: [&str; 8] = [
    "exit_nonzero",
    "exit_75",
    "timeout",
    "interrupted",
    "permission_denied",
    "hook_block",
    "edit_not_found",
    "tool_error",
];

/// Programs whose second word (a subcommand) is part of what the command is. Everything else keeps
/// the program alone: `ls -la` and `ls src` are the same program.
const SUBCOMMAND_PROGRAMS: [&str; 9] = [
    "cargo", "git", "npm", "npx", "go", "python", "python3", "node", "bash",
];

/// Record types the parser knows. Anything else is `Unknown`, counted and never a failure.
const KNOWN_TYPES: [&str; 10] = [
    "user",
    "assistant",
    "system",
    "queue-operation",
    "summary",
    "file-history-snapshot",
    "last-prompt",
    "atis-latch",
    "attachment",
    "progress",
];

/// The result of reading one line.
// A per-line transient return value that is moved straight into the chunk's items; boxing the
// event would cost one allocation per transcript line.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Event(Event),
    /// Known and untimed, or known and not worth an event (`<synthetic>`, a `progress` tick).
    Skipped,
    /// A `type` this parser has never heard of.
    Unknown,
    /// Not JSON, not an object, or a timed record with no readable timestamp or session.
    Failed,
}

/// What every event carries, wrapped around one [`EventKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// UTC, millisecond precision, `Z` suffix: strings of this form sort as time.
    pub ts: String,
    pub session_id: String,
    pub cwd: Option<String>,
    pub is_sidechain: bool,
    pub agent_id: Option<String>,
    pub entrypoint: Option<String>,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    Assistant {
        message_id: String,
        model: String,
        effort: Option<String>,
        /// `[input, cache_read, cache_creation, output]`, the column order of `devtime_messages`.
        usage: [i64; 4],
        tool_uses: Vec<ToolUse>,
        has_text: bool,
    },
    ToolResults(Vec<ToolResult>),
    HumanPrompt {
        /// The prompt's text mentions the interrupt mark somewhere other than at its start.
        interrupted_marker: bool,
    },
    Interrupt,
    Notification {
        task_id: String,
        tool_use_id: String,
        status: String,
        exit_code: Option<i64>,
    },
    Compact {
        duration_ms: Option<i64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub background: bool,
    /// Program token(s) of the first non-`cd` segment of a Bash/PowerShell command.
    pub cmd_program: Option<String>,
    /// First 16 hex chars of the sha256 of the whitespace-collapsed command.
    pub cmd_hash: Option<String>,
    pub timeout_ms: Option<i64>,
    /// Absolute paths the call writes.
    pub files: Vec<String>,
    /// `(path, before_hash, after_hash)`; a hash of text that is not in the input is the hash of `""`.
    pub edits: Vec<(String, String, String)>,
    /// `(path, offset)`, offset 0 when absent.
    pub reads: Vec<(String, i64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub is_error: bool,
    /// One of `ok`, `error`, `interrupted`, `launched` (`devtime_store::OUTCOMES`).
    pub outcome: &'static str,
    pub exit_code: Option<i64>,
    /// One of [`ERROR_CLASSES`]; set only when the result failed or was interrupted.
    pub error_class: Option<&'static str>,
    pub agent_id: Option<String>,
    pub resolved_model: Option<String>,
    pub async_launched: bool,
    pub bg_task_id: Option<String>,
}

fn text_of<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

fn normalise_ts(raw: &str) -> Option<String> {
    let parsed = DateTime::parse_from_rfc3339(raw).ok()?;
    Some(
        parsed
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true),
    )
}

fn sha16(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// PURE: one transcript line to one [`Parsed`]. Never panics, never keeps text.
pub fn parse_line(line: &[u8]) -> Parsed {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Parsed::Failed;
    };
    let Some(rec) = value.as_object() else {
        return Parsed::Failed;
    };
    let Some(kind) = text_of(rec, "type") else {
        return Parsed::Unknown;
    };
    match kind {
        "assistant" => parse_assistant(rec),
        "user" => parse_user(rec),
        "system" => parse_system(rec),
        "queue-operation" => parse_queue(rec),
        k if KNOWN_TYPES.contains(&k) => Parsed::Skipped,
        _ => Parsed::Unknown,
    }
}

/// Wraps a kind with the fields every record carries, or fails the line when it has no usable
/// timestamp or session.
fn wrap(rec: &Map<String, Value>, kind: EventKind) -> Parsed {
    let Some(ts) = text_of(rec, "timestamp").and_then(normalise_ts) else {
        return Parsed::Failed;
    };
    let Some(session_id) = text_of(rec, "sessionId") else {
        return Parsed::Failed;
    };
    Parsed::Event(Event {
        ts,
        session_id: session_id.to_string(),
        cwd: text_of(rec, "cwd").map(str::to_string),
        is_sidechain: rec
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        agent_id: text_of(rec, "agentId").map(str::to_string),
        entrypoint: text_of(rec, "entrypoint").map(str::to_string),
        kind,
    })
}

fn parse_assistant(rec: &Map<String, Value>) -> Parsed {
    let Some(msg) = rec.get("message").and_then(Value::as_object) else {
        return Parsed::Failed;
    };
    let model = text_of(msg, "model").unwrap_or("");
    if model == "<synthetic>" {
        return Parsed::Skipped;
    }
    let Some(message_id) = text_of(msg, "id") else {
        return Parsed::Failed;
    };
    let count = |key: &str| {
        msg.get("usage")
            .and_then(|u| u.get(key))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    let usage = [
        count("input_tokens"),
        count("cache_read_input_tokens"),
        count("cache_creation_input_tokens"),
        count("output_tokens"),
    ];
    let mut tool_uses = Vec::new();
    let mut has_text = false;
    if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
        for block in blocks.iter().filter_map(Value::as_object) {
            match text_of(block, "type") {
                Some("text") => {
                    has_text |= text_of(block, "text").is_some_and(|t| !t.trim().is_empty());
                }
                Some("tool_use") => tool_uses.push(parse_tool_use(block)),
                _ => {}
            }
        }
    }
    wrap(
        rec,
        EventKind::Assistant {
            message_id: message_id.to_string(),
            model: model.to_string(),
            effort: text_of(rec, "effort").map(str::to_string),
            usage,
            tool_uses,
            has_text,
        },
    )
}

fn parse_tool_use(block: &Map<String, Value>) -> ToolUse {
    let name = text_of(block, "name").unwrap_or("").to_string();
    let empty = Map::new();
    let input = block
        .get("input")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut tool = ToolUse {
        id: text_of(block, "id").unwrap_or("").to_string(),
        background: input.get("run_in_background").and_then(Value::as_bool) == Some(true),
        timeout_ms: input.get("timeout").and_then(Value::as_i64),
        cmd_program: None,
        cmd_hash: None,
        files: Vec::new(),
        edits: Vec::new(),
        reads: Vec::new(),
        name,
    };
    let hash_of = |key: &str| sha16(text_of(input, key).unwrap_or(""));
    let path = text_of(input, "file_path").map(str::to_string);
    match tool.name.as_str() {
        "Bash" | "PowerShell" => {
            if let Some(cmd) = text_of(input, "command") {
                let shell = if tool.name == "Bash" {
                    Shell::Posix
                } else {
                    Shell::PowerShell
                };
                tool.cmd_program = command_program(cmd, shell);
                tool.cmd_hash = Some(sha16(&cmd.split_whitespace().collect::<Vec<_>>().join(" ")));
            }
        }
        "Edit" => {
            if let Some(p) = path {
                tool.edits
                    .push((p.clone(), hash_of("old_string"), hash_of("new_string")));
                tool.files.push(p);
            }
        }
        "Write" => {
            if let Some(p) = path {
                tool.edits
                    .push((p.clone(), String::new(), hash_of("content")));
                tool.files.push(p);
            }
        }
        "MultiEdit" => {
            if let Some(p) = path {
                let each = input
                    .get("edits")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice);
                for edit in each {
                    let h = |k: &str| sha16(edit.get(k).and_then(Value::as_str).unwrap_or(""));
                    tool.edits
                        .push((p.clone(), h("old_string"), h("new_string")));
                }
                tool.files.push(p);
            }
        }
        "NotebookEdit" => {
            if let Some(p) = text_of(input, "notebook_path").map(str::to_string).or(path) {
                tool.edits
                    .push((p.clone(), String::new(), hash_of("new_source")));
                tool.files.push(p);
            }
        }
        "Read" => {
            if let Some(p) = path {
                tool.reads
                    .push((p, input.get("offset").and_then(Value::as_i64).unwrap_or(0)));
            }
        }
        _ => {}
    }
    tool
}

fn program_of(word: &str) -> String {
    let word = word.trim_matches(|c| c == '"' || c == '\'');
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    match base.len().checked_sub(4) {
        Some(at) if base.is_char_boundary(at) && base[at..].eq_ignore_ascii_case(".exe") => {
            base[..at].to_string()
        }
        _ => base.to_string(),
    }
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The program (and subcommand, for the programs that have one) of the first segment that is not a
/// `cd`. Never any other argument.
fn command_program(cmd: &str, shell: Shell) -> Option<String> {
    for segment in command_reader::segments(cmd, shell) {
        let mut words = segment.split_whitespace().skip_while(|w| is_assignment(w));
        let Some(first) = words.next() else { continue };
        let program = program_of(first);
        if program.is_empty() || program == "cd" {
            continue;
        }
        if SUBCOMMAND_PROGRAMS.contains(&program.as_str())
            && let Some(second) = words.next()
            && !second.starts_with('-')
        {
            return Some(format!("{program} {second}"));
        }
        return Some(program);
    }
    None
}

/// The text of a `tool_result`'s content, whichever of the two shapes it has.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn exit_code_of(text: &str) -> Option<i64> {
    let rest = &text[text.find("Exit code ")? + "Exit code ".len()..];
    let end = rest
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && *c == '-')))
        .map_or(rest.len(), |(i, _)| i);
    rest[..end].parse().ok()
}

/// The closed class of a failed result's text, and its exit code when the class carries one.
fn classify_error(text: &str) -> (&'static str, Option<i64>) {
    if let Some(code) = exit_code_of(text) {
        return (
            if code == 75 {
                "exit_75"
            } else {
                "exit_nonzero"
            },
            Some(code),
        );
    }
    let class = if text.contains("Command timed out") {
        "timeout"
    } else if DENIAL_MARKS.iter().any(|m| text.contains(m)) {
        "permission_denied"
    } else if text.contains("PreToolUse") && text.contains("hook") {
        "hook_block"
    } else if text.contains("String to replace not found") {
        "edit_not_found"
    } else {
        "tool_error"
    };
    (class, None)
}

fn parse_tool_results(rec: &Map<String, Value>, blocks: &[Value]) -> Vec<ToolResult> {
    let own: Vec<&Map<String, Value>> = blocks
        .iter()
        .filter_map(Value::as_object)
        .filter(|b| text_of(b, "type") == Some("tool_result"))
        .collect();
    // `toolUseResult` is per record, so it can only be attributed when the record holds one result.
    let meta = if own.len() == 1 {
        rec.get("toolUseResult").and_then(Value::as_object)
    } else {
        None
    };
    own.iter()
        .map(|block| {
            let text = result_text(block.get("content"));
            let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
            let meta_str = |k: &str| meta.and_then(|m| text_of(m, k)).map(str::to_string);
            let bg_task_id = meta_str("backgroundTaskId");
            let async_launched =
                meta.and_then(|m| m.get("isAsync")).and_then(Value::as_bool) == Some(true);
            let interrupted = text.contains(INTERRUPT_MARK)
                || meta
                    .and_then(|m| m.get("interrupted"))
                    .and_then(Value::as_bool)
                    == Some(true);
            let (error_class, exit_code) = if interrupted {
                (Some("interrupted"), None)
            } else if is_error {
                let (class, code) = classify_error(&text);
                (Some(class), code)
            } else {
                (None, None)
            };
            let outcome = if interrupted {
                "interrupted"
            } else if is_error {
                "error"
            } else if async_launched || bg_task_id.is_some() {
                "launched"
            } else {
                "ok"
            };
            ToolResult {
                tool_use_id: text_of(block, "tool_use_id").unwrap_or("").to_string(),
                is_error,
                outcome,
                exit_code,
                error_class,
                agent_id: meta_str("agentId"),
                resolved_model: meta_str("resolvedModel"),
                async_launched,
                bg_task_id,
            }
        })
        .collect()
}

/// The text pieces of a user record's content: the string, or each `text` block.
fn text_pieces(content: Option<&Value>) -> Vec<String> {
    match content {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_user(rec: &Map<String, Value>) -> Parsed {
    if rec.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return Parsed::Skipped;
    }
    let content = rec.get("message").and_then(|m| m.get("content"));
    if let Some(Value::Array(blocks)) = content
        && blocks
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
    {
        return wrap(rec, EventKind::ToolResults(parse_tool_results(rec, blocks)));
    }
    let pieces = text_pieces(content);
    if pieces
        .iter()
        .any(|p| p.trim_start().starts_with(INTERRUPT_MARK))
    {
        return wrap(rec, EventKind::Interrupt);
    }
    let origin = rec
        .get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str);
    let joined = pieces.join(" ");
    if origin == Some("task-notification") || joined.trim_start().starts_with(NOTIFICATION_TAG) {
        return match parse_notification(&pieces.join("\n")) {
            Some(kind) => wrap(rec, kind),
            None => Parsed::Skipped,
        };
    }
    let is_prompt = match origin {
        Some(kind) => kind == "human",
        None => !joined.trim().is_empty() && !joined.trim_start().starts_with('<'),
    };
    if !is_prompt {
        return Parsed::Skipped;
    }
    wrap(
        rec,
        EventKind::HumanPrompt {
            interrupted_marker: joined.contains(INTERRUPT_MARK),
        },
    )
}

fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let len = text[start..].find(&format!("</{name}>"))?;
    Some(text[start..start + len].trim())
}

/// Plain string search, not an XML parser: the body is the CLI's, and a tag we cannot find is a
/// field we do not have rather than a line we cannot read.
fn parse_notification(body: &str) -> Option<EventKind> {
    let task_id = tag(body, "task-id").filter(|t| !t.is_empty())?;
    let summary = tag(body, "summary").unwrap_or("");
    let exit_code = summary.find("(exit code ").and_then(|at| {
        let rest = &summary[at + "(exit code ".len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end].parse().ok()
    });
    Some(EventKind::Notification {
        task_id: task_id.to_string(),
        tool_use_id: tag(body, "tool-use-id").unwrap_or("").to_string(),
        status: tag(body, "status").unwrap_or("").to_string(),
        exit_code,
    })
}

fn parse_system(rec: &Map<String, Value>) -> Parsed {
    if text_of(rec, "subtype") != Some("compact_boundary") {
        return Parsed::Skipped;
    }
    let duration_ms = rec
        .get("compactMetadata")
        .and_then(|m| m.get("durationMs"))
        .and_then(Value::as_i64);
    wrap(rec, EventKind::Compact { duration_ms })
}

fn parse_queue(rec: &Map<String, Value>) -> Parsed {
    if text_of(rec, "operation") != Some("enqueue") {
        return Parsed::Skipped;
    }
    match text_of(rec, "content")
        .filter(|c| c.contains(NOTIFICATION_TAG))
        .and_then(parse_notification)
    {
        Some(kind) => wrap(rec, kind),
        None => Parsed::Skipped,
    }
}

// ---- stripping a line to its structure -------------------------------------------------------

/// The record-level keys a stripped line keeps, verbatim.
#[cfg_attr(not(test), allow(dead_code))]
const KEPT_RECORD_KEYS: [&str; 14] = [
    "type",
    "subtype",
    "operation",
    "timestamp",
    "sessionId",
    "uuid",
    "parentUuid",
    "isSidechain",
    "agentId",
    "isMeta",
    "promptId",
    "entrypoint",
    "version",
    "effort",
];

/// Strips a transcript to its structure, remembering the first record's `cwd` as the project root so
/// later paths can be made relative to it.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Stripper {
    fixture_root: String,
    original_root: Option<String>,
}

/// `path` below `root` (separator- and drive-case-insensitive), `""` when equal, `None` when outside.
#[cfg_attr(not(test), allow(dead_code))]
fn below(path: &str, root: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let root = root.replace('\\', "/");
    let root = root.trim_end_matches('/');
    let (lp, lr) = (path.to_ascii_lowercase(), root.to_ascii_lowercase());
    if lp == lr {
        return Some(String::new());
    }
    let prefix = format!("{lr}/");
    lp.starts_with(&prefix)
        .then(|| path[prefix.len()..].to_string())
}

#[cfg_attr(not(test), allow(dead_code))]
impl Stripper {
    pub fn new(fixture_root: &str) -> Self {
        Self {
            fixture_root: fixture_root.trim_end_matches('/').to_string(),
            original_root: None,
        }
    }

    fn fixture_cwd(&self, cwd: &str) -> String {
        match self.original_root.as_deref().and_then(|r| below(cwd, r)) {
            Some(rest) if rest.is_empty() => self.fixture_root.clone(),
            Some(rest) => format!("{}/{rest}", self.fixture_root),
            None => format!("{}/outside", self.fixture_root),
        }
    }

    /// A file path relative to the project root; a path outside it keeps only its file name.
    fn relative(&self, path: &str) -> String {
        match self.original_root.as_deref().and_then(|r| below(path, r)) {
            Some(rest) => rest,
            None => {
                let name = path.rsplit(['/', '\\']).next().unwrap_or("");
                format!("outside/{name}")
            }
        }
    }

    /// `None` for a line that is not a JSON object with a `type`: a stripped fixture holds only lines
    /// the parser can say something about.
    pub fn strip(&mut self, line: &[u8]) -> Option<String> {
        let value: Value = serde_json::from_slice(line).ok()?;
        let rec = value.as_object()?;
        let kind = text_of(rec, "type")?;
        let mut out = Map::new();
        let copy = |out: &mut Map<String, Value>, key: &str| {
            if let Some(v) = rec.get(key).filter(|v| !v.is_object() && !v.is_array()) {
                out.insert(key.to_string(), v.clone());
            }
        };
        if !KNOWN_TYPES.contains(&kind) {
            for key in ["type", "timestamp", "sessionId", "uuid"] {
                copy(&mut out, key);
            }
            return Some(Value::Object(out).to_string());
        }
        for key in KEPT_RECORD_KEYS {
            copy(&mut out, key);
        }
        if let Some(kind) = rec
            .get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str)
        {
            out.insert("origin".into(), json!({ "kind": kind }));
        }
        if let Some(cwd) = text_of(rec, "cwd") {
            if self.original_root.is_none() {
                self.original_root = Some(cwd.to_string());
            }
            out.insert("cwd".into(), json!(self.fixture_cwd(cwd)));
        }
        if let Some(meta) = rec.get("compactMetadata").and_then(Value::as_object) {
            let mut kept = Map::new();
            for key in ["trigger", "durationMs", "preTokens"] {
                if let Some(v) = meta.get(key).filter(|v| !v.is_object() && !v.is_array()) {
                    kept.insert(key.to_string(), v.clone());
                }
            }
            out.insert("compactMetadata".into(), Value::Object(kept));
        }
        if let Some(meta) = rec.get("toolUseResult").and_then(Value::as_object) {
            let mut kept = Map::new();
            for key in [
                "isAsync",
                "status",
                "agentId",
                "resolvedModel",
                "totalDurationMs",
                "backgroundTaskId",
                "interrupted",
            ] {
                if let Some(v) = meta.get(key).filter(|v| !v.is_object() && !v.is_array()) {
                    kept.insert(key.to_string(), v.clone());
                }
            }
            out.insert("toolUseResult".into(), Value::Object(kept));
        }
        if kind == "queue-operation"
            && let Some(content) = text_of(rec, "content")
        {
            // Only a notification keeps any content: the parser reads nothing else from this record.
            let marker = text_marker(content);
            let kept = if marker.starts_with(NOTIFICATION_TAG) {
                marker
            } else {
                String::new()
            };
            out.insert("content".into(), json!(kept));
        }
        if let Some(msg) = rec.get("message").and_then(Value::as_object) {
            out.insert("message".into(), self.strip_message(msg));
        }
        Some(Value::Object(out).to_string())
    }

    fn strip_message(&self, msg: &Map<String, Value>) -> Value {
        let mut out = Map::new();
        for key in ["id", "model", "role"] {
            if let Some(v) = msg.get(key).filter(|v| v.is_string()) {
                out.insert(key.to_string(), v.clone());
            }
        }
        if let Some(usage) = msg.get("usage").and_then(Value::as_object) {
            let mut kept = Map::new();
            for key in [
                "input_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
                "output_tokens",
            ] {
                if let Some(n) = usage.get(key).filter(|v| v.is_number()) {
                    kept.insert(key.to_string(), n.clone());
                }
            }
            out.insert("usage".into(), Value::Object(kept));
        }
        match msg.get("content") {
            Some(Value::String(text)) => {
                out.insert("content".into(), json!(text_marker(text)));
            }
            Some(Value::Array(blocks)) => {
                let kept: Vec<Value> = blocks
                    .iter()
                    .filter_map(Value::as_object)
                    .map(|b| self.strip_block(b))
                    .collect();
                out.insert("content".into(), Value::Array(kept));
            }
            _ => {}
        }
        Value::Object(out)
    }

    fn strip_block(&self, block: &Map<String, Value>) -> Value {
        let kind = text_of(block, "type").unwrap_or("");
        let mut out = Map::new();
        out.insert("type".into(), json!(kind));
        match kind {
            "text" => {
                out.insert(
                    "text".into(),
                    json!(text_marker(text_of(block, "text").unwrap_or(""))),
                );
            }
            "tool_result" => {
                let text = result_text(block.get("content"));
                let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                out.insert("content".into(), json!(result_marker(&text, is_error)));
                for key in ["tool_use_id", "is_error"] {
                    if let Some(v) = block.get(key).filter(|v| v.is_string() || v.is_boolean()) {
                        out.insert(key.to_string(), v.clone());
                    }
                }
            }
            "tool_use" => {
                for key in ["id", "name"] {
                    if let Some(v) = block.get(key).filter(|v| v.is_string()) {
                        out.insert(key.to_string(), v.clone());
                    }
                }
                let name = text_of(block, "name").unwrap_or("");
                if let Some(input) = block.get("input").and_then(Value::as_object) {
                    out.insert("input".into(), self.strip_input(name, input));
                }
            }
            _ => {}
        }
        Value::Object(out)
    }

    fn strip_input(&self, name: &str, input: &Map<String, Value>) -> Value {
        let mut out = Map::new();
        for key in ["run_in_background", "timeout", "offset"] {
            if let Some(v) = input.get(key).filter(|v| v.is_boolean() || v.is_number()) {
                out.insert(key.to_string(), v.clone());
            }
        }
        for key in ["file_path", "notebook_path"] {
            if let Some(path) = text_of(input, key) {
                out.insert(key.to_string(), json!(self.relative(path)));
            }
        }
        if matches!(name, "Bash" | "PowerShell")
            && let Some(cmd) = text_of(input, "command")
        {
            let shell = if name == "Bash" {
                Shell::Posix
            } else {
                Shell::PowerShell
            };
            out.insert(
                "command".into(),
                json!(command_program(cmd, shell).unwrap_or_default()),
            );
        }
        if name == "MultiEdit"
            && let Some(edits) = input.get("edits").and_then(Value::as_array)
        {
            out.insert("edits".into(), Value::Array(vec![json!({}); edits.len()]));
        }
        Value::Object(out)
    }
}

/// PURE: re-emits a line keeping only its structure (see [`Stripper`] for the version that keeps the
/// first record's cwd as the root). With no earlier record, this line's own `cwd` is the root, so it
/// becomes `fixture_root` itself.
#[cfg_attr(not(test), allow(dead_code))]
pub fn strip_to_structure(line: &[u8], fixture_root: &str) -> Option<String> {
    Stripper::new(fixture_root).strip(line)
}

/// The canonical stand-in for a piece of user or assistant text, chosen so that the parser derives
/// from it what it derived from the original: an interrupt stays one, a notification is rebuilt from
/// its parsed fields, a `<...>` wrapper stays a wrapper, anything else stays prose.
#[cfg_attr(not(test), allow(dead_code))]
fn text_marker(text: &str) -> String {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with(INTERRUPT_MARK) {
        return "[Request interrupted by user]".into();
    }
    if trimmed.starts_with(NOTIFICATION_TAG) {
        return match parse_notification(text) {
            Some(EventKind::Notification {
                task_id,
                tool_use_id,
                status,
                exit_code,
            }) => {
                let summary = exit_code
                    .map(|c| format!("(exit code {c})"))
                    .unwrap_or_default();
                format!(
                    "{NOTIFICATION_TAG}\n<task-id>{task_id}</task-id>\n<tool-use-id>{tool_use_id}</tool-use-id>\n\
                     <status>{status}</status>\n<summary>{summary}</summary>\n</task-notification>"
                )
            }
            _ => "<stripped>".into(),
        };
    }
    if text.contains(INTERRUPT_MARK) {
        return "[text] [Request interrupted by user]".into();
    }
    if trimmed.starts_with('<') {
        return "<stripped>".into();
    }
    "[text]".into()
}

/// The canonical stand-in for a `tool_result`'s content: the one mark the classifier reads, or empty.
#[cfg_attr(not(test), allow(dead_code))]
fn result_marker(text: &str, is_error: bool) -> String {
    if text.contains(INTERRUPT_MARK) {
        return "[Request interrupted by user]".into();
    }
    if !is_error {
        return String::new();
    }
    match classify_error(text) {
        ("exit_nonzero" | "exit_75", Some(code)) => format!("Exit code {code}"),
        ("timeout", _) => "Command timed out".into(),
        ("permission_denied", _) => DENIAL_MARKS[0].into(),
        ("hook_block", _) => "PreToolUse hook".into(),
        ("edit_not_found", _) => "String to replace not found".into(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    const SENTINEL: &str = "SECRET-TEXT-42";
    const TS: &str = "2026-10-01T12:00:00+02:00";

    fn line(v: Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    fn base(kind: &str) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("type".into(), json!(kind));
        m.insert("timestamp".into(), json!(TS));
        m.insert("sessionId".into(), json!("sess-1"));
        m.insert("cwd".into(), json!("/work/proj"));
        m.insert("uuid".into(), json!("u-1"));
        m
    }

    fn assistant_line(id: &str, model: &str, usage: Value, content: Value) -> Vec<u8> {
        let mut m = base("assistant");
        m.insert("effort".into(), json!("high"));
        m.insert(
            "message".into(),
            json!({"id": id, "model": model, "role": "assistant", "usage": usage, "content": content}),
        );
        line(Value::Object(m))
    }

    fn user_line(extra: Value, content: Value) -> Vec<u8> {
        let mut m = base("user");
        m.insert(
            "message".into(),
            json!({"role": "user", "content": content}),
        );
        if let Value::Object(e) = extra {
            for (k, v) in e {
                m.insert(k, v);
            }
        }
        line(Value::Object(m))
    }

    fn tool_result_line(extra: Value, block: Value) -> Vec<u8> {
        user_line(extra, json!([block]))
    }

    fn event(p: Parsed) -> Event {
        match p {
            Parsed::Event(e) => e,
            other => panic!("expected an event, got {other:?}"),
        }
    }

    fn results(p: Parsed) -> Vec<ToolResult> {
        match event(p).kind {
            EventKind::ToolResults(r) => r,
            other => panic!("expected tool results, got {other:?}"),
        }
    }

    fn sha16(s: &str) -> String {
        let d = Sha256::digest(s.as_bytes());
        d.iter().map(|b| format!("{b:02x}")).collect::<String>()[..16].to_string()
    }

    #[test]
    fn assistant_lines_sharing_a_message_id_yield_one_usage() {
        let usage = json!({"input_tokens": 3, "cache_read_input_tokens": 100,
                           "cache_creation_input_tokens": 20, "output_tokens": 7});
        let content = json!([{"type": "text", "text": "hi"}]);
        let a = event(parse_line(&assistant_line(
            "msg_1",
            "claude-x",
            usage.clone(),
            content.clone(),
        )));
        let b = event(parse_line(&assistant_line(
            "msg_1", "claude-x", usage, content,
        )));
        assert_eq!(a.ts, "2026-10-01T10:00:00.000Z");
        assert_eq!(a.session_id, "sess-1");
        assert_eq!(a.cwd.as_deref(), Some("/work/proj"));
        match (&a.kind, &b.kind) {
            (
                EventKind::Assistant {
                    message_id: ia,
                    model,
                    effort,
                    usage: ua,
                    has_text,
                    tool_uses,
                },
                EventKind::Assistant {
                    message_id: ib,
                    usage: ub,
                    ..
                },
            ) => {
                assert_eq!(ia, "msg_1");
                assert_eq!(ia, ib);
                assert_eq!(ua, ub);
                assert_eq!(*ua, [3, 100, 20, 7]);
                assert_eq!(model, "claude-x");
                assert_eq!(effort.as_deref(), Some("high"));
                assert!(*has_text);
                assert!(tool_uses.is_empty());
            }
            other => panic!("expected two assistants, got {other:?}"),
        }
    }

    #[test]
    fn synthetic_model_lines_are_skipped() {
        let l = assistant_line("msg_s", "<synthetic>", json!({}), json!([]));
        assert_eq!(parse_line(&l), Parsed::Skipped);
    }

    #[test]
    fn tool_results_meta_and_wrappers_are_not_human_prompts() {
        let tr = tool_result_line(
            json!({}),
            json!({"type": "tool_result", "tool_use_id": "toolu_1", "content": "fine"}),
        );
        assert!(matches!(
            event(parse_line(&tr)).kind,
            EventKind::ToolResults(_)
        ));
        let meta = user_line(json!({"isMeta": true}), json!("hello"));
        assert_eq!(parse_line(&meta), Parsed::Skipped);
        for wrapper in [
            "<command-name>/clear</command-name>",
            "<local-command-stdout>x</local-command-stdout>",
            "<system-reminder>be good</system-reminder>",
        ] {
            assert_eq!(
                parse_line(&user_line(json!({}), json!(wrapper))),
                Parsed::Skipped,
                "{wrapper}"
            );
        }
        let listed = user_line(
            json!({}),
            json!([{"type": "text", "text": "<system-reminder>x</system-reminder>"}]),
        );
        assert_eq!(parse_line(&listed), Parsed::Skipped);
        let human = user_line(json!({}), json!("please fix it"));
        assert_eq!(
            event(parse_line(&human)).kind,
            EventKind::HumanPrompt {
                interrupted_marker: false
            }
        );
    }

    #[test]
    fn origin_human_prompt_opens_a_turn() {
        let human = user_line(
            json!({"origin": {"kind": "human"}}),
            json!("<odd> but typed by a person"),
        );
        assert_eq!(
            event(parse_line(&human)).kind,
            EventKind::HumanPrompt {
                interrupted_marker: false
            }
        );
        let other = user_line(
            json!({"origin": {"kind": "coordinator"}}),
            json!("do the thing"),
        );
        assert_eq!(parse_line(&other), Parsed::Skipped);
    }

    #[test]
    fn interrupt_mark_is_found_in_text_and_in_tool_result() {
        for text in [
            "[Request interrupted by user]",
            "[Request interrupted by user for tool use]",
        ] {
            let l = user_line(json!({}), json!(text));
            assert_eq!(event(parse_line(&l)).kind, EventKind::Interrupt, "{text}");
        }
        let listed = user_line(
            json!({}),
            json!([{"type": "text", "text": "[Request interrupted by user for tool use]"}]),
        );
        assert_eq!(event(parse_line(&listed)).kind, EventKind::Interrupt);
        let tr = tool_result_line(
            json!({}),
            json!({"type": "tool_result", "tool_use_id": "toolu_9", "is_error": true,
                   "content": [{"type": "text", "text": "[Request interrupted by user for tool use]"}]}),
        );
        let r = results(parse_line(&tr));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].tool_use_id, "toolu_9");
        assert_eq!(r[0].outcome, "interrupted");
        assert_eq!(r[0].error_class, Some("interrupted"));
    }

    #[test]
    fn task_notification_from_queue_operation_and_user_record() {
        let body = "<task-notification>\n<task-id>bAAA</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n\
                    <output-file>/tmp/x</output-file>\n<status>failed</status>\n\
                    <summary>Background command \"make\" failed (exit code 3)</summary>\n</task-notification>";
        let want = EventKind::Notification {
            task_id: "bAAA".into(),
            tool_use_id: "toolu_1".into(),
            status: "failed".into(),
            exit_code: Some(3),
        };
        let mut q = base("queue-operation");
        q.insert("operation".into(), json!("enqueue"));
        q.insert("content".into(), json!(body));
        assert_eq!(event(parse_line(&line(Value::Object(q)))).kind, want);
        let u = user_line(
            json!({"origin": {"kind": "task-notification"}}),
            json!(body),
        );
        assert_eq!(event(parse_line(&u)).kind, want);
        let bare = user_line(json!({}), json!(body));
        assert_eq!(event(parse_line(&bare)).kind, want);
        let mut dq = base("queue-operation");
        dq.insert("operation".into(), json!("dequeue"));
        assert_eq!(parse_line(&line(Value::Object(dq))), Parsed::Skipped);
    }

    #[test]
    fn background_bash_and_async_agent_launches_are_recognised() {
        let l = assistant_line(
            "msg_b",
            "claude-x",
            json!({}),
            json!([
                {"type": "tool_use", "id": "toolu_b", "name": "Bash",
                 "input": {"command": "cargo build", "run_in_background": true, "timeout": 120000}},
                {"type": "tool_use", "id": "toolu_a", "name": "Agent",
                 "input": {"prompt": "x", "run_in_background": true}},
                {"type": "tool_use", "id": "toolu_f", "name": "Bash", "input": {"command": "ls"}}
            ]),
        );
        let EventKind::Assistant { tool_uses, .. } = event(parse_line(&l)).kind else {
            panic!("not an assistant");
        };
        assert_eq!(tool_uses.len(), 3);
        assert!(tool_uses[0].background);
        assert_eq!(tool_uses[0].timeout_ms, Some(120000));
        assert!(tool_uses[1].background);
        assert!(!tool_uses[2].background);
        assert_eq!(tool_uses[2].timeout_ms, None);

        let bg = tool_result_line(
            json!({"toolUseResult": {"backgroundTaskId": "bTASK"}}),
            json!({"type": "tool_result", "tool_use_id": "toolu_b", "content": "started"}),
        );
        let r = results(parse_line(&bg));
        assert_eq!(r[0].bg_task_id.as_deref(), Some("bTASK"));
        assert_eq!(r[0].outcome, "launched");
        assert!(!r[0].async_launched);

        let ag = tool_result_line(
            json!({"toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": "a0123456789abcdef",
                                     "resolvedModel": "claude-y", "totalDurationMs": 10}}),
            json!({"type": "tool_result", "tool_use_id": "toolu_a", "content": "launched"}),
        );
        let r = results(parse_line(&ag));
        assert!(r[0].async_launched);
        assert_eq!(r[0].agent_id.as_deref(), Some("a0123456789abcdef"));
        assert_eq!(r[0].resolved_model.as_deref(), Some("claude-y"));
        assert_eq!(r[0].outcome, "launched");
    }

    #[test]
    fn compact_boundary_becomes_a_marker() {
        let mut m = base("system");
        m.insert("subtype".into(), json!("compact_boundary"));
        m.insert(
            "compactMetadata".into(),
            json!({"trigger": "auto", "preTokens": 170000, "durationMs": 5000}),
        );
        assert_eq!(
            event(parse_line(&line(Value::Object(m)))).kind,
            EventKind::Compact {
                duration_ms: Some(5000)
            }
        );
        let mut other = base("system");
        other.insert("subtype".into(), json!("informational"));
        assert_eq!(parse_line(&line(Value::Object(other))), Parsed::Skipped);
    }

    #[test]
    fn malformed_line_counted_unknown_type_skipped() {
        assert_eq!(parse_line(b"{not json"), Parsed::Failed);
        assert_eq!(parse_line(b"[1,2]"), Parsed::Failed);
        assert_eq!(parse_line(b""), Parsed::Failed);
        let unknown = line(json!({"type": "zzz-new", "timestamp": TS}));
        assert_eq!(parse_line(&unknown), Parsed::Unknown);
        let no_ts = line(json!({"type": "user", "sessionId": "s", "message": {"content": "hi"}}));
        assert_eq!(parse_line(&no_ts), Parsed::Failed);
        for untimed in [
            "file-history-snapshot",
            "last-prompt",
            "atis-latch",
            "summary",
            "attachment",
            "progress",
        ] {
            assert_eq!(
                parse_line(&line(json!({"type": untimed}))),
                Parsed::Skipped,
                "{untimed}"
            );
        }
    }

    #[test]
    fn bash_command_keeps_program_and_hash_never_arguments() {
        let cmd = "cd /x && cargo test --flag SECRET-ARG";
        let l = assistant_line(
            "msg_c",
            "claude-x",
            json!({}),
            json!([
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": cmd}},
                {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "git   -C y status"}},
                {"type": "tool_use", "id": "t3", "name": "Bash", "input": {"command": "/usr/bin/python3 run.py a"}},
                {"type": "tool_use", "id": "t4", "name": "Bash", "input": {"command": "ls -la"}},
                {"type": "tool_use", "id": "t5", "name": "Bash", "input": {"command": "cd /only"}}
            ]),
        );
        let ev = event(parse_line(&l));
        let EventKind::Assistant { tool_uses, .. } = &ev.kind else {
            panic!("not an assistant")
        };
        assert_eq!(tool_uses[0].cmd_program.as_deref(), Some("cargo test"));
        assert_eq!(tool_uses[0].cmd_hash.as_deref(), Some(sha16(cmd).as_str()));
        assert_eq!(tool_uses[1].cmd_program.as_deref(), Some("git"));
        assert_eq!(tool_uses[2].cmd_program.as_deref(), Some("python3 run.py"));
        assert_eq!(tool_uses[3].cmd_program.as_deref(), Some("ls"));
        assert_eq!(tool_uses[4].cmd_program, None);
        let spaced = assistant_line(
            "msg_c",
            "claude-x",
            json!({}),
            json!([{"type": "tool_use", "id": "t1", "name": "Bash",
                    "input": {"command": "cd  /x &&   cargo test --flag\tSECRET-ARG"}}]),
        );
        let EventKind::Assistant {
            tool_uses: again, ..
        } = event(parse_line(&spaced)).kind
        else {
            panic!("not an assistant")
        };
        assert_eq!(again[0].cmd_hash, tool_uses[0].cmd_hash);
        let dump = format!("{ev:?}");
        assert!(
            !dump.contains("SECRET-ARG") && !dump.contains("--flag"),
            "{dump}"
        );
    }

    #[test]
    fn error_class_is_a_closed_token() {
        let classify = |text: &str, is_error: bool| {
            let l = tool_result_line(
                json!({}),
                json!({"type": "tool_result", "tool_use_id": "t", "is_error": is_error, "content": text}),
            );
            results(parse_line(&l)).remove(0)
        };
        let r = classify("Exit code 2\nboom", true);
        assert_eq!(
            (r.error_class, r.exit_code, r.outcome),
            (Some("exit_nonzero"), Some(2), "error")
        );
        let r = classify("Exit code 75\nbusy", true);
        assert_eq!((r.error_class, r.exit_code), (Some("exit_75"), Some(75)));
        assert_eq!(
            classify("Command timed out after 120s", true).error_class,
            Some("timeout")
        );
        for denial in [
            "The user doesn't want to proceed with this tool use.",
            "User rejected tool use",
            "the user declined",
        ] {
            assert_eq!(
                classify(denial, true).error_class,
                Some("permission_denied"),
                "{denial}"
            );
        }
        assert_eq!(
            classify("PreToolUse:Bash hook blocked this", true).error_class,
            Some("hook_block")
        );
        assert_eq!(
            classify("String to replace not found in file.", true).error_class,
            Some("edit_not_found")
        );
        assert_eq!(
            classify("something odd", true).error_class,
            Some("tool_error")
        );
        let ok = classify("Exit code 2 mentioned in prose", false);
        assert_eq!((ok.error_class, ok.outcome), (None, "ok"));
        for class in [
            "exit_nonzero",
            "exit_75",
            "timeout",
            "interrupted",
            "permission_denied",
            "hook_block",
            "edit_not_found",
            "tool_error",
        ] {
            assert!(ERROR_CLASSES.contains(&class), "{class}");
        }
        assert_eq!(ERROR_CLASSES.len(), 8);
    }

    /// The fields a stripped line legitimately changes: the cwd is rewritten onto the fixture root, paths
    /// become relative, and the hashes of text that is no longer there cannot survive. Counts do.
    fn normalised(p: Parsed) -> Parsed {
        let Parsed::Event(mut e) = p else { return p };
        e.cwd = None;
        if let EventKind::Assistant { tool_uses, .. } = &mut e.kind {
            for t in tool_uses {
                t.cmd_hash = None;
                for f in &mut t.files {
                    f.clear();
                }
                for ed in &mut t.edits {
                    *ed = (String::new(), String::new(), String::new());
                }
                for rd in &mut t.reads {
                    rd.0.clear();
                }
            }
        }
        Parsed::Event(e)
    }

    #[test]
    fn strip_to_structure_removes_text_and_preserves_derivation() {
        let cwd = format!("/home/{SENTINEL}/proj");
        let with_cwd = |mut v: Value| {
            v["cwd"] = json!(cwd);
            v
        };
        let mut notif = base("queue-operation");
        notif.insert("operation".into(), json!("enqueue"));
        notif.insert(
            "content".into(),
            json!(format!(
                "<task-notification>\n<task-id>bQ</task-id>\n<tool-use-id>toolu_n</tool-use-id>\n\
                 <status>completed</status>\n<summary>{SENTINEL} (exit code 4)</summary>\n</task-notification>"
            )),
        );
        let lines: Vec<Vec<u8>> = vec![
            line(with_cwd(json!({
                "type": "assistant", "timestamp": TS, "sessionId": "s", "uuid": "u", "isSidechain": true,
                "agentId": "a1", "entrypoint": "cli", "effort": "low",
                "message": {"id": "m1", "model": "claude-x", "role": "assistant",
                    "usage": {"input_tokens": 1, "output_tokens": 2, "cache_read_input_tokens": 3,
                              "cache_creation_input_tokens": 4, "note": SENTINEL},
                    "content": [
                        {"type": "thinking", "thinking": SENTINEL, "signature": SENTINEL},
                        {"type": "text", "text": SENTINEL},
                        {"type": "tool_use", "id": "tb", "name": "Bash",
                         "input": {"command": format!("cargo test {SENTINEL}"), "run_in_background": true,
                                   "timeout": 5, "description": SENTINEL}},
                        {"type": "tool_use", "id": "te", "name": "Edit",
                         "input": {"file_path": format!("{cwd}/src/a.rs"), "old_string": SENTINEL,
                                   "new_string": SENTINEL}},
                        {"type": "tool_use", "id": "tr", "name": "Read",
                         "input": {"file_path": format!("{cwd}/src/b.rs"), "offset": 40, "limit": 5}}
                    ]}
            }))),
            line(with_cwd(json!({
                "type": "user", "timestamp": TS, "sessionId": "s", "uuid": "u2",
                "toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": "a9",
                                  "resolvedModel": "claude-z", "stdout": SENTINEL},
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tb", "is_error": true,
                     "content": format!("Exit code 1\n{SENTINEL}")},
                ]}
            }))),
            line(with_cwd(json!({
                "type": "user", "timestamp": TS, "sessionId": "s", "uuid": "u3",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "te", "is_error": true,
                     "content": [{"type": "text", "text": format!("The user doesn't want to proceed {SENTINEL}")}]},
                ]}
            }))),
            line(with_cwd(json!({
                "type": "user", "timestamp": TS, "sessionId": "s", "uuid": "u4",
                "origin": {"kind": "human"},
                "message": {"role": "user", "content": format!("fix {SENTINEL} please")}
            }))),
            line(with_cwd(json!({
                "type": "user", "timestamp": TS, "sessionId": "s", "uuid": "u5",
                "message": {"role": "user", "content": format!("<system-reminder>{SENTINEL}</system-reminder>")}
            }))),
            line(with_cwd(json!({
                "type": "user", "timestamp": TS, "sessionId": "s", "uuid": "u6",
                "message": {"role": "user", "content": [
                    {"type": "text", "text": format!("[Request interrupted by user for tool use] {SENTINEL}")}]}
            }))),
            line(Value::Object(notif)),
            line(with_cwd(json!({
                "type": "system", "subtype": "compact_boundary", "timestamp": TS, "sessionId": "s",
                "content": SENTINEL, "compactMetadata": {"trigger": "auto", "preTokens": 9, "durationMs": 77}
            }))),
        ];
        for l in &lines {
            let stripped = strip_to_structure(l, "/fx").expect("a JSON object line strips");
            assert!(!stripped.contains(SENTINEL), "{stripped}");
            assert_eq!(
                normalised(parse_line(stripped.as_bytes())),
                normalised(parse_line(l)),
                "{stripped}"
            );
        }
        // The cwd is rewritten onto the fixture root.
        let first = strip_to_structure(&lines[0], "/fx").unwrap();
        assert_eq!(
            event(parse_line(first.as_bytes())).cwd.as_deref(),
            Some("/fx")
        );
        // A stateful stripper keeps the path below the first record's cwd, and file paths relative.
        let mut s = Stripper::new("/fx");
        let a = s
            .strip(&line(
                json!({"type": "user", "timestamp": TS, "sessionId": "s", "cwd": "C:\\p\\proj",
                "message": {"content": "x"}}),
            ))
            .unwrap();
        let b = s
            .strip(&line(json!({"type": "assistant", "timestamp": TS, "sessionId": "s", "cwd": "c:\\p\\proj\\sub",
                "message": {"id": "m", "model": "x", "content": [
                    {"type": "tool_use", "id": "t", "name": "Read",
                     "input": {"file_path": "C:\\p\\proj\\src\\a.rs", "offset": 3}}]}})))
            .unwrap();
        assert_eq!(event(parse_line(a.as_bytes())).cwd.as_deref(), Some("/fx"));
        assert_eq!(
            event(parse_line(b.as_bytes())).cwd.as_deref(),
            Some("/fx/sub")
        );
        let EventKind::Assistant { tool_uses, .. } = event(parse_line(b.as_bytes())).kind else {
            panic!("not an assistant")
        };
        assert_eq!(tool_uses[0].reads, vec![("src/a.rs".to_string(), 3)]);
        // Unknown types keep only their identity.
        let unk = strip_to_structure(
            &line(json!({"type": "zzz", "timestamp": TS, "sessionId": "s", "uuid": "u", "secret": SENTINEL})),
            "/fx",
        )
        .unwrap();
        let v: Value = serde_json::from_str(&unk).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 4);
        assert_eq!(strip_to_structure(b"{broken", "/fx"), None);
    }
}
