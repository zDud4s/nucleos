//! One line of a Claude Code transcript, as the fleet shows it. Pure: no IO, no clock.
//!
//! Third-party, undocumented format (spec §4 "Tolerância"): everything is read through
//! `serde_json::Value`, so a missing field degrades instead of failing the line.

// This is the file's FINAL production `use` block (later tasks add none).
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::fleet::model::{TermKind, TermLine, Timestamp, TranscriptEntry, collapse_ws};
use crate::fleet::source::Usage;

/// Longest `detail` (chars) an entry carries; a tool input or result past this is cut.
const DETAIL_CHARS: usize = 16 * 1024;

/// Whether a conversation record leaves the turn open or closed (spec §3, dead roots).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnMark {
    /// A final answer: the last block is text and `stop_reason` is `end_turn`.
    Closed,
    /// A pending tool call, a tool result nobody answered, a bare prompt, a thinking block.
    Open,
}

/// Everything one transcript line yields.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub entries: Vec<TranscriptEntry>,
    /// Context tokens (assistant records) and/or cost (`cost-state` records) carried by this line.
    pub usage: Option<Usage>,
    pub model: Option<String>,
    pub at: Option<Timestamp>,
    /// `None` for records that are not part of the conversation (attachments, titles, ...).
    pub mark: Option<TurnMark>,
}

/// `None` = the line is not JSON (malformed or half-written); the caller logs it once.
pub fn parse_line(line: &str) -> Option<Parsed> {
    let rec: Value = serde_json::from_str(line).ok()?;
    let mut out = Parsed {
        at: timestamp_of(&rec),
        ..Parsed::default()
    };
    match rec.get("type").and_then(Value::as_str) {
        Some("user") => user(&rec, &mut out),
        Some("assistant") => assistant(&rec, &mut out),
        Some("cost-state") => {
            out.usage = rec
                .get("totalCostUSD")
                .and_then(Value::as_f64)
                .map(|usd| Usage {
                    context_tokens: None,
                    cost_usd: Some(usd),
                });
        }
        _ => {}
    }
    Some(out)
}

pub fn term_line(entry: &TranscriptEntry) -> TermLine {
    TermLine::new(entry.kind, &entry.text)
}

/// Folds `new` into `into`, a later non-`None` field replacing an earlier one.
pub fn merge_usage(into: &mut Option<Usage>, new: Usage) {
    if let Some(current) = into.as_mut() {
        if new.context_tokens.is_some() {
            current.context_tokens = new.context_tokens;
        }
        if new.cost_usd.is_some() {
            current.cost_usd = new.cost_usd;
        }
        return;
    }
    *into = Some(new);
}

fn timestamp_of(rec: &Value) -> Option<Timestamp> {
    let raw = rec.get("timestamp")?.as_str()?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// A `TranscriptEntry.text` is uncut but still one line: whitespace runs and newlines become one
/// space through the shared helper (`TermLine::new` uses the same one; amendment 7).
fn entry(kind: TermKind, text: &str, detail: Option<String>) -> TranscriptEntry {
    TranscriptEntry {
        kind,
        text: collapse_ws(text),
        detail,
    }
}

fn user(rec: &Value, out: &mut Parsed) {
    if rec.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return;
    }
    out.mark = Some(TurnMark::Open);
    match rec.pointer("/message/content") {
        Some(Value::String(text)) => out.entries.push(entry(TermKind::Prompt, text, None)),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            out.entries.push(entry(TermKind::Prompt, text, None));
                        }
                    }
                    Some("tool_result") => out.entries.push(result_entry(rec, block)),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn assistant(rec: &Value, out: &mut Parsed) {
    let Some(msg) = rec.get("message") else {
        return;
    };
    out.mark = Some(TurnMark::Open);
    out.model = msg
        .get("model")
        .and_then(Value::as_str)
        // `<synthetic>` marks the CLI's own stand-in messages (API errors), not a model.
        .filter(|m| !m.starts_with('<'))
        .map(str::to_owned);
    if let Some(usage) = msg.get("usage") {
        let n = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
        let total =
            n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
        // The CLI's stand-in messages carry all-zero usage; reporting 0 would reset a real figure.
        if total > 0 {
            out.usage = Some(Usage {
                context_tokens: Some(total),
                cost_usd: None,
            });
        }
    }
    let end_turn = msg.get("stop_reason").and_then(Value::as_str) == Some("end_turn");
    let Some(blocks) = msg.get("content").and_then(Value::as_array) else {
        return;
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                if !text.trim().is_empty() {
                    let kind = if end_turn {
                        TermKind::Final
                    } else {
                        TermKind::Text
                    };
                    out.entries.push(entry(kind, text, None));
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                let detail = serde_json::to_string_pretty(&input).unwrap_or_default();
                out.entries.push(entry(
                    TermKind::Tool,
                    &tool_line(name, &input),
                    Some(cut(&detail, DETAIL_CHARS)),
                ));
            }
            // Reasoning never becomes a line (spec §3); other block kinds are not shown.
            _ => {}
        }
    }
    let last_is_text = blocks
        .last()
        .and_then(|b| b.get("type"))
        .and_then(Value::as_str)
        == Some("text");
    if end_turn && last_is_text {
        out.mark = Some(TurnMark::Closed);
    }
}

