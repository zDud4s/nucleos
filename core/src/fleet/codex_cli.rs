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
    clean_label, collapse_ws, Cursor, NodeKey, NodeState, TermKind, TermLine, Timestamp, TranscriptEntry, TranscriptPage,
};
use super::source::{AgentSource, LinesRead, NodeRef, Offset, Sighting, SourceError, SourceResult, Usage};
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
        Self { kind, text: text.into(), detail: None }
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
        TranscriptEntry { kind: self.kind, text: collapse_ws(&self.text), detail: self.detail }
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
            items.iter().filter_map(|item| str_of(item, "text")).collect::<Vec<_>>().join("\n")
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
        (Some("response_item"), "function_call") => function_call(payload),
        (Some("response_item"), "custom_tool_call") => custom_call(payload),
        (Some("response_item"), "function_call_output" | "custom_tool_call_output") => output(payload),
        _ => Vec::new(),
    }
}

fn assistant_message(payload: &Value) -> Vec<Mapped> {
    if str_of(payload, "role") != Some("assistant") {
        return Vec::new();
    }
    let text = content_text(payload.get("content"));
    if text.trim().is_empty() { Vec::new() } else { vec![Mapped::new(TermKind::Text, text)] }
}

fn user_item(payload: &Value) -> Vec<Mapped> {
    let Some(item) = payload.get("item") else { return Vec::new() };
    if str_of(item, "type") != Some("UserMessage") {
        return Vec::new();
    }
    prompt(&content_text(item.get("content")))
}

fn prompt(text: &str) -> Vec<Mapped> {
    if text.trim().is_empty() { Vec::new() } else { vec![Mapped::new(TermKind::Prompt, text)] }
}

/// Tool names whose calls say nothing a reader needs (`wait` polls a running script cell).
const QUIET_TOOLS: [&str; 1] = ["wait"];

/// The first non-blank line of `text`, trimmed.
fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("").to_owned()
}

/// The argument most likely to say what a tool call does, from a parsed `arguments` object.
fn argument_preview(arguments: &Value) -> Option<String> {
    ["command", "cmd", "path", "file_path", "query", "patch", "explanation"]
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

#[cfg(test)]
mod tests {
    // `super::*` brings in every name above (private `use` items included), so the tests import only
    // what the module itself does not: the `json!` macro.
    use super::*;
    use serde_json::json;

    /// One rollout line as the CLI writes it.
    fn rec(kind: &str, payload: Value) -> String {
        json!({"timestamp": "2026-10-20T10:00:00.000Z", "type": kind, "payload": payload}).to_string()
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
        let event = rec("event_msg", json!({"type": "user_message", "message": "fix the build"}));
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
        assert!(mapped(&rec("event_msg", json!({"type": "agent_message", "message": "x"}))).is_empty());
    }

    #[test]
    fn a_function_call_is_a_tool_line_with_the_command_as_preview() {
        let line = rec(
            "response_item",
            json!({"type": "function_call", "name": "shell_command", "call_id": "c1",
                   "arguments": "{\"command\":\"Get-ChildItem -Recurse\nSelect-Object -First 3\",\"workdir\":\"C:\\x\"}"}),
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
        assert_eq!(got[0].text, "exec const r = await tools.exec_command({cmd: \"ls\"});");
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
        let odd = rec("response_item", json!({"type": "function_call_output", "output": {"a": 1}}));
        assert_eq!(mapped(&odd)[0].text, "0 lines");
    }

    #[test]
    fn a_huge_tool_output_keeps_a_capped_detail() {
        let big = "x".repeat(DETAIL_CHARS * 2);
        let line = rec("response_item", json!({"type": "function_call_output", "output": big}));
        let detail = mapped(&line)[0].detail.clone().unwrap();
        assert_eq!(detail.chars().count(), DETAIL_CHARS + 1);
        assert!(detail.ends_with('…'));
    }
}
