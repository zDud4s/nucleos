//! Serving turns and tracking background task output.

use super::*;

/// Serves one turn: down the conversation's living process when it has one, by starting one when it
/// does not, and by the one-shot path every turn used to take when it may not have one at all.
///
/// The answer shape is unchanged, so everything downstream reads one thing whichever door the turn
/// took; the deadlines are the turn's own: `ceiling` bounds the whole turn, `silence` how long it may
/// go without a new stream event.
///
/// `fresh_key` is the pool to mint the conversation's key from, when the turn is a rooted one. A
/// process is keyed when it is SPAWNED and a kept process keeps the key it was spawned with, so a
/// turn written down a kept process must not rotate `chat_tokens` (that rotation was the 401 on
/// turn two): the key is minted only on the path that starts a process.
///
/// `fingerprint` is [`spawn_fingerprint`] of the turn: a kept process is spoken to only when it was
/// spawned with the same one.
///
/// The turn path calls [`serve_turn_briefed`], which is this with the briefing a skipped kept
/// process leaves unread; this form, with none, is what the tests drive.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_turn(
    runner: &std::sync::Arc<dyn crate::execution::runner::CommandRunner>,
    request: crate::execution::runner::RunRequest,
    session_tx: tokio::sync::mpsc::UnboundedSender<String>,
    transcript: &std::sync::Arc<Mutex<String>>,
    chat_id: &str,
    deadlines: TurnDeadlines,
    may_live: bool,
    fresh_key: Option<&sqlx::SqlitePool>,
    fingerprint: &str,
) -> Result<std::io::Result<crate::execution::runner::RunOutcome>, tokio::time::error::Elapsed> {
    serve_turn_briefed(
        runner,
        request,
        session_tx,
        transcript,
        chat_id,
        deadlines,
        may_live,
        fresh_key,
        fingerprint,
        None,
    )
    .await
}

/// The briefing a turn left unread because a kept process was going to serve it (the block is a
/// per-spawn flag such a process would discard), read only if the turn has to spawn after all.
pub(super) type LateBriefing =
    std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>;

