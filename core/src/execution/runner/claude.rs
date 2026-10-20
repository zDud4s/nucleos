//! The Claude CLI backend: `ClaudeCliRunner`.

use super::*;

pub struct ClaudeCliRunner {
    pub model: String,
    /// Where a job's `plan` stage runs, when an operator has named somewhere. `None` keeps `model`.
    pub plan_model: Option<String>,
    /// Where a job's `review` stage runs. `None` keeps `model`.
    pub review_model: Option<String>,
    /// Where the queue's conflict resolution runs ([`RESOLVE_STAGE`]). `None` keeps `model`.
    pub resolve_model: Option<String>,
    /// The effort a conflict resolution is launched with. `None` sends no `--effort`.
    pub resolve_effort: Option<String>,
}

/// Reads the next stdout line, giving up after `silence` without one.
///
/// Not while a tool call is open: a long Bash or Agent call prints nothing until its result, and
/// that quiet is work in progress, not a stalled run — the same exemption `LiveChat::gather` makes
/// for a kept process. The run's wall clock still bounds the wait. `Err` carries the deadline that
/// expired.
pub(super) async fn next_line_unless_silent<R>(
    lines: &mut tokio::io::Lines<R>,
    silence: Option<Duration>,
    open_tools: &std::collections::HashSet<String>,
) -> Result<std::io::Result<Option<String>>, Duration>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    unless_silent(lines.next_line(), silence, open_tools).await
}

/// `read`, given up after `silence` unless a tool call is open — the rule of
/// [`next_line_unless_silent`], for any read of the next line.
async fn unless_silent<F, T>(
    read: F,
    silence: Option<Duration>,
    open_tools: &std::collections::HashSet<String>,
) -> Result<T, Duration>
where
    F: std::future::Future<Output = T>,
{
    match silence {
        Some(deadline) if open_tools.is_empty() => tokio::time::timeout(deadline, read)
            .await
            .map_err(|_| deadline),
        _ => Ok(read.await),
    }
}

/// The stage name a conflict resolution is routed by. Not a job stage — no node carries it — but
/// the resolution is the daemon's own read-and-merge turn, and naming it lets the same
/// `model_for_stage` seam route it without a second mechanism.
pub const RESOLVE_STAGE: &str = "resolve";

/// How long stdout and stderr are still drained once the CLI process itself has exited.
///
/// EOF on a pipe means every holder of its write end has closed it, not that the CLI has. A
/// grandchild that inherited the pipe — a background shell, an MCP server — keeps it open after
/// the CLI is gone, and waiting for EOF then waits on that grandchild: until the progress deadline,
/// or forever for a run that has none. Whatever the CLI wrote before exiting is already in the pipe
/// and reads back in well under this; anything after it is not the CLI's to report.
const CHILD_EXIT_STDIO_GRACE: Duration = Duration::from_millis(1500);

/// The next stdout line, or `Ok(None)` once the stream is over — which is EOF, or the direct child
/// having exited `grace` ago, whichever comes first.
///
/// `child_exit` is polled only until it first completes, and `exited_at` records when; it must be
/// cancel safe (tokio's `Child::wait` is), since a fresh one is passed on every call and dropped
/// unfinished whenever a line arrives first. Lines win ties, so what is already buffered is read
/// before the exit is even noticed.
async fn next_line_until_exit_grace<R, E>(
    lines: &mut tokio::io::Lines<R>,
    child_exit: E,
    exited_at: &mut Option<tokio::time::Instant>,
    grace: Duration,
) -> std::io::Result<Option<String>>
where
    R: tokio::io::AsyncBufRead + Unpin,
    E: std::future::Future,
{
    if exited_at.is_none() {
        tokio::select! {
            biased;
            line = lines.next_line() => return line,
            _ = child_exit => *exited_at = Some(tokio::time::Instant::now()),
        }
    }
    let deadline = (*exited_at).expect("set above") + grace;
    match tokio::time::timeout_at(deadline, lines.next_line()).await {
        Ok(line) => line,
        // Treated as EOF: the CLI has gone, and nothing it wrote can still be on the way.
        Err(_) => Ok(None),
    }
}

