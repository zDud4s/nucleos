//! Single facts read off a run's stream: task events, cache TTL, model, context fill, background tasks and compaction.

use super::*;

/// The CLI's word for a task's state, in the four the agent map draws, or `None` for one it does
/// not know. `stopped` is what `task_notification` says of a task that was killed.
pub(super) fn task_status(word: &str) -> Option<&'static str> {
    match word {
        "running" | "pending" => Some("running"),
        "completed" => Some("completed"),
        "failed" => Some("failed"),
        "killed" | "stopped" => Some("killed"),
        _ => None,
    }
}

/// Folds one `system` line's `task_*` event into the call that launched the task.
///
/// The shapes, read out of CLI 2.1.280's own emitters:
///
/// ```text
/// {"type":"system","subtype":"task_started","task_id":..,"tool_use_id":..,"description":..,
///  "subagent_type":..,"is_backgrounded":..,"task_type":"local_bash"|..}
/// {"type":"system","subtype":"task_progress","task_id":..,"tool_use_id":..,
///  "usage":{"total_tokens":..,"tool_uses":..,"duration_ms":..}}
/// {"type":"system","subtype":"task_updated","task_id":..,"patch":{"status":..,"end_time":<ms>}}
/// {"type":"system","subtype":"task_notification","task_id":..,"tool_use_id":..,
///  "status":"completed"|"failed"|"stopped","usage":..}
/// ```
///
/// `task_updated` names no tool call, so the task id is learned from whichever line named both.
pub(super) fn task_event(
    value: &serde_json::Value,
    stamp: Option<&str>,
    did: &mut [ToolCall],
    called: &[Option<String>],
    tasks: &mut std::collections::HashMap<String, usize>,
) {
    let subtype = value.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
    if !subtype.starts_with("task_") {
        return;
    }
    let Some(task) = value.get("task_id").and_then(|t| t.as_str()) else {
        return;
    };
    if let Some(index) = value
        .get("tool_use_id")
        .and_then(|id| id.as_str())
        .and_then(|id| called.iter().position(|made| made.as_deref() == Some(id)))
    {
        tasks.insert(task.to_string(), index);
    }
    let Some(&index) = tasks.get(task) else {
        return;
    };
    let call = &mut did[index];
    call.task_id.get_or_insert_with(|| task.to_string());
    if subtype == "task_started"
        && value.get("is_backgrounded").and_then(|b| b.as_bool()) == Some(true)
    {
        // An answer already paired with it only acknowledged the launch.
        if !call.background {
            call.finished_at = None;
        }
        call.background = true;
        call.status.get_or_insert_with(|| "running".to_string());
    }
    if let Some(total) = value
        .pointer("/usage/total_tokens")
        .and_then(serde_json::Value::as_u64)
    {
        call.tokens = Some(total);
    }
    let word = match subtype {
        "task_updated" => value.pointer("/patch/status"),
        "task_notification" => value.get("status"),
        _ => None,
    };
    let Some(status) = word.and_then(|w| w.as_str()).and_then(task_status) else {
        return;
    };
    if !call.background {
        return;
    }
    call.status = Some(status.to_string());
    if status != "running" && call.finished_at.is_none() {
        call.finished_at = value
            .pointer("/patch/end_time")
            .and_then(serde_json::Value::as_i64)
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|end| end.to_rfc3339())
            .or_else(|| Some(seen_at(stamp, Some(format!("task:{task}")))));
    }
}

/// How long this turn's prompt cache lives: `"1h"`, `"5m"`, or `None` when the stream never said.
///
/// Read off the `cache_creation` split the API reports on every usage block. The main agent's
/// messages decide it — a subagent's cache is its own — and the `result`'s usage is the fallback
/// for a stream with no such message. Any 1-hour write wins: that is the entry that outlives the
/// others.
pub fn cache_ttl_from_stream(stdout: &str) -> Option<&'static str> {
    fn ttl_of(usage: Option<&serde_json::Value>) -> Option<&'static str> {
        let split = usage?.get("cache_creation")?;
        let count = |key: &str| {
            split
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        if count("ephemeral_1h_input_tokens") > 0 {
            Some("1h")
        } else if count("ephemeral_5m_input_tokens") > 0 {
            Some("5m")
        } else {
            None
        }
    }
    let mut own: Option<&'static str> = None;
    let mut result: Option<&'static str> = None;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let found = match value.get("type").and_then(|t| t.as_str()) {
            Some("assistant")
                if value
                    .get("parent_tool_use_id")
                    .is_none_or(serde_json::Value::is_null) =>
            {
                &mut own
            }
            Some("result") => &mut result,
            _ => continue,
        };
        let usage = value
            .pointer("/message/usage")
            .or_else(|| value.get("usage"));
        match (ttl_of(usage), *found) {
            (Some("1h"), _) => *found = Some("1h"),
            (Some(ttl), None) => *found = Some(ttl),
            _ => {}
        }
    }
    own.or(result)
}

