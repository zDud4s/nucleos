//! Between-turn watching and spontaneous assistant turns.

use super::*;

/// How often a kept process is looked at between turns.
pub(super) const BETWEEN_TURNS_TICK: std::time::Duration = std::time::Duration::from_millis(200);

/// Hands out the tokens that say which reader owns a process.
pub(super) static NEXT_WATCHER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Starts the one reader of a kept process between turns, replacing any earlier one.
///
/// Without it nothing reads a kept process's stream while no turn is in flight, so when a background
/// task ends and the CLI answers on its own, those lines wait for the NEXT person's turn, who reads
/// them as the reply to their own question and is billed for them. The reader notices such an answer
/// beginning and gives it a turn of its own. A conversation with no kept process has nothing to read.
pub(super) fn watch_between_turns(state: &crate::state::AppState, chat_id: &str) {
    let token = NEXT_WATCHER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    {
        let mut kept = relock(&LIVE_CHATS);
        match kept.get_mut(chat_id) {
            Some(live) => live.watcher = token,
            None => return,
        }
    }
    let state = state.clone();
    let chat_id = chat_id.to_owned();
    tokio::spawn(async move {
        // How many of the process's carried events were already written to `chat_tasks`. Valid for
        // this reader's life: `carried` only grows while no turn holds the process, and a turn
        // taking the process ends this reader.
        let mut recorded = 0_usize;
        loop {
            tokio::time::sleep(BETWEEN_TURNS_TICK).await;
            // No lock is held across an await: the guard lives only in this block. An entry that is
            // gone (a turn took it) or owned by a newer reader ends this one.
            let (begun, task_lines, has_preempted) = {
                let mut kept = relock(&LIVE_CHATS);
                match kept.get_mut(&chat_id) {
                    Some(live) if live.watcher == token => {
                        let begun = live.drain_idle();
                        let fresh: Vec<String> = live
                            .carried
                            .iter()
                            .skip(recorded)
                            .filter_map(|event| match event {
                                crate::execution::runner::TurnEvent::Line(line)
                                    if is_task_bookkeeping(line) =>
                                {
                                    Some(line.clone())
                                }
                                _ => None,
                            })
                            .collect();
                        recorded = live.carried.len();
                        (begun, fresh, live.preempted.is_some())
                    }
                    _ => return,
                }
            };
            // A task that ends between turns is recorded now, not when a later turn reads the line.
            for line in &task_lines {
                crate::chats::tasks::apply_line(&state.pool, &chat_id, line).await;
            }
            // An answer a person's message interrupted was set aside whole by `LiveChat::turn`; it
            // is recorded at once as the turn of its own that it was.
            if has_preempted && let Some(slot) = ChatSlot::acquire(&chat_id) {
                let mut slot = slot;
                let taken = {
                    let mut kept = relock(&LIVE_CHATS);
                    kept.get_mut(&chat_id)
                        .filter(|live| live.watcher == token)
                        .and_then(|live| {
                            let session = live
                                .session_id
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let key = live.process_key();
                            live.preempted.take().map(|aside| (aside, key, session))
                        })
                };
                if let Some(((stdout, outcome), key, session)) = taken {
                    slot.drain_on_release(&state);
                    record_preempted(&state, &chat_id, key, session, stdout, outcome).await;
                }
                drop(slot);
            }
            if !begun {
                continue;
            }
            // A person's turn owns the slot, and its `gather` reads `carried` first.
            let Some(slot) = ChatSlot::acquire(&chat_id) else {
                continue;
            };
            let live = {
                let mut kept = relock(&LIVE_CHATS);
                let ours = kept.get(&chat_id).is_some_and(|live| live.watcher == token);
                if ours { kept.remove(&chat_id) } else { None }
            };
            let Some(live) = live else {
                return;
            };
            start_spontaneous_turn(&state, &chat_id, slot, live).await;
            return;
        }
    });
}

