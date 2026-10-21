//! A run's raw stdout as terminal lines. The live buffer behind `GET /runs/{id}/tail` holds the
//! runner's stdout verbatim: Claude's `stream-json` (one JSON record per line) or plain text.
//! This is deliberately NOT the Claude Code transcript parser (track B): it only has to be
//! tolerant, and everything it does not recognise is skipped.

use serde_json::Value;

use crate::fleet::limits::TAIL_BYTES;
use crate::fleet::model::{TermKind, TermLine};
use crate::fleet::source::{Offset, Usage};
use crate::state::RunTails;

/// The offset a finished run's read returns once its last line was delivered.
pub const FINAL_DONE: Offset = Offset(u64::MAX - 1);

#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub lines: Vec<TermLine>,
    pub usage: Option<Usage>,
}

const INPUT_KEYS: [&str; 6] = ["file_path", "command", "pattern", "path", "url", "description"];

/// Terminal lines for complete stdout lines. Reasoning never becomes a line; a tool result is one
/// summary line (`N lines`); a non-JSON line is plain text.
pub fn parse_lines(raw: &[String]) -> Parsed {
    let mut parsed = Parsed::default();
    for line in raw {
        match serde_json::from_str::<Value>(line) {
            Ok(record @ Value::Object(_)) => from_record(&record, &mut parsed),
            Ok(_) => {}
            Err(_) => parsed.lines.push(TermLine::new(TermKind::Text, line)),
        }
    }
    parsed
}

fn from_record(record: &Value, parsed: &mut Parsed) {
    match record.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            if let Some(usage) = usage_of(record) {
                parsed.usage = Some(usage);
            }
            for block in content_blocks(record) {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str)
                            && !text.trim().is_empty()
                        {
                            parsed.lines.push(TermLine::new(TermKind::Text, text));
                        }
                    }
                    Some("tool_use") => parsed
                        .lines
                        .push(TermLine::new(TermKind::Tool, tool_summary(block))),
                    // `thinking` and anything newer: the model's reasoning never becomes a line.
                    _ => {}
                }
            }
        }
        Some("user") => {
            for block in content_blocks(record) {
                if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                    parsed.lines.push(tool_result_line(block));
                }
            }
        }
        _ => {}
    }
}

fn content_blocks(record: &Value) -> impl Iterator<Item = &Value> {
    record
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn tool_summary(block: &Value) -> String {
    let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
    let detail = block.get("input").and_then(|input| {
        INPUT_KEYS
            .iter()
            .find_map(|key| input.get(*key).and_then(Value::as_str))
    });
    match detail {
        Some(detail) => format!("{name} {detail}"),
        None => name.to_owned(),
    }
}

fn tool_result_line(block: &Value) -> TermLine {
    let text = match block.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    if block.get("is_error").and_then(Value::as_bool) == Some(true) {
        return TermLine::new(TermKind::Error, text);
    }
    let summary = match text.lines().count() {
        0 => "empty".to_owned(),
        1 => "1 line".to_owned(),
        n => format!("{n} lines"),
    };
    TermLine::new(TermKind::Result, summary)
}

fn usage_of(record: &Value) -> Option<Usage> {
    let usage = record.pointer("/message/usage")?;
    let part = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
    let total = part("input_tokens")
        + part("cache_read_input_tokens")
        + part("cache_creation_input_tokens");
    (total > 0).then_some(Usage {
        context_tokens: Some(total),
        cost_usd: None,
    })
}

/// The last line of a run that ended: `Final` when it completed, `Error` carrying the status
/// otherwise. `stdout_tail` is the end of `runs.stdout`; a Claude `result` record supplies the text.
pub fn final_line(status: &str, stdout_tail: &str) -> TermLine {
    if status == "completed" {
        let text = result_text(stdout_tail).unwrap_or_else(|| "completed".to_owned());
        TermLine::new(TermKind::Final, text)
    } else {
        TermLine::new(TermKind::Error, status)
    }
}

fn result_text(tail: &str) -> Option<String> {
    for line in tail.lines().rev() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) == Some("result")
            && let Some(text) = record.get("result").and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            return Some(text.to_owned());
        }
    }
    None
}

