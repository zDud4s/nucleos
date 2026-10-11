//! The Claude CLI's stream-json both ways: the stdin lines a run is fed, the tools it advertises, its reply and how a turn failed.

use super::*;

/// One line of `--input-format stream-json` stdin that interrupts the running turn.
///
/// Measured in `.ai/spikes/2026-10-05-chat-cli-control.md` (a): the CLI ends the turn in about 30 ms
/// and the process and session survive. Every request carries an id of its own, because
/// `control_response` echoes it and two interrupts sharing one could not be told apart.
pub fn interrupt_line() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut line = serde_json::json!({
        "type": "control_request",
        "request_id": format!("nucleos-interrupt-{n}"),
        "request": { "subtype": "interrupt" },
    })
    .to_string();
    line.push('\n');
    line
}

/// One line of `--input-format stream-json` stdin that stops ONE task of the running process: a
/// subagent or a background shell, named by the CLI's own `task_id` (see `ToolCall::task_id`).
///
/// The CLI's `stop_task` control request (present in 2.1.280: it refuses a `task_id` that is not a
/// string). Unlike the interrupt, the turn that launched the task carries on; it is told the task
/// stopped, as it is when one ends on its own. Its own id per request, for the interrupt's reason.
pub fn stop_task_line(task_id: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut line = serde_json::json!({
        "type": "control_request",
        "request_id": format!("nucleos-stop-task-{n}"),
        "request": { "subtype": "stop_task", "task_id": task_id },
    })
    .to_string();
    line.push('\n');
    line
}

/// One line of `--input-format stream-json` stdin: a single user turn.
///
/// Measured against CLI 2.1.198, this shape is accepted and the run proceeds — the `init` event fires
/// and the process exits 0. Built through `serde_json` rather than `format!` because a turn is
/// delimited by a newline: a prompt containing one, or a quote, would otherwise arrive as two
/// half-parsed lines instead of the single instruction it is.
pub fn user_message_line(text: &str, images: &[Attachment]) -> String {
    // A plain string when there is nothing to carry, and that is not tidiness: the string form is
    // the one measured working, and every run in this daemon that is not a chat uses it. Rewriting
    // them all as arrays to make one new case uniform would change what is proven to make room for
    // what is not.
    //
    // An array when there is. The image comes first and the words after — the order the API
    // documents for a question about a picture, and the order a person types in.
    let content = match images.is_empty() {
        true => serde_json::Value::String(text.to_string()),
        false => {
            let mut blocks: Vec<serde_json::Value> = images
                .iter()
                .map(|image| {
                    serde_json::json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": image.media_type,
                            "data": image.data,
                        },
                    })
                })
                .collect();
            blocks.push(serde_json::json!({ "type": "text", "text": text }));
            serde_json::Value::Array(blocks)
        }
    };
    let mut line = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": content },
    })
    .to_string();
    line.push('\n');
    line
}

pub(super) fn advertised_tools_from_init(init: &serde_json::Value) -> Option<Vec<String>> {
    init.get("tools")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .map(|tool| tool.as_str().map(str::to_string))
        .collect()
}

pub(super) fn advertised_tools_violate(
    policy: ToolPolicy,
    advertised: Option<&[String]>,
) -> Option<String> {
    if policy == ToolPolicy::Unrestricted {
        return None;
    }
    let Some(advertised) = advertised else {
        return Some(format!(
            "ToolPolicy::{policy:?} could not be verified: CLI init event did not advertise tools"
        ));
    };

    let offending: Vec<&str> = match policy {
        ToolPolicy::Unrestricted => unreachable!("handled above"),
        ToolPolicy::None => advertised.iter().map(String::as_str).collect(),
        ToolPolicy::McpOnly => advertised
            .iter()
            .map(String::as_str)
            .filter(|name| {
                name.strip_prefix("mcp__nucleos__")
                    .is_none_or(|tool| tool.contains("__"))
            })
            .collect(),
    };

    (!offending.is_empty()).then(|| {
        format!(
            "ToolPolicy::{policy:?} violated by CLI-advertised tools: {}",
            offending.join(", ")
        )
    })
}

