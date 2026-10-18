//! Spawning a run's process: registration, the event stream, the context-fill mirror, and `spawn_run`.

use super::*;

/// Releases a run's abort handle when its task ends — by returning, by panicking, or by being
/// aborted, including aborted before its first poll, when the task drops its captured state without
/// running a line of the body.
pub(super) struct Registration {
    pub(super) handles: crate::state::RunHandles,
    /// Released here rather than by the steering endpoint or the runner, because the same three ways
    /// a task can end are the three ways a run stops listening — and a sender outliving its receiver
    /// would let `post_run_message` accept a turn nothing will ever read.
    pub(super) messages: crate::state::RunMessages,
    /// Released here for the same reason as the two above, and the cost of getting it wrong is
    /// different in kind: an abort handle is a word, a transcript is the run's entire output. Left
    /// behind, every run the daemon has ever executed stays in memory until restart.
    pub(super) tails: crate::state::RunTails,
    pub(super) id: i64,
}

impl Drop for Registration {
    // Poison is recovered rather than unwrapped. This runs while a panicking body unwinds — the
    // very case the guard exists for — and a second panic inside a destructor during unwinding
    // aborts the whole daemon. The maps hold plain entries, so a poisoned one is still coherent
    // enough to remove a key from.
    fn drop(&mut self) {
        self.handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        self.messages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        self.tails
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

/// What a live run has written so far, from `since` bytes in.
///
/// `None` means there is no live tail — the run finished, or this daemon never started it. It does
/// NOT mean the run wrote nothing, and the difference is the whole point: `run_events` is written
/// once at the end (`append_run_events` has two callers and both are terminal), so a finished run's
/// output lives in the database and not here. A caller that renders `None` as an empty transcript
/// claims a run produced nothing when the durable copy may hold thousands of lines.
///
/// `since` is in BYTES. The last line of a working run has not ended, so a line count would give a
/// cursor that moves backwards as that line grows.
pub fn read_tail(tails: &crate::state::RunTails, run_id: i64, since: usize) -> Option<String> {
    let buffer = tails.lock().ok()?.get(&run_id).cloned()?;
    let text = buffer.lock().ok()?;
    // Saturating rather than slicing: `since` past the end is what every poll of a run that wrote
    // nothing since the last one looks like, so it is the common path, and `&text[since..]` would
    // panic on it inside the daemon's HTTP thread.
    Some(text.get(since..).unwrap_or("").to_owned())
}

/// Tells a steerable run that no more turns are coming.
///
/// Dropping the run's sender is the whole mechanism, and it is the same event the registry already
/// uses to mean "this run is no longer listening". `--input-format stream-json` ends the CLI's turn
/// at stdin EOF, and the writing task reaches EOF only once the last sender is gone — so a sender
/// kept for the run's whole life is not neutral bookkeeping. It holds stdin open, and a steerable
/// run then had no way to finish except its progress deadline: recorded `timed_out`, a failure
/// status, for a run that did exactly what it was asked and was simply never told to stop.
///
/// Idempotent, and reports nothing. A run with no channel is a run already not listening — because
/// it never opted in, because it has ended, or because it was closed a moment ago — and all three
/// are the state the caller asked for. What a closed run does with a later turn is the refusal
/// matrix's job, unchanged: `post_run_message` finds no sender and refuses it exactly as it refuses
/// every other run nothing is listening to.
pub fn close_steering_channel(state: &AppState, id: i64) {
    state.run_messages.lock().unwrap().remove(&id);
}

/// Spawn a run's driver task and register its abort handle so an in-flight run can be terminated
/// (cancel / `pending_approval`).
///
/// The handle is released by a guard the task captures rather than by a statement after
/// `body.await`, because a stale entry here is not merely untidy bookkeeping:
/// `hooks::pretooluse_decision` reads this map as its "is this run_id really in flight" check, so an
/// entry that outlives its run lets a finished run be terminated and pended all over again, and
/// `cancel_run` will overwrite a completed run's final status. An abort is already covered — the
/// terminator (`finalize_termination`) removes the entry itself — but a panicking body is not.
///
/// Registration holds the map lock across the spawn on purpose: the guard runs on whichever thread
/// picks the task up, so a body that finishes before the insert would otherwise release a handle
/// that is only inserted afterwards, pinning it for the life of the daemon.
pub fn spawn_registered<F>(state: &AppState, id: i64, body: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let registration = Registration {
        handles: state.run_handles.clone(),
        messages: state.run_messages.clone(),
        tails: state.run_tails.clone(),
        id,
    };
    let mut handles = state.run_handles.lock().unwrap();
    let join = tokio::spawn(async move {
        let _registration = registration;
        body.await;
    });
    handles.insert(id, join.abort_handle());

    // A panic unwinds past every terminal-status write in the body, and nothing awaits this handle,
    // so the panic went unobserved and the run stayed `running` behind a dead task — `/cancel`
    // answering 404, the GC skipping it, and migration 0009 blocking the project until a restart.
    // The guard above releases the abort handle on a panic, which the existing test checks; the
    // database row was the half nobody was watching.
    //
    // A supervisor rather than a guard because writing that status needs to await, and `Drop`
    // cannot. Cancellation is deliberately ignored here: `finalize_termination` owns that path and
    // has already written the status it chose.
    let pool = state.pool.clone();
    tokio::spawn(async move {
        let Err(error) = join.await else { return };
        if error.is_cancelled() {
            return;
        }
        tracing::error!(run_id = id, %error, "run task panicked; recording it as failed");
        let failed = sqlx::query(
            "UPDATE runs SET status = 'failed', completed_at = ? WHERE id = ? AND status = 'running'",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(id)
        .execute(&pool)
        .await;
        warn_on_terminal_write_err(&failed, id, "failed");
    });
}

/// A terminal-status UPDATE failing is not the same severity as a feed write failing: a feed
/// insert is genuinely best-effort, but a lost terminal write leaves the run `running` behind a
/// dead task — the exact jam class this crate guards against elsewhere, reached here through a DB
/// error instead of a dropped future. `rows_affected() == 0` is a lost first-writer race (see the
/// comments at each call site), a legal outcome rather than a failure, so only `Err` warns.
pub fn warn_on_terminal_write_err(
    result: &Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error>,
    run_id: i64,
    target_status: &str,
) {
    if let Err(error) = result {
        tracing::warn!(run_id, target_status, %error, "terminal-status update failed");
    }
}

/// Stores the runner's complete event stream without making observability part of run correctness.
pub(super) async fn append_run_events(pool: &sqlx::SqlitePool, run_id: i64, stdout: &str) {
    let created_at = chrono::Utc::now().to_rfc3339();
    for (seq, payload) in stdout.lines().enumerate() {
        let kind = serde_json::from_str::<serde_json::Value>(payload)
            .ok()
            .and_then(|event| {
                event
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "unknown".to_owned());

        // Observability must stay best-effort: a missing or temporarily unavailable history table
        // cannot turn successfully completed work into a failed run.
        let _ = sqlx::query(
            "INSERT INTO run_events (run_id, seq, kind, payload, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(run_id)
        .bind(seq as i64)
        .bind(kind)
        .bind(payload)
        .bind(&created_at)
        .execute(pool)
        .await;
    }
}

/// Records how much of this run's prompt the daemon itself wrote, in characters.
///
/// `None` writes nothing at all, and that is the whole reason this takes an `Option` rather than an
/// `AuthoredPrompt`: the column is NULL for every run whose prompt is not one this daemon authored —
/// the local model, the Codex CLI, every fake — and NULL there is a real answer. Writing `0` instead
/// would say those runs were launched with an empty prompt and no tools, which is a claim about
/// their contents rather than an admission of not knowing.
///
/// In CHARACTERS, not in the estimate. The four-characters-to-the-token ruler belongs to whoever
/// reads the column, so a row written today is not stuck with today's ruler if the ruler improves.
///
/// Best effort, like `append_run_events` above and for the same reason: this is bookkeeping about a
/// run, and a failure to write it must not turn a launch that is otherwise fine into a failed one.
///
/// `pub(crate)` because `spawn_run` is not the only launcher that writes a `runs` row. An assistant
/// turn inserts its own and spawns its own CLI, and it is the launcher that actually carries an
/// `--mcp-config` — so a column wired only here would be non-null on every run whose schema cost is
/// a real zero and null on every run that pays one, which is exactly backwards.
pub async fn record_authored_prompt(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    authored: Option<crate::prompt_budget::AuthoredPrompt>,
) {
    let Some(authored) = authored else {
        return;
    };
    let _ = sqlx::query("UPDATE runs SET authored_prompt_chars = ? WHERE id = ?")
        .bind(authored.total_chars() as i64)
        .bind(run_id)
        .execute(pool)
        .await;
}

/// How often a LIVE run's context fill is copied from the stream mirror into its row.
///
/// Throttled on purpose. The mirror is rewritten on every streamed line — many per second — and the
/// number is only ever read by a human or by the handoff check, neither of which needs line
/// resolution. A period plus a change check bounds this to at most one small UPDATE per run per
/// period, and to zero for a run that is thinking rather than emitting.
pub(super) const CONTEXT_FILL_PERSIST_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(500);

/// Aborts the task it names when whatever owns it is dropped.
///
/// The run body is left by more paths than it returns from: the wall clock drops its future and
/// `finalize_termination` aborts it, and neither runs a statement placed after the await. A guard is
/// the only cleanup that fires on all of them — the same reason `Registration` is one.
pub struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Mirrors a live run's context fill into `runs.context_fill`, and the fullest it has been into
/// `runs.context_peak`, until the returned guard is dropped.
///
/// The number was measured from the stream all along, but it only ever reached the database in the
/// terminal UPDATE — so `GET /runs/{id}` answered `context_fill: null` for the entire life of every
/// run, which is exactly when someone deciding whether to steer or to hand off wants to read it.
///
/// Written through to the row rather than kept in an `AppState` map beside `run_handles`: a cancel
/// aborts the run task and `finalize_termination` writes only a status, so an in-memory number would
/// be dropped with the task and the killed run — the one most worth inspecting — would report
/// nothing. A column already written outlives every one of those paths.
///
/// Compare-and-set on `status = 'running'` because this task is not the only writer: a tick that
/// lands after the terminal UPDATE must not put a stale number back onto a finished run.
pub fn mirror_context_fill(
    pool: &sqlx::SqlitePool,
    id: i64,
    context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
) -> AbortOnDrop {
    let pool = pool.clone();
    AbortOnDrop(
        tokio::spawn(async move {
            let mut persisted: Option<i64> = None;
            let mut persisted_peak: Option<i64> = None;
            // The peak lives here and only here — no second `Arc`, and nothing added to
            // `RunOutcome`. The paths that most need it are the ones that never see an outcome.
            let mut peak: Option<i64> = None;
            loop {
                tokio::time::sleep(CONTEXT_FILL_PERSIST_INTERVAL).await;
                let current = context_fill.lock().map(|fill| *fill).unwrap_or(None);
                let Some(now) = current else { continue };
                peak = Some(peak.map_or(now, |seen: i64| seen.max(now)));
                // The short-circuit was `current == persisted`, and that was right with one column.
                // With two, "nothing changed" means both agree: the fill can fall on a compaction
                // while the peak stands still, and the peak can rise on a tick where the fill did
                // not move.
                if current == persisted && peak == persisted_peak {
                    continue;
                }
                let written = sqlx::query(
                    "UPDATE runs SET context_fill = ?, context_peak = ? WHERE id = ? AND status = 'running'",
                )
                .bind(current)
                .bind(peak)
                .bind(id)
                .execute(&pool)
                .await;
                // Only a write that landed counts as persisted, so a transient database error is
                // retried on the next tick instead of being remembered as done.
                if written.is_ok() {
                    persisted = current;
                    persisted_peak = peak;
                }
            }
        })
        .abort_handle(),
    )
}

/// The fullest this stream ever got.
///
/// Named rather than folded at each call site because there are four of them now — two terminal
/// writes here and two in `team.rs` — and four copies of a fold is four places to forget that the
/// peak is not the last line.
pub fn peak_of(stream: &str) -> Option<i64> {
    stream.lines().fold(None, |peak, line| {
        crate::execution::runner::context_peak_from_line(line, peak)
    })
}

/// The tool calls this stream made, as the JSON `runs.tools_used` stores.
///
/// An empty list is stored as `[]`, which says "used no tools". NULL stays reserved for "nobody
/// asked" — the distinction `compacted` lost by being `NOT NULL DEFAULT 0`.
pub fn tools_of(stream: &str) -> String {
    serde_json::to_string(&crate::execution::runner::live_from_stream(stream).did)
        .unwrap_or_else(|_| "[]".to_string())
}

/// Reduces the mirrored stream as a fallback for runners that do not publish context separately.
pub fn observed_context_fill(
    mirror: &std::sync::Mutex<Option<i64>>,
    transcript: &str,
) -> Option<i64> {
    let current = mirror.lock().map(|fill| *fill).unwrap_or(None);
    transcript.lines().fold(current, |fill, line| {
        crate::execution::runner::context_fill_from_line(line, fill)
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_run(
    state: &AppState,
    runner: std::sync::Arc<dyn crate::execution::runner::CommandRunner>,
    id: i64,
    prompt: String,
    project_id: Option<String>,
    spawn_cwd: Option<std::path::PathBuf>,
    // The whole of "what `--permission-mode` does this run launch with". It used to be this
    // boolean plus `classifier_governs_tools`, two fields competing for one flag with an ordering
    // rule holding the safety property up; the choice is made once now, by the caller that knows
    // what kind of run this is.
    permission: crate::execution::runner::Permission,
    resume_session_id: Option<String>,
    session_id: String,
    fork_session: bool,
    completion_feed: Option<(String, String)>,
    gate_config: GateConfig,
    max_attempts: u32,
    tool_policy: crate::execution::runner::ToolPolicy,
    // The environment, built by the caller rather than from a token here: a job node needs the
    // handoff directory alongside the key, and only the caller knows whether this run is one.
    env: Vec<(String, String)>,
    steerable: bool,
    run_timeout: std::time::Duration,
    // Decided by the caller for `run_timeout`'s reason: the mode is what sets it, and the mode is
    // not a fact this function has. Passed beside the wall clock rather than read from `state`
    // here, so the two deadlines can never be derived from different beliefs about the run.
    progress_timeout: std::time::Duration,
    classifier_governs_tools: bool,
    // Which model answers this run, or `None` for the runner's own. Decided by the caller, because
    // only it knows the stage — `spawn_run` must not learn to read job nodes.
    model: Option<String>,
    // The reasoning effort, or `None` for the CLI's own default.
    effort: Option<String>,
    // What the llm-router is told about this run, or `None` for a run that is never routed (a
    // triage run, whose mail must not leave the machine, and a handoff successor).
    route: Option<crate::models::route_advice::RouteQuery>,
    job_mcp: Option<JobNodeMcp>,
) {
    let pool = state.pool.clone();
    let feed_project_id = project_id.clone();
    let handoff_state = state.clone();
    let run_messages = state.run_messages.clone();
    let run_tails = state.run_tails.clone();

    // Whether a gate will judge this run's work: its own (`gate_config`) or the job's, run after it
    // ends. Read before `resolve` takes the query. A run neither judges — a plan, a review, an
    // ad-hoc run — reports on its own exit instead (`route_advice::outcome_at_run_end`).
    let judged_by_a_gate = !matches!(gate_config, GateConfig::NotConfigured)
        || route
            .as_ref()
            .is_some_and(crate::models::route_advice::judged_by_a_job_gate);

    spawn_registered(state, id, async move {
        // The router is asked HERE, inside the run's own task and before its first attempt, so
        // neither the HTTP handler nor the job tick ever waits on it. With routing off this hands
        // back exactly what it was given and writes nothing.
        let resolved = crate::models::route_advice::resolve(
            &pool,
            id,
            runner,
            model,
            effort,
            route,
            crate::models::route_advice::RunShape {
                permission,
                classifier_governs_tools,
                tool_policy,
                resume: resume_session_id.is_some(),
            },
            crate::models::route_advice::speed_of(&env),
        )
        .await;
        let runner = resolved.runner;
        let model = resolved.model;
        let effort = resolved.effort;
        // What an outcome is reported against once the run has ended: both `Some` only for a run
        // the router actually advised, so a fallback or an unrouted run reports nothing.
        let route_decision = resolved.decision_id.zip(resolved.router);
        let mut attempt: u32 = 1;
        loop {
            // A fresh session channel per attempt: persist session_id the instant the runner parses it
            // (spec §3.3), in a separate task so it lands even if the run is terminated mid-flight.
            let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            {
                let pool = pool.clone();
                tokio::spawn(async move {
                    if let Some(session_id) = session_rx.recv().await {
                        let _ = sqlx::query("UPDATE runs SET session_id = ? WHERE id = ?")
                            .bind(&session_id)
                            .bind(id)
                            .execute(&pool)
                            .await;
                    }
                });
            }

            // Owned out here so it survives the timeout below dropping the run future.
            let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            // Published so `GET /runs/{id}/tail` can read it while the CLI is still writing. This
            // is inside the retry loop, and the insert therefore REPLACES the previous attempt's
            // buffer rather than adding to it — which is the wanted behaviour: a retry starts a
            // fresh CLI on a fresh context, so the old transcript describes work that is no longer
            // happening, and a screen still showing it would be reporting a dead attempt as live.
            //
            // Removal is not here. `Registration`'s `Drop` takes this entry out beside the abort
            // handle and the steering channel, which is what covers the abort and panic paths too.
            if let Ok(mut map) = run_tails.lock() {
                map.insert(id, std::sync::Arc::clone(&transcript));
            }
            let context_fill = std::sync::Arc::new(std::sync::Mutex::new(None));
            // Held for this attempt only: a retry starts a fresh CLI on a fresh context, so the
            // previous attempt's mirror has nothing left to say. Dropping the guard at the end of
            // the iteration stops it on every path out of the body, abort included.
            let _live_context_fill =
                mirror_context_fill(&pool, id, std::sync::Arc::clone(&context_fill));
            let mut request = crate::execution::runner::RunRequest {
                prompt: prompt.clone(),
                // A background run carries no pictures: nobody is here to attach one.
                images: Vec::new(),
                env: env.clone(),
                cwd: spawn_cwd.clone(),
                permission,
                resume_session_id: resume_session_id.clone(),
                mcp_config: job_mcp.as_ref().map(|mcp| mcp.path.clone()),
                mcp_job: job_mcp.as_ref().map(|mcp| mcp.job_id),
                mcp_team_run: None,
                tool_policy,
                progress_timeout: Some(progress_timeout),
                // The brake that was missing. These are the runs nobody is watching, and the
                // wall clock above is a poor guard against the failure that matters here: a run
                // looping quickly costs little per turn and reaches neither the clock nor the job's
                // money ceiling, which is only checked between nodes.
                max_turns: Some(crate::execution::runner::DEFAULT_MAX_TURNS),
                session_id: Some(session_id.clone()),
                fork_session,
                include_partial_messages: false,
                steerable,
                classifier_governs_tools,
                messages: None,
                // Autopilot runs pay for the ambient surface and call none of it.
                ambient_mcp: false,
                // Cloned rather than moved: the request is built once per attempt.
                model: model.clone(),
                effort: effort.clone(),
                fallback_model: Vec::new(),
                // The directories its loadout's context references name; none without one.
                add_dirs: job_mcp
                    .as_ref()
                    .map(|mcp| mcp.add_dirs.clone())
                    .unwrap_or_default(),
                max_budget_usd: None,
                agents: Vec::new(),
                append_system_prompt: None,
                denied_tools: Vec::new(),
                session_name: None,
                context_window: None,
                // A job node sees exactly the tools its loadout lists (the box's fixed list when it
                // has none); every other run still has no server and therefore nothing to narrow.
                allowed_mcp_tools: job_mcp.as_ref().map(|mcp| mcp.allowed.clone()),
                background_tasks: false,
            };
            // Driven by the request's own flag, and beside the spawn that decides it: which run may
            // be spoken to is settled where its argument vector is chosen, not by whatever later
            // change opens the door. The flag arrives from `create_run_inner`, which has already
            // refused the modes that must never be steerable — this is where that decision is read,
            // not where it is made.
            //
            // A fresh channel per attempt, because a turn addressed to an attempt that has already
            // failed is not owed to its retry — and `insert` replaces the previous attempt's sender,
            // so nothing can go on holding a stale one.
            if request.steerable {
                let (messages_tx, messages_rx) =
                    tokio::sync::mpsc::unbounded_channel::<crate::execution::runner::LaterTurn>();
                run_messages.lock().unwrap().insert(id, messages_tx);
                request.messages = Some(messages_rx);
            }
            // What this launch itself wrote into the model's prompt, priced from the request that is
            // about to become an argument vector — and priced HERE, in the last statement before the
            // spawn, because `request` is moved into the runner on the very next line and nothing
            // downstream ever sees those values again.
            //
            // The runner decides whether there is anything to say: `authored_prompt` answers `None`
            // for every runner whose prompt this daemon does not author, and a `None` writes no row
            // rather than a zero. Recomputing this later from the run's stored `prompt` would be a
            // second source of truth that omits the three flags — which are most of the number.
            //
            // Re-written on every attempt, deliberately: a retry starts a fresh CLI on a fresh
            // context and pays for the whole prompt again, so the column describes the attempt that
            // is running rather than the first one that was tried.
            record_authored_prompt(&pool, id, runner.authored_prompt(&request)).await;
            let result = tokio::time::timeout(
                run_timeout,
                runner.run_prompt_with_context_fill(
                    request,
                    session_tx,
                    std::sync::Arc::clone(&transcript),
                    std::sync::Arc::clone(&context_fill),
                ),
            )
            .await;
            let completed_at = chrono::Utc::now().to_rfc3339();
            // Every terminal write below is guarded on the run still being `running`. This body is
            // not the only writer racing for the last word: `finalize_termination` aborts the task,
            // but the abort only lands where the future is next dropped, so a cancel or an
            // approval-pause can already have written its own status while this attempt was on its
            // way here. First writer wins; no rows means someone else finalised the run, which is an
            // outcome rather than a failure — the loop breaks either way.
            match result {
                Ok(Ok(o)) => {
                    // A CLI that exited non-zero did not do the work, and recording it `completed`
                    // announced a success its own exit code denies — including in the feed row the
                    // user reads. The runner now also reports -1 for a stream that broke after
                    // launch, so that lands here as a failure rather than being mistaken for a
                    // launch failure and retried over work that was already applied.
                    let terminal_status =
                        if o.exit_code == crate::execution::runner::PROGRESS_TIMEOUT_EXIT_CODE {
                            "timed_out"
                        } else if o.exit_code == 0 {
                            "completed"
                        } else {
                            "failed"
                        };
                    let context_fill = observed_context_fill(&context_fill, &o.stdout);
                    // Read from the same stream everything else came from, and kept here because
                    // here is where an agent run ends. `assistant.rs` does the equivalent in its
                    // own terminal write; the difference is that an agent never passes through
                    // there, and that is why the column was empty in 168 runs in a row.
                    //
                    // An empty list is stored as `[]`, which says "used no tools". NULL stays
                    // reserved for "nobody asked" — the distinction `compacted` lost by being
                    // `NOT NULL DEFAULT 0`.
                    let tools_used = tools_of(&o.stdout);
                    // The peak comes off the whole stream, at full fidelity. The periodic mirror
                    // writes a peak too, sampled every 500ms; this write comes after it and is the
                    // more exact of the two.
                    let context_peak = peak_of(&o.stdout);
                    // `run_prompt` does not return until the CLI process is dead and reaped. The
                    // gate belongs after that boundary: an orphaned build can otherwise retain file
                    // locks in the worktree for the lifetime of every later cleanup retry.
                    let gate_outcome = match (terminal_status, &gate_config, spawn_cwd.as_deref()) {
                        (
                            "completed",
                            GateConfig::Command {
                                command,
                                project_root,
                                scoped_final,
                            },
                            Some(worktree),
                        ) => Some(
                            gate_run_with(
                                &pool,
                                id,
                                project_id.as_deref(),
                                worktree,
                                std::path::Path::new(project_root),
                                command,
                                *scoped_final,
                                crate::verify::installed(),
                            )
                            .await,
                        ),
                        ("completed", GateConfig::Unreadable(reason), _) => {
                            Some(crate::gate::GateOutcome::Errored {
                                reason: reason.clone(),
                            })
                        }
                        _ => None,
                    };
                    let (gate_status, gate_exit_code, gate_output) = match &gate_outcome {
                        Some(crate::gate::GateOutcome::Passed) => (Some("passed"), None, None),
                        Some(crate::gate::GateOutcome::Failed { exit_code, output }) => {
                            (Some("failed"), Some(*exit_code), Some(output.as_str()))
                        }
                        Some(crate::gate::GateOutcome::Errored { reason }) => {
                            (Some("errored"), None, Some(reason.as_str()))
                        }
                        None => (None, None, None),
                    };
                    let completed = sqlx::query(
                        "UPDATE runs SET status = ?, exit_code = ?, stdout = ?, stderr = ?, session_id = COALESCE(?, session_id), cost_usd = ?, input_tokens = ?, output_tokens = ?, cache_read_tokens = ?, cache_creation_tokens = ?, num_turns = ?, context_fill = ?, context_peak = ?, compacted = ?, tools_used = ?, completed_at = ?, attempt = ?, gate_status = ?, gate_exit_code = ?, gate_output = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(terminal_status)
                    .bind(o.exit_code)
                    .bind(&o.stdout)
                    .bind(&o.stderr)
                    .bind(&o.session_id)
                    .bind(o.cost_usd)
                    .bind(o.input_tokens)
                    .bind(o.output_tokens)
                    .bind(o.cache_read_tokens)
                    .bind(o.cache_creation_tokens)
                    .bind(o.num_turns)
                    .bind(context_fill)
                    .bind(context_peak)
                    .bind(o.compacted)
                    .bind(&tools_used)
                    .bind(&completed_at)
                    .bind(attempt as i64)
                    .bind(gate_status)
                    .bind(gate_exit_code)
                    .bind(gate_output)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    warn_on_terminal_write_err(&completed, id, terminal_status);
                    let terminal_write_won =
                        matches!(&completed, Ok(result) if result.rows_affected() == 1);
                    // The events follow the write that won, never precede it. Appended before the
                    // gate, they were already in the table when a cancel landed DURING the gate —
                    // minutes, for a real one — and that cancel won the CAS, found `stdout` still
                    // NULL and recorded the cut stream with its events a second time: every row of
                    // the trajectory twice. A lost CAS means the terminator owns the record.
                    if terminal_write_won {
                        append_run_events(&pool, id, &o.stdout).await;
                    }
                    // The router hears how this run ended, under the same CAS as every other
                    // announcement of it: the gate's verdict when there is one; `error` for a run
                    // its progress deadline killed; for a run no gate judges (a plan, a review),
                    // its own exit; otherwise what a failed CLI says about its infrastructure.
                    // `route_advice::outcome_at_run_end` holds the rule. Detached: the report
                    // never delays the handoff or the sweep below.
                    if terminal_write_won
                        && let Some((decision_id, router)) = &route_decision
                        && let Some(outcome) = crate::models::route_advice::outcome_at_run_end(
                            terminal_status,
                            o.exit_code,
                            &o.stdout,
                            gate_outcome.as_ref(),
                            judged_by_a_gate,
                        )
                    {
                        crate::models::route_advice::report_detached(
                            std::sync::Arc::clone(router),
                            decision_id.clone(),
                            outcome,
                        );
                    }
                    // Taken before `gate_outcome` is consumed by the feed `match` below.
                    let gate_failed_exit = match &gate_outcome {
                        Some(crate::gate::GateOutcome::Failed { exit_code, .. }) => {
                            Some(*exit_code)
                        }
                        _ => None,
                    };
                    // A progress deadline kills the CLI before it can report what it spent, so the
                    // write above just recorded that NULL as final. The wall-clock arm approximates
                    // such a run from its duration; one cut by its own runner's deadline arrives
                    // here instead, and was simply free - run 900501 sat silent for half an hour on
                    // 2026-09-13 and the budget never saw it. Inside the won-the-race guard for the
                    // wall-clock arm's reason: a lost CAS means another terminator owns the row.
                    if terminal_status == "timed_out" && terminal_write_won {
                        // The same ending as the wall-clock arm below, reached through the runner's
                        // own deadline instead: the agent was killed and will never come back to
                        // collect what it queued, so its unstarted vcs requests go with it (spec §7)
                        // exactly as they do there. Best-effort for the same reason.
                        if let Err(error) = crate::execution::vcs::cancel_for_run(&pool, id).await {
                            tracing::warn!(
                                run_id = id,
                                %error,
                                "could not cancel the timed-out run's queued vcs requests"
                            );
                        }
                        record_time_approx_cost(&pool, id).await;
                    }
                    // A CLI that exited non-zero ended the run as well, and its unstarted vcs
                    // requests go the same way as a cancelled or timed-out run's (spec §7): a failed
                    // run must not put work on master that nobody looked at. Its branch stays, so a
                    // human can still land it deliberately. Only for the write that won — a lost CAS
                    // means another terminator owns the row and already decided its queue — and never
                    // for `completed`, whose queued merge is the work it was asked to deliver.
                    // `vcs::reap_requests_of_ended_runs` would retire these at the next drain anyway;
                    // this makes it immediate, as `finalize_termination` does for a cancel.
                    if terminal_status == "failed"
                        && terminal_write_won
                        && let Err(error) = crate::execution::vcs::cancel_for_run(&pool, id).await
                    {
                        tracing::warn!(
                            run_id = id,
                            %error,
                            "could not cancel the failed run's queued vcs requests"
                        );
                    }
                    // The feed row announces this run *finished* — only true if this write won the
                    // CAS race. `Ok` with 0 rows means a concurrent terminator (cancel/timeout) got
                    // there first, so this attempt never actually completed as far as the runs table
                    // is concerned; appending anyway would announce a completion it denies.
                    if terminal_status == "completed" && terminal_write_won {
                        match gate_outcome {
                            Some(crate::gate::GateOutcome::Failed { exit_code, .. }) => {
                                let _ = crate::feed::append(
                                    &pool,
                                    feed_project_id.as_deref(),
                                    "worktree_gate_failed",
                                    &format!("worktree gate failed with exit code {exit_code}"),
                                    Some(id),
                                    Some(&crate::feed::run_subject(&pool, id).await),
                                )
                                .await;
                            }
                            Some(crate::gate::GateOutcome::Errored { reason }) => {
                                let _ = crate::feed::append(
                                    &pool,
                                    feed_project_id.as_deref(),
                                    "worktree_gate_failed",
                                    &format!("worktree gate errored: {reason}"),
                                    Some(id),
                                    Some(&crate::feed::run_subject(&pool, id).await),
                                )
                                .await;
                            }
                            Some(crate::gate::GateOutcome::Passed) | None => {
                                if let Some((kind, summary)) = completion_feed.as_ref() {
                                    let _ = crate::feed::append(
                                        &pool,
                                        feed_project_id.as_deref(),
                                        kind,
                                        summary,
                                        Some(id),
                                        Some(&crate::feed::run_subject(&pool, id).await),
                                    )
                                    .await;
                                }
                            }
                        }
                        // Said after the completion row above, not instead of it. The status stays
                        // `completed` — the CLI did exit 0, and a resumed run may legitimately
                        // decide the approved action is no longer the right step — but the person
                        // who granted the authorization is the one who needs to know it went unused,
                        // and until now nothing told them. See `proposals::unconsumed_grant` for the
                        // two runs that produced this.
                        if let Ok(Some((tool_name, proposal_id))) =
                            crate::proposals::unconsumed_grant(&pool, id).await
                        {
                            let _ = crate::feed::append(
                                &pool,
                                feed_project_id.as_deref(),
                                "resume_did_not_act",
                                &format!(
                                    "resumed for the {tool_name} action approved in proposal \
                                     #{proposal_id}, and finished without attempting it"
                                ),
                                Some(id),
                                Some(&crate::feed::run_subject(&pool, id).await),
                            )
                            .await;
                        }
                    }
                    if terminal_write_won {
                        Box::pin(spawn_handoff_if_needed(
                            handoff_state.clone(),
                            runner.clone(),
                            id,
                            project_id.clone(),
                            spawn_cwd.clone(),
                            permission,
                            completion_feed.clone(),
                            gate_config.clone(),
                            max_attempts,
                            tool_policy,
                            run_timeout,
                            progress_timeout,
                            classifier_governs_tools,
                            model.clone(),
                            effort.clone(),
                        ))
                        .await;
                    }

                    // A handed-off run starts a chain that is still live, so sweep only after the
                    // handoff has had the chance to link its successor. The verdict reader leaves
                    // that chain for a later terminal pass.
                    if terminal_write_won && let Err(error) = crate::brief::sweep(&pool).await {
                        tracing::warn!(%error, run_id = id, "briefing credit not recorded");
                    }

                    // Judged from the row the terminal write just put there, so it is gated on the
                    // same CAS: losing the race means the numbers in `runs` belong to whoever won,
                    // and reading them here would count another attempt's spending as this one's.
                    //
                    // LAST on this arm, deliberately. `let _ =` swallows an `Err` but not a panic,
                    // and a panic here unwinds through `spawn_registered` — where the supervisor's
                    // recovery write is `WHERE status = 'running'` and therefore matches nothing,
                    // this arm having already written `completed`. Ahead of the completion feed row
                    // and the handoff, that would cost the run both; behind them, it can only cost
                    // the observation, which is what nothing depends on. It also has to run after
                    // `spawn_handoff_if_needed` for a second reason: `successor_run_id` is what
                    // tells `ContextSwelling` the handoff already happened.
                    if terminal_write_won
                        && let Err(error) = crate::token_efficiency::observe_run(&pool, id).await
                    {
                        // Warned rather than swallowed, like every other best-effort call on this
                        // arm. A lock held past the busy timeout is the realistic failure, and a
                        // detector that goes quiet without saying so is indistinguishable from one
                        // that has nothing to report.
                        tracing::warn!(%error, run_id = id, "token efficiency not observed");
                    }
                    // Spec E4, detached, and AFTER everything above. After the handoff, so a run
                    // that handed off reads its `successor_run_id` (condition 1) and two agents
                    // never share a tree. After `worktree_gate_failed`, which goes out exactly as
                    // today (D1): the resolver's own line follows it. After `observe_run`, which
                    // stays the last work done INLINE on this arm: a spawn runs nothing inline,
                    // so a panic in the E4 cannot cost the observation, and an E4 that is never
                    // born is today's outcome. Only a gate that FAILED: `errored` is the gate
                    // that did not run, not the code.
                    if terminal_status == "completed"
                        && terminal_write_won
                        && let Some(exit_code) = gate_failed_exit
                    {
                        tokio::spawn(crate::judge::correction::after_gate_failed(
                            handoff_state.clone(),
                            id,
                            exit_code,
                        ));
                    }
                    break;
                }
                Ok(Err(e)) => {
                    // A launch failure means the CLI never ran — no work happened — so retrying cannot
                    // double-apply a mutation. Retry up to max_attempts; otherwise fail for good.
                    if attempt < max_attempts {
                        let _ = crate::feed::append(
                            &pool,
                            feed_project_id.as_deref(),
                            "run_retry",
                            &format!("run {id} attempt {attempt} failed to launch, retrying: {e}"),
                            Some(id),
                            Some(&crate::feed::run_subject(&pool, id).await),
                        )
                        .await;
                        attempt += 1;
                        let _ = sqlx::query("UPDATE runs SET attempt = ? WHERE id = ?")
                            .bind(attempt as i64)
                            .bind(id)
                            .execute(&pool)
                            .await;
                        continue;
                    }
                    let failed = sqlx::query(
                        "UPDATE runs SET status = 'failed', stderr = ?, completed_at = ?, attempt = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(e.to_string())
                    .bind(&completed_at)
                    .bind(attempt as i64)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    warn_on_terminal_write_err(&failed, id, "failed");
                    // The CLI never ran on any attempt: infrastructure, not the model's work.
                    if matches!(&failed, Ok(result) if result.rows_affected() == 1)
                        && let Some((decision_id, router)) = &route_decision
                    {
                        crate::models::route_advice::report_detached(
                            std::sync::Arc::clone(router),
                            decision_id.clone(),
                            crate::models::route_advice::Outcome::Error,
                        );
                    }
                    // Same principle as the completion feed row above: this announces the run's
                    // terminal outcome, so it must only fire when this write actually won the race.
                    if max_attempts > 1
                        && matches!(&failed, Ok(result) if result.rows_affected() == 1)
                    {
                        let _ = crate::feed::append(
                            &pool,
                            feed_project_id.as_deref(),
                            "run_failed_final",
                            &format!("run {id} failed after {attempt} attempts"),
                            Some(id),
                            Some(&crate::feed::run_subject(&pool, id).await),
                        )
                        .await;
                    }
                    break;
                }
                Err(_elapsed) => {
                    // The wall clock dropped the run future, so there is no `RunOutcome` to read —
                    // no stdout, no usage, no session id. What the run did emit before the clock ran
                    // out is in the shared transcript, and it is the whole record of a run that hit
                    // this branch. Persisting it is not cosmetic: this is the run most worth reading
                    // afterwards, and until now it was the one that left nothing at all behind.
                    let seen = transcript
                        .lock()
                        .map(|shared| shared.clone())
                        .unwrap_or_default();
                    let context_fill = observed_context_fill(&context_fill, &seen);
                    // Off the partial transcript, for the same reason the branch above persists it:
                    // this is the run most worth reading afterwards. There is no outcome here, so
                    // `seen` is the whole record — and `compacted` cannot be known from it, which is
                    // why only these two are written.
                    let context_peak = peak_of(&seen);
                    let tools_used = tools_of(&seen);
                    // A timeout is not a launch failure — retrying would likely time out again.
                    let timed_out = sqlx::query(
                        "UPDATE runs SET status = 'timed_out', stdout = ?, context_fill = ?, context_peak = ?, tools_used = ?, completed_at = ?, attempt = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(&seen)
                    .bind(context_fill)
                    .bind(context_peak)
                    .bind(&tools_used)
                    .bind(&completed_at)
                    .bind(attempt as i64)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    warn_on_terminal_write_err(&timed_out, id, "timed_out");
                    if matches!(&timed_out, Ok(result) if result.rows_affected() == 1) {
                        // Only for the write that won, as in the branch above: a terminator that got
                        // here first already recorded this stream and its events.
                        append_run_events(&pool, id, &seen).await;
                        // A deadline is not a verdict on the work, so the router hears `error`
                        // (`route_advice::outcome_at_run_end` argues it), first and detached, so
                        // nothing below can hold it up.
                        if let Some((decision_id, router)) = &route_decision {
                            crate::models::route_advice::report_detached(
                                std::sync::Arc::clone(router),
                                decision_id.clone(),
                                crate::models::route_advice::Outcome::Error,
                            );
                        }
                        // The run is over; anything it queued and never started goes with it (spec
                        // §7). A run its wall clock killed is that spec's "the agent that submitted
                        // dies" as much as one a human cancelled — it will never come back to
                        // collect the merge — and nothing routes this path through
                        // `finalize_termination`, so the sweep is called here directly.
                        //
                        // Inside the won-the-race guard, for the reason the launch-failure feed row
                        // above states and one more that is specific to this: a lost race means
                        // another terminator already chose the status, and one of those —
                        // `pause_for_approval`'s `awaiting_approval` — is a run that RESUMES.
                        // Sweeping for it would cancel the very merge it paused to have approved.
                        //
                        // Best-effort, because the run IS terminated either way and a queue row that
                        // outlives its run is a stale request a human can cancel, not a broken run.
                        if let Err(error) = crate::execution::vcs::cancel_for_run(&pool, id).await {
                            tracing::warn!(
                                run_id = id,
                                %error,
                                "could not cancel the timed-out run's queued vcs requests"
                            );
                        }
                        // This is the run that reports no cost at all, so its spend has to be
                        // approximated from the duration the write above just made final. Inside
                        // the same guard: losing that CAS means another terminator owns the row,
                        // and it is the owner's business what the run's cost and status are.
                        record_time_approx_cost(&pool, id).await;
                        // No context handoff here: a deadline is a brake, and a successor would
                        // relaunch the work it just stopped (`handoff::ends_in_a_handoff`).
                    }
                    break;
                }
            }
        }
    });
}

/// Retires a run that failed somewhere in worktree provisioning, and says so in the feed.
///
/// Provisioning happens after the run row exists, so a failure that just returns leaves it
/// `running` with no task — a state only a daemon restart reconciles, while migration 0009 blocks
/// the project's next worktree run for as long as it lasts. Compare-and-set on `running` so a
/// concurrent cancel keeps the last word.
pub(super) async fn fail_provisioning(
    state: &AppState,
    id: i64,
    project_id: Option<&str>,
    summary: &str,
) {
    let failed = sqlx::query(
        "UPDATE runs SET status = 'failed', completed_at = ? WHERE id = ? AND status = 'running'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(&state.pool)
    .await;
    warn_on_terminal_write_err(&failed, id, "failed");
    let _ = crate::feed::append(
        &state.pool,
        project_id,
        "worktree_provision_failed",
        summary,
        Some(id),
        Some(&crate::feed::run_subject(&state.pool, id).await),
    )
    .await;
}