/// Complete lines of a live run's buffer from byte `from`, or `None` when this daemon holds no
/// buffer for the run (finished, or started by a previous daemon). A buffer shorter than `from`
/// is a retry attempt that replaced it, and a `from` that is not a char boundary of the current
/// text is stale for the same reason: both start over from the tail.
pub fn tail_lines(tails: &RunTails, run_id: i64, from: Offset) -> Option<(Vec<String>, Offset)> {
    let buffer = tails.lock().ok()?.get(&run_id).cloned()?;
    let text = buffer.lock().ok()?;
    // Every offset handed out here is just after a '\n'; one that is past the end or inside a
    // character belongs to a buffer that was replaced, so the read restarts from the tail.
    let stale = from == Offset::TAIL
        || from.0 > text.len() as u64
        || !text.is_char_boundary(from.0 as usize);
    let start = if stale {
        match tail_start(&text) {
            Some(start) => start,
            None => return Some((Vec::new(), Offset::TAIL)),
        }
    } else {
        from.0 as usize
    };
    let fresh = &text[start..];
    let Some(last) = fresh.rfind('\n') else {
        return Some((Vec::new(), Offset(start as u64)));
    };
    let lines = fresh[..last]
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect();
    Some((lines, Offset((start + last + 1) as u64)))
}

