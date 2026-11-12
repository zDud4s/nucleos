//! What a turn's CLI output settles into: the failure text, the settled reply, and the live
//! follower that applies `task_*` lines while the turn streams.

use super::*;

/// The error a failed turn records. Never empty: the owner sees a failed turn, so it has to say
/// why. The CLI's stderr (its tail, when long) if it wrote any, else a line naming the exit code.
pub(super) fn failure_text(stderr: &str, exit_code: Option<i32>) -> String {
    const TAIL_CHARS: usize = 2000;
    let stderr = stderr.trim();
    if stderr.is_empty() {
        return match exit_code {
            Some(code) => format!("turn failed with no error output (exit {code})"),
            None => "turn failed with no error output".to_string(),
        };
    }
    let count = stderr.chars().count();
    if count <= TAIL_CHARS {
        return stderr.to_string();
    }
    let tail: String = stderr.chars().skip(count - TAIL_CHARS).collect();
    format!("...{tail}")
}

/// How often the live task follower looks at the transcript.
const TASK_FOLLOW_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// Stops the task follower when the turn's future ends or is dropped (a `/cancel` aborts it).
pub(super) struct StopOnDrop(tokio::task::JoinHandle<()>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Applies `task_*` lines to the chat's task rows while the turn is still streaming, reading the
/// transcript the runner mirrors. Before this a notification that arrived mid-turn only landed at
/// `record_turn` at the turn's end, so AgentMap showed a finished subagent as working.
pub(super) fn follow_task_events(
    pool: SqlitePool,
    chat_id: String,
    run_id: i64,
    transcript: std::sync::Arc<std::sync::Mutex<String>>,
) -> StopOnDrop {
    StopOnDrop(tokio::spawn(async move {
        let mut applied = 0usize;
        loop {
            tokio::time::sleep(TASK_FOLLOW_EVERY).await;
            // Copied out only when a task event arrived, so a quiet stream costs one scan of its
            // new lines and not a clone of the whole transcript.
            let snapshot = {
                let Ok(stream) = transcript.lock() else {
                    return;
                };
                let (end, has_task_event) = crate::chats::tasks::scan_new_lines(&stream, applied);
                if has_task_event {
                    Some(stream[..end].to_string())
                } else {
                    applied = end;
                    None
                }
            };
            if let Some(stream) = snapshot {
                crate::chats::tasks::apply_live(&pool, &chat_id, run_id, &stream, &mut applied)
                    .await;
            }
        }
    }))
}

/// How a turn that came back from the CLI is recorded: its status and what it answered.
///
/// A `result` text is a completed turn. Without one, a turn the person interrupted is `cancelled`
/// and keeps what it had already said (`None` when it had said nothing); anything else answered
/// nothing, and the caller records it as the failure it is.
pub(super) fn settled_reply(stdout: &str) -> Option<(&'static str, Option<String>)> {
    if let Some(reply) = extract_reply(stdout) {
        return Some(("completed", Some(reply)));
    }
    if crate::execution::runner::interrupted_by_user(stdout) {
        let partial = crate::execution::runner::live_from_stream(stdout).text;
        return Some((
            "cancelled",
            Some(partial).filter(|text| !text.trim().is_empty()),
        ));
    }
    None
}