/// Whether a run whose stream is exhausted ever got the chance to verify its tool policy.
///
/// Separate from `advertised_tools_violate` because the two absences are different failures. An
/// `init` that omits `tools` is a CLI that answered the question badly; no `init` at all is a CLI
/// that was never asked, because the assertion only runs inside the `init` branch. Both must fail
/// closed under a restrictive policy: the assertion exists so that a CLI change cannot turn barrier
/// 1 of the triage model into a silent no-op, and renaming or dropping the event is exactly such a
/// change — the one the field-level check above cannot see.
///
/// This does not make the run unsafe on its own. `ToolPolicy::None` is enforced by the CLI's own
/// refusal (see the variant's doc comment), not by this check; what would be lost without it is the
/// evidence that the refusal took effect, which is the whole point of measuring it.
pub(super) fn policy_unverified_after_stream(
    policy: ToolPolicy,
    init_seen: bool,
) -> Option<String> {
    if policy == ToolPolicy::Unrestricted || init_seen {
        return None;
    }
    Some(format!(
        "ToolPolicy::{policy:?} could not be verified: CLI stream contained no system init event"
    ))
}

/// The final text of a `claude -p --output-format stream-json` run.
///
/// This lives at the núcleo↔CLI boundary because knowing the CLI's output format is this module's
/// job — every caller that needs the answer of a run needs the same parse, and a second copy of it
/// would drift the day the format does.
///
/// Each line is a JSON object; the reply is the last non-empty `result` string. `None` means there
/// was no such event, and callers fall back to the raw stream rather than lose the output.
pub fn extract_reply(stdout: &str) -> Option<String> {
    let mut reply: Option<String> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let text = match v.get("type").and_then(|t| t.as_str()) {
                Some("result") => v.get("result").and_then(|r| r.as_str()),
                Some("item.completed")
                    if v.get("item")
                        .and_then(|item| item.get("type"))
                        .and_then(|kind| kind.as_str())
                        == Some("agent_message") =>
                {
                    v.get("item")
                        .and_then(|item| item.get("text"))
                        .and_then(|text| text.as_str())
                }
                _ => None,
            };
            if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
                reply = Some(text.to_string());
            }
        }
    }
    reply
}

/// Whether a `claude -p --output-format stream-json` run ended on an API error that says nothing
/// about the work: the network, or the service being busy or down.
///
/// Read from the LAST `result` event and from nowhere else, beside `extract_reply` for the reason
/// that function gives: knowing the CLI's output format is this module's job. True only when that
/// event says `is_error: true` with `terminal_reason: "api_error"`, and the status the CLI got back
/// is one a second attempt can get past — none at all (nothing answered: DNS, a dropped
/// connection), 408, 429, or any 5xx, 529 "overloaded" among them. Any other 4xx is the request
/// itself being refused, which it will be again, and a turn that ended for any other reason
/// (`max_turns`, a hook) ended on something the work did.
///
/// Measured on job 26's review, run 900483, 2026-09-13: ten `api_retry` events, then a result line
/// with `terminal_reason: "api_error"`, `api_error_status: null` and "API Error: Can't reach the
/// API server — check your internet or DNS (ENOTFOUND)". Its `subtype` said `success`: only
/// `is_error` and `terminal_reason` told the truth, which is why neither `subtype` nor the exit
/// code is read.
pub fn failed_on_a_transient_api_error(stdout: &str) -> bool {
    let Some(result) = final_api_error(stdout) else {
        return false;
    };
    match result.get("api_error_status") {
        None | Some(serde_json::Value::Null) => true,
        Some(status) => status
            .as_u64()
            .is_some_and(|code| code == 408 || code == 429 || (500..=599).contains(&code)),
    }
}