/// Where a first read starts: `TAIL_BYTES` from the end, moved to the start of the next line.
fn tail_start(text: &str) -> Option<usize> {
    let mut start = text.len().saturating_sub(TAIL_BYTES as usize);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    if start == 0 || text.as_bytes()[start - 1] == b'\n' {
        return Some(start);
    }
    text[start..].find('\n').map(|at| start + at + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn raw(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| (*l).to_owned()).collect()
    }

    #[test]
    fn assistant_text_and_tool_use_become_lines_and_thinking_does_not() {
        let parsed = parse_lines(&raw(&[
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"secret plan"},{"type":"text","text":"Reading the file\nnow"},{"type":"tool_use","name":"Read","input":{"file_path":"src/a.rs"}}]}}"#,
        ]));
        assert_eq!(
            parsed.lines,
            vec![
                TermLine::new(TermKind::Text, "Reading the file now"),
                TermLine::new(TermKind::Tool, "Read src/a.rs"),
            ]
        );
    }

    #[test]
    fn a_tool_result_is_one_summary_line_and_an_error_result_is_an_error_line() {
        let parsed = parse_lines(&raw(&[
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"a\nb\nc"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"one"}]}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"boom","is_error":true}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":""}]}}"#,
        ]));
        assert_eq!(
            parsed.lines,
            vec![
                TermLine::new(TermKind::Result, "3 lines"),
                TermLine::new(TermKind::Result, "1 line"),
                TermLine::new(TermKind::Error, "boom"),
                TermLine::new(TermKind::Result, "empty"),
            ]
        );
    }

    #[test]
    fn plain_text_is_kept_and_unknown_json_is_skipped() {
        let parsed = parse_lines(&raw(&[
            "compiling crate 1/9",
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"result","result":"done"}"#, // the final line is the daemon's, not the stream's
            "[1,2,3]",
            "{broken json",
        ]));
        assert_eq!(
            parsed.lines,
            vec![
                TermLine::new(TermKind::Text, "compiling crate 1/9"),
                TermLine::new(TermKind::Text, "{broken json"),
            ]
        );
    }

    #[test]
    fn usage_is_the_sum_of_the_context_the_message_read() {
        let parsed = parse_lines(&raw(&[
            r#"{"type":"assistant","message":{"content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":300,"cache_creation_input_tokens":5,"output_tokens":99}}}"#,
        ]));
        assert_eq!(
            parsed.usage,
            Some(Usage { context_tokens: Some(315), cost_usd: None })
        );
    }

    #[test]
    fn a_completed_run_ends_in_final_with_the_result_text() {
        let tail = "garbage half line\n{\"type\":\"assistant\"}\n{\"type\":\"result\",\"result\":\"All 12 tests pass\"}\n";
        assert_eq!(
            final_line("completed", tail),
            TermLine::new(TermKind::Final, "All 12 tests pass")
        );
        assert_eq!(
            final_line("completed", ""),
            TermLine::new(TermKind::Final, "completed")
        );
    }

    #[test]
    fn every_other_end_is_an_error_line_with_the_status() {
        for status in ["failed", "timed_out", "cancelled", "interrupted", "superseded"] {
            assert_eq!(
                final_line(status, r#"{"type":"result","result":"nope"}"#),
                TermLine::new(TermKind::Error, status),
                "{status}"
            );
        }
    }

    fn tails_with(run_id: i64, text: &str) -> (RunTails, Arc<Mutex<String>>) {
        let buffer = Arc::new(Mutex::new(text.to_owned()));
        let tails: RunTails = Default::default();
        tails.lock().unwrap().insert(run_id, buffer.clone());
        (tails, buffer)
    }

    #[test]
    fn a_live_buffer_is_read_in_complete_lines_from_an_offset() {
        let (tails, buffer) = tails_with(5, "one\ntwo\nthr");
        let (lines, offset) = tail_lines(&tails, 5, Offset::TAIL).unwrap();
        assert_eq!(lines, ["one", "two"]);
        assert_eq!(offset, Offset(8));

        buffer.lock().unwrap().push_str("ee\nfour\n");
        let (lines, offset) = tail_lines(&tails, 5, offset).unwrap();
        assert_eq!(lines, ["three", "four"]);
        assert_eq!(offset, Offset(19));
        assert_eq!(tail_lines(&tails, 5, offset).unwrap().0, Vec::<String>::new());
    }

    #[test]
    fn no_buffer_means_none() {
        let tails: RunTails = Default::default();
        assert!(tail_lines(&tails, 1, Offset::TAIL).is_none());
    }

    #[test]
    fn a_buffer_replaced_by_a_retry_restarts_from_its_tail() {
        let (tails, buffer) = tails_with(5, "a long first attempt\nwith two lines\n");
        let (_, offset) = tail_lines(&tails, 5, Offset::TAIL).unwrap();
        *buffer.lock().unwrap() = "fresh\n".to_owned();
        let (lines, offset) = tail_lines(&tails, 5, offset).unwrap();
        assert_eq!(lines, ["fresh"]);
        assert_eq!(offset, Offset(6));
    }

    #[test]
    fn an_offset_inside_a_multibyte_char_restarts_from_the_tail() {
        let (tails, _) = tails_with(5, "é\nnext\n");
        // Byte 1 is the middle of the two-byte 'é': no offset this module hands out, so the
        // buffer was replaced since. It is a restart, not "no buffer" (`None`) and not a panic.
        let (lines, offset) = tail_lines(&tails, 5, Offset(1)).unwrap();
        assert_eq!(lines, ["é", "next"]);
        assert_eq!(offset, Offset(8));
    }

    #[test]
    fn the_first_read_of_a_big_buffer_starts_on_a_line() {
        let text: String = (0..20_000).map(|i| format!("line-{i:05}\n")).collect();
        let (tails, _) = tails_with(5, &text);
        let (lines, offset) = tail_lines(&tails, 5, Offset::TAIL).unwrap();
        assert_eq!(offset, Offset(text.len() as u64));
        assert!(lines.iter().all(|l| l.len() == 10 && l.starts_with("line-")));
        assert_eq!(lines.last().unwrap(), "line-19999");
        assert!(lines.len() < 20_000);
    }

    #[test]
    fn a_multibyte_boundary_in_the_first_window_is_not_a_panic() {
        let text = "é".repeat(100_000) + "\nlast\n";
        let (tails, _) = tails_with(5, &text);
        let (lines, _) = tail_lines(&tails, 5, Offset::TAIL).unwrap();
        assert_eq!(lines.last().unwrap(), "last");
    }
}
