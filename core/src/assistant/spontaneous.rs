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
        let mut kept = LIVE_CHATS.lock().unwrap();
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
            let (begun, task_lines) = {
                let mut kept = LIVE_CHATS.lock().unwrap();
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
                        (begun, fresh)
                    }
                    _ => return,
                }
            };
            // A task that ends between turns is recorded now, not when a later turn reads the line.
            for line in &task_lines {
                crate::chats::tasks::apply_line(&state.pool, &chat_id, line).await;
            }
            if !begun {
                continue;
            }
            // A person's turn owns the slot, and its `gather` reads `carried` first.
            let Some(slot) = ChatSlot::acquire(&chat_id) else {
                continue;
            };
            let live = {
                let mut kept = LIVE_CHATS.lock().unwrap();
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
    let prompt = spontaneous_prompt(&live.carried);
    let session_id = live.session_id.lock().unwrap().clone();
    // The barrier comes from the PROCESS: the mode stamped on the last turn it served and the
    // strictest `read_untrusted` of every turn it served. Never the chat's newest row, which may be
    // a turn that was stopped before it stamped anything.
    let barrier = PROCESS_BARRIERS
        .lock()
        .unwrap()
        .get(&live.process_key())
        .map(|held| (held.mode, held.served.clone()));
    let Some((mode, served)) = barrier else {
        tracing::warn!(
            chat_id = %chat_id,
            "the assistant's unprompted turn has no stamped permission mode to run under; its process was stopped"
        );
        drop(live);
        drop(slot);
        drain_queued(state, chat_id).await;
        return;
    };
    // The rung the conversation is on NOW must be one this process can be on. `plan`, `acceptEdits`
    // and bypass are the CLI's own `--permission-mode`, fixed at spawn, and the hook keeps a turn on
    // its process's rung whenever `plan` is involved — so an owner who moved the conversation to
    // `plan` since the last turn would otherwise watch this answer go on acting on the old one. A
    // person's turn gets a new process for that (`serve_turn`); an answer nobody asked for gets
    // none, and the process is stopped. A rung that cannot be read stops it too.
    //
    // The three rungs that share one command line (`manual`, `auto`, `dont_ask`) need nothing here:
    // the hook reads the conversation's current rung on every call (`hooks::rooted::rooted_turn`).
    let now_on = crate::chats::permission_mode_of(&state.pool, chat_id)
        .await
        .map(crate::execution::runner::Permission::for_chat);
    // Only a NARROWER rung stops it: an answer on a wider rung than the process was started on
    // keeps the process's own, stricter barrier, which is what it is recorded under.
    let strictness = |permission: crate::execution::runner::Permission| {
        use crate::execution::runner::Permission::*;
        match permission {
            Plan => 0,
            Default => 1,
            AcceptEdits => 2,
            Bypass => 3,
        }
    };
    if now_on
        .ok()
        .is_none_or(|now| strictness(now) < strictness(live.permission))
    {
        tracing::warn!(
            chat_id = %chat_id,
            "the assistant's unprompted turn belongs to a process started on a rung the conversation has left; its process was stopped"
        );
        drop(live);
        drop(slot);
        drain_queued(state, chat_id).await;
        return;
    }
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
        .bind(&prompt)
        .bind(&session_id)
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
    let id = match inserted {
        Ok(done) => done.last_insert_rowid(),
        Err(error) => {
            tracing::warn!(
                chat_id = %chat_id,
                %error,
                "could not record the assistant's unprompted turn; its process was stopped"
            );
            // The process goes first and the slot after it, then whatever queued up while the slot
            // was held is let through: nothing else is going to drain it.
            drop(live);
            drop(slot);
            drain_queued(state, chat_id).await;
            return;
        }
    };
    crate::chats::stamp_project(&state.pool, id, chat_id).await;

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
        let served = tokio::time::timeout(
            deadlines.ceiling,
            live.gather(&transcript, deadlines.silence),
        )
        .await;
        let completed_at = chrono::Utc::now().to_rfc3339();
        match served {
            Ok(LiveTurn::Answered(outcome)) => {
                let stdout = transcript
                    .lock()
                    .map(|held| held.clone())
                    .unwrap_or_default();
                let idle_for = idle_after_spontaneous(live.idle_for, &stdout, began.elapsed());
                let known = live.session_id.lock().unwrap().clone().unwrap_or_default();
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
        // Released before the drain, which goes back through `send_message` and needs the slot.
        drop(slot);
        watch_between_turns(&after, &chat);
        drain_queued(&after, &chat).await;
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
