//! The test double for `CommandRunner`, built only for tests and the testkit feature.

use super::*;

/// What a run was launched WITH, as opposed to what it went on to do.
///
/// Its own type rather than four loose fields, because the four answer one question together --
/// does this run start with a past?
#[cfg(any(test, feature = "testkit"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub prompt: String,
    pub resume_session_id: Option<String>,
    pub session_id: Option<String>,
    pub fork_session: bool,
}

/// The test double for `CommandRunner`. `#[cfg(test)]` because every user of it is a test — building
/// it into the daemon would ship a runner that can fake a run's outcome.
#[cfg(any(test, feature = "testkit"))]
pub type JobMcpLaunch = (Option<std::path::PathBuf>, Option<i64>, Option<Vec<String>>);

#[cfg(any(test, feature = "testkit"))]
#[derive(Default)]
pub struct FakeCommandRunner {
    pub canned: std::sync::Mutex<Option<RunOutcome>>,
    // Set by Task 4's cancellation/timeout tests to simulate a slow/hung run.
    pub delay: std::sync::Mutex<Option<std::time::Duration>>,
    pub last_permission: std::sync::Mutex<Option<Permission>>,
    /// The prompt the launch was handed. Recorded because a turn's prompt is not always the text
    /// the person typed — a launcher may prepend context to it — so what the CLI actually
    /// received is the only place that injection can be observed.
    pub last_prompt: std::sync::Mutex<Option<String>>,
    pub last_cwd: std::sync::Mutex<Option<std::path::PathBuf>>,
    pub last_resume: std::sync::Mutex<Option<String>>,
    pub last_mcp_config: std::sync::Mutex<Option<std::path::PathBuf>>,
    pub last_job_mcp: std::sync::Mutex<Option<JobMcpLaunch>>,
    pub last_tool_policy: std::sync::Mutex<Option<ToolPolicy>>,
    /// The turn ceiling the launch was handed. Recorded because it is the brake on a run nobody is
    /// watching, and a project that moved it has no other way to observe that its value arrived:
    /// the effect is a run that does NOT stop, which no assertion can wait for.
    pub last_max_turns: std::sync::Mutex<Option<Option<i64>>>,
    pub last_session_id: std::sync::Mutex<Option<String>>,
    pub last_fork_session: std::sync::Mutex<Option<bool>>,
    pub last_include_partial_messages: std::sync::Mutex<Option<bool>>,
    /// Whether the launch was given a stdin a second author could arrive on. Recorded for the same
    /// reason `last_tool_policy` is: it decides what a run CAN have done to it, so which value
    /// reached the runner is a safety property rather than a detail of the request.
    pub last_steerable: std::sync::Mutex<Option<bool>>,
    /// The pictures the launch was handed. Recorded because bytes can only travel the stdin path,
    /// so a run given images and not `steerable` drops them without a word — a failure invisible
    /// from everywhere except here.
    pub last_images: std::sync::Mutex<Option<Vec<Attachment>>>,
    /// Whether the launch handed the run the classifier's permission surface instead of the CLI's.
    /// Recorded for the same reason `last_tool_policy` is: it decides what a run CAN do.
    pub last_classifier_governs_tools: std::sync::Mutex<Option<bool>>,
    /// What the CLI was handed in its environment. Recorded because a run with a Bash tool can read
    /// its own environment, so which key lands here is a safety property and not a detail.
    pub last_env: std::sync::Mutex<Option<Vec<(String, String)>>>,
    /// Which model and effort the launch was handed. Recorded because there is nowhere else to
    /// observe them: `cli_args` proves the flags are BUILT from a request, and this proves the
    /// request a conversation produces carries what that conversation chose. Between the two there
    /// used to be a gap wide enough for `model: None` to sit in unnoticed for the life of the
    /// feature.
    pub last_model: std::sync::Mutex<Option<Option<String>>>,
    pub last_effort: std::sync::Mutex<Option<Option<String>>>,
    /// Recorded for the reason `last_model` is: `cli_args` proves the flags are built out of a
    /// request, and this proves the request a conversation produces carries what it was told.
    pub last_fallback_model: std::sync::Mutex<Option<Vec<String>>>,
    pub last_add_dirs: std::sync::Mutex<Option<Vec<PathBuf>>>,
    pub last_max_budget_usd: std::sync::Mutex<Option<Option<f64>>>,
    pub last_agents: std::sync::Mutex<Option<Vec<Subagent>>>,
    pub last_append_system_prompt: std::sync::Mutex<Option<Option<String>>>,
    pub last_denied_tools: std::sync::Mutex<Option<Vec<String>>>,
    pub last_session_name: std::sync::Mutex<Option<Option<String>>>,
    /// Test-only: whether this double answers `authored_prompt` the way `ClaudeCliRunner` does.
    ///
    /// `false` by default, and that default is the honest one: a fake builds no argument vector, so
    /// the trait's own reasoning applies to it unchanged — it leaves `runs.authored_prompt_chars`
    /// NULL, exactly as the local model and the Codex CLI do, and every existing test here wants
    /// precisely that.
    ///
    /// Turned on by the handful of tests asking a question about a LAUNCH SITE rather than about a
    /// runner: does this launcher record what it wrote into the prompt, and does it record the right
    /// figure? There is no other way to ask it. The one runner that answers `Some(_)` is the one
    /// that spawns a real `claude`, so a test wanting the recording to happen would have to spawn a
    /// process — and what it would then be testing is the CLI's presence on the machine, not the
    /// call this daemon makes. The arithmetic itself is not on trial here; it is pinned against the
    /// real function in `runner`'s own tests.
    pub prices_its_prompt: bool,
    /// Test-only: whether this double answers `honours_max_budget` the way `ClaudeCliRunner` does.
    /// `false` by default, which is the trait's own answer and the one a Codex daemon gives.
    pub honours_budget: bool,
    /// Test-only: whether this double answers `reports_model_request` the way `ClaudeCliRunner`
    /// does. A launch handed a `request_tx` fires it where the CLI's `init` would arrive, beside
    /// the session id; the outcome's `model_requested` stays whatever the canned one says, so a
    /// test can tell the marker's recorder from the terminal write's repair.
    pub reports_request: bool,
    /// Test-only: drop `request_tx` unsent, as a marker write that never landed would lose it.
    pub loses_request_signal: bool,
    /// Test-only: return an `Err` (simulated launch failure — no work done) for the first N calls.
    pub fail_times: std::sync::Mutex<u32>,
    /// Test-only: count of run_prompt invocations.
    pub calls: std::sync::Mutex<u32>,
    /// Test-only: how the last call was launched.
    ///
    /// `runs.rs`'s context handoff is why this exists. Its successor was launched
    /// `--resume <predecessor> --fork-session`, which copies a conversation rather than ending one,
    /// and nothing here could see it: this fake recorded the environment and the outcome, never the
    /// session shape, so a run inheriting everything looked exactly like one inheriting nothing. It
    /// took a live CLI to notice.
    pub last_launch: std::sync::Mutex<Option<Launch>>,
    /// Test-only: whether a living process was STOPPED rather than allowed to end.
    ///
    /// Set by a guard the fake's own future holds, and disarmed just before that future returns —
    /// so it says "this was dropped mid-flight" and not merely "this finished". Nothing else can
    /// tell those apart from outside, and the difference is the whole of what a cancel has to do.
    pub stopped_early: std::sync::Arc<std::sync::Mutex<bool>>,
    /// Test-only: the turns written to a living process's stdin after the one it was launched with.
    ///
    /// Recorded because a later turn's pictures can only be observed here: they travel down a
    /// channel into a task that writes them to a pipe, and a run given them and not steerable drops
    /// them without a word.
    pub later_turns: std::sync::Mutex<Vec<LaterTurn>>,
    /// Test-only: the queue a plan node writes, taken by the first call that is given a handoff
    /// directory.
    ///
    /// Written into the directory named by `NUCLEOS_JOB_ARTIFACTS`, exactly where a real plan node
    /// would put it — so a test of the job chain goes through the env plumbing and reads the queue
    /// off disk, instead of reaching around both to seed a queue the daemon never saw. Taken rather
    /// than copied, because only the first node of a job plans: an implement node that rewrote the
    /// queue it is working from is a fiction no real run can produce.
    pub plan_to_write: std::sync::Mutex<Option<String>>,
    /// Test-only: a scripted agent. `(marker, path, contents)` — a run whose PROMPT contains the
    /// marker writes `contents` to `path` inside the checkout it was handed.
    ///
    /// **Keyed on the prompt and not on the order of calls**, which is the whole point. Two items of
    /// one batch start as two spawned tasks and reach this in whatever order the runtime picks, so a
    /// queue taken one entry per call would hand item 3's file to item 1 about half the time, and
    /// the test built on it would be measuring the scheduler's mood. The prompt carries the item's
    /// description, which is the one thing that identifies the item from in here.
    ///
    /// It writes and does NOT commit, because that is what a real implement node does: the prompt
    /// tells it to leave the tree uncommitted and `merge_item` is what commits. A double that
    /// committed would put a fixture back to asserting something no code does.
    pub writes: std::sync::Mutex<Vec<(String, String, String)>>,
    /// Test-only: whole answers the process gives with no line written to its stdin -- what the CLI
    /// does when a background task ends between turns (spike 2026-10-05 (c)). Each string is one
    /// answer's stdout; it is split into turn events like any other answer.
    pub unprompted: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>,
}

