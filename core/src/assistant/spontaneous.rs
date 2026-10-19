//! Between-turn watching and spontaneous assistant turns.

use super::*;

/// How often a kept process is looked at between turns.
pub(super) const BETWEEN_TURNS_TICK: std::time::Duration = std::time::Duration::from_millis(200);

/// The slowest a quiet kept process is looked at. A process that has said nothing for a while is
/// almost always waiting on a long background task, so an unprompted answer starting up to this much
/// late is invisible, while 200 ms forever per kept conversation is a permanent wake-up.
pub(super) const BETWEEN_TURNS_TICK_MAX: std::time::Duration = std::time::Duration::from_secs(2);

/// The wait before the next look: back to the fast tick as soon as the process showed any activity,
/// otherwise doubling up to [`BETWEEN_TURNS_TICK_MAX`].
pub(super) fn next_between_turns_tick(
    current: std::time::Duration,
    active: bool,
) -> std::time::Duration {
    if active {
        BETWEEN_TURNS_TICK
    } else {
        (current * 2).min(BETWEEN_TURNS_TICK_MAX)
    }
}

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
        let mut tick = BETWEEN_TURNS_TICK;
        loop {
            tokio::time::sleep(tick).await;
            // No lock is held across an await: the guard lives only in this block. An entry that is
            // gone (a turn took it) or owned by a newer reader ends this one.
            let (begun, task_lines, has_set_aside, grew) = {
                let mut kept = relock(&LIVE_CHATS);
                match kept.get_mut(&chat_id) {
                    Some(live) if live.watcher == token => {
                        // `serve_turn` moves a set-aside answer off the process already; this only
                        // catches one a turn path left on it.
                        keep_set_aside(&chat_id, live);
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
                        let grew = live.carried.len() != recorded;
                        recorded = live.carried.len();
                        (
                            begun,
                            fresh,
                            relock(&SET_ASIDE).contains_key(&chat_id),
                            grew,
                        )
                    }
                    _ => return,
                }
            };
            tick = next_between_turns_tick(tick, begun || grew || has_set_aside);
            // A task that ends between turns is recorded now, not when a later turn reads the line.
            for line in &task_lines {
                crate::chats::tasks::apply_line(&state.pool, &chat_id, line).await;
            }
            // An answer a person's message interrupted was set aside whole by `LiveChat::turn`. The
            // turn that set it aside records it before letting go of the slot; whatever still waits
            // (that turn was cancelled first) is recorded here, as the turn of its own that it was.
            if has_set_aside && let Some(slot) = ChatSlot::acquire(&chat_id) {
                let mut slot = slot;
                slot.drain_on_release(&state);
                record_set_aside(&state, &chat_id).await;
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

/// The barrier a process's unprompted answer runs under (see [`open_task_run`]).
///
/// It comes from the PROCESS: the mode stamped on the last turn it served and the strictest
/// `read_untrusted` of every turn it served. Never the chat's newest row, which may be a turn that
/// was stopped before it stamped anything.
fn barrier_of(process_key: usize) -> Option<(&'static str, Vec<i64>)> {
    relock(&PROCESS_BARRIERS)
        .get(&process_key)
        .map(|held| (held.mode, held.served.clone()))
}

/// Writes the `origin = 'task'` run row a spontaneous answer is recorded under, and returns its id
/// and the mode it runs under. `None` when the process has no stamped mode or the row cannot be
/// written; the caller then stops the process rather than let an answer run unaccounted.
async fn open_task_run(
    state: &crate::state::AppState,
    chat_id: &str,
    prompt: &str,
    session_id: &Option<String>,
    barrier: Option<(&'static str, Vec<i64>)>,
    process_rung: Option<crate::execution::runner::Permission>,
) -> Option<(i64, &'static str)> {
    let Some((mode, served)) = barrier else {
        tracing::warn!(
            chat_id = %chat_id,
            "the assistant's unprompted turn has no stamped permission mode to run under; its process was stopped"
        );
        return None;
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
    if let Some(process_rung) = process_rung
        && now_on
            .ok()
            .is_none_or(|now| strictness(now) < strictness(process_rung))
    {
        tracing::warn!(
            chat_id = %chat_id,
            "the assistant's unprompted turn belongs to a process started on a rung the conversation has left; its process was stopped"
        );
        return None;
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

/// An answer `LiveChat::turn` set aside, taken off its process with everything recording it needs,
/// so it outlives that process and cannot be replaced by the next answer set aside on it.
pub(super) struct SetAside {
    process_key: usize,
    session_id: Option<String>,
    /// The process's barrier when the answer was set aside: the turns that answer continues. Taken
    /// then because a process that is dropped takes its barrier with it.
    barrier: Option<(&'static str, Vec<i64>)>,
    stdout: String,
    outcome: crate::execution::runner::TurnOutcome,
}

/// Set-aside answers waiting to be recorded, by chat, oldest first.
///
/// Not on the `LiveChat`, where one used to wait for the between-turns watcher: a process dropped
/// before the watcher ran (it died, went silent, no longer fit) took the answer with it, and a
/// queued turn that took the process first and set another one aside replaced it.
pub(super) static SET_ASIDE: LazyLock<Mutex<HashMap<String, Vec<SetAside>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Moves the answer `live` set aside, if any, into [`SET_ASIDE`] under `chat_id`.
pub(super) fn keep_set_aside(chat_id: &str, live: &mut LiveChat) {
    let Some((stdout, outcome)) = live.preempted.take() else {
        return;
    };
    let process_key = live.process_key();
    let aside = SetAside {
        process_key,
        session_id: relock(&live.session_id).clone(),
        barrier: barrier_of(process_key),
        stdout,
        outcome,
    };
    relock(&SET_ASIDE)
        .entry(chat_id.to_owned())
        .or_default()
        .push(aside);
}

/// Records every answer set aside for `chat_id`, in the order they were set aside. Called by a
/// holder of the chat's slot.
pub(super) async fn record_set_aside(state: &crate::state::AppState, chat_id: &str) {
    let waiting = relock(&SET_ASIDE).remove(chat_id).unwrap_or_default();
    for aside in waiting {
        record_preempted(state, chat_id, aside).await;
    }
}

/// Records, at once, an answer a person's message arrived in the middle of: `LiveChat::turn` set it
/// aside whole, so there is nothing left to gather, only a row to write it under.
async fn record_preempted(state: &crate::state::AppState, chat_id: &str, aside: SetAside) {
    let SetAside {
        process_key,
        session_id,
        barrier,
        stdout,
        outcome,
    } = aside;
    let lines: std::collections::VecDeque<crate::execution::runner::TurnEvent> = stdout
        .lines()
        .map(|line| crate::execution::runner::TurnEvent::Line(line.to_owned()))
        .collect();
    let prompt = spontaneous_prompt(&lines);
    // No rung to compare: the process that began this answer may be gone, and the answer is already
    // whole — there is nothing left to stop.
    let Some((id, mode)) = open_task_run(state, chat_id, &prompt, &session_id, barrier, None).await
    else {
        return;
    };
    let completed_at = chrono::Utc::now().to_rfc3339();
    let known = session_id.unwrap_or_default();
    let o = gathered(outcome, stdout, known);
    // Only while the process still stands: a dropped one has no barrier left to add this row to,
    // and a fresh entry under its key would outlive it.
    if relock(&PROCESS_BARRIERS).contains_key(&process_key) {
        note_served(process_key, chat_id, id, mode, &state.pool);
    }
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
    let Some((id, mode)) = open_task_run(
        state,
        chat_id,
        &prompt,
        &session_id,
        barrier_of(live.process_key()),
        Some(live.permission),
    )
    .await
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

    #[test]
    fn a_quiet_watcher_backs_off_to_the_cap_and_activity_resets_it() {
        let mut tick = BETWEEN_TURNS_TICK;
        let mut seen = vec![tick];
        for _ in 0..6 {
            tick = next_between_turns_tick(tick, false);
            seen.push(tick);
        }
        assert_eq!(seen[1], Duration::from_millis(400));
        assert_eq!(seen[2], Duration::from_millis(800));
        assert_eq!(
            *seen.last().unwrap(),
            BETWEEN_TURNS_TICK_MAX,
            "never slower than the cap"
        );
        assert_eq!(
            next_between_turns_tick(BETWEEN_TURNS_TICK_MAX, true),
            BETWEEN_TURNS_TICK
        );
    }

    async fn test_state() -> AppState {
        AppState {
            startup_config: Default::default(),
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
            SpawnShape::blank(),
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

    /// An answer the process began on its own, set aside by a person's turn, is still recorded when
    /// the next (drained) turn takes the process first and sets another one aside, and when the
    /// process is gone before anything recorded either: both become turns of their own.
    #[tokio::test]
    async fn a_preempted_spontaneous_answer_survives_a_drained_turn() {
        let state = test_state().await;
        let chat = "spontaneous-set-aside-chat";

        let (messages, mut said) = tokio::sync::mpsc::unbounded_channel::<LaterTurn>();
        let (events, events_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
        let person = |prompt: &str| {
            chat_run_request(
                prompt.to_owned(),
                Vec::new(),
                Vec::new(),
                None,
                crate::execution::runner::Permission::Default,
                Some("s-1".to_owned()),
                "s-1".to_owned(),
                std::env::temp_dir().join("set-aside-unused.json"),
                crate::execution::runner::ToolPolicy::Unrestricted,
                crate::chats::Answering::default(),
                None,
                "claude",
                None,
            )
        };
        // The process stands on the ground a person's turn would ask for, or `serve_turn` would
        // retire it instead of speaking down it.
        let live = LiveChat::new(
            messages,
            events_rx,
            Arc::new(Mutex::new(Some("s-1".to_owned()))),
            tokio::spawn(async {}).abort_handle(),
            tokio::sync::watch::channel(None).1,
            crate::execution::runner::Permission::Default,
            None,
            SpawnShape::of(&person("")),
            String::new(),
            None,
        );
        note_served(live.process_key(), chat, 1, "auto", &state.pool);
        relock(&LIVE_CHATS).insert(chat.to_owned(), live);

        // The process answers each person once it has been written to.
        let answer = events.clone();
        let responder = tokio::spawn(async move {
            while said.recv().await.is_some() {
                answer
                    .send(TurnEvent::Line(
                        serde_json::json!({
                            "type": "assistant",
                            "message": {"content": [{"type": "text", "text": "your answer"}]}
                        })
                        .to_string(),
                    ))
                    .unwrap();
                answer
                    .send(TurnEvent::Ended(TurnOutcome::default()))
                    .unwrap();
            }
        });
        // An answer the CLI writes on its own, whole, before the next person's message.
        let on_its_own = |text: &str| {
            events
                .send(TurnEvent::Line(
                    serde_json::json!({
                        "type": "assistant",
                        "message": {"content": [{"type": "text", "text": text}]}
                    })
                    .to_string(),
                ))
                .unwrap();
            events
                .send(TurnEvent::Line(
                    serde_json::json!({"type": "result", "subtype": "success", "result": text})
                        .to_string(),
                ))
                .unwrap();
            events
                .send(TurnEvent::Ended(TurnOutcome::default()))
                .unwrap();
        };
        let runner: Arc<dyn crate::execution::runner::CommandRunner> =
            Arc::new(FakeCommandRunner::default());
        let deadlines = turn_deadlines(
            Duration::from_secs(10),
            Duration::from_secs(10),
            crate::execution::runner::ToolPolicy::Unrestricted,
        );

        on_its_own("first on its own");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let first = Arc::new(Mutex::new(String::new()));
        let served = serve_turn(
            &runner,
            person("me first"),
            tx,
            &first,
            chat,
            deadlines,
            true,
            None,
            "",
        )
        .await;
        assert!(matches!(served, Ok(Ok(_))), "the first turn did not answer");

        // A queued turn drained before anything recorded the first answer set aside.
        on_its_own("second on its own");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let second = Arc::new(Mutex::new(String::new()));
        let served = serve_turn(
            &runner,
            person("me again"),
            tx,
            &second,
            chat,
            deadlines,
            true,
            None,
            "",
        )
        .await;
        assert!(
            matches!(served, Ok(Ok(_))),
            "the drained turn did not answer"
        );

        // And the process is gone before either was recorded.
        evict_live(chat);
        responder.abort();
        record_set_aside(&state, chat).await;

        let recorded: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT status, stdout FROM runs WHERE chat_id = ? AND origin = 'task' ORDER BY id",
        )
        .bind(chat)
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            recorded.len(),
            2,
            "an answer set aside was lost or overwritten: {recorded:?}"
        );
        assert_eq!(recorded[0].0, "completed");
        assert!(
            recorded[0]
                .1
                .as_deref()
                .is_some_and(|out| out.contains("first on its own")),
            "{recorded:?}"
        );
        assert!(
            recorded[1]
                .1
                .as_deref()
                .is_some_and(|out| out.contains("second on its own")),
            "{recorded:?}"
        );
    }
}
