//! Individual council seat execution and run recording.

use super::*;

impl Driver {
    /// Runs one seat and returns how it ended, with the record written on both sides of it.
    ///
    /// **The run id is persisted BEFORE the model is asked anything**, and that ordering is the
    /// whole of two properties. Somebody reading the record mid-phase sees phase 1 filling in
    /// rather than a blank; and `cancel` finds the seats to terminate by reading exactly these
    /// columns, so a council whose ids landed only at the end of the phase would be a council
    /// nobody could stop while it was working.
    ///
    /// `with_tools` is the phase-1 flag. It decides the tool policy AND whether an MCP config is
    /// passed at all, because those are one decision said twice — a policy of `None` beside a config
    /// file would advertise a server the CLI is forbidden to reach.
    pub(super) async fn run_seat(
        &self,
        slot: Slot,
        seat: &CouncilSeat,
        prompt: String,
        with_tools: bool,
    ) -> SeatOutcome {
        // The seat's own instructions, ahead of the phase's. Prepended to the prompt rather than
        // sent as a system prompt because `RunRequest` has none, and because
        // `team::specialist_prompt` already answers this exact question this exact way — the
        // persona is the first thing the agent reads.
        //
        // This is the seat's OWN persona and it never travels. Phase 2 shows peers' ANSWERS and
        // nothing else, so a seat knows who it is and never who the others are. What is not
        // defensible, and is said rather than pretended away: a strong persona writes recognisably,
        // and a reader may guess. That is already true between different models today. The system
        // does not print the name; it does not promise the style will not give it away.
        let prompt = match &seat.agent {
            Some(agent) if !agent.prompt.trim().is_empty() => {
                format!("{}\n\n{prompt}", agent.prompt)
            }
            _ => prompt,
        };
        // Two narrowings meeting at an `&&`: the phase says no to tools after phase 1, and an agent
        // whose `tool_policy` is `none` says no to them full stop. Neither direction can ADD one —
        // `mcp_only` is exactly what a seat gets today and `unrestricted` never reaches the
        // catalogue.
        let with_tools = with_tools && seat.allows_tools();

        // The hard wall on spend: the most launches a council of this shape may ever make. Counted
        // before anything is opened, so a launch past it costs nothing and leaves no run behind.
        let rounds = u32::try_from(self.rounds).unwrap_or(u32::MAX);
        let ceiling = tally::call_ceiling(self.members.len(), rounds);
        if self
            .launched
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            >= ceiling
        {
            let outcome = SeatOutcome {
                status: SEAT_ERROR,
                answer: String::new(),
                error: Some(format!(
                    "the council reached its ceiling of {ceiling} runner calls"
                )),
            };
            self.record(&slot, None, &outcome).await;
            return outcome;
        }

        let (run_id, session_id) = match self.open_run(&prompt).await {
            Ok(opened) => opened,
            Err(error) => {
                // No run row, so nothing to attach; the seat is recorded as failed by its caller's
                // slot all the same.
                let outcome = SeatOutcome {
                    status: SEAT_ERROR,
                    answer: String::new(),
                    error: Some(error.to_string()),
                };
                self.record(&slot, None, &outcome).await;
                return outcome;
            }
        };

        self.record(
            &slot,
            Some(run_id),
            &SeatOutcome {
                status: SEAT_PENDING,
                answer: String::new(),
                error: None,
            },
        )
        .await;

        let outcome = match seat.kind {
            SeatKind::Cloud => {
                self.run_cloud_seat(run_id, session_id, seat, prompt, with_tools)
                    .await
            }
            SeatKind::Local => self.run_local_seat(run_id, seat, prompt, with_tools).await,
        };
        // The REPLY, not the stream it arrived in: a CLI seat's stdout is its whole `stream-json`
        // transcript, and what a peer is shown, a critique is parsed from and the chairman reads is
        // the text the model wrote. A runner that already returns plain text is passed through.
        let outcome = if outcome.status == SEAT_OK {
            let answer =
                crate::execution::runner::extract_reply(&outcome.answer).unwrap_or(outcome.answer);
            SeatOutcome { answer, ..outcome }
        } else {
            outcome
        };
        self.record(&slot, Some(run_id), &outcome).await;
        outcome
    }