#[async_trait]
impl CommandRunner for ClaudeCliRunner {
    /// Only `plan` and `review` are routable: both are read-mostly turns with short output.
    /// `implement` is the turn that writes the code, and answering `Some(_)` for it would move real
    /// work onto whatever a config file happens to name. An unnamed stage is every non-job run.
    fn model_for_stage(&self, stage: Option<&str>) -> Option<String> {
        match stage {
            Some("plan") => self.plan_model.clone(),
            Some("review") => self.review_model.clone(),
            Some(RESOLVE_STAGE) => self.resolve_model.clone(),
            _ => None,
        }
    }

    /// Only the resolution carries a configured effort; every other stage keeps the CLI default.
    fn effort_for_stage(&self, stage: Option<&str>) -> Option<String> {
        match stage {
            Some(RESOLVE_STAGE) => self.resolve_effort.clone(),
            _ => None,
        }
    }

    /// The one runner that authors a prompt this daemon can price, because it is the one whose
    /// argument vector `cli_args` builds. See the trait's default for why nobody else answers.
    fn authored_prompt(
        &self,
        request: &RunRequest,
    ) -> Option<crate::prompt_budget::AuthoredPrompt> {
        Some(authored_prompt(request))
    }

    /// `cli_args` writes the ceiling as `--max-budget-usd`, and the CLI stops itself at it.
    fn honours_max_budget(&self) -> bool {
        true
    }

    /// The stream loop below fires `request_tx` on the CLI's `system/init` line.
    fn reports_model_request(&self) -> bool {
        true
    }