/// Writes the `origin = 'task'` run row a spontaneous answer is recorded under, and returns its id
/// and the mode it runs under. `None` when the process has no stamped mode or the row cannot be
/// written; the caller then stops the process rather than let an answer run unaccounted.
async fn open_task_run(
    state: &crate::state::AppState,
    chat_id: &str,
    prompt: &str,
    session_id: &Option<String>,
    process_key: usize,
) -> Option<(i64, &'static str)> {
    // The barrier comes from the PROCESS: the mode stamped on the last turn it served and the
    // strictest `read_untrusted` of every turn it served. Never the chat's newest row, which may be
    // a turn that was stopped before it stamped anything.
    let barrier = relock(&PROCESS_BARRIERS)
        .get(&process_key)
        .map(|held| (held.mode, held.served.clone()));
    let Some((mode, served)) = barrier else {
        tracing::warn!(
            chat_id = %chat_id,
            "the assistant's unprompted turn has no stamped permission mode to run under; its process was stopped"
        );
        return None;
    };
    // Fails closed: a served row that cannot be read or found counts as having read untrusted text.
    let mut read_untrusted = 0_i64;
    let mut readable = true;
    for served_id in &served {
        match sqlx::query_scalar::<_, i64>("SELECT read_untrusted FROM runs WHERE id = ?")
            .bind(served_id)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(Some(marked)) => read_untrusted = read_untrusted.max(marked),
            Ok(None) => read_untrusted = 1,
            Err(error) => {
                tracing::warn!(chat_id = %chat_id, %error, "could not read a served turn's barrier");
                readable = false;
                break;
            }
        }
    }
    let inserted = if readable {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, answered_by, origin,
                               read_untrusted, permission_mode, created_at)
             VALUES (?, 'running', 'assistant', ?, ?, 'cloud', 'task', ?, ?, ?)",
        )
        .bind(prompt)
        .bind(session_id)
        .bind(chat_id)
        .bind(read_untrusted)
        .bind(mode)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .map_err(|error| error.to_string())
    } else {
        Err("a served turn's barrier could not be read".to_owned())
    };
    match inserted {
        Ok(done) => {
            let id = done.last_insert_rowid();
            crate::chats::stamp_project(&state.pool, id, chat_id).await;
            Some((id, mode))
        }
        Err(error) => {
            tracing::warn!(
                chat_id = %chat_id,
                %error,
                "could not record the assistant's unprompted turn; its process was stopped"
            );
            None
        }
    }
}

/// Records, at once, an answer a person's message arrived in the middle of: `LiveChat::turn` set it
/// aside whole, so there is nothing left to gather, only a row to write it under.
async fn record_preempted(
    state: &crate::state::AppState,
    chat_id: &str,
    process_key: usize,
    session_id: Option<String>,
    stdout: String,
    outcome: crate::execution::runner::TurnOutcome,
) {
    let lines: std::collections::VecDeque<crate::execution::runner::TurnEvent> = stdout
        .lines()
        .map(|line| crate::execution::runner::TurnEvent::Line(line.to_owned()))
        .collect();
    let prompt = spontaneous_prompt(&lines);
    let Some((id, mode)) = open_task_run(state, chat_id, &prompt, &session_id, process_key).await
    else {
        return;
    };
    let completed_at = chrono::Utc::now().to_rfc3339();
    let known = session_id.unwrap_or_default();
    let o = gathered(outcome, stdout, known);
    note_served(process_key, chat_id, id, mode, &state.pool);
    record_spontaneous_answer(&state.pool, id, chat_id, &o, &completed_at).await;
}