/// Serves one turn (see `serve_turn`), for a turn that may have skipped its briefing on the strength
/// of a kept process.
///
/// That process can turn out to be gone by the time the turn reaches it (`LiveTurn::NotWritten`), or
/// be taken away in between; the turn then spawns one, and that spawn is exactly what the briefing
/// is for. `late_briefing` is read at that point and appended the way `chat_run_request` appends it,
/// so a fallback spawn carries what a turn with no kept process would have carried.
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_turn_briefed(
    runner: &std::sync::Arc<dyn crate::execution::runner::CommandRunner>,
    mut request: crate::execution::runner::RunRequest,
    session_tx: tokio::sync::mpsc::UnboundedSender<String>,
    transcript: &std::sync::Arc<Mutex<String>>,
    chat_id: &str,
    deadlines: TurnDeadlines,
    may_live: bool,
    fresh_key: Option<&sqlx::SqlitePool>,
    fingerprint: &str,
    late_briefing: Option<LateBriefing>,
) -> Result<std::io::Result<crate::execution::runner::RunOutcome>, tokio::time::error::Elapsed> {
    if !may_live {
        request.progress_timeout = deadlines.silence;
        return tokio::time::timeout(
            deadlines.ceiling,
            runner.run_prompt(request, session_tx, std::sync::Arc::clone(transcript)),
        )
        .await;
    }

    if let Some(mut live) = take_live(chat_id) {
        // What the process has been calling this conversation since it started. Sent into the turn's
        // own recorder because that is what puts the session on this turn's row — the same job the
        // CLI's first `init` does for a process being started.
        //
        // A process that never said is one whose `init` never arrived, which is not a process worth
        // speaking to; it falls through and is dropped on the way out.
        let known = relock(&live.session_id).clone();
        // Everything a process fixed when it was spawned and cannot be told to change: where it is
        // standing, which mode it acts under, how it was configured (the fingerprint: model, effort,
        // denied tools, directories, instructions...), the rest of its command line — the tools it
        // may not reach for, the directories it may, who answers and how — and which conversation it
        // is having. All are resolved per turn — a fresh context abandons the session, a narrowing
        // removes reach — so a process that no longer matches this turn is not a process this turn
        // may be answered by. It falls through and is dropped, which stops it, and a new one is
        // started to the turn's own shape.
        let same_ground = live.cwd == request.cwd
            && live.permission == request.permission
            && live.shape == SpawnShape::of(&request)
            && live.spawned_with == fingerprint;
        let same_conversation = conversation_of(
            known.as_deref(),
            live.resumed.as_deref(),
            request.resume_session_id.as_deref(),
        );
        if let Some(session_id) = same_conversation.filter(|_| same_ground) {
            let _ = session_tx.send(session_id.clone());
            // Bound before the match, not inside its scrutinee: a temporary there would hold the
            // borrow of `live` through every arm, and one of them has to hand it back.
            //
            // The steering entry is open for exactly as long as the turn is being gathered, so a
            // line written into the process lands in THIS turn. It is dropped before the process
            // goes back to the registry.
            let began = std::time::Instant::now();
            let steer = SteerGuard::open(chat_id, &live.messages);
            // A kept process sent its `init` long ago, for some earlier turn, so the runner never
            // fires this turn's signal: it is fired here, BEFORE the line is written. After would
            // lose it to a turn cancelled mid-answer, pricing a sent prompt at zero; firing early
            // only over-marks a `NotWritten` turn, which errs toward the estimate.
            if let Some(request_tx) = request.request_tx.take() {
                let _ = request_tx.send(());
            }
            let served = tokio::time::timeout(
                deadlines.ceiling,
                live.turn(
                    &request.prompt,
                    &request.images,
                    transcript,
                    deadlines.silence,
                ),
            )
            .await;
            // An answer the CLI began on its own before this message, set aside by `turn`, leaves
            // the process HERE, whatever became of the turn: the process may be dropped below, or
            // taken by the next turn before anything recorded it, and either would lose it.
            keep_set_aside(chat_id, &mut live);
            match served {
                Ok(LiveTurn::Answered(outcome)) => {
                    drop(steer);
                    let stdout = transcript
                        .lock()
                        .map(|held| held.clone())
                        .unwrap_or_default();
                    let idle_for = idle_for_turn(&stdout, began.elapsed());
                    let gathered = gathered(outcome, stdout, session_id);
                    keep_live(chat_id, live, idle_for);
                    return Ok(Ok(gathered));
                }
                // It heard the turn and died before answering. Starting it again would re-run
                // whatever it had already done, so this is reported as the failure it is — with
                // whatever the process said on its way out, which is what this row's `stderr`
                // becomes and therefore what the conversation shows.
                Ok(LiveTurn::DiedMidTurn(why)) => {
                    return Ok(Err(std::io::Error::other(match why {
                        Some(why) => {
                            format!(
                                "the conversation's process ended in the middle of this turn: {why}"
                            )
                        }
                        None => {
                            "the conversation's process ended in the middle of this turn".to_owned()
                        }
                    })));
                }
                // Nothing was written, so as far as anything outside is concerned this turn has not
                // happened yet, and starting a process for it is safe.
                Ok(LiveTurn::NotWritten) => {}
                // Silent for the whole deadline: dropping `live` stops the process.
                Ok(LiveTurn::WentSilent(after)) => {
                    // Its line was written to a process already talking to a model.
                    let mut silent = went_silent(transcript, after, Some(session_id));
                    silent.model_requested = true;
                    return Ok(Ok(silent));
                }
                // Dropping `live` on the way out is what stops a process that stopped answering.
                Err(elapsed) => return Err(elapsed),
            }
        }
    }

    // A process is about to be spawned, so this is where its key is minted. The kept-process branch
    // above never reaches here with a turn it answered, and so never rotates the key.
    if let Some(pool) = fresh_key {
        chat_key::rekey(&mut request.env, pool, chat_id).await;
    }
    // Only a Claude turn ever skips its briefing (`briefing_wanted`), so the block is appended as
    // Claude's.
    if let Some(late) = late_briefing
        && let Some(block) = late.await
    {
        request.append_system_prompt = system_prompt_with_knowledge(
            "claude",
            request.append_system_prompt.take(),
            Some(&block),
        );
    }

    start_live_chat(
        runner,
        request,
        session_tx,
        transcript,
        chat_id,
        deadlines,
        fingerprint,
    )
    .await
}