    async fn run_prompt(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome> {
        self.run_prompt_with_context_fill(
            request,
            session_tx,
            transcript,
            std::sync::Arc::new(std::sync::Mutex::new(None)),
        )
        .await
    }

    async fn run_prompt_with_context_fill(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
        context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
    ) -> std::io::Result<RunOutcome> {
        // Nobody listening for turns, which is every caller but a conversation keeping its process.
        self.run_prompt_with_turns(request, session_tx, transcript, context_fill, None)
            .await
    }

    async fn run_prompt_with_turns(
        &self,
        mut request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
        context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
        turn_events: Option<UnboundedSender<TurnEvent>>,
    ) -> std::io::Result<RunOutcome> {
        // The Claude Code CLI binary. Overridable via `NUCLEOS_CLAUDE_BIN` because on Windows the
        // npm-installed `claude` is a `.cmd` shim that Rust's `Command` can't spawn by name — the
        // daemon points this at the real `claude.exe`. Defaults to `claude` where it's on PATH.
        let claude_bin =
            std::env::var("NUCLEOS_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
        let mut cmd = Command::new(&claude_bin);
        cmd.args(cli_args(&request, &self.model));
        // Before `request.env` and not after, so an explicit entry still wins. That is what a test
        // needs to force a window the CLI would otherwise clamp away, and it costs nothing here:
        // no caller sets both.
        if let Some((name, value)) = window_env(&request) {
            cmd.env(name, value);
        }
        // Read now, while `request.messages` is still there to be asked about: the steering task
        // takes it once the process is running.
        let background_off = background_env(&request);
        if let Some((name, value)) = background_off {
            cmd.env(name, value);
        }
        for (k, v) in &request.env {
            cmd.env(k, v);
        }
        if let Some(dir) = &request.cwd {
            cmd.current_dir(dir);
        }
        // A closed stdin for every run that did not opt in: there is then no channel for a second
        // author to arrive on, which is the property the email pillar's triage runs depend on.
        if request.steerable {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // kill_on_drop turns an aborted awaiting-task (cancel/timeout, Task 4) into the OS `claude`
        // process actually dying. DO NOT drop this line — Chunk 5 Task 1 adds `--model` and preserves it.
        cmd.kill_on_drop(true);
        // The other half of the `TreeKiller` below: off Windows, killing a tree means killing a
        // process GROUP, and the child has to lead one before it can be named.
        crate::process_tree::spawn_in_own_group(&mut cmd);

        // The ONLY `?` from here on. Everything below turns a failure into a failed RunOutcome
        // instead of an `Err`, because `runs::spawn_run` reads `Err` as "the CLI never ran, so a
        // retry cannot double-apply a mutation". A non-UTF-8 byte on stdout or a broken pipe used
        // to share that type with a spawn failure, so a run that had already worked for minutes
        // inside a worktree — and committed — was re-run from the top.
        let mut child = cmd.spawn()?;

        // Dropped BEFORE `child` (reverse declaration order), which is what keeps this sound: tokio
        // holds the process handle for as long as `child` lives, and Windows will not reuse a pid
        // while a handle to it is open. So an armed killer always names the process we spawned.
        //
        // `kill_on_drop` alone terminates the direct child only. `claude` spawns its own tools —
        // bash, cargo, git, node — and TerminateProcess on the parent orphans every one of them:
        // a cancelled run leaves a `cargo build` holding file locks in the worktree, which is
        // exactly what makes `git worktree remove` fail through its whole backoff afterwards.
        let mut tree_killer = child.id().map(TreeKiller::new);

        // A process meant to outlive its TURN must not outlive the DAEMON.
        //
        // `TreeKiller` above covers being dropped, which is a thing that happens in a program that
        // is still running. It does nothing for `TerminateProcess` — what Task Manager,
        // `Stop-Process` and a crash all do — which runs no destructor and leaves every child
        // alive. `process_tree` records what that costs, measured this month: two days of daemon
        // restarts left 31 orphaned sidecars, each holding the loopback port its own replacement
        // then died trying to bind.
        //
        // A one-turn run is already bounded by its turn and is not enrolled. A CLI held idle
        // between a conversation's turns is exactly the shape of thing that incident was about.
        if turn_events.is_some()
            && let Some(pid) = child.id()
        {
            KEPT_ALIVE.adopt(pid);
        }

        // A steerable run's turns are written in their OWN task, concurrently with the stdout loop
        // below, for the same reason stderr is drained in one: a turn written while the CLI is
        // mid-answer must not stop anything reading what it is saying.
        //
        // The task owns the writer, so its end closes the CLI's stdin. That is deliberate and is the
        // whole of the lifetime rule: with no channel it ends after the opening turn, which is the
        // one-turn run the argv path performs; with one it ends when the sender is dropped, or when
        // the abort below reaps it.
        let mut steering_task = None;
        if request.steerable {
            let mut stdin = child
                .stdin
                .take()
                .expect("stdin piped above when steerable");
            let opening = user_message_line(&request.prompt, &request.images);
            // A process started ahead of its first message (a conversation opened in the window):
            // nothing to say yet, so no opening line is written and the CLI waits on its stdin for
            // the first turn to arrive down `messages`.
            let warmed = request.prompt.is_empty() && request.images.is_empty();
            let mut messages = request.messages.take();
            steering_task = Some(tokio::spawn(async move {
                // `ChildStdin` writes straight to the OS pipe, so a completed `write_all` has
                // already been handed over — there is no buffer left to flush. A failed one means
                // the CLI is gone, which the exit code and stderr below already report; there is
                // nothing this task could add and nobody awaiting it to tell.
                if !warmed && stdin.write_all(opening.as_bytes()).await.is_err() {
                    return;
                }
                let Some(messages) = messages.as_mut() else {
                    return;
                };
                while let Some(turn) = messages.recv().await {
                    // Its own pictures, not none. The opening turn is no longer the only one
                    // anybody attaches anything to: a conversation that keeps its process makes
                    // every turn after the first arrive here.
                    //
                    // An interrupt is not a turn: it is one control line and carries no text.
                    let line = if turn.interrupt {
                        interrupt_line()
                    } else if let Some(task) = &turn.stop_task {
                        stop_task_line(task)
                    } else {
                        user_message_line(&turn.text, &turn.images)
                    };
                    if stdin.write_all(line.as_bytes()).await.is_err() {
                        return;
                    }
                }
            }));
        }

        let stdout = child.stdout.take().expect("stdout piped above");
        let stderr = child.stderr.take().expect("stderr piped above");

        // Drain stderr in a *concurrent* task — reading only stdout while the child writes stderr would
        // deadlock the moment the OS stderr pipe buffer fills (spec §3.3's explicit trap).
        //
        // Into a shared buffer, chunk by chunk, rather than one `read_to_string`: stderr can be held
        // open by a grandchild exactly as stdout can (see `CHILD_EXIT_STDIO_GRACE`), and a task
        // abandoned after the grace must still leave behind what it had read.
        let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let mut stderr_task = tokio::spawn({
            let stderr_buf = stderr_buf.clone();
            async move {
                let mut reader = stderr;
                let mut chunk = [0u8; 8192];
                loop {
                    match reader.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut buf) = stderr_buf.lock() {
                                buf.extend_from_slice(&chunk[..n]);
                            }
                        }
                    }
                }
            }
        });

        let mut lines = BufReader::new(stdout).lines();
        let mut stdout_acc = String::new();
        let mut session_id = request
            .session_id
            .clone()
            .or_else(|| request.resume_session_id.clone());
        let mut cli_session_seen = false;
        let mut cost_usd: Option<f64> = None;
        // Where one answer ends and the next begins. Held across the whole stream because that is
        // the only place the running total lives.
        let mut splitter = TurnSplitter::new();
        let mut usage = RunUsage::default();
        let mut turn_ended = false;
        let mut running_context_fill: Option<i64> = None;
        let mut compacted = false;

        let mut post_launch_error: Option<std::io::Error> = None;
        let mut policy_violation: Option<String> = None;
        let mut init_seen = false;
        // Taken out of the request so the first `init` can consume it: one signal at most, and a
        // stream that ends before any `init` drops it unsent.
        let mut request_tx = request.request_tx.take();
        let mut progress_timeout_elapsed: Option<Duration> = None;
        let mut turns = TurnCounter::default();
        let mut turns_exceeded: Option<i64> = None;
        // Tool calls the CLI has started and not yet answered. While any is open the stream is
        // quiet because a tool is working, not because the run stalled.
        let mut open_tools = std::collections::HashSet::new();
        // When the CLI itself was seen to exit. From then on stdout is only drained, for at most
        // `CHILD_EXIT_STDIO_GRACE`: a grandchild that inherited the pipe can hold it open forever.
        let mut child_exited_at: Option<tokio::time::Instant> = None;

        loop {
            let read = next_line_until_exit_grace(
                &mut lines,
                child.wait(),
                &mut child_exited_at,
                CHILD_EXIT_STDIO_GRACE,
            );
            let next_line = match unless_silent(read, request.progress_timeout, &open_tools).await {
                Ok(result) => result,
                Err(deadline) => {
                    progress_timeout_elapsed = Some(deadline);
                    break;
                }
            };
            let line = match next_line {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    post_launch_error = Some(error);
                    break;
                }
            };
            stdout_acc.push_str(&line);
            stdout_acc.push('\n');
            track_open_tools(&line, &mut open_tools);
            // Mirrored as it arrives, not at the end: the end is exactly what a wall-clock timeout
            // never reaches. A poisoned lock would mean another thread panicked mid-write, which
            // says nothing about this run — keep going rather than take the run down with it.
            if let Ok(mut shared) = transcript.lock() {
                shared.push_str(&line);
                shared.push('\n');
            }
            running_context_fill = context_fill_from_line(&line, running_context_fill);
            if let Ok(mut shared) = context_fill.lock() {
                *shared = running_context_fill;
            }
            // Read on every line and NOT only near the end: the CLI decides to compact before it
            // answers. A run cut short here — a timeout, a turn ceiling — has still had its context
            // summarised, and the record should say so.
            compacted = compacted_from_line(&line, compacted);
            // After the line is accumulated and mirrored, never before: a run stopped here still has
            // to leave the transcript of the turn that stopped it, or the evidence for why it was
            // stopped is the one thing missing from the record.
            turns.line(&line);
            if over_turn_ceiling(turns.count(), request.max_turns) {
                turns_exceeded = Some(turns.count());
                break;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if !cli_session_seen && let Some(sid) = v.get("session_id").and_then(|x| x.as_str())
                {
                    cli_session_seen = true;
                    session_id = Some(sid.to_string());
                    // Best-effort: the receiver may already be gone if the run was cancelled.
                    let _ = session_tx.send(sid.to_string());
                }
                if v.get("type").and_then(|x| x.as_str()) == Some("system")
                    && v.get("subtype").and_then(|x| x.as_str()) == Some("init")
                {
                    init_seen = true;
                    // The earliest point a model request may be in flight — not `session_id`
                    // above, which a `hook_started` line can carry before any request. Sent before
                    // the policy check, since a run stopped by it may already have asked.
                    // Best-effort, like `session_tx`: the watcher may be gone.
                    if let Some(request_tx) = request_tx.take() {
                        let _ = request_tx.send(());
                    }
                    if let Some(source) = super::auth_watch::api_key_source(&v) {
                        super::auth_watch::report(super::auth_watch::AuthSignal::Source(
                            source.to_string(),
                        ));
                    }
                    let advertised = advertised_tools_from_init(&v);
                    if let Some(reason) =
                        advertised_tools_violate(request.tool_policy, advertised.as_deref())
                    {
                        policy_violation = Some(reason);
                        break;
                    }
                }
                // Every `result` is the end of a TURN, which is the same thing as the end of the
                // process only while a process serves one. `TurnSplitter` is where that distinction
                // is written down — the cost a result carries is a running total and the counts
                // beside it are not — so it is read here even when nobody is listening for turns,
                // rather than kept as a second account that can drift from this one.
                //
                // What this loop reports is unchanged: `cost_usd` is still the process's whole bill,
                // and `usage` still describes the turn that ended last.
                for event in splitter.line(line.clone()) {
                    if let TurnEvent::Ended(turn) = &event {
                        // A turn that ended has no tool call left running, whatever it opened.
                        open_tools.clear();
                        turn_ended = true;
                        // `line` is the `result` that ended the turn, which is all the 429 test reads.
                        super::auth_watch::report(super::auth_watch::AuthSignal::TurnEnded {
                            rate_limited: failed_on_rate_limit(&line),
                        });
                        usage = turn.usage;
                        if turn.cost_usd.is_some() {
                            cost_usd = Some(splitter.spent());
                        }
                    }
                    // Best-effort, like `session_tx` above it: a listener that has gone away is a
                    // conversation that stopped caring, and this stream has a process to keep
                    // draining either way.
                    if let Some(turn_events) = &turn_events {
                        let _ = turn_events.send(event);
                    }
                }
            }
        }