/// Records an answer the CLI began on its own as a turn of its own, and gathers the rest of it.
///
/// A run with `origin = 'task'`, whose barrier is the PROCESS's (the mode stamped on the last turn it
/// served, the strictest `read_untrusted` of every turn it served), so it is never cleaner than the
/// turns it continues. A process with no stamped mode, or a row that cannot be written, stops the
/// process rather than let an answer run unaccounted, and whatever queued meanwhile is drained.
pub(super) async fn start_spontaneous_turn(
    state: &crate::state::AppState,
    chat_id: &str,
    slot: ChatSlot,
    mut live: LiveChat,
) {
    // Armed first, so every way out of this function and of the task below lets through whatever
    // queued while the slot was held: the slot's release drains, nothing here does.
    let mut slot = slot;
    slot.drain_on_release(state);
    let prompt = spontaneous_prompt(&live.carried);
    let session_id = live
        .session_id
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some((id, mode)) =
        open_task_run(state, chat_id, &prompt, &session_id, live.process_key()).await
    else {
        // The process goes first and the slot after it.
        drop(live);
        drop(slot);
        return;
    };

    // Published before the body is spawned, for the reason `spawn_assistant_turn` gives.
    let transcript = std::sync::Arc::new(Mutex::new(String::new()));
    state
        .run_tails
        .lock()
        .unwrap()
        .insert(id, std::sync::Arc::clone(&transcript));
    let deadlines = turn_deadlines(
        state.run_timeout,
        state.progress_timeout,
        crate::execution::runner::ToolPolicy::Unrestricted,
    );
    let pool = state.pool.clone();
    let after = state.clone();
    let chat = chat_id.to_owned();
    crate::execution::runs::spawn_registered(state, id, async move {
        let began = std::time::Instant::now();
        // Open while the answer is gathered, so Stop reaches this turn softly and a line a person
        // sends lands in it, exactly as in a person's own turn.
        let steer = SteerGuard::open(&chat, &live.messages);
        let served = tokio::time::timeout(
            deadlines.ceiling,
            live.gather(&transcript, deadlines.silence),
        )
        .await;
        drop(steer);
        let completed_at = chrono::Utc::now().to_rfc3339();
        match served {
            Ok(LiveTurn::Answered(outcome)) => {
                let stdout = transcript
                    .lock()
                    .map(|held| held.clone())
                    .unwrap_or_default();
                let idle_for = idle_after_spontaneous(live.idle_for, &stdout, began.elapsed());
                let known = live
                    .session_id
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .unwrap_or_default();
                let o = gathered(outcome, stdout, known);
                // The process has now served this turn too, under the same barrier.
                note_served(live.process_key(), &chat, id, mode, &pool);
                // Kept first, so the process is back for the next turn before the row says done.
                keep_live(&chat, live, idle_for);
                record_spontaneous_answer(&pool, id, &chat, &o, &completed_at).await;
            }
            Ok(LiveTurn::DiedMidTurn(why)) => {
                drop(live);
                let stderr = match why {
                    Some(why) => {
                        format!(
                            "the conversation's process ended in the middle of this turn: {why}"
                        )
                    }
                    None => {
                        "the conversation's process ended in the middle of this turn".to_owned()
                    }
                };
                close_spontaneous(&pool, id, "failed", &stderr, &completed_at).await;
            }
            Ok(LiveTurn::NotWritten) => {
                drop(live);
                close_spontaneous(
                    &pool,
                    id,
                    "failed",
                    "the conversation's process ended without answering",
                    &completed_at,
                )
                .await;
            }
            Ok(LiveTurn::WentSilent(quiet)) => {
                drop(live);
                let stderr =
                    format!("nucleos: run went silent for {quiet:?}; progress deadline expired");
                close_spontaneous(&pool, id, "timed_out", &stderr, &completed_at).await;
            }
            Err(_) => {
                drop(live);
                let stderr = "nucleos: run went silent; progress deadline expired";
                close_spontaneous(&pool, id, "timed_out", stderr, &completed_at).await;
            }
        }
        // Released, which drains what queued meanwhile in a task of its own (armed above).
        drop(slot);
        watch_between_turns(&after, &chat);
    });
}

/// Ends a spontaneous turn that produced no answer. Guarded on `running`: first writer wins.
pub(super) async fn close_spontaneous(
    pool: &SqlitePool,
    id: i64,
    status: &str,
    stderr: &str,
    at: &str,
) {
    let written = sqlx::query(
        "UPDATE runs SET status = ?, stderr = ?, completed_at = ? WHERE id = ? AND status = 'running'",
    )
    .bind(status)
    .bind(stderr)
    .bind(at)
    .bind(id)
    .execute(pool)
    .await;
    crate::execution::runs::warn_on_terminal_write_err(&written, id, status);
}