/// The model that answered this turn: the main agent's first message names it, and the `init`
/// line names what the CLI launched with when no message got that far.
pub fn model_from_stream(stdout: &str) -> Option<String> {
    let mut launched: Option<String> = None;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("assistant")
                if value
                    .get("parent_tool_use_id")
                    .is_none_or(serde_json::Value::is_null) =>
            {
                if let Some(model) = value
                    .pointer("/message/model")
                    .and_then(|m| m.as_str())
                    .filter(|m| !m.is_empty() && *m != "<synthetic>")
                {
                    return Some(model.to_string());
                }
            }
            Some("system")
                if launched.is_none()
                    && value.get("subtype").and_then(|s| s.as_str()) == Some("init") =>
            {
                launched = value
                    .get("model")
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    launched
}

/// Context occupied while a Claude `stream-json` run is still alive.
///
/// This is deliberately separate from `extract_usage`: assistant events describe current context
/// pressure during a run, while the final result describes aggregate usage after it has ended.
pub fn context_fill_from_line(line: &str, current: Option<i64>) -> Option<i64> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return current;
    };

    if value.get("type").and_then(serde_json::Value::as_str) == Some("assistant") {
        let usage = value
            .get("message")
            .and_then(|message| message.get("usage"));
        let input_tokens = usage
            .and_then(|usage| usage.get("input_tokens"))
            .and_then(serde_json::Value::as_i64);
        let cache_read_tokens = usage
            .and_then(|usage| usage.get("cache_read_input_tokens"))
            .and_then(serde_json::Value::as_i64);

        if let (Some(input_tokens), Some(cache_read_tokens)) = (input_tokens, cache_read_tokens) {
            // Cache-read tokens occupy the context window exactly as fresh input tokens do.
            return input_tokens.checked_add(cache_read_tokens).or(current);
        }
    }

    // `system`/`thinking_tokens` is deliberately NOT read here, although it is the only other line
    // carrying a token count. It measures what the model spent reasoning, not how full its window
    // is, and against a real stream the two are out by two orders of magnitude — 177 of thinking on
    // a turn carrying 48,733 of context. It is read by `live_from_stream` instead, under its own
    // name, where it says the thing it actually means.
    current
}

/// The fullest the stream ever got, rather than where it ended.
///
/// Written on top of `context_fill_from_line` instead of re-reading the JSON: there is one
/// definition of "what occupies the window" — `input_tokens + cache_read_input_tokens` — and it
/// lives there. Two copies would drift the day the CLI added a third counter, and drift silently.
///
/// The `None` in the call is deliberate: what is wanted is what THIS line says, not the running
/// total, so that the larger of the two can be chosen here.
pub fn context_peak_from_line(line: &str, current: Option<i64>) -> Option<i64> {
    match context_fill_from_line(line, None) {
        Some(fill) => Some(current.map_or(fill, |peak| peak.max(fill))),
        None => current,
    }
}