        // Only once the stream has ENDED ON ITS OWN can absence of an `init` be read as the CLI
        // never having sent one. A progress timeout or a mid-stream read error means we stopped
        // listening, not that the event was never coming, and asserting otherwise would swap a true
        // diagnosis for a false one.
        if policy_violation.is_none()
            && progress_timeout_elapsed.is_none()
            && post_launch_error.is_none()
        {
            policy_violation = policy_unverified_after_stream(request.tool_policy, init_seen);
        }

        // A stdout read error stops this loop listening, and nothing else ever will: the CLI may be
        // alive and still writing into a pipe nobody drains, so `wait()` below would block until it
        // chose to exit — or, with a full pipe, forever. Stopping it is the only way to reach the
        // terminal write the error already decided on.
        if policy_violation.is_some()
            || progress_timeout_elapsed.is_some()
            || turns_exceeded.is_some()
            || post_launch_error.is_some()
        {
            // Kill the whole tree first so terminating the supervisor cannot orphan its tools.
            drop(tree_killer.take());
            let _ = child.start_kill();
        }

        // Before `wait()`, and unconditionally: dropping the writer the task owns is the only way a
        // steerable run is told no more turns are coming, and a CLI still listening for one has not
        // finished — so waiting first would be waiting on a process this call is what releases.
        if let Some(steering) = steering_task.take() {
            steering.abort();
        }