/// `Name main-argument`: the first of the usual argument keys the tool input has.
fn tool_line(name: &str, input: &Value) -> String {
    const KEYS: [&str; 8] = [
        "file_path",
        "path",
        "pattern",
        "command",
        "description",
        "query",
        "url",
        "skill",
    ];
    match KEYS
        .iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))
    {
        Some(arg) => format!("{name} {arg}"),
        None => name.to_owned(),
    }
}

fn result_entry(rec: &Value, block: &Value) -> TranscriptEntry {
    let body = result_text(block.get("content"));
    let detail = Some(cut(&body, DETAIL_CHARS));
    let structured = rec.get("toolUseResult");
    let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
    let interrupted = structured
        .and_then(|t| t.get("interrupted"))
        .and_then(Value::as_bool)
        == Some(true);
    if is_error {
        let first = body
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("error");
        return entry(TermKind::Error, first, detail);
    }
    if interrupted {
        return entry(TermKind::Error, "interrupted", detail);
    }
    entry(TermKind::Result, &summarize(structured, &body), detail)
}

/// The text of a `tool_result.content`: a string, or a list of blocks (images become `[image]`).
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| match b.get("text").and_then(Value::as_str) {
                Some(text) => text.to_owned(),
                None => "[image]".to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn count_lines(text: &str) -> u64 {
    text.lines().count() as u64
}

/// The one-line summary of a tool result (spec §3): `+18 −3`, `12 matches`, `N lines`.
/// Keyed by the SHAPE of `toolUseResult`, not by the tool name: the tool_use that names the tool
/// may be outside the 64 KB tail window being read.
fn summarize(structured: Option<&Value>, body: &str) -> String {
    if let Some(t) = structured.and_then(Value::as_object) {
        if let Some(patch) = t.get("structuredPatch").and_then(Value::as_array)
            && !patch.is_empty()
        {
            let (mut added, mut removed) = (0u64, 0u64);
            for hunk in patch {
                for line in hunk
                    .get("lines")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match line.as_str().and_then(|l| l.chars().next()) {
                        Some('+') => added += 1,
                        Some('-') => removed += 1,
                        _ => {}
                    }
                }
            }
            return format!("+{added} −{removed}");
        }
        if t.get("type").and_then(Value::as_str) == Some("create")
            && let Some(content) = t.get("content").and_then(Value::as_str)
        {
            return plural(count_lines(content), "line", "lines");
        }
        if let Some(n) = t
            .get("file")
            .and_then(|f| f.get("numLines"))
            .and_then(Value::as_u64)
        {
            return plural(n, "line", "lines");
        }
        // WebFetch: `{code, bytes, ...}` -> `200 · 41 KB` (rounded to the nearest KB, at least 1).
        if let (Some(code), Some(bytes)) = (
            t.get("code").and_then(Value::as_u64),
            t.get("bytes").and_then(Value::as_u64),
        ) {
            return format!("{code} · {} KB", ((bytes + 512) / 1024).max(1));
        }
        if t.get("mode").and_then(Value::as_str) == Some("content")
            && let Some(n) = t.get("numLines").and_then(Value::as_u64)
        {
            return plural(n, "match", "matches");
        }
        if let Some(n) = t.get("numFiles").and_then(Value::as_u64) {
            return plural(n, "file", "files");
        }
        if t.contains_key("stdout") || t.contains_key("stderr") {
            let text = format!(
                "{}\n{}",
                t.get("stdout").and_then(Value::as_str).unwrap_or(""),
                t.get("stderr").and_then(Value::as_str).unwrap_or("")
            );
            let n = text.lines().filter(|l| !l.trim().is_empty()).count() as u64;
            return if n == 0 {
                "no output".to_owned()
            } else {
                plural(n, "line", "lines")
            };
        }
        if let Some(n) = t.get("totalToolUseCount").and_then(Value::as_u64) {
            return format!("agent done · {}", plural(n, "tool", "tools"));
        }
    }
    match count_lines(body) {
        0 => "no output".to_owned(),
        n => plural(n, "line", "lines"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::claude_code::fixture as fx;
    use crate::fleet::limits;
    use serde_json::json;

    fn kinds(line: &str) -> Vec<(TermKind, String)> {
        parse_line(line)
            .expect("the line should parse")
            .entries
            .into_iter()
            .map(|e| (e.kind, e.text))
            .collect()
    }

    #[test]
    fn a_prompt_collapses_whitespace_and_a_meta_user_record_is_not_a_prompt() {
        assert_eq!(
            kinds(&fx::user_prompt(fx::TS, "hello   there\nworld")),
            vec![(TermKind::Prompt, "hello there world".to_owned())]
        );
        assert!(kinds(&fx::meta_user(fx::TS, "[Image: original 10x10]")).is_empty());
    }

    #[test]
    fn assistant_text_is_text_until_the_turn_ends_and_final_after() {
        let mid = fx::assistant(fx::TS, Some("tool_use"), fx::text_block("looking"), None);
        let last = fx::assistant(fx::TS, Some("end_turn"), fx::text_block("done"), None);
        assert_eq!(kinds(&mid), vec![(TermKind::Text, "looking".to_owned())]);
        assert_eq!(kinds(&last), vec![(TermKind::Final, "done".to_owned())]);
    }

    #[test]
    fn thinking_never_becomes_a_line() {
        let line = fx::assistant(fx::TS, Some("end_turn"), fx::thinking_block(), None);
        assert!(kinds(&line).is_empty());
    }

    #[test]
    fn a_tool_call_names_the_tool_and_its_main_argument_and_keeps_the_input_as_detail() {
        let line = fx::assistant(
            fx::TS,
            Some("tool_use"),
            fx::tool_block(
                "t1",
                "Read",
                json!({"file_path":"core/src/main.rs","limit":50}),
            ),
            None,
        );
        let parsed = parse_line(&line).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].kind, TermKind::Tool);
        assert_eq!(parsed.entries[0].text, "Read core/src/main.rs");
        assert!(
            parsed.entries[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("\"limit\": 50")
        );

        let no_arg = fx::assistant(
            fx::TS,
            Some("tool_use"),
            fx::tool_block("t2", "TaskList", json!({})),
            None,
        );
        assert_eq!(
            kinds(&no_arg),
            vec![(TermKind::Tool, "TaskList".to_owned())]
        );
    }

    #[test]
    fn an_entry_keeps_the_whole_text_and_the_term_line_cuts_it() {
        let long = "word ".repeat(200);
        let line = fx::assistant(fx::TS, Some("tool_use"), fx::text_block(&long), None);
        let entry = parse_line(&line).unwrap().entries.remove(0);
        assert!(entry.text.chars().count() > limits::LINE_CHARS);
        let term = term_line(&entry);
        assert!(term.text.chars().count() <= limits::LINE_CHARS + 1);
        assert!(term.text.ends_with('…'));
    }

    #[test]
    fn a_malformed_or_empty_line_is_not_a_record() {
        assert!(parse_line("{\"type\":\"us").is_none());
        assert!(parse_line("").is_none());
        // Valid JSON of an unknown record type is a record with nothing to show.
        assert!(
            parse_line("{\"type\":\"mode\"}")
                .unwrap()
                .entries
                .is_empty()
        );
    }

    fn result_of(tool_use_result: Option<serde_json::Value>, body: &str) -> (TermKind, String) {
        let line = match tool_use_result {
            Some(tur) => fx::tool_result(fx::TS, "t1", body, tur),
            // A record without `toolUseResult`: build it by hand.
            None => json!({
                "type":"user","timestamp":fx::TS,
                "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":body}]}
            })
            .to_string(),
        };
        let entry = parse_line(&line).unwrap().entries.remove(0);
        (entry.kind, entry.text)
    }

    #[test]
    fn tool_results_reduce_to_one_summary_line() {
        let cases = [
            // Edit / overwrite: counted from the structured patch.
            (
                json!({"structuredPatch":[{"lines":[" a","-b","+c","+d"]}]}),
                "+2 −1",
            ),
            (
                json!({"mode":"content","numFiles":0,"numLines":12}),
                "12 matches",
            ),
            (json!({"mode":"files_with_matches","numFiles":3}), "3 files"),
            (
                json!({"filenames":[],"numFiles":4,"truncated":false}),
                "4 files",
            ),
            (
                json!({"type":"text","file":{"numLines":75,"totalLines":900}}),
                "75 lines",
            ),
            (
                json!({"type":"create","content":"a\nb\nc\n","structuredPatch":[]}),
                "3 lines",
            ),
            (
                json!({"stdout":"a\r\nb","stderr":"","interrupted":false}),
                "2 lines",
            ),
            (
                json!({"stdout":"","stderr":"","interrupted":false}),
                "no output",
            ),
            (
                json!({"status":"completed","totalToolUseCount":7}),
                "agent done · 7 tools",
            ),
            // WebFetch (spec §3, `200 · 41 KB`): 41_984 bytes is exactly 41 KB.
            (
                json!({"bytes":41984,"code":200,"codeText":"OK","durationMs":310,"url":"https://example.com"}),
                "200 · 41 KB",
            ),
            (json!({"bytes":12,"code":404}), "404 · 1 KB"),
        ];
        for (tur, expected) in cases {
            assert_eq!(
                result_of(Some(tur.clone()), "ignored body"),
                (TermKind::Result, expected.to_owned()),
                "for {tur}"
            );
        }
    }

    #[test]
    fn a_result_without_a_structured_payload_falls_back_to_its_line_count() {
        assert_eq!(
            result_of(None, "x\ny\nz"),
            (TermKind::Result, "3 lines".to_owned())
        );
        assert_eq!(
            result_of(None, "only one"),
            (TermKind::Result, "1 line".to_owned())
        );
        assert_eq!(
            result_of(None, ""),
            (TermKind::Result, "no output".to_owned())
        );
    }

    #[test]
    fn an_interrupted_command_and_an_is_error_result_are_errors() {
        assert_eq!(
            result_of(
                Some(json!({"stdout":"","stderr":"","interrupted":true})),
                ""
            ),
            (TermKind::Error, "interrupted".to_owned())
        );
        let line = fx::tool_error(fx::TS, "t1", "Error: PreToolUse hook denied\nsecond line");
        let entry = parse_line(&line).unwrap().entries.remove(0);
        assert_eq!(entry.kind, TermKind::Error);
        assert_eq!(entry.text, "Error: PreToolUse hook denied");
        assert!(entry.detail.unwrap().contains("second line"));
    }

    #[test]
    fn the_result_detail_is_the_whole_body_and_a_list_body_joins_its_text_blocks() {
        let line = json!({
            "type":"user","timestamp":fx::TS,
            "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1",
                "content":[{"type":"text","text":"first"},{"type":"image"},{"type":"text","text":"second"}]}]}
        })
        .to_string();
        let entry = parse_line(&line).unwrap().entries.remove(0);
        assert_eq!(entry.detail.as_deref(), Some("first\n[image]\nsecond"));
        assert_eq!(entry.text, "3 lines");
    }

    #[test]
    fn an_assistant_record_carries_context_tokens_model_and_time() {
        let line = fx::assistant(
            fx::TS,
            Some("end_turn"),
            fx::text_block("x"),
            Some((10, 20, 300)),
        );
        let p = parse_line(&line).unwrap();
        assert_eq!(
            p.usage,
            Some(Usage {
                context_tokens: Some(330),
                cost_usd: None
            })
        );
        assert_eq!(p.model.as_deref(), Some("claude-test-1"));
        assert_eq!(p.at, Some("2026-10-20T10:00:00Z".parse().unwrap()));
    }

    #[test]
    fn zero_usage_and_synthetic_models_are_not_reported() {
        let line = fx::assistant(
            fx::TS,
            Some("end_turn"),
            fx::text_block("x"),
            Some((0, 0, 0)),
        );
        assert_eq!(parse_line(&line).unwrap().usage, None);
        let synthetic = line.replace("claude-test-1", "<synthetic>");
        assert_eq!(parse_line(&synthetic).unwrap().model, None);
    }

    #[test]
    fn a_cost_state_record_carries_the_session_cost() {
        let p = parse_line(&fx::cost_state(3.5)).unwrap();
        assert_eq!(
            p.usage,
            Some(Usage {
                context_tokens: None,
                cost_usd: Some(3.5)
            })
        );
        assert!(p.entries.is_empty());
    }

    #[test]
    fn merging_usage_keeps_the_latest_value_of_each_field() {
        let mut acc = None;
        merge_usage(
            &mut acc,
            Usage {
                context_tokens: Some(100),
                cost_usd: None,
            },
        );
        merge_usage(
            &mut acc,
            Usage {
                context_tokens: None,
                cost_usd: Some(2.0),
            },
        );
        merge_usage(
            &mut acc,
            Usage {
                context_tokens: Some(150),
                cost_usd: None,
            },
        );
        assert_eq!(
            acc,
            Some(Usage {
                context_tokens: Some(150),
                cost_usd: Some(2.0)
            })
        );
    }

    fn mark(line: &str) -> Option<TurnMark> {
        parse_line(line).unwrap().mark
    }

    #[test]
    fn only_a_final_text_answer_closes_the_turn() {
        assert_eq!(
            mark(&fx::assistant(
                fx::TS,
                Some("end_turn"),
                fx::text_block("done"),
                None
            )),
            Some(TurnMark::Closed)
        );
        // A text block followed by a tool call in the same message: not closed.
        assert_eq!(
            mark(&fx::assistant(
                fx::TS,
                Some("tool_use"),
                fx::text_block("let me check"),
                None
            )),
            Some(TurnMark::Open)
        );
        // A pending tool call.
        assert_eq!(
            mark(&fx::assistant(
                fx::TS,
                Some("tool_use"),
                fx::tool_block("t1", "Bash", json!({"command":"ls"})),
                None
            )),
            Some(TurnMark::Open)
        );
        // A stream cut before `stop_reason` was written (observed: null on some text records).
        assert_eq!(
            mark(&fx::assistant(
                fx::TS,
                None,
                fx::text_block("partial"),
                None
            )),
            Some(TurnMark::Open)
        );
        // A thinking block, even when its record says end_turn.
        assert_eq!(
            mark(&fx::assistant(
                fx::TS,
                Some("end_turn"),
                fx::thinking_block(),
                None
            )),
            Some(TurnMark::Open)
        );
    }

    #[test]
    fn prompts_and_unanswered_tool_results_leave_the_turn_open_and_noise_has_no_mark() {
        assert_eq!(mark(&fx::user_prompt(fx::TS, "hi")), Some(TurnMark::Open));
        assert_eq!(
            mark(&fx::tool_result(fx::TS, "t1", "ok", json!({}))),
            Some(TurnMark::Open)
        );
        assert_eq!(mark(&fx::meta_user(fx::TS, "ctx")), None);
        assert_eq!(mark("{\"type\":\"attachment\"}"), None);
        assert_eq!(mark(&fx::cost_state(1.0)), None);
    }
}