/// The session a kept process is having, when it is the one `wanted` asks for.
///
/// `known` is what the process announced. A process warmed ahead of its first message has announced
/// nothing yet — the CLI says nothing until it is spoken to — so it is accepted on the session it was
/// spawned to resume (`resumed`) instead, which is the one thing that tells it apart from a process
/// whose `init` never arrived.
fn conversation_of(
    known: Option<&str>,
    resumed: Option<&str>,
    wanted: Option<&str>,
) -> Option<String> {
    match (known, resumed) {
        (Some(id), _) => (wanted == Some(id)).then(|| id.to_owned()),
        (None, Some(resumed)) => (wanted == Some(resumed)).then(|| resumed.to_owned()),
        (None, None) => None,
    }
}

/// Whether the conversation has a kept process of any kind.
pub(super) fn has_kept_process(chat_id: &str) -> bool {
    relock(&LIVE_CHATS).contains_key(chat_id)
}

/// Whether the process the conversation has kept would serve a turn with these facts: the same rules
/// [`serve_turn_briefed`] applies before it speaks down one, asked without taking it.
///
/// Used before the turn builds its request, to skip what a kept process would discard.
pub(super) fn kept_process_fits(
    chat_id: &str,
    fingerprint: &str,
    cwd: Option<&std::path::Path>,
    permission: crate::execution::runner::Permission,
    resume: Option<&str>,
) -> bool {
    let kept = relock(&LIVE_CHATS);
    kept.get(chat_id).is_some_and(|live| {
        let known = relock(&live.session_id).clone();
        live.cwd.as_deref() == cwd
            && live.permission == permission
            && live.spawned_with == fingerprint
            && conversation_of(known.as_deref(), live.resumed.as_deref(), resume).is_some()
    })
}