        let status = match child.wait().await {
            Ok(status) => Some(status),
            Err(error) => {
                post_launch_error.get_or_insert(error);
                None
            }
        };
        // Reaped, so the process is gone and there is nothing left to kill.
        if let Some(killer) = tree_killer.as_mut() {
            killer.disarm();
        }

        // The CLI has been reaped, so whatever still holds stderr open is a descendant of it; give the
        // pipe the same grace stdout got and then stop listening.
        if tokio::time::timeout(CHILD_EXIT_STDIO_GRACE, &mut stderr_task)
            .await
            .is_err()
        {
            stderr_task.abort();
        }
        let mut stderr_str = stderr_buf
            .lock()
            .map(|buf| String::from_utf8_lossy(&buf).into_owned())
            .unwrap_or_default();
        if let Some(reason) = &policy_violation {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            stderr_str.push_str(&format!("nucleos: {reason}\n"));
        }
        if let Some(deadline) = progress_timeout_elapsed {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            stderr_str.push_str(&format!(
                "nucleos: run went silent for {deadline:?}; progress deadline expired\n"
            ));
        }
        if let Some(reached) = turns_exceeded {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            // Says what was reached AND what the limit was. "stopped at 200" alone leaves the reader
            // unable to tell a ceiling that is too low from a run that was never going to finish.
            stderr_str.push_str(&format!(
                "nucleos: stopped after {reached} turns; this run's ceiling was {}\n",
                request.max_turns.unwrap_or_default()
            ));
        }
        // Only for a run nothing can wake, which is the run background tasks were taken from: a
        // conversation that keeps its process is still there when its task finishes.
        let orphaned = if background_off.is_some() {
            orphaned_background_tasks(&stdout_acc)
        } else {
            Vec::new()
        };
        if !orphaned.is_empty() {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            stderr_str.push_str(&format!(
                "nucleos: this run ended its turn with background task(s) {} still running, and \
                 they died with the process; nothing can deliver their result to a run with no \
                 later turn, so the work they were doing never finished\n",
                orphaned.join(", ")
            ));
        }
        let exit_code = match (
            progress_timeout_elapsed,
            turns_exceeded,
            &policy_violation,
            &post_launch_error,
        ) {
            (Some(_), _, _, _) => PROGRESS_TIMEOUT_EXIT_CODE,
            // After the deadline and before the rest: a run killed for looping may well also be a
            // run whose stream then stopped, and the ceiling is the diagnosis that explains the
            // other rather than the other way round.
            (None, Some(_), _, _) => TURN_CEILING_EXIT_CODE,
            (None, None, Some(_), _) => -1,
            // A stream that failed mid-run is a failed run, never a zero exit: the transcript is
            // incomplete, so "succeeded" is a claim this cannot make.
            (None, None, None, Some(error)) => {
                stderr_str.push_str(&format!("\nnucleos: stream failed after launch: {error}\n"));
                -1
            }
            // The CLI's zero is a claim about the turn, and says nothing about the work it left
            // running when the turn ended.
            (None, None, None, None) if !orphaned.is_empty() => -1,
            (None, None, None, None) => status.and_then(|status| status.code()).unwrap_or(-1),
        };