#[cfg(any(test, feature = "testkit"))]
#[async_trait]
impl CommandRunner for FakeCommandRunner {
    /// `None` unless a test has explicitly asked this double to stand in for the CLI runner here —
    /// see [`FakeCommandRunner::prices_its_prompt`] for why that is opt-in and what it is for.
    ///
    /// When it is asked, it answers through the very same free function `ClaudeCliRunner` calls, so
    /// the double cannot come to price a request differently from the runner it is standing in for.
    /// A second copy of that arithmetic living in the test double would be a test that keeps passing
    /// after the production rule changes underneath it.
    fn authored_prompt(
        &self,
        request: &RunRequest,
    ) -> Option<crate::prompt_budget::AuthoredPrompt> {
        self.prices_its_prompt.then(|| authored_prompt(request))
    }

    fn honours_max_budget(&self) -> bool {
        self.honours_budget
    }

    fn reports_model_request(&self) -> bool {
        self.reports_request
    }

    /// The live-process door, so a conversation that keeps its CLI is exercised in tests rather than
    /// only in production.
    ///
    /// Without this the default would delegate to `run_prompt`, which sends no turn events at all —
    /// and a rooted chat turn would sit waiting for a boundary that never came, fail, and still let
    /// every existing assertion pass, because those look at what the runner was HANDED. That is the
    /// worst shape a gap can have.
    ///
    /// It answers the opening turn and then one more for every line written to its stdin, which is
    /// what a real process does. A fresh `TurnSplitter` per turn, unlike the CLI runner's one across
    /// the whole stream: a canned outcome is one answer repeated, so a shared splitter would report
    /// every turn after the first as having cost nothing — true of the CLI's running total, and
    /// nonsense for a double whose whole job is to be legible.
    async fn run_prompt_with_turns(
        &self,
        mut request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
        _context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
        turn_events: Option<UnboundedSender<TurnEvent>>,
    ) -> std::io::Result<RunOutcome> {
        // Taken before the request is handed over, because the request is what carries it.
        let mut later = request.messages.take();
        let mut unprompted = self.unprompted.lock().unwrap().take();
        // A process started ahead of its first message, as the CLI runner does it: no opening line
        // is written, so nothing answers and no `init` announces a session until a turn arrives.
        let warmed = request.prompt.is_empty() && request.images.is_empty();
        let session_tx = if warmed {
            // Nobody hears the session id the launch would have announced.
            tokio::sync::mpsc::unbounded_channel().0
        } else {
            session_tx
        };
        let outcome = self.run_prompt(request, session_tx, transcript).await?;

        let Some(turn_events) = turn_events else {
            return Ok(outcome);
        };

        /// Records that the future holding it was dropped before it finished.
        struct Stopped {
            flag: std::sync::Arc<std::sync::Mutex<bool>>,
            armed: bool,
        }
        impl Drop for Stopped {
            fn drop(&mut self) {
                if self.armed {
                    *self.flag.lock().unwrap() = true;
                }
            }
        }
        let mut stopped = Stopped {
            flag: std::sync::Arc::clone(&self.stopped_early),
            armed: true,
        };

        let answer = |stdout: &str| {
            let mut splitter = TurnSplitter::new();
            for line in stdout.lines() {
                for event in splitter.line(line.to_owned()) {
                    let _ = turn_events.send(event);
                }
            }
        };

        if !warmed {
            answer(&outcome.stdout);
        }
        if let Some(later) = later.as_mut() {
            // Stays alive until its stdin closes, exactly as the process does — which is what makes
            // a test of "the second turn reused the process" mean anything.
            'turns: loop {
                // `heard` rather than acting inside the select arm, so nothing borrows `unprompted`
                // while it is reassigned.
                let mut heard: Option<Option<String>> = None;
                let turn = match unprompted.as_mut() {
                    Some(spoken) => tokio::select! {
                        turn = later.recv() => turn,
                        said = spoken.recv() => {
                            heard = Some(said);
                            None
                        }
                    },
                    None => later.recv().await,
                };
                match heard {
                    Some(Some(stdout)) => {
                        answer(&stdout);
                        continue;
                    }
                    Some(None) => {
                        unprompted = None;
                        continue;
                    }
                    None => {}
                }
                let Some(turn) = turn else { break };
                // A control line (an interrupt, or a stop of one task) is not a turn: with no turn
                // in flight it is recorded and answers nothing, as the real CLI does.
                let control = turn.interrupt || turn.stop_task.is_some();
                self.later_turns.lock().unwrap().push(turn);
                if control {
                    continue;
                }
                // A process that does not answer instantly, so a test can catch a turn in flight.
                // The delay is the same knob a hung one-shot run uses.
                let waiting = *self.delay.lock().unwrap();
                if let Some(waiting) = waiting {
                    let sleep = tokio::time::sleep(waiting);
                    tokio::pin!(sleep);
                    loop {
                        tokio::select! {
                            () = &mut sleep => break,
                            next = later.recv() => match next {
                                // A line written mid-turn is folded into it: recorded, no answer.
                                Some(next) if !next.interrupt => {
                                    self.later_turns.lock().unwrap().push(next);
                                }
                                // An interrupt ends the turn in flight with the interrupted result.
                                Some(next) => {
                                    self.later_turns.lock().unwrap().push(next);
                                    answer(INTERRUPTED_TURN);
                                    continue 'turns;
                                }
                                None => break 'turns,
                            },
                        }
                    }
                }
                answer(&outcome.stdout);
            }
        }
        // Reached only by ending on its own, which is what makes the flag mean "stopped".
        stopped.armed = false;
        Ok(outcome)
    }

    async fn run_prompt(
        &self,
        mut request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome> {
        {
            *self.calls.lock().unwrap() += 1;
        }
        // Before the failure injection below: what a run was handed is worth knowing even when the
        // launch is made to fail.
        *self.last_env.lock().unwrap() = Some(request.env.clone());
        *self.last_model.lock().unwrap() = Some(request.model.clone());
        *self.last_effort.lock().unwrap() = Some(request.effort.clone());
        *self.last_fallback_model.lock().unwrap() = Some(request.fallback_model.clone());
        *self.last_add_dirs.lock().unwrap() = Some(request.add_dirs.clone());
        *self.last_max_budget_usd.lock().unwrap() = Some(request.max_budget_usd);
        *self.last_agents.lock().unwrap() = Some(request.agents.clone());
        *self.last_append_system_prompt.lock().unwrap() =
            Some(request.append_system_prompt.clone());
        *self.last_denied_tools.lock().unwrap() = Some(request.denied_tools.clone());
        *self.last_session_name.lock().unwrap() = Some(request.session_name.clone());
        *self.last_launch.lock().unwrap() = Some(Launch {
            prompt: request.prompt.clone(),
            resume_session_id: request.resume_session_id.clone(),
            session_id: request.session_id.clone(),
            fork_session: request.fork_session,
        });
        {
            let mut remaining = self.fail_times.lock().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                return Err(std::io::Error::other("fake launch failure"));
            }
        }
        // Where a plan node's only output goes. Nothing is written unless the caller armed a plan
        // AND the run was handed a handoff directory, so an ordinary run cannot produce one.
        if let Some(artifacts) = request
            .env
            .iter()
            .find(|(key, _)| key == "NUCLEOS_JOB_ARTIFACTS")
            .map(|(_, value)| value)
            // Only the planner (or the replanner) writes the queue: "REPLAN node" contains this too.
            // The spec and review runs also carry the artifacts directory and must not consume it.
            && request.prompt.contains("PLAN node of an autonomous job")
            && let Some(plan) = self.plan_to_write.lock().unwrap().take()
        {
            let _ = std::fs::create_dir_all(artifacts);
            std::fs::write(std::path::Path::new(artifacts).join("plan.json"), plan)
                .expect("the plan node writes its queue");
        }
        // The scripted agent, after the plan node's file and before anything is recorded: an
        // implement node's whole observable effect is what it left in its checkout.
        if let Some(cwd) = request.cwd.as_ref() {
            for (marker, path, contents) in self.writes.lock().unwrap().iter() {
                if request.prompt.contains(marker.as_str()) {
                    let target = cwd.join(path);
                    if let Some(parent) = target.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    std::fs::write(&target, contents).expect("the scripted agent writes its file");
                }
            }
        }
        *self.last_prompt.lock().unwrap() = Some(request.prompt.clone());
        *self.last_images.lock().unwrap() = Some(request.images.clone());
        *self.last_cwd.lock().unwrap() = request.cwd.clone();
        *self.last_permission.lock().unwrap() = Some(request.permission);
        *self.last_resume.lock().unwrap() = request.resume_session_id.clone();
        *self.last_mcp_config.lock().unwrap() = request.mcp_config.clone();
        *self.last_job_mcp.lock().unwrap() = Some((
            request.mcp_config.clone(),
            request.mcp_job,
            request.allowed_mcp_tools.clone(),
        ));
        *self.last_tool_policy.lock().unwrap() = Some(request.tool_policy);
        *self.last_max_turns.lock().unwrap() = Some(request.max_turns);
        *self.last_session_id.lock().unwrap() = request.session_id.clone();
        *self.last_fork_session.lock().unwrap() = Some(request.fork_session);
        *self.last_include_partial_messages.lock().unwrap() =
            Some(request.include_partial_messages);
        *self.last_steerable.lock().unwrap() = Some(request.steerable);
        *self.last_classifier_governs_tools.lock().unwrap() =
            Some(request.classifier_governs_tools);
        // Clone the canned outcome in its own scope so the MutexGuard drops before any `.await`.
        let mut outcome = {
            let guard = self.canned.lock().unwrap();
            // A `result` event rather than bare text, because callers parse this. `extract_reply`
            // reads the reply out of a completed run's stream, and a default that was not a stream
            // meant every test taking this outcome exercised the no-reply path by accident — which
            // is how "a turn with no result event" stayed indistinguishable from a successful one
            // long enough to publish a CLI hook payload to a Telegram chat.
            guard.clone().unwrap_or(RunOutcome {
                exit_code: 0,
                stdout: r#"{"type":"result","subtype":"success","result":"fake output"}"#.into(),
                stderr: String::new(),
                session_id: Some("fake-session-id".into()),
                cost_usd: Some(0.0),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
                model_requested: false,
            })
        };
        if outcome.session_id.is_none() {
            outcome.session_id = request
                .session_id
                .clone()
                .or_else(|| request.resume_session_id.clone());
        }
        // Emit session_id *before* any simulated delay — mirrors the real CLI's early `init` message.
        if let Some(sid) = &outcome.session_id {
            let _ = session_tx.send(sid.clone());
        }
        // Where the CLI's `init` fires it. Taken either way, so a lost signal is dropped unsent.
        if let Some(request_tx) = request.request_tx.take()
            && !self.loses_request_signal
        {
            let _ = request_tx.send(());
        }
        let delay = *self.delay.lock().unwrap();
        let mut streamed = false;
        match (delay, request.progress_timeout) {
            (Some(delay), Some(deadline)) => {
                streamed = true;
                let mut emitted_stdout = String::new();
                let stdout_lines = outcome
                    .stdout
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                for line in stdout_lines {
                    if tokio::time::timeout(deadline, tokio::time::sleep(delay))
                        .await
                        .is_err()
                    {
                        let mut timed_out = outcome;
                        timed_out.exit_code = PROGRESS_TIMEOUT_EXIT_CODE;
                        timed_out.stdout = emitted_stdout;
                        if !timed_out.stderr.is_empty() && !timed_out.stderr.ends_with('\n') {
                            timed_out.stderr.push('\n');
                        }
                        timed_out.stderr.push_str(&format!(
                            "nucleos: run went silent for {deadline:?}; progress deadline expired\n"
                        ));
                        return Ok(timed_out);
                    }
                    emitted_stdout.push_str(&line);
                    emitted_stdout.push('\n');
                    // Same contract as the real runner: what has been emitted is visible to the
                    // caller even if this future never gets to return.
                    if let Ok(mut shared) = transcript.lock() {
                        shared.push_str(&line);
                        shared.push('\n');
                    }
                }
            }
            (Some(delay), None) => tokio::time::sleep(delay).await,
            (None, _) => {}
        }
        if !streamed && let Ok(mut shared) = transcript.lock() {
            shared.push_str(&outcome.stdout);
        }
        Ok(outcome)
    }
}