/// Whether the run ended on the API rate-limiting it: the last `result` event is an `api_error`
/// with `api_error_status: 429`. Read exactly as [`failed_on_a_transient_api_error`] reads it, of
/// which this is the one status that says "this subscription, right now" rather than "the network"
/// or "the service" — the llm-router locks the subscription on it, and must not on a 529.
pub fn failed_on_rate_limit(stdout: &str) -> bool {
    final_api_error(stdout)
        .and_then(|result| result.get("api_error_status").and_then(|s| s.as_u64()))
        == Some(429)
}

/// The LAST `result` event, when it says `is_error: true` with `terminal_reason: "api_error"`.
pub(super) fn final_api_error(stdout: &str) -> Option<serde_json::Value> {
    let result = stdout.lines().rev().find_map(|line| {
        serde_json::from_str::<serde_json::Value>(line.trim())
            .ok()
            .filter(|event| event.get("type").and_then(|kind| kind.as_str()) == Some("result"))
    })?;
    (result.get("is_error").and_then(|flag| flag.as_bool()) == Some(true)
        && result
            .get("terminal_reason")
            .and_then(|reason| reason.as_str())
            == Some("api_error"))
    .then_some(result)
}

/// Job 26's review, run 900483, cut down to what the detector above reads: the first of its ten
/// retries, and its result line with the zeroed counters left out. Shared with `job.rs`, whose
/// tests seed a review that printed exactly this.
#[cfg(any(test, feature = "testkit"))]
pub const REVIEW_THAT_NEVER_REACHED_THE_API: &str = r#"{"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,"retry_delay_ms":614,"error_status":null,"error":"unknown","session_id":"f4a94b9c-f0fe-484b-9514-9fefa640a6b6"}
{"stop_reason":"stop_sequence","session_id":"f4a94b9c-f0fe-484b-9514-9fefa640a6b6","total_cost_usd":0,"terminal_reason":"api_error","is_error":true,"num_turns":1,"subtype":"success","api_error_status":null,"result":"API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)","type":"result","duration_ms":172362}"#;

/// The text of the user line the CLI writes into the stream when a turn is interrupted.
pub const INTERRUPT_MARKER: &str = "[Request interrupted by user]";

/// Whether a stream-json transcript ended on a turn the person interrupted, as opposed to one that
/// failed.
///
/// Both halves are needed and neither is enough alone: the CLI writes a user line carrying
/// [`INTERRUPT_MARKER`], and the LAST `result` event says `error_during_execution`. A failed turn
/// has the second without the first; an earlier interrupt followed by a later failure has the first
/// but a different last result.
pub fn interrupted_by_user(stdout: &str) -> bool {
    // The marker only counts when it sits AFTER the previous result line and before the last one,
    // so it is cleared whenever a result closes a turn.
    let mut marked = false;
    let mut last_result: Option<String> = None;
    let mut marked_before_last = false;
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match event.get("type").and_then(|kind| kind.as_str()) {
            Some("user") => {
                let said = event
                    .pointer("/message/content")
                    .and_then(|content| content.as_array())
                    .is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block.get("text").and_then(|text| text.as_str())
                                == Some(INTERRUPT_MARKER)
                        })
                    });
                marked = marked || said;
            }
            Some("result") => {
                marked_before_last = marked;
                marked = false;
                last_result = event
                    .get("subtype")
                    .and_then(|subtype| subtype.as_str())
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    marked_before_last && last_result.as_deref() == Some("error_during_execution")
}

/// A turn interrupted half way, as the CLI 2.1.280 wrote it in the spike: the partial answer, the
/// `control_response` to the request, the user line carrying the marker, and an
/// `error_during_execution` result. Shared with `assistant.rs`, whose fake answers with it.
#[cfg(any(test, feature = "testkit"))]
pub const INTERRUPTED_TURN: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"half an answer"}]}}
{"type":"control_response","response":{"subtype":"success","request_id":"nucleos-interrupt-1"}}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}
{"type":"result","subtype":"error_during_execution","is_error":true,"stop_reason":null,"num_turns":2,"errors":["[ede_diagnostic] result_type=user"],"total_cost_usd":0}"#;