/// Spawns a conversation's process and returns it, with nothing asked of it yet.
///
/// The half of starting a conversation that does not depend on there being a turn to carry: a
/// process warmed ahead of its first message is made here, and so is the one a turn starts. The
/// request's prompt is the opening line; empty, the CLI is left waiting for the first one.
///
/// Must run inside the runtime: the process lives in tasks this spawns.
pub(super) fn spawn_live_process(
    runner: &std::sync::Arc<dyn crate::execution::runner::CommandRunner>,
    mut request: crate::execution::runner::RunRequest,
    session_tx: tokio::sync::mpsc::UnboundedSender<String>,
    chat_id: &str,
    fingerprint: &str,
) -> LiveChat {
    let (messages, incoming) =
        tokio::sync::mpsc::unbounded_channel::<crate::execution::runner::LaterTurn>();
    let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
    let (process_session_tx, mut process_session_rx) =
        tokio::sync::mpsc::unbounded_channel::<String>();

    // Read before the request is handed over, because that is what carries it, and kept so a later
    // turn wanting a different directory — or a different mode — can be told this process is the
    // wrong one. `resumed` is the session the process was told to continue, which is how a process
    // that has not spoken yet is still known for the conversation it was started for.
    let started_in = request.cwd.clone();
    let was_permission = request.permission;
    let shape = SpawnShape::of(&request);
    let resumed = request.resume_session_id.clone();

    // stdin IS the channel a later turn arrives on, so a process meant to serve more than one has to
    // take that door whether or not this turn carries anything that could only fit through it.
    request.steerable = true;
    // Never the runner's progress deadline here: its read loop spans the whole multi-turn process,
    // so the idle time between two turns would count as silence. Silence is enforced per event in
    // `LiveChat::gather` instead.
    request.progress_timeout = None;
    request.messages = Some(incoming);

    // The PROCESS's transcript, which is nobody's turn. What the window watches is built out of the
    // turn events instead, one turn at a time — a process serving five turns would otherwise hand
    // the fifth the other four.
    let process_transcript = std::sync::Arc::new(Mutex::new(String::new()));
    // Set once, by the supervisor, at the moment the process stops. A turn watching the stream
    // close reads it through the other end.
    let (why_stopped, stopped_because) = tokio::sync::watch::channel(None);
    let runner = std::sync::Arc::clone(runner);
    let named = chat_id.to_owned();
    let supervisor = tokio::spawn(async move {
        // The outcome describes the PROCESS — every turn's cost added together, its whole stream —
        // and each turn has already been recorded from its own `Ended` long before this resolves.
        // What is left is why it STOPPED, and that has nowhere else to go.
        //
        // Discarding it was the wrong shape of quiet. A conversation whose process cannot start
        // simply starts one per turn from then on and keeps working — at exactly the speed this
        // whole thing exists to improve, with nothing anywhere saying so. Slow for a knowable reason
        // is worth a line; slow for a reason nobody can find is what this avoids.
        let ended = runner
            .run_prompt_with_turns(
                request,
                process_session_tx,
                process_transcript,
                std::sync::Arc::new(Mutex::new(None)),
                Some(events_tx),
            )
            .await;
        match ended {
            // The last few lines rather than the whole stream: it is a process's stderr, it can be
            // long, and what says why something stopped is at the end of it.
            Ok(outcome) if outcome.exit_code != 0 => {
                let tail = outcome
                    .stderr
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" | ");
                tracing::warn!(
                    chat_id = %named,
                    exit_code = outcome.exit_code,
                    stderr = %tail,
                    "a conversation's process stopped badly; its turns will each start their own"
                );
                // Said to the turn as well as to the log. A turn cut off mid-answer shows this as
                // its own failure, and "the process ended" on its own is not something anybody can
                // act on — where the tool-policy barrier is what killed it, this names the tools.
                let _ = why_stopped.send(Some(tail));
            }
            Err(error) => {
                tracing::warn!(
                    chat_id = %named,
                    %error,
                    "a conversation's process could not be started; its turns will each start their own"
                );
                let _ = why_stopped.send(Some(error.to_string()));
            }
            Ok(_) => {}
        }
    });

    let session_id = std::sync::Arc::new(Mutex::new(None));
    {
        let known = std::sync::Arc::clone(&session_id);
        tokio::spawn(async move {
            if let Some(id) = process_session_rx.recv().await {
                *relock(&known) = Some(id.clone());
                // Onward to the turn's own recorder, which writes it to this turn's row and to the
                // chat's resumable session — the two writes that let the conversation survive the
                // process it is being answered by.
                let _ = session_tx.send(id);
            }
        });
    }

    LiveChat::new(
        messages,
        events,
        session_id,
        supervisor.abort_handle(),
        stopped_because,
        was_permission,
        started_in,
        shape,
        fingerprint.to_owned(),
        resumed,
    )
}

/// Starts a conversation's process, gathers the turn it was started with, and keeps it for the next.
pub(super) async fn start_live_chat(
    runner: &std::sync::Arc<dyn crate::execution::runner::CommandRunner>,
    request: crate::execution::runner::RunRequest,
    session_tx: tokio::sync::mpsc::UnboundedSender<String>,
    transcript: &std::sync::Arc<Mutex<String>>,
    chat_id: &str,
    deadlines: TurnDeadlines,
    fingerprint: &str,
) -> Result<std::io::Result<crate::execution::runner::RunOutcome>, tokio::time::error::Elapsed> {
    let began = std::time::Instant::now();
    let mut live = spawn_live_process(runner, request, session_tx, chat_id, fingerprint);
    let session_id = std::sync::Arc::clone(&live.session_id);

    // Open while the opening turn is gathered, exactly as in `serve_turn`.
    let steer = SteerGuard::open(chat_id, &live.messages);
    let served = tokio::time::timeout(
        deadlines.ceiling,
        live.opening(transcript, deadlines.silence),
    )
    .await;
    match served {
        Ok(LiveTurn::Answered(outcome)) => {
            drop(steer);
            let stdout = transcript
                .lock()
                .map(|held| held.clone())
                .unwrap_or_default();
            let idle_for = idle_for_turn(&stdout, began.elapsed());
            let known = relock(&session_id).clone().unwrap_or_default();
            let gathered = gathered(outcome, stdout, known);
            keep_live(chat_id, live, idle_for);
            Ok(Ok(gathered))
        }
        // A process that fell over without answering. `live` is dropped on the way out, which takes
        // down whatever is left of it.
        Ok(LiveTurn::NotWritten) => Ok(Err(std::io::Error::other(
            "the conversation's process ended without answering",
        ))),
        Ok(LiveTurn::DiedMidTurn(why)) => Ok(Err(std::io::Error::other(match why {
            Some(why) => format!("the conversation's process ended without answering: {why}"),
            None => "the conversation's process ended without answering".to_owned(),
        }))),
        Ok(LiveTurn::WentSilent(after)) => Ok(Ok(went_silent(
            transcript,
            after,
            relock(&session_id).clone(),
        ))),
        Err(elapsed) => Err(elapsed),
    }
}