/// The environment this run's context window is expressed in, or nothing when it names none.
///
/// A function rather than two lines at the spawn site for one reason: the variable's NAME is the
/// part that fails silently. A typo in it leaves the CLI on its own default window, the daemon
/// still writes the number the window draws, and the only symptom is a conversation that compacts
/// at a size nobody asked for. Spelled once, here, where a test can read it back.
pub fn window_env(request: &RunRequest) -> Option<(&'static str, String)> {
    request
        .context_window
        .map(|window| ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", window.to_string()))
}

/// The environment that takes background tasks away from a run nothing can wake, or nothing for a
/// run something can.
///
/// A background task reports back by waking the session that started it, and a run with no later
/// turn has no session left to wake: once its turn ends the CLI exits and kills the task with it.
/// Measured on run 900473 (CLI 2.1.263): the model launched the gate's build in the background,
/// ended its turn with "I'll wait for the background gate build (task `b84qqcytz`) to finish before
/// continuing — it'll notify automatically when done", and the stream closed on that task being
/// `killed`. The run was recorded `completed`, its work half done and uncommitted. A `sleep 20`
/// launched the same way reproduces it in thirteen seconds.
///
/// With `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` the CLI takes `run_in_background` out of the
/// `Bash` schema, so the same request is refused as an unexpected parameter and nothing is left
/// running when the turn ends — measured with the same prompt against the same CLI.
///
/// Only a steerable run with somewhere its later turns come from keeps them: its process outlives
/// the turn, which is what a task's notification needs. A steerable run with no channel closes its
/// stdin after the opening turn, and for this purpose is a headless run.
///
/// A chat turn (`background_tasks`) keeps them too: a person is watching it, and returning `None`
/// also exempts it from the orphan check at turn end.
///
/// Set before `request.env` at the spawn site, like [`window_env`], so an explicit entry still wins.
pub fn background_env(request: &RunRequest) -> Option<(&'static str, &'static str)> {
    let can_be_woken = request.steerable && request.messages.is_some();
    (!can_be_woken && !request.background_tasks)
        .then_some(("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"))
}

/// PURE: the background tasks this stream reports killed after its last answer, each named once.
///
/// The signature, read off run 900473's own stream:
///
/// ```text
/// {"type":"result","subtype":"success","stop_reason":"end_turn",...}
/// {"type":"system","subtype":"task_updated","task_id":"b84qqcytz","patch":{"status":"killed",...}}
/// {"type":"system","subtype":"task_notification","task_id":"b84qqcytz","status":"stopped",...}
/// ```
///
/// After the `result` and never before it: a task stopped mid-turn was stopped by the model, which
/// is a decision; one killed after the last answer was killed by the process ending under it.
///
/// The second line behind [`background_env`], not the first. With background tasks taken away this
/// finds nothing, and it exists for the day it would: a CLI that renames the variable would
/// otherwise bring back a run recorded `completed` with its work abandoned, and nothing saying so.
pub fn orphaned_background_tasks(stdout: &str) -> Vec<String> {
    let mut answered = false;
    let mut orphaned: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("result") => {
                answered = true;
                continue;
            }
            Some("system") if answered => {}
            _ => continue,
        }
        let killed = match value.get("subtype").and_then(serde_json::Value::as_str) {
            Some("task_updated") => {
                value
                    .pointer("/patch/status")
                    .and_then(serde_json::Value::as_str)
                    == Some("killed")
            }
            Some("task_notification") => matches!(
                value.get("status").and_then(serde_json::Value::as_str),
                Some("stopped" | "killed")
            ),
            _ => false,
        };
        if killed
            && let Some(task) = value.get("task_id").and_then(serde_json::Value::as_str)
            && !orphaned.iter().any(|seen| seen == task)
        {
            orphaned.push(task.to_string());
        }
    }
    orphaned
}

/// Whether this line says the CLI compacted its own context.
///
/// The event, read off a real headless stream rather than inferred from the source:
///
/// ```text
/// {"type":"system","subtype":"status","status":"compacting","session_id":...}
/// {"type":"system","subtype":"status","status":null,"compact_result":"failed",
///  "compact_error":"too_few_groups","session_id":...}
/// ```
///
/// `status: "compacting"` is what is read, and the later `compact_result` deliberately is not. The
/// question this answers is "was the context summarised during this turn" — which is a thing the
/// transcript should say, because the alternative is a conversation that quietly got shorter — and
/// a compaction that began is the honest answer to it whether or not it finished. A `failed` result
/// means the context was left as it was; the turn still answered, and a mark that appeared and then
/// had to be taken back would be worse than one that says "this is where it summarised".
///
/// Sticky once true, like `larger` above: a turn can compact and then go on for many more lines,
/// and a flag recomputed from the last line alone would report only whatever happened to come last.
pub fn compacted_from_line(line: &str, current: bool) -> bool {
    if current {
        return true;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return current;
    };
    value.get("type").and_then(serde_json::Value::as_str) == Some("system")
        && value.get("subtype").and_then(serde_json::Value::as_str) == Some("status")
        && value.get("status").and_then(serde_json::Value::as_str) == Some("compacting")
}

/// Usage reported by the final `result` event of a Claude `stream-json` transcript.
///
/// Missing fields stay unknown rather than becoming measured zeroes. `num_turns` belongs to the
/// result event itself; the token counts live under its `usage` object.
/// The processes kept alive between a conversation's turns, held so the kernel takes them down if
/// this daemon goes without getting the chance to.
///
/// A `Litter` and not a `TreeKiller`: the two are the same primitive with opposite flags, and this
/// is the one whose promise survives the daemon being killed rather than asked to stop.
pub(super) static KEPT_ALIVE: std::sync::LazyLock<crate::process_tree::Litter> =
    std::sync::LazyLock::new(crate::process_tree::Litter::new);