/// Writes a spontaneous turn's answer the way a person's turn writes its own, without sharing the
/// block: the numbers are the turn's, taken off the outcome the splitter built for it.
pub(super) async fn record_spontaneous_answer(
    pool: &SqlitePool,
    id: i64,
    chat_id: &str,
    o: &crate::execution::runner::RunOutcome,
    completed_at: &str,
) {
    let Some(reply) = extract_reply(&o.stdout) else {
        close_spontaneous(
            pool,
            id,
            "failed",
            "the assistant's unprompted turn ended without an answer",
            completed_at,
        )
        .await;
        return;
    };
    let stream = crate::execution::runner::live_from_stream(&o.stdout);
    let tools_used = serde_json::to_string(&stream.did).unwrap_or_else(|_| "[]".to_string());
    let thought = serde_json::to_string(&stream.thought).unwrap_or_else(|_| "[]".to_string());
    let model = crate::execution::runner::model_from_stream(&o.stdout);
    // The rows must exist before the turn stops being `running`, or a live task's next call finds no authority.
    crate::chats::tasks::record_turn(pool, chat_id, id, &o.stdout).await;
    let completed = sqlx::query(
        "UPDATE runs SET status = 'completed', exit_code = ?, stdout = ?, session_id = COALESCE(?, session_id), cost_usd = ?, input_tokens = ?, output_tokens = ?, cache_read_tokens = ?, cache_creation_tokens = ?, num_turns = ?, tools_used = ?, thought = ?, thought_tokens = ?, compacted = ?, model = COALESCE(?, model), completed_at = ? WHERE id = ? AND status = 'running'",
    )
    .bind(o.exit_code)
    .bind(&reply)
    .bind(&o.session_id)
    .bind(o.cost_usd)
    .bind(o.input_tokens)
    .bind(o.output_tokens)
    .bind(o.cache_read_tokens)
    .bind(o.cache_creation_tokens)
    .bind(o.num_turns)
    .bind(&tools_used)
    .bind(&thought)
    .bind(stream.thought_tokens)
    .bind(o.compacted)
    .bind(&model)
    .bind(completed_at)
    .bind(id)
    .execute(pool)
    .await;
    crate::execution::runs::warn_on_terminal_write_err(&completed, id, "completed");
    if let Some(session_id) = o.session_id.as_deref() {
        // The same rule as a person's turn: a session that read third-party text is not resumable.
        match crate::execution::runs::read_untrusted_context(pool, id).await {
            Ok(false) => {
                let _ = upsert_session(pool, chat_id, session_id, completed_at).await;
            }
            _ => {
                let _ = forget_session(pool, chat_id).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistants::NoAssistants;
    use crate::auth::Token;
    use crate::execution::runner::{FakeCommandRunner, LaterTurn, TurnEvent, TurnOutcome};
    use crate::state::AppState;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    async fn test_state() -> AppState {
        AppState {
            token: Token("t".into()),
            pool: crate::testdb::fresh_pool().await,
            telegram_doctrine: None,
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: Arc::new(NoAssistants),
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_messages: Arc::new(Mutex::new(HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// An answer the CLI began on its own is a turn like any other, so Stop reaches it softly: the
    /// process is asked to interrupt, ends the turn, and lives on — rather than being killed.
    #[tokio::test]
    async fn a_spontaneous_turn_can_be_stopped_softly() {
        let state = test_state().await;
        let chat = "spontaneous-stop-chat";

        let (messages, mut said) = tokio::sync::mpsc::unbounded_channel::<LaterTurn>();
        let (events, events_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
        let live = LiveChat::new(
            messages,
            events_rx,
            Arc::new(Mutex::new(Some("s-1".to_owned()))),
            tokio::spawn(async {}).abort_handle(),
            tokio::sync::watch::channel(None).1,
            crate::execution::runner::Permission::Default,
            None,
            String::new(),
            None,
        );
        note_served(live.process_key(), chat, 1, "auto", &state.pool);

        // The process hears the interrupt and ends the turn, the way the CLI does.
        let ender = events.clone();
        let hearing = tokio::spawn(async move {
            let heard = said.recv().await.expect("no interrupt was written");
            ender
                .send(TurnEvent::Ended(TurnOutcome::default()))
                .unwrap();
            heard
        });

        let slot = ChatSlot::acquire(chat).expect("the slot was taken");
        start_spontaneous_turn(&state, chat, slot, live).await;

        let id: i64 =
            sqlx::query_scalar("SELECT id FROM runs WHERE chat_id = ? AND origin = 'task'")
                .bind(chat)
                .fetch_one(&state.pool)
                .await
                .expect("the spontaneous turn was not recorded");

        // Steerable once its gather has begun; the task that opens the guard is spawned.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !is_steerable(chat) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "a spontaneous turn is not steerable while it gathers"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let stopped = stop_turn_within(&state, id, Duration::from_secs(5)).await;
        assert_eq!(stopped, Stopped::Interrupted);
        assert!(
            hearing.await.unwrap().interrupt,
            "what was written was not an interrupt"
        );
    }
}