/// A turn served by a living process, in the shape everything downstream already reads.
///
/// `exit_code: 0` because the turn ended with a `result` and the process is still standing — two
/// different facts, and a turn only arrives here having produced the first. `stderr` empty for the
/// same reason: it belongs to the process, which is still using it, and a turn that answered has no
/// failure to explain. A turn that did not answer never comes through here.
pub(super) fn gathered(
    outcome: crate::execution::runner::TurnOutcome,
    stdout: String,
    session_id: String,
) -> crate::execution::runner::RunOutcome {
    crate::execution::runner::RunOutcome {
        exit_code: 0,
        stdout,
        stderr: String::new(),
        session_id: Some(session_id),
        cost_usd: outcome.cost_usd,
        input_tokens: outcome.usage.input_tokens,
        output_tokens: outcome.usage.output_tokens,
        cache_read_tokens: outcome.usage.cache_read_tokens,
        cache_creation_tokens: outcome.usage.cache_creation_tokens,
        num_turns: outcome.usage.num_turns,
        compacted: outcome.compacted,
        // A turn that answered had its request read by a model, whichever door it took.
        model_requested: true,
    }
}

/// What a turn that went silent leaves behind: the runner's own shape for a run whose progress
/// deadline expired, so everything downstream reads one thing whichever door the turn took.
pub(super) fn went_silent(
    transcript: &std::sync::Arc<Mutex<String>>,
    after: std::time::Duration,
    session_id: Option<String>,
) -> crate::execution::runner::RunOutcome {
    crate::execution::runner::RunOutcome {
        exit_code: crate::execution::runner::PROGRESS_TIMEOUT_EXIT_CODE,
        stdout: transcript
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default(),
        stderr: format!(
            "nucleos: run went silent for {after:?}; progress deadline expired
"
        ),
        session_id,
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        num_turns: None,
        compacted: false,
        model_requested: false,
    }
}

/// Whether a stream line is the CLI's bookkeeping about background tasks rather than speech.
pub(super) fn is_task_bookkeeping(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return false;
    };
    value.get("type").and_then(serde_json::Value::as_str) == Some("system")
        && value
            .get("subtype")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|subtype| {
                subtype.starts_with("task_") || subtype == "background_tasks_changed"
            })
}

/// Keeps `running` equal to the background tasks the stream has started and not yet ended.
pub(super) fn track_background(line: &str, running: &mut HashSet<String>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return;
    };
    if value.get("type").and_then(serde_json::Value::as_str) != Some("system") {
        return;
    }
    let task_id = value.get("task_id").and_then(serde_json::Value::as_str);
    match value.get("subtype").and_then(serde_json::Value::as_str) {
        // An authoritative snapshot: whatever it lists is running and nothing else is.
        Some("background_tasks_changed") => {
            if let Some(tasks) = value.get("tasks").and_then(serde_json::Value::as_array) {
                running.clear();
                running.extend(
                    tasks
                        .iter()
                        .filter_map(|task| task.get("task_id").and_then(serde_json::Value::as_str))
                        .map(str::to_owned),
                );
            }
        }
        // A foreground task is part of its own turn; only a backgrounded one outlives it.
        Some("task_started")
            if value
                .get("is_backgrounded")
                .and_then(serde_json::Value::as_bool)
                == Some(true) =>
        {
            if let Some(id) = task_id {
                running.insert(id.to_owned());
            }
        }
        Some("task_notification") => {
            if let Some(id) = task_id {
                running.remove(id);
            }
        }
        Some("task_updated")
            if matches!(
                value
                    .pointer("/patch/status")
                    .and_then(serde_json::Value::as_str),
                Some("completed" | "failed" | "killed" | "stopped")
            ) =>
        {
            if let Some(id) = task_id {
                running.remove(id);
            }
        }
        _ => {}
    }
}

