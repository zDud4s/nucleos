//! Stripping transcript lines down to their durable structure.

use super::*;
use serde_json::{Map, Value, json};

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

/// The canonical stand-in for a `tool_result`'s content: the marks the classifier reads (the class's
/// own and the exit code, which every class keeps), or empty. A class that only the vocabulary can
/// give (`wrong_shell`) has no built-in mark, so it leaves none.
#[cfg_attr(not(test), allow(dead_code))]
fn result_marker(text: &str, is_error: bool) -> String {
    if text.contains(INTERRUPT_MARK) {
        return "[Request interrupted by user]".into();
    }
    if !is_error {
        return String::new();
    }
    let (class, code) = classify_error(text, &ParseVocab::empty());
    let mut marks: Vec<String> = Vec::new();
    match class {
        "timeout" => marks.push("Command timed out".into()),
        "permission_denied" => marks.push(DENIAL_MARKS[0].into()),
        "hook_block" => marks.push("PreToolUse hook".into()),
        "edit_not_found" => marks.push("String to replace not found".into()),
        // `exit_*` and `tool_error` keep only the exit code below; `wrong_shell` can only come from
        // the vocabulary, which a stripped line does not carry, so its mark is empty.
        _ => {}
    }
    if let Some(code) = code {
        marks.push(format!("Exit code {code}"));
    }
    marks.join("\n")
}
