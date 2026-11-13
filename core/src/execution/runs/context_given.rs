//! What a run was given, read off its own stream: the tools the CLI launched it with and the
//! context its first turn started from (`runs.tools_given`, `runs.initial_context`).
//!
//! Both are written once, when the run ends, from the whole transcript: the CLI prints the `init`
//! event first and the first assistant `usage` is the first usage there is, so a terminal read
//! sees exactly what a live reader would, and a failed write loses nothing a later one could not
//! repeat.

use serde_json::Value;
use sqlx::SqlitePool;

/// The input-side token count of the first assistant turn: `input_tokens +
/// cache_creation_input_tokens + cache_read_input_tokens`, a missing counter being 0. The same
/// definition as `scripts/leaf_initial_context.py`, which reads the first `assistant` event and
/// answers nothing when it carries no `message.usage`. `None` for a stream with no assistant turn.
pub fn initial_context_from_stream(stream: &str) -> Option<i64> {
    let first = stream.lines().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        (value.get("type").and_then(Value::as_str) == Some("assistant")).then_some(value)
    })?;
    let usage = first.get("message")?.get("usage")?.as_object()?;
    let counter = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
    Some(
        counter("input_tokens")
            + counter("cache_creation_input_tokens")
            + counter("cache_read_input_tokens"),
    )
}

/// The tool list of the stream's first `system`/`init` event, as the JSON `runs.tools_given`
/// stores. `None` when the stream has no such event or it carries no `tools` array.
pub fn tools_given_from_stream(stream: &str) -> Option<String> {
    stream.lines().find_map(|line| {
        let value: Value = serde_json::from_str(line).ok()?;
        let is_init = value.get("type").and_then(Value::as_str) == Some("system")
            && value.get("subtype").and_then(Value::as_str) == Some("init");
        let tools = value
            .get("tools")
            .filter(|tools| is_init && tools.is_array())?;
        serde_json::to_string(tools).ok()
    })
}

/// Records both on the run row, first value wins (`COALESCE`), whatever the run's status. Best
/// effort: the pair informs a later reading of the run and decides nothing now.
pub async fn record(pool: &SqlitePool, run_id: i64, stream: &str) {
    let initial_context = initial_context_from_stream(stream);
    let tools_given = tools_given_from_stream(stream);
    if initial_context.is_none() && tools_given.is_none() {
        return;
    }
    if let Err(error) = sqlx::query(
        "UPDATE runs SET initial_context = COALESCE(initial_context, ?),
                         tools_given = COALESCE(tools_given, ?)
         WHERE id = ?",
    )
    .bind(initial_context)
    .bind(tools_given)
    .bind(run_id)
    .execute(pool)
    .await
    {
        tracing::warn!(run_id, %error, "could not record what the run was given");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INIT: &str = r#"{"type":"system","subtype":"init","model":"m","tools":["Read","Edit","mcp__nucleos__node_open"]}"#;
    const FIRST: &str = r#"{"type":"assistant","message":{"usage":{"input_tokens":6,"cache_read_input_tokens":100,"cache_creation_input_tokens":12000,"output_tokens":3}}}"#;
    const LATER: &str = r#"{"type":"assistant","message":{"usage":{"input_tokens":9,"cache_read_input_tokens":50000,"cache_creation_input_tokens":0}}}"#;

    #[test]
    fn initial_context_is_the_first_assistant_usage_summed() {
        let stream = [INIT, FIRST, LATER].join("\n");
        assert_eq!(initial_context_from_stream(&stream), Some(12_106));
    }

    #[test]
    fn a_missing_counter_counts_as_zero() {
        let line = r#"{"type":"assistant","message":{"usage":{"input_tokens":7}}}"#;
        assert_eq!(initial_context_from_stream(line), Some(7));
    }

    #[test]
    fn a_stream_without_usage_has_no_initial_context() {
        assert_eq!(initial_context_from_stream(INIT), None);
        assert_eq!(initial_context_from_stream("not json\n"), None);
        let bare = r#"{"type":"assistant","message":{"content":[]}}"#;
        assert_eq!(initial_context_from_stream(bare), None);
        assert_eq!(initial_context_from_stream(""), None);
    }

    #[test]
    fn the_tools_come_from_the_init_event_only() {
        let stream = [FIRST, INIT].join("\n");
        assert_eq!(
            tools_given_from_stream(&stream).as_deref(),
            Some(r#"["Read","Edit","mcp__nucleos__node_open"]"#)
        );
        assert_eq!(tools_given_from_stream(FIRST), None);
        let no_tools = r#"{"type":"system","subtype":"init","model":"m"}"#;
        assert_eq!(tools_given_from_stream(no_tools), None);
    }
}