/// What a turn the assistant started on its own is recorded as having been asked: the task
/// notifications that woke it, one per line.
pub(super) fn spontaneous_prompt(
    carried: &std::collections::VecDeque<crate::execution::runner::TurnEvent>,
) -> String {
    let notices = spontaneous_notices(carried);
    if notices.is_empty() {
        "[the assistant continued on its own]".to_owned()
    } else {
        notices.join("\n")
    }
}

/// The text between `<name>` and `</name>`, if the body has that tag.
fn xml_tag<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&format!("</{name}>"))? + start;
    Some(body[start..end].trim())
}

/// Every piece of text a stream line carries: `message.content` as a string or as text blocks, or a
/// top-level `content` string.
fn line_text(value: &serde_json::Value) -> String {
    let content = value
        .pointer("/message/content")
        .or_else(|| value.get("content"));
    match content {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One notification in the text format the shell parses (`taskMarkerText` in `Chats.tsx`):
/// `[background task <id> completed|failed|killed]: <description> (exit code N)` or
/// `[subagent <name> completed|failed]`. PURE.
///
/// The CLI words the summary itself (`Background command "x" completed (exit code 2)`,
/// `Agent "name" completed`), so the status word it repeats is dropped from the description.
fn format_notice(task_id: &str, status: &str, summary: Option<&str>) -> String {
    let status = match status {
        "stopped" | "killed" => "killed",
        "failed" | "error" => "failed",
        _ => "completed",
    };
    let Some(summary) = summary.map(str::trim).filter(|summary| !summary.is_empty()) else {
        return format!("[background task {task_id} {status}]");
    };
    if let Some(rest) = summary.strip_prefix("Agent \"")
        && let Some((name, _)) = rest.split_once('"')
    {
        let word = if status == "completed" {
            "completed"
        } else {
            "failed"
        };
        return format!("[subagent {name} {word}]");
    }
    let (body, exit) = match summary.rfind("(exit code ") {
        Some(at) if summary.ends_with(')') => (summary[..at].trim_end(), Some(&summary[at..])),
        _ => (summary, None),
    };
    let body = [" completed", " failed", " was stopped", " killed"]
        .iter()
        .find_map(|word| body.strip_suffix(word))
        .unwrap_or(body);
    match exit {
        Some(exit) => format!("[background task {task_id} {status}]: {body} {exit}"),
        None => format!("[background task {task_id} {status}]: {body}"),
    }
}

/// The task notifications a process said while no turn held it, one formatted line per task, oldest
/// first.
///
/// The CLI hands a finished task to the model as a `user` line (the stream's echo of its queue)
/// whose text is a `<task-notification>` block, and only sometimes also as a `system` /
/// `task_notification` event; both are read, and a task named twice is listed once.
pub(super) fn spontaneous_notices(
    carried: &std::collections::VecDeque<crate::execution::runner::TurnEvent>,
) -> Vec<String> {
    notice_records(carried)
        .into_iter()
        .map(|(id, status, summary)| format_notice(&id, &status, summary.as_deref()))
        .collect()
}

/// The carried notices as `system` / `task_notification` stream lines, so `apply_line` can settle
/// the task rows a `user`-line notice names (it reads only `system` lines). PURE.
pub(super) fn notice_task_lines(
    carried: &std::collections::VecDeque<crate::execution::runner::TurnEvent>,
) -> Vec<String> {
    notice_records(carried)
        .into_iter()
        .filter(|(id, _, _)| id != "?")
        .map(|(id, status, _)| {
            serde_json::json!({"type": "system", "subtype": "task_notification",
                "task_id": id, "status": status})
            .to_string()
        })
        .collect()
}

/// `(task id, raw status, summary)` of each distinct notice, oldest first.
fn notice_records(
    carried: &std::collections::VecDeque<crate::execution::runner::TurnEvent>,
) -> Vec<(String, String, Option<String>)> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut notices = Vec::new();
    for event in carried {
        let crate::execution::runner::TurnEvent::Line(line) = event else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let kind = value.get("type").and_then(serde_json::Value::as_str);
        let (id, status, summary) = if kind == Some("system")
            && value.get("subtype").and_then(serde_json::Value::as_str) == Some("task_notification")
        {
            (
                value
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?")
                    .to_owned(),
                value
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("completed")
                    .to_owned(),
                value
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            )
        } else if matches!(kind, Some("user" | "queue-operation")) {
            let text = line_text(&value);
            if !text.contains("<task-notification>") {
                continue;
            }
            (
                xml_tag(&text, "task-id").unwrap_or("?").to_owned(),
                xml_tag(&text, "status").unwrap_or("completed").to_owned(),
                xml_tag(&text, "summary").map(str::to_owned),
            )
        } else {
            continue;
        };
        if seen.insert(id.clone()) {
            notices.push((id, status, summary));
        }
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runner::TurnEvent;

    fn carried_of(lines: &[&str]) -> std::collections::VecDeque<TurnEvent> {
        lines
            .iter()
            .map(|line| TurnEvent::Line((*line).to_owned()))
            .collect()
    }

    #[test]
    fn a_user_line_notification_becomes_the_text_the_shell_parses() {
        let body = "<task-notification>\n<task-id>bV2</task-id>\n<tool-use-id>toolu_b</tool-use-id>\n\
                    <status>completed</status>\n<summary>Background command \"cargo build\" completed (exit code 2)</summary>\n</task-notification>";
        let line =
            serde_json::json!({"type": "user", "message": {"role": "user", "content": body}})
                .to_string();
        assert_eq!(
            spontaneous_prompt(&carried_of(&[&line])),
            "[background task bV2 completed]: Background command \"cargo build\" (exit code 2)"
        );
    }

    #[test]
    fn several_notifications_join_with_newlines_and_a_task_named_twice_counts_once() {
        let block = |id: &str, status: &str, summary: &str| {
            serde_json::json!({"type": "user", "message": {"content": [{"type": "text",
                "text": format!("<task-notification>\n<task-id>{id}</task-id>\n<status>{status}</status>\n<summary>{summary}</summary>\n</task-notification>")}]}})
            .to_string()
        };
        let agent = block("a1", "completed", "Agent \"reviewer\" completed");
        let failed = block(
            "b2",
            "failed",
            "Background command \"make\" failed (exit code 3)",
        );
        let again = r#"{"type":"system","subtype":"task_notification","task_id":"b2","status":"failed","summary":"x"}"#;
        assert_eq!(
            spontaneous_prompt(&carried_of(&[&agent, &failed, again])),
            "[subagent reviewer completed]\n[background task b2 failed]: Background command \"make\" (exit code 3)"
        );
    }

    #[test]
    fn notice_task_lines_name_each_task_once_as_system_lines() {
        let user = serde_json::json!({"type": "user", "message": {"content":
            "<task-notification>
<task-id>bV2</task-id>
<status>failed</status>
</task-notification>"}})
        .to_string();
        let lines = notice_task_lines(&carried_of(&[&user, &user]));
        assert_eq!(lines.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["subtype"], "task_notification");
        assert_eq!(v["task_id"], "bV2");
        assert_eq!(v["status"], "failed");
    }

    #[test]
    fn a_system_notification_still_reads_and_nothing_still_falls_back() {
        let system = r#"{"type":"system","subtype":"task_notification","task_id":"t9","status":"stopped","summary":"Background command \"sleep\" was stopped"}"#;
        assert_eq!(
            spontaneous_prompt(&carried_of(&[system])),
            "[background task t9 killed]: Background command \"sleep\""
        );
        assert_eq!(
            spontaneous_prompt(&carried_of(&[r#"{"type":"assistant","message":{}}"#])),
            "[the assistant continued on its own]"
        );
    }
}