        // Only when no turn ended: a `result` is the CLI's own account and is never second-guessed,
        // and a stream without one still said, message by message, what it read.
        if !turn_ended {
            usage = usage_without_a_result(&stdout_acc);
        }

        Ok(RunOutcome {
            exit_code,
            stdout: stdout_acc,
            stderr: stderr_str,
            session_id,
            cost_usd,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            num_turns: usage.num_turns,
            compacted,
            model_requested: init_seen,
        })
    }
}

#[cfg(test)]
mod stdio_grace_tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// The finding this guards: a pipe whose write end outlives the CLI, as it does when a
    /// grandchild inherited it. The writer is kept alive to the end of the test, so EOF never comes.
    #[tokio::test]
    async fn a_pipe_held_open_after_the_child_exited_ends_after_the_grace() {
        let (reader, mut writer) = tokio::io::duplex(1024);
        writer.write_all(b"first\nsecond\n").await.unwrap();
        let mut lines = BufReader::new(reader).lines();
        let mut exited_at = None;
        let grace = Duration::from_millis(200);
        let started = std::time::Instant::now();

        let mut read = Vec::new();
        loop {
            let next = next_line_until_exit_grace(
                &mut lines,
                std::future::ready(()),
                &mut exited_at,
                grace,
            )
            .await
            .unwrap();
            match next {
                Some(line) => read.push(line),
                None => break,
            }
        }

        // Buffered lines are drained before the exit is noticed, and the stream then ends on its
        // own, well before anything like a progress deadline.
        assert_eq!(read, vec!["first".to_string(), "second".to_string()]);
        assert!(exited_at.is_some());
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        drop(writer);
    }

    /// What the CLI wrote just before exiting can land a moment after the exit is seen; the grace
    /// is there so it is still read.
    #[tokio::test]
    async fn a_line_arriving_within_the_grace_is_still_read() {
        let (reader, mut writer) = tokio::io::duplex(1024);
        let mut lines = BufReader::new(reader).lines();
        let mut exited_at = None;
        let late = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            writer.write_all(b"late\n").await.unwrap();
            writer
        });

        let first = next_line_until_exit_grace(
            &mut lines,
            std::future::ready(()),
            &mut exited_at,
            Duration::from_secs(2),
        )
        .await
        .unwrap();

        assert_eq!(first.as_deref(), Some("late"));
        drop(late.await.unwrap());
    }

    /// Normal EOF is unchanged: a closed pipe ends the stream at once, with the child still running.
    #[tokio::test]
    async fn eof_before_the_child_exits_ends_the_stream() {
        let (reader, mut writer) = tokio::io::duplex(1024);
        writer.write_all(b"only\n").await.unwrap();
        drop(writer);
        let mut lines = BufReader::new(reader).lines();
        let mut exited_at = None;

        let mut read = Vec::new();
        for _ in 0..2 {
            read.push(
                next_line_until_exit_grace(
                    &mut lines,
                    std::future::pending::<()>(),
                    &mut exited_at,
                    Duration::from_secs(60),
                )
                .await
                .unwrap(),
            );
        }
        assert_eq!(read, vec![Some("only".to_string()), None]);
        assert!(exited_at.is_none());
    }

    /// End to end, with a real process: the shell prints, leaves a sleeper holding its stdout and
    /// exits. Needs `sh` on PATH (Git's under Git bash on Windows); skipped where there is none.
    #[tokio::test]
    async fn a_real_child_leaving_a_sleeper_on_its_stdout_does_not_stall_the_read() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo hi; sleep 30 &"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let Ok(mut child) = cmd.spawn() else {
            eprintln!("no `sh` on PATH; skipping");
            return;
        };
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut exited_at = None;
        let started = std::time::Instant::now();

        let mut read = Vec::new();
        while let Some(line) = next_line_until_exit_grace(
            &mut lines,
            child.wait(),
            &mut exited_at,
            CHILD_EXIT_STDIO_GRACE,
        )
        .await
        .unwrap()
        {
            read.push(line);
        }

        assert_eq!(read, vec!["hi".to_string()]);
        assert!(exited_at.is_some());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }
}