    /// Writes one seat's state: its step, or the chairman's run on the council.
    pub(super) async fn record(&self, slot: &Slot, run_id: Option<i64>, outcome: &SeatOutcome) {
        let written = match slot {
            Slot::Step {
                round,
                seat_idx,
                phase,
                shown,
            } => {
                let (status, error, payload) = step_record(phase, shown, outcome);
                store::upsert_step(
                    &self.state.pool,
                    &self.id,
                    *round,
                    *seat_idx as i64,
                    phase,
                    run_id,
                    status,
                    error.as_deref(),
                    payload.as_deref(),
                )
                .await
            }
            // The chairman has no seat row; its run hangs off the council itself. Nothing to write
            // when the row could not even be opened.
            Slot::Chairman => match run_id {
                Some(run_id) => set_chairman_run(&self.state.pool, &self.id, run_id).await,
                None => Ok(()),
            },
        };
        if let Err(error) = written {
            tracing::warn!(council = %self.id, %error, "could not record a seat's state");
        }
    }

    /// Inserts the seat's `runs` row before anything is spawned, so the row exists for the hook to
    /// resolve a mode from and for the reconciliation to find if the daemon dies here.
    ///
    /// A `session_id` is assigned by the daemon rather than waited for, exactly as
    /// `runs::create_run_inner` and `assistant::send_message` do it: `budget.rs` keys spend on that
    /// column, so a seat whose stream never announced an id would be money charged against nothing.
    ///
    /// The seat's MODEL is not written here, because `runs` has no column for one — it lives on
    /// `council_seats.model_ref`, which is the row that knows what a seat is.
    pub(super) async fn open_run(&self, prompt: &str) -> sqlx::Result<(i64, String)> {
        let session_id = crate::auth::generate_uuid_v4();
        let id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, created_at)
             VALUES (?, 'running', ?, ?, ?)",
        )
        .bind(prompt)
        .bind(COUNCIL_MODE)
        .bind(&session_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.state.pool)
        .await?
        .last_insert_rowid();
        Ok((id, session_id))
    }

    async fn run_cloud_seat(
        &self,
        run_id: i64,
        session_id: String,
        seat: &CouncilSeat,
        prompt: String,
        with_tools: bool,
    ) -> SeatOutcome {
        // Section 6 gives council seats machine knowledge only. Append it to the prompt because
        // state.runner may be Codex, and leave no trace because a seat is not an outcome (D15).
        let prompt = match crate::brief::for_prompt(
            &self.state.pool,
            &crate::knowledge::Context::for_project(None),
            &prompt,
            "council",
        )
        .await
        .and_then(|briefing| briefing.block)
        {
            Some(block) => format!("{prompt}{block}"),
            None => prompt,
        };
        let request = crate::execution::runner::RunRequest {
            prompt,
            // The council's OWN key, never `state.token`. `auth::COUNCIL_ROUTES` is what it reaches.
            env: crate::execution::runs::run_env(
                &self.token,
                run_id,
                None,
                crate::speed::Capacity::solo(),
            ),
            cwd: None,
            permission: crate::execution::runner::Permission::Default,
            resume_session_id: None,
            mcp_config: with_tools.then(|| mcp_config_path(&self.id)),
            mcp_job: None,
            mcp_team_run: None,
            // The council writes its config with `build_mcp_config(&exe)` above, so a seat that is
            // given tools is offered the whole surface and pays for the whole surface, which
            // `runner::authored_prompt` reads off the line above.
            tool_policy: if with_tools {
                crate::execution::runner::ToolPolicy::McpOnly
            } else {
                crate::execution::runner::ToolPolicy::None
            },
            progress_timeout: None,
            max_turns: Some(crate::execution::runner::DEFAULT_MAX_TURNS),
            session_id: Some(session_id),
            fork_session: false,
            include_partial_messages: false,
            // Not because a seat is ever steered — `messages` is `None` and no second turn is ever
            // sent — but because this is the only way to keep the prompt OFF the command line.
            // `cli_args` pushes the prompt as a positional argument unless this is set, and Windows
            // caps a command line at 32 767 characters; a phase-2 prompt carries every peer's whole
            // answer, so it passes that ceiling on any question worth asking. The first real council
            // died exactly there: phase 1 succeeded on all three seats (62 KB and 57 KB from the two
            // cloud models), and every cloud seat then failed phase 2 and phase 3 with
            // `ERROR_FILENAME_EXCED_RANGE` — os error 206, whose name says filename and whose
            // meaning is argv. No test could have found it: they all drive a scripted runner that
            // never builds an argv.
            //
            // `steerable: true` with `messages: None` is a documented state, not a borrowed one —
            // the writer task ends after the opening turn and closes stdin, which `run_prompt`'s own
            // comment calls "the one-turn run the argv path performs".
            images: Vec::new(),
            steerable: true,
            // The classifier never sees a seat: `hooks.rs` answers before it, because a
            // `pending_approval` would terminate the seat and mint an approval that resumes into a
            // worktree a council does not have.
            classifier_governs_tools: false,
            messages: None,
            request_tx: None,
            ambient_mcp: false,
            model: Some(seat.model_ref.clone()),
            effort: None,
            fallback_model: Vec::new(),
            add_dirs: Vec::new(),
            max_budget_usd: None,
            agents: Vec::new(),
            append_system_prompt: None,
            denied_tools: Vec::new(),
            session_name: None,
            context_window: None,
            // The wildcard: a seat's `mcp_config` is written per council and already advertises
            // only `COUNCIL_TOOLS`, so there is nothing here left to narrow.
            allowed_mcp_tools: None,
            background_tasks: false,
        };

        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<SeatOutcome>();
        let pool = self.state.pool.clone();
        let runner = self.state.runner.clone();
        let timeout = self.timeout;
        crate::execution::runs::spawn_registered(&self.state, run_id, async move {
            // Captured, so an abort drops it and the awaiting driver learns the seat was
            // cancelled — the same mechanism, and the same reason, as every other guard here.
            let result_tx = result_tx;
            // Inside the task, so a cancel aborts a slow adviser along with the seat. Only the
            // effort can move; `off` asks nothing. The decision is kept and reported below with
            // this seat's final status: every stage opens its own run, so one decision is one seat
            // turn and is reported once.
            let mut request = request;
            let decision = crate::models::seat_advice::route_council_seat(
                &pool,
                runner.router(),
                run_id,
                &mut request,
            )
            .await;
            let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let outcome = tokio::time::timeout(
                timeout,
                runner.run_prompt(request, session_tx, transcript.clone()),
            )
            .await;
            let completed_at = chrono::Utc::now().to_rfc3339();

            let seat_outcome = match outcome {
                Err(_) => {
                    let partial = transcript.lock().unwrap().clone();
                    crate::storage::settle_write("council seat run (timed out)", run_id, || {
                        sqlx::query(
                            "UPDATE runs SET status = 'timed_out', stdout = ?, completed_at = ?
                         WHERE id = ? AND status = 'running'",
                        )
                        .bind(&partial)
                        .bind(&completed_at)
                        .bind(run_id)
                        .execute(&pool)
                    })
                    .await;
                    SeatOutcome {
                        status: SEAT_TIMEOUT,
                        answer: partial,
                        error: Some("the seat ran out of wall clock".to_string()),
                    }
                }
                Ok(Ok(run)) if run.exit_code == 0 => {
                    crate::storage::settle_write("council seat run (completed)", run_id, || {
                        sqlx::query(
                            "UPDATE runs SET status = 'completed', exit_code = 0, stdout = ?,
                                cost_usd = ?, input_tokens = ?, output_tokens = ?,
                                cache_read_tokens = ?, cache_creation_tokens = ?, num_turns = ?,
                                completed_at = ?
                         WHERE id = ? AND status = 'running'",
                        )
                        .bind(&run.stdout)
                        .bind(run.cost_usd)
                        .bind(run.input_tokens)
                        .bind(run.output_tokens)
                        .bind(run.cache_read_tokens)
                        .bind(run.cache_creation_tokens)
                        .bind(run.num_turns)
                        .bind(&completed_at)
                        .bind(run_id)
                        .execute(&pool)
                    })
                    .await;
                    SeatOutcome {
                        status: SEAT_OK,
                        answer: run.stdout,
                        error: None,
                    }
                }
                Ok(Ok(run)) => {
                    // A non-zero exit is still a run that spent money, so its cost is recorded
                    // exactly as a successful one's is. The council counts against the budget, and
                    // a failed seat that cost nothing on paper would understate what was spent.
                    //
                    // `num_turns` and the token columns travel here too, and did not used to: a
                    // seat that answers once and then dies at its turn ceiling —
                    // `crate::execution::runner::TURN_CEILING_EXIT_CODE`, a non-zero exit, so it lands in
                    // THIS arm — carries the completed turn's own count and usage in `RunOutcome`
                    // exactly as the `completed` arm above does, and this `UPDATE` was the one
                    // place that dropped them on the floor instead of binding them.
                    //
                    // Cache creation among them, in both arms: the window a seat spent building its
                    // cache is spend like any other, and a row without it reads as cheaper than it was.
                    crate::storage::settle_write("council seat run (failed)", run_id, || {
                        sqlx::query(
                            "UPDATE runs SET status = 'failed', exit_code = ?, stdout = ?, stderr = ?,
                                cost_usd = ?, input_tokens = ?, output_tokens = ?,
                                cache_read_tokens = ?, cache_creation_tokens = ?, num_turns = ?,
                                completed_at = ?
                         WHERE id = ? AND status = 'running'",
                        )
                        .bind(run.exit_code)
                        .bind(&run.stdout)
                        .bind(&run.stderr)
                        .bind(run.cost_usd)
                        .bind(run.input_tokens)
                        .bind(run.output_tokens)
                        .bind(run.cache_read_tokens)
                        .bind(run.cache_creation_tokens)
                        .bind(run.num_turns)
                        .bind(&completed_at)
                        .bind(run_id)
                        .execute(&pool)
                    })
                    .await;
                    SeatOutcome {
                        status: SEAT_ERROR,
                        answer: run.stdout,
                        error: Some(tail_of(&run.stderr)),
                    }
                }
                Ok(Err(error)) => {
                    crate::storage::settle_write("council seat run (failed)", run_id, || {
                        sqlx::query(
                            "UPDATE runs SET status = 'failed', stderr = ?, completed_at = ?
                         WHERE id = ? AND status = 'running'",
                        )
                        .bind(error.to_string())
                        .bind(&completed_at)
                        .bind(run_id)
                        .execute(&pool)
                    })
                    .await;
                    SeatOutcome {
                        status: SEAT_ERROR,
                        answer: String::new(),
                        error: Some(error.to_string()),
                    }
                }
            };
            // A cancel aborts this task before it gets here, which is why a cancelled seat is
            // never reported.
            crate::models::seat_advice::report_council_seat(
                runner.router(),
                decision,
                seat_outcome.status,
                &seat_outcome.answer,
            );
            let _ = result_tx.send(seat_outcome);
        });

        // A dropped sender means the task was aborted, which here means cancelled — the one way a
        // seat's future stops without writing an outcome.
        result_rx.await.unwrap_or(SeatOutcome {
            status: SEAT_CANCELLED,
            answer: String::new(),
            error: None,
        })
    }

    async fn run_local_seat(
        &self,
        run_id: i64,
        seat: &CouncilSeat,
        prompt: String,
        with_tools: bool,
    ) -> SeatOutcome {
        // The FACTORY's client, where this built its own `runner::OllamaChat` against
        // `runner::OLLAMA_BASE_URL`. `assistants::Assistants::local_chat` is the one place that
        // decides which client a local model gets, and asking it here is what stops the two halves
        // of one daemon drifting onto different engines: on an install whose `local_engine` is
        // `openai_compatible`, the owner's chat reached the server they configured while every seat reached
        // an Ollama that may not be running, may not hold the model, and may not be the machine
        // that was paid for. A seat that fails for that reason — or answers as some other model —
        // looks from outside exactly like a seat that simply had nothing to say.
        let chat = match self.state.assistants.local_chat(&seat.model_ref) {
            Ok(chat) => chat,
            Err(refusal) => {
                // An outcome, never a panic and never an `unwrap`. This runs inside a spawned
                // task's caller and the council's whole design is that a seat which failed casts
                // no votes (`council.rs:14`), so the remaining seats go on without this one. The
                // refusal's OWN sentence travels — `Refusal::message` is what the chat route
                // already shows an operator for the same misconfiguration, and a second, looser
                // wording invented here would describe the same problem differently depending on
                // which door somebody came through.
                let message = refusal.message(crate::chats::Brain::Local);
                // The row is closed here rather than left `running`, exactly as the transport
                // error arm below closes it: nothing was spawned, so nothing else ever will.
                crate::storage::settle_write("council seat run (failed)", run_id, || {
                    sqlx::query(
                        "UPDATE runs SET status = 'failed', stderr = ?, cost_usd = 0, completed_at = ?
                     WHERE id = ? AND status = 'running'",
                    )
                    .bind(&message)
                    .bind(chrono::Utc::now().to_rfc3339())
                    .bind(run_id)
                    .execute(&self.state.pool)
                })
                .await;
                return SeatOutcome {
                    status: SEAT_ERROR,
                    answer: String::new(),
                    error: Some(message),
                };
            }
        };
        // The council's list, not the chat's: `LOCAL_TOOLS` carries `create_run` and `create_job`,
        // which is exactly what a seat must not have. Phases 2 and 3 get an empty box — no tools is
        // no tools whichever machine answers.
        //
        // UNCHANGED by the move to the factory above, and deliberately so: `local_chat` hands out
        // the engine and nothing else, so the toolbox stays `LocalToolBox::for_council` here. It
        // is this half — not the client — that keeps `create_run` and `create_job` away from a
        // seat, and taking the chat route's box along with its client would have handed them over.
        let tools = crate::mcp_tools::LocalToolBox::for_council(
            daemon_url(),
            self.token.clone(),
            self.state.pool.clone(),
        );
        let no_tools = crate::models::local_agent::NoTools;

        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<SeatOutcome>();
        let pool = self.state.pool.clone();
        let timeout = self.timeout;
        crate::execution::runs::spawn_registered(&self.state, run_id, async move {
            let result_tx = result_tx;
            let tool_box: &dyn crate::models::local_agent::ToolBox =
                if with_tools { &tools } else { &no_tools };
            // Owned out here rather than read off the returned `Turn`, for the reason `run_turn`
            // gives: a wall-clock timeout drops that future and a transport error propagates past
            // it, and in both cases a seat that HAS read mail would be written down clean.
            let taint = std::sync::atomic::AtomicBool::new(false);
            let turn = tokio::time::timeout(
                timeout,
                crate::models::local_agent::run_turn(
                    // `as_ref`, because what the factory hands back is a `Box<dyn LocalChat>` and
                    // the loop takes the trait object itself - the box is the seam, not the value.
                    chat.as_ref(),
                    tool_box,
                    crate::models::local_agent::SYSTEM_PROMPT,
                    &[],
                    &prompt,
                    &taint,
                ),
            )
            .await;
            let completed_at = chrono::Utc::now().to_rfc3339();
            // A seat can never act, so the latch cannot stop anything here the way it stops a chat
            // turn — but the row is what the REST of the system reads, and a council seat that read
            // mail must be as legible as any other run that did.
            if taint.load(std::sync::atomic::Ordering::SeqCst)
                && let Err(error) =
                    crate::execution::runs::mark_untrusted_context(&pool, run_id).await
            {
                tracing::warn!(
                    run_id,
                    %error,
                    "could not mark a council seat as having read third-party text"
                );
            }

            let seat_outcome = match turn {
                Err(_) => {
                    crate::storage::settle_write("council local seat run (timed out)", run_id, || {
                        sqlx::query(
                            "UPDATE runs SET status = 'timed_out', completed_at = ?
                         WHERE id = ? AND status = 'running'",
                        )
                        .bind(&completed_at)
                        .bind(run_id)
                        .execute(&pool)
                    })
                    .await;
                    SeatOutcome {
                        status: SEAT_TIMEOUT,
                        answer: String::new(),
                        error: Some("the seat ran out of wall clock".to_string()),
                    }
                }
                Ok(Ok(turn)) => {
                    // `cost_usd = 0` and not NULL. A local seat spends nothing, and NULL is what
                    // `budget.rs` time-approximates a cost for — an unmeasured run — so leaving it
                    // unset would charge the window for electricity.
                    crate::storage::settle_write("council local seat run (completed)", run_id, || {
                        sqlx::query(
                            "UPDATE runs SET status = 'completed', exit_code = 0, stdout = ?,
                                cost_usd = 0, completed_at = ?
                         WHERE id = ? AND status = 'running'",
                        )
                        .bind(&turn.answer)
                        .bind(&completed_at)
                        .bind(run_id)
                        .execute(&pool)
                    })
                    .await;
                    SeatOutcome {
                        status: SEAT_OK,
                        answer: turn.answer,
                        error: None,
                    }
                }
                Ok(Err(error)) => {
                    crate::storage::settle_write("council local seat run (failed)", run_id, || {
                        sqlx::query(
                            "UPDATE runs SET status = 'failed', stderr = ?, cost_usd = 0,
                                completed_at = ?
                         WHERE id = ? AND status = 'running'",
                        )
                        .bind(error.to_string())
                        .bind(&completed_at)
                        .bind(run_id)
                        .execute(&pool)
                    })
                    .await;
                    SeatOutcome {
                        status: SEAT_ERROR,
                        answer: String::new(),
                        error: Some(error.to_string()),
                    }
                }
            };
            let _ = result_tx.send(seat_outcome);
        });

        result_rx.await.unwrap_or(SeatOutcome {
            status: SEAT_CANCELLED,
            answer: String::new(),
            error: None,
        })
    }
}
