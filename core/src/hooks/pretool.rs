//! The PreToolUse decision: `pretooluse_decision` and the one function that walks every rule in order.

use super::*;

pub async fn pretooluse_decision(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(payload): Json<PreToolUsePayload>,
) -> Json<Decision> {
    pretooluse_decision_from(state, scope, payload, Instant::now()).await
}

/// Whether `tool_name` is one of the job-node tools: `mcp__nucleos__<name>` for a `<name>` in
/// `mcp_tools::JOB_NODE_BASE`, matched as a WHOLE name and never by prefix or wildcard.
///
/// This is the base list an unattended run may call, and it is a branch in
/// `pretooluse_decision` and not a classifier class: the classifier is pure and per-tool, and a
/// class would also have to be kept away from the judge and the scoreboard. A widening of
/// `JOB_NODE_BASE` widens this by construction, so that list is the thing review must watch. The
/// extras (`JOB_NODE_EXTRAS`) are admitted only from the run's loadout, see `job_node_extra`.
pub(super) fn is_job_node_tool(tool_name: &str) -> bool {
    tool_name
        .strip_prefix("mcp__nucleos__")
        .is_some_and(|name| crate::mcp_tools::JOB_NODE_BASE.contains(&name))
}

/// The `<n>` of a whole-segment `mcp__nucleos__<n>` that names a job-node extra, else `None`.
pub(super) fn job_node_extra(tool_name: &str) -> Option<&str> {
    tool_name
        .strip_prefix("mcp__nucleos__")
        .filter(|name| !name.contains("__"))
        .filter(|name| crate::mcp_tools::JOB_NODE_EXTRAS.contains(name))
}

/// The job a run belongs to, `None` for a run that is not a job's node (or whose row is gone).
pub(super) async fn job_id_of(
    pool: &sqlx::SqlitePool,
    run_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    Ok(
        sqlx::query_scalar::<_, Option<i64>>("SELECT job_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await?
            .flatten(),
    )
}

/// `pretooluse_decision` with the moment the request began taken as a parameter, so the budget the
/// judge may spend (`judge_wait`) can be tested without sleeping through the probes that use it up.
pub(super) async fn pretooluse_decision_from(
    state: AppState,
    scope: Scope,
    mut payload: PreToolUsePayload,
    started: Instant,
) -> Json<Decision> {
    // `run_id` arrives in the body, which makes it a claim the caller makes about itself, and every
    // branch below reads `mode` from it. A scoped key names its own run, and the daemon resolved
    // that name from its own state rather than from anything the caller chose — so the key decides
    // and the claim is dropped. A run cannot borrow another run's rules, or spend another run's
    // denial allowance to have it stopped, because it has no way to say which run it is.
    //
    // Dropped rather than compared-and-refused, which is what this was. The guarantee is the same
    // one, kept by construction instead of by inspection; the difference is that a stale claim is
    // now irrelevant instead of fatal. A CLI kept alive across turns is handed its environment once,
    // at spawn, and `ask_daemon.py` echoes `NUCLEOS_RUN_ID` out of it — so from the second turn on,
    // a comparison would refuse every tool call the conversation made.
    //
    // A key that names no run — the control token an orchestrator turn carries, which it can hold
    // because `ToolPolicy::McpOnly` leaves it no way to read its own environment — leaves the claim
    // as the only identifier there is. Hence the fallback, and hence no `deny` here: the claim is
    // still the truth for exactly the callers that cannot usefully lie about it.
    let run_id = match scope {
        Scope::Run(id) => id,
        _ => payload.run_id,
    };
    // Corrected IN the payload, and not merely beside it. Every branch below is handed `&payload`
    // and several read the id back out of it — `rooted_decision` checks the read-untrusted barrier
    // with it, `assistant_decision` both checks and SETS it — so leaving the claim in place would
    // fix the id in this function and leave it stale in the ones that decide with it.
    //
    // Which is the hole a kept process opens, and it is not the one the key closed: a CLI is handed
    // its environment once, so from the second turn on `ask_daemon.py` echoes the FIRST turn's id
    // for the rest of the conversation. A turn that read a stranger's text is marked on its own row;
    // a barrier reading the first turn's row finds nothing there, and the ordering rule holds on
    // turn one and is walked around on every turn after it.
    payload.run_id = run_id;

    // Validate run_id against runs actually in flight before trusting anything derived from it (spec
    // §3.4 — the hook's environment sits inside the same cooperative trust model as the token, so the
    // core never blindly trusts what the hook sends).
    let is_in_flight = state.run_handles.lock().unwrap().contains_key(&run_id);

    // `mode` is resolved for EVERY request, in flight or not, because it decides WHICH set of rules
    // applies — and a run that has left `run_handles` is exactly when defaulting to `real` is most
    // dangerous: the classifier permits `Read` there, so an email triage run would be handed the one
    // tool the pillar exists to keep away from a stranger's text. `cwd` stays behind the in-flight
    // check: it only feeds the classifier's path sensitivity, which is meaningful for a run that is
    // actually executing.
    //
    // `project_id` sits with `mode` and NOT with `cwd`, for the same sentence: a project's two shell
    // lists say what may run at all, not where a path may point, and a run that has left
    // `run_handles` is precisely when losing that project's `deny` would cost the most. One more
    // column on a query that already ran, so the reach costs nothing.
    //
    // `permission_mode` rides along for the same sentence, and it is the SNAPSHOT the turn started
    // with rather than what the conversation's selector says now: moving the menu while a turn is
    // running must not change the rules underneath it. NULL means no CLI turn wrote one — every
    // autopilot run, and every turn the local brain answered — and reads as `Auto`, which is what a
    // rooted conversation did before the column existed. One more column on a query that already
    // ran, so the reach costs nothing.
    // `judge` rides along for the sentence `permission_mode` gives above: it is the SNAPSHOT the
    // run was launched with (spec A D2), so changing a project's judge while a run is working
    // does not change the rules underneath it. `off` when the row is gone.
    // Spec B D6.1: the lineage root rides along for D12's mark. NULL means this run is its own
    // root, hence the COALESCE; an absent row is its own root too, with nothing to match.
    // Spec B D2/D10: `job_id` and the run's `judge_resolve` snapshot ride along too, for the
    // resolver's eligibility; the same query, so no extra read on the hook's path.
    let (cwd, mode, project_id, permission, judge_snapshot, lineage_root, job_id, resolve_snapshot) =
        match sqlx::query_as::<
            _,
            (
                Option<String>,
                String,
                Option<String>,
                Option<String>,
                String,
                i64,
                Option<i64>,
                String,
            ),
        >(
            "SELECT cwd, mode, project_id, permission_mode, judge, COALESCE(lineage_root_id, id),
                job_id, judge_resolve
         FROM runs WHERE id = ?",
        )
        .bind(run_id)
        .fetch_optional(&state.pool)
        .await
        {
            Ok(Some((
                cwd,
                mode,
                project_id,
                permission,
                judge,
                lineage_root,
                job_id,
                judge_resolve,
            ))) => (
                is_in_flight.then_some(cwd).flatten(),
                mode,
                project_id,
                permission.as_deref().map_or(
                    crate::chats::PermissionMode::Auto,
                    crate::chats::PermissionMode::from_wire,
                ),
                crate::judge::JudgeMode::from_db_str(&judge)
                    .unwrap_or(crate::judge::JudgeMode::Off),
                lineage_root,
                job_id,
                crate::judge::JudgeMode::from_db_str(&judge_resolve)
                    .unwrap_or(crate::judge::JudgeMode::Off),
            ),
            Ok(None) => (
                None,
                "real".to_owned(),
                None,
                crate::chats::PermissionMode::Auto,
                crate::judge::JudgeMode::Off,
                run_id,
                None,
                crate::judge::JudgeMode::Off,
            ),
            // `mode` decides WHICH set of rules applies, so an unreadable one cannot resolve to the
            // most permissive of them. `Ok(None)` above can safely default to `real` because the row is
            // genuinely absent — there is no run whose rules we are guessing at. An `Err` is different:
            // the run may well be a triage or shadow run whose barrier we would be stepping over, and
            // the pool this reads through is shared with feed appends and run-status writes, so
            // SQLITE_BUSY under contention is an ordinary event rather than a theoretical one.
            Err(error) => {
                tracing::warn!(
                    run_id = run_id,
                    %error,
                    "pretooluse-decision: failed to resolve the run's mode — failing closed"
                );
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not resolve the run's mode — failing closed".to_owned(),
                });
            }
        };

    // Barrier 2 of spec §5.5. A triage run is launched with no tools at all (barrier 1), so a tool
    // call arriving here means barrier 1 is not in force — which is the entire reason this branch
    // exists. There is no allowlist and no read-only exception: the run's whole job is to read text
    // a stranger wrote and answer with a verdict, and every tool is a way for that text to act.
    //
    // It returns before the classifier, so it never terminates the run and never mints a proposal.
    if mode == crate::integrations::email::TRIAGE_MODE {
        // Only a run that is actually executing can have reached a tool through barrier 1, and that
        // is the alarming case. The startup verification deliberately probes this branch with a
        // throwaway row that is NOT in flight, so warning on both would fire a false alarm on every
        // single boot — and an alarm that cries wolf at startup is one nobody reads when it matters.
        if is_in_flight {
            tracing::warn!(
                run_id = run_id,
                tool = %payload.tool_name,
                "pretooluse-decision: a triage run attempted a tool — barrier 1 is not in force"
            );
        } else {
            tracing::debug!(
                run_id = run_id,
                tool = %payload.tool_name,
                "pretooluse-decision: denied a tool for a triage run that is not in flight"
            );
        }
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: "email triage runs have no tools".to_owned(),
        });
    }

    // Orchestrator (assistant) turns are constrained to the NucleOS MCP tools by their tool policy
    // (`ToolPolicy::McpOnly`) and delegate all real work to governed runs, so they must NOT go
    // through the autopilot classifier — doing so would terminate the turn and mint action-approval proposals it
    // can never satisfy (a resume expects a worktree run). Allow the sanctioned MCP tools, block
    // everything else, and never create a proposal or terminate the turn.
    if mode == "assistant" {
        // ...unless the turn is ROOTED: one continuing a session had in the IDE, spoken to from the
        // machine, in a directory that registers this hook. That turn was launched
        // `ToolPolicy::Unrestricted` precisely so it can touch the code the conversation is about,
        // and the branch below would deny every one of those calls — leaving it holding tools it can
        // never use, which is worse than not having them.
        if let Some(root) = rooted_turn(&state, run_id).await {
            return rooted_decision(&state, &payload, &root, project_id.as_deref(), permission)
                .await;
        }
        return assistant_decision(&state, &payload).await;
    }

    // A council seat reads in order to answer a question, and does nothing else. Same shape as the
    // branch above and a strictly narrower list: `mcp_tools::COUNCIL_TOOLS` carries no `Acts` at
    // all, so there is no ordering rule to apply and nothing a seat can do that the owner would
    // have to undo.
    //
    // Like the orchestrator's, this returns BEFORE the classifier — a council run has no worktree
    // and no proposal to resume into, so a `pending_approval` here would terminate the seat and
    // mint an approval nothing could ever satisfy.
    if mode == crate::council::COUNCIL_MODE {
        return council_decision(&state, &payload).await;
    }

    // A department is in the same position as a seat, and for the question of WHICH tools this
    // branch is the SECOND layer rather than the first. `team.rs` launches with `cwd: None`, so no
    // `.claude/settings.json` of the owner's resolves and this hook may never fire at all — which
    // is why `auth::TEAM_ROUTES` is the barrier that has to hold alone there, and does.
    //
    // For the other question — having read, may it still act — this branch is not a second layer at
    // all. It is the ONLY one on the cloud path. `auth::permits` is a pure function over
    // `(Scope, Method, path)` and cannot know what a turn has read, and `permitted_after_untrusted`
    // has exactly one caller in this codebase (`local_agent.rs`), which is the Ollama path.
    //
    // The two sentences that used to stand here said `TEAM_TOOLS` carried no `Acts` and that there
    // was nothing for the ordering rule to bite on. Both were true when written and stopped being
    // true when the alçada landed: `propose_action` and `propose_teammate` are `Acts` today, and
    // `propose_action`'s own grading exists precisely so this door shuts.
    if mode == crate::team::TEAM_MODE {
        return team_decision(&state, &payload).await;
    }

    // A job's node may call its box's base tools (a finding, verification, `request_tool`,
    // `read_context`): the `mcp__` tools an unattended run is allowed without a loadout row. Before
    // the classifier, like the branches above, so it never reaches the judge, never parks and writes
    // no scoreboard row (the scoreboard measures the classifier, and this is not its decision).
    // Anything short of ALL the conditions falls through and is refused as an unrecognised tool.
    if is_job_node_tool(&payload.tool_name)
        && is_in_flight
        && crate::execution::runs::runs_unattended(&mode)
    {
        match job_id_of(&state.pool, run_id).await {
            Ok(Some(job_id)) => {
                // A base tool that carries a third party's words (`read_context`) taints the run
                // exactly as an extra does: the mark is what the later steps are judged against.
                let bare = payload
                    .tool_name
                    .strip_prefix("mcp__nucleos__")
                    .unwrap_or_default();
                if crate::mcp_tools::effect_of_call(&state.pool, bare, &payload.tool_input).await
                    == crate::mcp_tools::ToolEffect::ReadsUntrusted
                    && let Err(error) =
                        crate::execution::runs::mark_untrusted_context(&state.pool, run_id).await
                {
                    tracing::warn!(
                        run_id = run_id,
                        tool = bare,
                        %error,
                        "pretooluse-decision: could not record that a job node read third-party content - refusing the read"
                    );
                    return Json(Decision {
                        decision: "deny".to_owned(),
                        reason: "could not record that this run read third-party content"
                            .to_owned(),
                    });
                }
                tracing::info!(
                    run_id = run_id,
                    job_id = job_id,
                    tool = %payload.tool_name,
                    "pretooluse-decision: allowed a job-node tool"
                );
                return Json(Decision {
                    decision: "allow".to_owned(),
                    reason: "a job's run may use its box's base tools".to_owned(),
                });
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    run_id = run_id,
                    %error,
                    "pretooluse-decision: failed to resolve the run's job — failing closed"
                );
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not resolve the run's job — failing closed".to_owned(),
                });
            }
        }
    }

    // A job's node may also call an EXTRA tool its run's loadout lists. Same preconditions as the
    // base branch; a tool that is not listed is refused with the way to ask for it.
    if let Some(extra) = job_node_extra(&payload.tool_name)
        && is_in_flight
        && crate::execution::runs::runs_unattended(&mode)
    {
        match job_id_of(&state.pool, run_id).await {
            Ok(Some(job_id)) => {
                match crate::tool_loadout::run_lists_tool(&state.pool, run_id, extra).await {
                    Ok(true) => {
                        let effect = crate::mcp_tools::effect_of_call(
                            &state.pool,
                            extra,
                            &payload.tool_input,
                        )
                        .await;
                        if effect == crate::mcp_tools::ToolEffect::ReadsUntrusted
                            && let Err(error) =
                                crate::execution::runs::mark_untrusted_context(&state.pool, run_id)
                                    .await
                        {
                            tracing::warn!(
                                run_id = run_id,
                                tool = extra,
                                %error,
                                "pretooluse-decision: could not record that a job node read third-party content - refusing the read"
                            );
                            return Json(Decision {
                                decision: "deny".to_owned(),
                                reason: "could not record that this run read third-party content"
                                    .to_owned(),
                            });
                        }
                        tracing::info!(
                            run_id = run_id,
                            job_id = job_id,
                            tool = extra,
                            "pretooluse-decision: allowed a job-node extra tool its loadout lists"
                        );
                        return Json(Decision {
                            decision: "allow".to_owned(),
                            reason: "this run's loadout lists it".to_owned(),
                        });
                    }
                    Ok(false) => {
                        return Json(Decision {
                            decision: "deny".to_owned(),
                            reason: format!(
                                "{extra} is not available to an autonomous run: it is not in this run's loadout; ask the owner for it with request_tool"
                            ),
                        });
                    }
                    Err(error) => {
                        tracing::warn!(
                            run_id = run_id,
                            %error,
                            "pretooluse-decision: failed to read the run's loadout — failing closed"
                        );
                        return Json(Decision {
                            decision: "deny".to_owned(),
                            reason: "could not read the run's loadout — failing closed".to_owned(),
                        });
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    run_id = run_id,
                    %error,
                    "pretooluse-decision: failed to resolve the run's job — failing closed"
                );
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not resolve the run's job — failing closed".to_owned(),
                });
            }
        }
    }

    // The run's own project, off the row that already named it — and asked for only when the answer
    // could matter. `classifier::reads_shell_rules` is the same list the classifier's own branch
    // uses, so the filter cannot drift away from what it filters; see `shell_rules_of` for why a
    // read nobody would consult is still worth not taking.
    //
    // The GitHub policy comes from the same place under its OWN gate, and the two gates were one
    // until the rules learned about writes. `classify` consults `policy` only in `classify_segment`,
    // reached only for the two shell tools — a `Read` cannot be a `gh` line, and neither can an
    // `Edit`. But an `Edit` does now read the rules, so a shared gate would have started building a
    // per-project GitHub policy in front of every file edit, for an argument that branch can never
    // reach. `reads_github_policy` is the half that stayed still.
    let rules = if classifier::reads_shell_rules(&payload.tool_name) {
        shell_rules_of(&state, project_id.as_deref()).await
    } else {
        ProjectRules::none()
    };
    let policy = if classifier::reads_github_policy(&payload.tool_name) {
        github_policy_of(&state, project_id.as_deref()).await
    } else {
        std::borrow::Cow::Borrowed(&state.github.policy)
    };
    let classification = rules.downgrade_if_unreadable(classifier::classify(
        &payload.tool_name,
        &payload.tool_input,
        cwd.as_deref().map(Path::new),
        policy.as_ref(),
        rules.declared(),
        unrecognized_policy_for(&state.pool, run_id, &mode).await,
    ));
    tracing::info!(
        tool_name = %payload.tool_name,
        decision = %classification.decision.decision,
        action_class = classification.action_class,
        reason = %classification.reason,
        "pretooluse-decision: classified action"
    );

    if mode == "shadow" {
        // Gated on the run being in flight, which is what the mode lookup above used to guarantee
        // implicitly. A scoreboard is a record of decisions taken over live runs; a stray call
        // naming a finished run is not one of those.
        //
        // The digest is the EFFECTIVE policy's — the project's, where there is one — because the row
        // records a decision and this is the policy the decision was taken under. Recording the
        // machine's label beside a verdict a project's declaration produced would put the wrong
        // configuration on the evidence, which is the one thing this column exists to prevent.
        if is_in_flight {
            match shadow::record_decision(
                &state.pool,
                run_id,
                &payload.tool_name,
                &payload.tool_input,
                &classification,
                policy.digest(),
            )
            .await
            {
                Ok(decision_id) => {
                    if judge_snapshot.for_run(&mode) != crate::judge::JudgeMode::Off
                        && let Some(cwd) = cwd.as_deref()
                    {
                        crate::judge::observe_if_asked(
                            &state.pool,
                            &state.judge,
                            judge_asked(
                                &state,
                                run_id,
                                Some(decision_id),
                                project_id.as_deref(),
                                cwd,
                                &payload,
                                &classification,
                            ),
                        );
                    }
                }
                Err(error) => tracing::warn!(
                    run_id = run_id,
                    %error,
                    "pretooluse-decision: failed to record shadow decision"
                ),
            }
        }

        let read_only = matches!(payload.tool_name.as_str(), "Read" | "Grep" | "Glob")
            || (payload.tool_name == "Bash" && classification.action_class == "read-local");
        return if read_only {
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "shadow mode permits this read-only tool".to_owned(),
            })
        } else {
            Json(Decision {
                decision: "deny".to_owned(),
                reason: "shadow mode blocks tools that are not read-only".to_owned(),
            })
        };
    }

    let shadow_decision_id = if mode == "worktree" && is_in_flight {
        match shadow::record_decision(
            &state.pool,
            run_id,
            &payload.tool_name,
            &payload.tool_input,
            &classification,
            policy.digest(),
        )
        .await
        {
            Ok(decision_id) => Some(decision_id),
            Err(error) => {
                tracing::warn!(
                    run_id = run_id,
                    %error,
                    "pretooluse-decision: failed to record shadow decision"
                );
                None
            }
        }
    } else {
        None
    };

    // F2b steps 1-3: an in-flight worktree run that ran a verification command by itself is
    // recorded, after the decision above is taken. Under an explicit `verify_guard: refuse` the
    // call is refused here, before D12 / grants / judge / pause, so a verification command never
    // parks a run or fetches a person, and it is not counted against the prober allowance.
    // Step 3: detection reads the union of the worktree's map and the project root's, so the run
    // cannot escape by deleting, corrupting or widening its own copy.
    if mode == "worktree"
        && is_in_flight
        && matches!(payload.tool_name.as_str(), "Bash" | "PowerShell")
        && let (Some(project_id), Some(cwd)) = (project_id.as_deref(), cwd.as_deref())
        && observe_verification(
            &state,
            run_id,
            project_id,
            cwd,
            &payload,
            &classification.decision.decision,
            shadow_decision_id,
        )
        .await
    {
        tracing::info!(
            run_id,
            "pretooluse-decision: refused a verification command - the project verifies through the daemon"
        );
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: VERIFY_GUARD_REFUSAL.to_owned(),
        });
    }

    // **Spec B D12: a person already declined this exact action in this lineage.**
    //
    // Answered with the same no, without waking anybody: the answer was already given, and
    // without this the agent could ask again at once and put the same proposal back in front of
    // the owner, so the owner's no would stop meaning anything. Exact equality on the hashed
    // input, never "similar": deciding that two actions are the same by resemblance is a
    // judgement, and nothing here is a judge. A variant parks as it does today.
    //
    // HERE, ahead of spec A's judge and not just before the pause as spec B places it: a person's
    // decision comes before the judge, and with spec A in `enforce` a mark placed any later would
    // let the judge approve the very action the owner declined. And ahead of the grants below: a
    // grant covers a whole CLASS, a decline names one concrete action, and the specific refusal
    // wins over the general permission.
    //
    // Counted (D9): repeating what the owner refused is what the prober's brake exists for. And it
    // holds whatever `judge_resolve` says, `off` included: it is the owner's decision, not the
    // judge's.
    if classification.decision.decision == "pending_approval" && is_in_flight {
        match crate::proposals::declined_in_lineage(
            &state.pool,
            lineage_root,
            &payload.tool_name,
            &payload.tool_input.to_string(),
        )
        .await
        {
            Ok(true) => {
                let _ = count_denial_and_stop_a_prober(&state, run_id, &payload.tool_name).await;
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: A_PERSON_DECLINED_THIS.to_owned(),
                });
            }
            Ok(false) => {}
            // Falling through is the direction that asks a person again, which is today.
            Err(error) => tracing::warn!(
                run_id,
                %error,
                "pretooluse-decision: could not read the declined actions; asking as usual"
            ),
        }
    }

    // Class-scoped authorization (spec §8.4 step 6): once a human has approved an action, every
    // later action of that CLASS is allowed for the rest of the resume run, overriding the
    // pending_approval. Only a pending_approval is ever lifted — a `deny` (destructive) never
    // reaches this check, so a grant can never launder a denied action.
    //
    // The class is the key because neither of the alternatives is a boundary a human would
    // recognise: `tool_name` is "Bash" for every shell action, so an approved `git push` authorized
    // whatever this run tried next, while the exact input put the identical question a second time
    // for the next push. The class is what the human actually agreed to.
    if classification.decision.decision == "pending_approval" && is_in_flight {
        // Already taken over by the queue (migration 0054). Answered BEFORE the grant lookup and
        // before the pause below, because both would be wrong here: there is no grant to consume —
        // the approval deliberately minted none — and pausing would fetch a person to approve a
        // merge that is already queued, whose approval would queue it a second time.
        //
        // A `deny` rather than a pause, because the run is being told where its work went, not asked
        // to wait. Counted against the ordinary prober allowance, and that is deliberate: a run told
        // in its resume prompt and again in this reason that the merge is queued and must not be
        // retried, which does it three times regardless, is not obeying. One that reads either
        // message spends none of it.
        match crate::proposals::matching_queued_request(
            &state.pool,
            run_id,
            &payload.tool_name,
            &payload.tool_input.to_string(),
        )
        .await
        {
            Ok(Some(request_id)) => {
                tracing::info!(
                    run_id = run_id,
                    request_id,
                    "pretooluse-decision: the action is already queued — refusing the retry"
                );
                let _ = count_denial_and_stop_a_prober(&state, run_id, &payload.tool_name).await;
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: format!(
                        "this action was handed to the daemon's git queue as request {request_id} \
                         when it was approved, and will be carried out there — do not attempt it \
                         again"
                    ),
                });
            }
            Ok(None) => {}
            // Falling through to the grant lookup is the safe direction: the worst that follows is
            // a pause and a question for a person, which is what happened before any of this.
            Err(error) => {
                tracing::warn!(
                    run_id = run_id,
                    %error,
                    "pretooluse-decision: could not tell whether this action is already queued"
                );
            }
        }

        // **Not while the project's refusals are unreadable.** A saved grant and a declared git
        // operation are both authorizations derived from a decision about a command; while the list
        // that constrains either one cannot be read, there is no way to know that decision still
        // stands. Without this gate `downgrade_if_unreadable` is defeated for the rest of a resume
        // run by one earlier approval: it hands the grant lookup a `pending_approval` carrying an
        // allow-only class, and `read-local` covers `ls`, `cat`, `git status` and `cargo test`
        // between them. Measured — an outage, one grant of class `read-local`, and `ls -la` came
        // back `allow` under a project that denies `ls`. The declared-operation branch below has
        // the same failure in a sharper form: without this gate it would queue the forbidden push.
        //
        // `matching_queued_request` above is deliberately NOT gated. It answers a run that is
        // retrying something the queue already took over, which is a fact about a request that
        // exists and is true whatever the project's list says — and its answer is a refusal.
        if rules.were_read() {
            // **A declaration admits the operation; it never lets the shell perform it.** The
            // classifier has already said this call needs approval, and readable project rules
            // have had their chance to make that verdict a refusal. What the project declared in
            // advance replaces the pause and the question, not the queue: the run still receives a
            // `deny`, names the ticket now carrying its work, and must leave the operation alone.
            //
            // The declared list is deliberately the first I/O in this branch. A project that
            // granted nothing pays no git subprocess on every shell command that was going to ask,
            // which is the common case and the reason `session_git_decision`'s unavoidable probes
            // are not simply copied in front of every run. Every later failure falls through to the
            // existing approval path. That direction withholds autonomy without refusing an action
            // a person could still approve, and a failed submission leaves no ticket to name.
            let declared_git_decision: Option<Decision> = async {
                let project_id = project_id.as_deref()?;
                let declared = crate::project::policy::git_ops(&state.pool, project_id).await;
                if declared.is_empty() {
                    return None;
                }

                let cwd = cwd.as_deref()?;
                let command = payload.tool_input.get("command").and_then(Value::as_str)?;
                // The branch is read where the segment RUNS, not where the run stands: `cd x &&`
                // and `git -C x` move it. A directory change this reader cannot follow is not
                // auto-queued; it falls to the approval path, where a person sees the line.
                let segments = crate::execution::vcs::directed_segments(command);
                let segment = segments
                    .iter()
                    .find(|segment| queue_op(&segment.command, QUEUE_OP_PROBE_BRANCH).is_some())?;
                let cwd = match &segment.dir {
                    crate::execution::vcs::SegmentDir::Session => std::path::PathBuf::from(cwd),
                    crate::execution::vcs::SegmentDir::Steps(steps) => {
                        crate::execution::vcs::resolve_segment_dir(Path::new(cwd), steps)
                    }
                    crate::execution::vcs::SegmentDir::Unfollowable(_) => return None,
                };
                let deadline =
                    std::time::Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;
                let root = crate::execution::git_exec::toplevel(&cwd, deadline)
                    .await
                    .ok()?;
                let branch = crate::execution::git_exec::current_branch(&root, deadline)
                    .await
                    .ok()?;

                let worktree_project =
                    crate::execution::vcs::project_for_worktree(&state.pool, &root, deadline)
                        .await
                        .ok()?;
                if worktree_project != project_id {
                    return None;
                }
                let repo = crate::execution::vcs::resolve_repo(&state.pool, project_id)
                    .await
                    .ok()?;

                let op = queue_op(&segment.command, &branch)?;
                if !declared.iter().any(|kind| kind == op.kind()) {
                    return None;
                }

                let request_id = crate::execution::vcs::submit_declared(
                    &state.pool,
                    &repo,
                    &op,
                    crate::execution::vcs::Origin::Run(run_id),
                )
                .await
                .ok()?;
                Some(Decision {
                    decision: "deny".to_owned(),
                    reason: format!(
                        "this action was handed to the daemon's git queue as request {request_id} \
                         because {project_id} declared {} in advance, and will be carried out \
                         there — do not attempt it again",
                        op.kind()
                    ),
                })
            }
            .await;
            if let Some(decision) = declared_git_decision {
                tracing::info!(
                    run_id,
                    reason = %decision.reason,
                    "pretooluse-decision: admitted a project-declared git operation"
                );
                return Json(decision);
            }

            match crate::proposals::grant_covers_class(
                &state.pool,
                run_id,
                classification.action_class,
            )
            .await
            {
                Ok(true) => {
                    tracing::info!(
                        run_id = run_id,
                        tool = %payload.tool_name,
                        action_class = classification.action_class,
                        "pretooluse-decision: a grant covers this action class — authorizing the action"
                    );
                    let _ = crate::feed::append(
                        &state.pool,
                        None,
                        "action_authorized",
                        &format!(
                            "authorized approved {} action for run {}",
                            payload.tool_name, run_id
                        ),
                        Some(run_id),
                        Some(&crate::feed::run_subject(&state.pool, run_id).await),
                    )
                    .await;
                    return Json(Decision {
                        decision: "allow".to_owned(),
                        reason: format!(
                            "approved authorization for the {} action class",
                            classification.action_class
                        ),
                    });
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        run_id = run_id,
                        %error,
                        "pretooluse-decision: grant lookup failed; falling back to the classifier decision"
                    );
                }
            }
        }
    }

    // **The judge's point (spec A D4), and why it is exactly here.**
    //
    // - After the queued request, the declared git operation and the grants: each is an explicit
    //   human decision or already returned its own verdict, and a judge ahead of them could refuse
    //   what a person approved and spend that person's denial allowance. It also means the
    //   judge is NOT free of the declared-git branch: that branch's git probes (each allowed
    //   up to 300 s) run first and can return `None`, and the judge's wait then follows them.
    //   Both sit inside the 5 s the hook's caller allows, so the judge is given only what
    //   `judge_wait(started)` says is left, and below that it merely observes.
    // - OUTSIDE the `pending && in_flight` block above, because a classifier `allow` never enters
    //   it, and D6 sends writes the classifier allowed to the judge.
    // - Before the unattended conversions and the pause below, which only ever see what the judge
    //   left alone.
    // - In flight only: without it there is no cwd, no task and no run to charge.
    //
    // Observe changes nothing and waits for nothing. The judge NEVER rewrites `classification`:
    // the generic counter below counts every `deny` of a run in flight, and a judge's refusal must
    // be counted by D7's rule and not by that one.
    //
    // Spec B D10: with both judges in `enforce`, the resolver's park question is put HERE, beside
    // spec A's and in parallel with it, and its answer is carried to the E3 point below. The
    // eligibility is built once, for both places that read it. `early_park` records itself as
    // `moot` if the hook returns before the E3 point (D13); `b_stood_aside` says the resolver
    // already declined to speak here (a resolution lineage, or a lineage read that failed or ran
    // out of time), so the E3 point does not ask again.
    let eligibility = crate::judge::resolve::Eligibility {
        in_flight: is_in_flight,
        run_mode: &mode,
        job_id,
        resolution_lineage: false,
        resolve: resolve_snapshot,
        dont_ask: permission == crate::chats::PermissionMode::DontAsk,
        action_class: classification.action_class,
    };
    let mut early_park: Option<crate::judge::resolve::PendingPark> = None;
    let mut b_stood_aside = false;
    if mode == "worktree"
        && is_in_flight
        && let Some(cwd) = cwd.as_deref()
    {
        match judge_snapshot.for_run(&mode) {
            crate::judge::JudgeMode::Off => {}
            crate::judge::JudgeMode::Observe => crate::judge::observe_if_asked(
                &state.pool,
                &state.judge,
                judge_asked(
                    &state,
                    run_id,
                    shadow_decision_id,
                    project_id.as_deref(),
                    cwd,
                    &payload,
                    &classification,
                ),
            ),
            // Not while the project's rules are unreadable (review item 9), the gate the grants and
            // declared git operations above already have: a refusal nobody can read is not a
            // permission, and an approval by the judge would lift a `pending_approval` that
            // `downgrade_if_unreadable` may have produced for exactly that reason. The judge still
            // observes, so the call is measured.
            crate::judge::JudgeMode::Enforce if !rules.were_read() => {
                crate::judge::observe_if_asked(
                    &state.pool,
                    &state.judge,
                    judge_asked(
                        &state,
                        run_id,
                        shadow_decision_id,
                        project_id.as_deref(),
                        cwd,
                        &payload,
                        &classification,
                    ),
                )
            }
            // Out of budget: the hook's caller gives up at `HOOK_BUDGET`, and a ruling that arrives
            // after it is recorded and never delivered. Observe only; the classifier decides.
            crate::judge::JudgeMode::Enforce if judge_wait(started).is_none() => {
                crate::judge::observe_if_asked(
                    &state.pool,
                    &state.judge,
                    judge_asked(
                        &state,
                        run_id,
                        shadow_decision_id,
                        project_id.as_deref(),
                        cwd,
                        &payload,
                        &classification,
                    ),
                )
            }
            crate::judge::JudgeMode::Enforce => {
                let a_asked = judge_asked(
                    &state,
                    run_id,
                    shadow_decision_id,
                    project_id.as_deref(),
                    cwd,
                    &payload,
                    &classification,
                );
                // Read again at the call: the budget only shrinks, and the guard above held a moment ago.
                let wait = judge_wait(started).unwrap_or(JUDGE_FLOOR);
                // Spec B D10: the resolver is asked here only where the E3 point would ask it in
                // `enforce` — a real park, eligible, with the project's rules read.
                let b_parks_here = resolve_snapshot == crate::judge::JudgeMode::Enforce
                    && rules.were_read()
                    && classification.decision.decision == "pending_approval"
                    && crate::judge::resolve::park_eligible(&eligibility);
                // Both questions at once. Each holds itself to the same `wait`, so the pair takes
                // as long as the slower of the two, never the sum. The lineage read (D2/S1) is
                // INSIDE the resolver's future and its budget, never before the join: outside it,
                // a slow read would be added to spec A's whole budget.
                let ruling = if b_parks_here {
                    let run = RunSnapshot {
                        run_id,
                        lineage_root,
                        shadow_decision_id,
                        project_id: project_id.as_deref(),
                        cwd,
                    };
                    let b_asked = resolve_asked(
                        &state,
                        crate::judge::resolve::Event::Park,
                        &run,
                        &payload,
                        &classification,
                    );
                    let (ruling, b_row) = tokio::join!(
                        crate::judge::enforce_if_asked(&state.pool, &state.judge, a_asked, wait),
                        crate::judge::resolve::ask_unless_resolution(
                            &state.pool,
                            &state.judge,
                            &b_asked,
                            wait,
                        ),
                    );
                    match (&ruling, b_row) {
                        (crate::judge::Ruling::Classifier, Some(b_row)) => {
                            early_park =
                                Some(crate::judge::resolve::PendingPark::new(&state.pool, b_row));
                        }
                        // The resolver stood aside: the park stands, and the E3 point must not
                        // ask again.
                        (crate::judge::Ruling::Classifier, None) => b_stood_aside = true,
                        // Spec A decided the call; the resolver's answer no longer matters. Its
                        // cost still counts, and it never enters the review queue (D13).
                        (_, Some(b_row)) => crate::judge::resolve::record_later(
                            &state.pool,
                            b_row.settled(crate::judge::resolve::Outcome::Moot, false),
                        ),
                        (_, None) => {}
                    }
                    ruling
                } else {
                    crate::judge::enforce_if_asked(&state.pool, &state.judge, a_asked, wait).await
                };
                match ruling {
                    crate::judge::Ruling::Allow { reason } => {
                        return Json(Decision {
                            decision: "allow".to_owned(),
                            reason,
                        });
                    }
                    // The judge's own `Decision`, and its own count: `classification` stays what
                    // the classifier said, so the generic counter below never sees this refusal.
                    crate::judge::Ruling::Deny { reason, counts } => {
                        if counts {
                            let _ =
                                count_denial_and_stop_a_prober(&state, run_id, &payload.tool_name)
                                    .await;
                        }
                        return Json(Decision {
                            decision: "deny".to_owned(),
                            reason,
                        });
                    }
                    crate::judge::Ruling::Classifier => {}
                }
            }
        }
    }

    // A denied action is answered and then counted. Answering was all this used to do, which left
    // the run free to try the next spelling immediately — and against an unbounded number of
    // attempts a lexical classifier is a puzzle with a solution rather than a boundary. The two
    // verdicts were also the wrong way round in cost: `pending_approval` stopped the run and
    // fetched a human, while `deny` — the harsher judgement — cost the run nothing at all.
    if classification.decision.decision == "deny" && is_in_flight {
        let counted = count_denial_and_stop_a_prober(&state, run_id, &payload.tool_name).await;
        // Spec B E1 (D9): only on `Counted`. A spent allowance (`Stopped`) or a count that could
        // not be written (`NotCounted`) is never put to the judge: the limit exists to stop an
        // agent searching the classifier's grammar, and the judge has no defence against injection.
        // The judge may stop a run before the limit, never after it. Only the B's questions are
        // asked here (D10: a hard refusal never reaches spec A's judge). Observe decides nothing:
        // the answer is already `deny` and stays `deny` (D11).
        if matches!(counted, DenialCount::Counted(_))
            && let Some(cwd) = cwd.as_deref()
        {
            // The lineage read (D2) is NOT here. In observe, `observe` does it inside its own task,
            // off the response path (D10); in enforce, `ask_unless_resolution` does it inside the
            // question's deadline.
            if crate::judge::resolve::hard_deny_eligible(&eligibility) {
                let run = RunSnapshot {
                    run_id,
                    lineage_root,
                    shadow_decision_id,
                    project_id: project_id.as_deref(),
                    cwd,
                };
                let asked = resolve_asked(
                    &state,
                    crate::judge::resolve::Event::HardDeny,
                    &run,
                    &payload,
                    &classification,
                );
                match resolve_snapshot {
                    crate::judge::JudgeMode::Off => {}
                    crate::judge::JudgeMode::Observe => {
                        crate::judge::resolve::observe(&state.pool, &state.judge, asked)
                    }
                    // Out of the hook's budget, as spec A's hook does: a ruling that arrives after
                    // the caller gave up is never delivered. Observe only; the refusal stands.
                    crate::judge::JudgeMode::Enforce if judge_wait(started).is_none() => {
                        crate::judge::resolve::observe(&state.pool, &state.judge, asked)
                    }
                    // No `Enforce if !rules.were_read()` arm here, unlike the E3 point: spec A's
                    // lock exists so the judge never approves over a refusal nobody can read. Here
                    // the refusal is already given and stays, and stopping the run or telling the
                    // owner never widens what the run may do.
                    crate::judge::JudgeMode::Enforce => {
                        resolve_hard_deny(
                            &state,
                            run_id,
                            project_id.as_deref(),
                            classification.action_class,
                            &asked,
                            judge_wait(started).unwrap_or(JUDGE_FLOOR),
                        )
                        .await
                    }
                }
            }
        }
    }

    // **An unrecognized tool is refused, not parked, and the two are not the same verdict.**
    //
    // Parking exists to put a decision in front of a person: writes outside the workspace, changes
    // to the files that ARE the policy, a git operation with consequences. Those keep parking, and
    // must — the whole design is that nobody but the owner authorises them.
    //
    // `unrecognized-tool` is not that, and the hyphen is load-bearing. Until 2026-08-28 both this
    // and the shell path's unrecognized COMMANDS shared one label, and keying on it here refused
    // `git branch -D`, `cargo fix` and `gh run list` -- real decisions -- while meaning to refuse
    // only tool names. Two job-node tests caught it, and the classifier now names the two apart.
    //
    // A tool nobody has reasoned about is a capability nobody has bounded, which is a gap in
    // `classifier.rs` rather than a question about the work. Parked, it stopped an autonomous run
    // dead and minted a proposal saying, in effect,
    // "somebody please decide about WebSearch" — a question no owner asleep at 4am was going to
    // answer, and one that has the same answer every time. Measured 2026-08-27: two overnight
    // attempts died exactly here, hours of work each, having asked for a tool once.
    //
    // Refused, the run is told no and carries on with the tools it has, which is what a person
    // would have replied. **Deliberately not counted by `count_denial_and_stop_a_prober`**: that
    // counter is the guard against searching a grammar for a destructive command that gets through,
    // and reaching for an absent tool is not that search. `runner::DEFAULT_MAX_TURNS` is what bounds
    // a run that will not take no for an answer.
    //
    // Scoped to the unattended modes by the one definition of that word (`runs::runs_unattended`),
    // and not applied more widely, because the premise is literally "nobody is awake". A `real`-mode
    // run has its owner at the window, and for them the pause is exactly right: they can approve it
    // in ten seconds.
    if classification.decision.decision == "pending_approval"
        && classification.action_class == "unrecognized-tool"
        && crate::execution::runs::runs_unattended(&mode)
    {
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: format!(
                "{} is not available to an autonomous run. Nobody is awake to approve it, so this                  is a refusal and not a pause: do not retry it, and do the work with the tools you                  have.",
                payload.tool_name
            ),
        });
    }

    // **A park that cannot be answered is a refusal that also destroys an item, so it is spelled as
    // a refusal.** See `a_park_here_would_only_destroy` for the whole argument; the short of it is
    // that a job node's park never becomes an `action-approval` — it becomes a `skipped-item` — so
    // the command is refused either way and the only question is whether the item's work survives
    // the asking. Job 22 on 2026-08-30 is the measurement: it asked about one path outside its
    // worktree, was correctly refused, and lost the whole item plus everything it had already
    // written for it.
    //
    // Placed AFTER the two `deny` branches above and before the pause below, so it can only ever
    // convert a `pending_approval`, never soften a `deny`.
    if classification.decision.decision == "pending_approval"
        && is_in_flight
        && a_park_here_would_only_destroy(&state.pool, run_id, &mode).await
    {
        tracing::info!(
            run_id,
            tool_name = %payload.tool_name,
            action_class = classification.action_class,
            "pretooluse-decision: refused rather than parked — a park here would only end the item"
        );
        record_refused_action(&state, &payload, &payload.tool_name, &classification.reason).await;
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: format!("{} {A_PARK_HERE_WOULD_END_THE_ITEM}", classification.reason),
        });
    }

    // **The rung that asks nobody, on the side of the house where nobody was ever going to be
    // asked.** `dont_ask` has governed a CONVERSATION since `0132`: it permits precisely what
    // `auto` permits and refuses everything `auto` would have stopped to ask about. Every word
    // of that describes an unattended run better than it describes a chat, and until now the
    // column was read on this path and never consulted — `permission` was bound at the top of
    // this function and reached only `rooted_decision`.
    //
    // What it replaces is not an approval. It is a park nobody answers: `finalize_termination`
    // kills the CLI, the row sits in `awaiting_approval`, and `concurrency::LIVE_RUN_STATUSES`
    // holds that project's slot for as long as it sits there — the orphaned-slot sweep spares
    // the status by name, and `reconcile_stranded_approvals` spares a row whose proposal is
    // pending, which this one's is. If the run belongs to a job, `node_awaiting_approval` pauses
    // the whole job behind it. So the choice this rung offers is not "ask or refuse"; it is
    // "refuse one call and finish the work" against "stop everything until somebody looks".
    //
    // The three positions are the rooted rung's, for the rooted rung's reasons, and they are
    // load-bearing:
    //
    // - **After both `deny` returns above**, so this can only ever convert a `pending_approval`.
    //   Moved above them it would restate their refusals with a weaker reason and lose the
    //   sentence each of them exists to say.
    // - **After every allow**, which on this path means after `classify` itself: an allow never
    //   reaches here, so the promise "exactly what `auto` runs" holds without a line of code.
    // - **Before the pause**, which is the whole of the change: nothing is terminated, no
    //   `action-approval` is minted, no slot is held and no job stops.
    //
    // Deliberately NOT scoped to `runs_unattended`. The two refusals above are, because both
    // infer that nobody is watching from the run's shape; this one was TOLD, by whoever created
    // the run, and a `real`-mode run whose author asked for this rung asked for it knowingly.
    // Inferring over an explicit instruction is how a control comes to mean nothing.
    //
    // The refusal is recorded for the same reason `a_park_here_would_only_destroy` records one:
    // a call refused with nobody present leaves no other trace, and an owner reading back has to
    // be able to see what their run was stopped from doing.
    if classification.decision.decision == "pending_approval"
        && permission == crate::chats::PermissionMode::DontAsk
    {
        tracing::info!(
            run_id,
            tool_name = %payload.tool_name,
            action_class = classification.action_class,
            "pretooluse-decision: refused rather than parked — this run asks nobody"
        );
        record_refused_action(&state, &payload, &payload.tool_name, &classification.reason).await;
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: format!("{}{DONT_ASK_CLAUSE}", classification.reason),
        });
    }

    // Spec B E3, D10: after the branches above that already turn a park into a refusal
    // (`unrecognized-tool`, a job node's park, `dont_ask`), so only a real park is asked about;
    // after spec A's judge, so what reaches here is what it left alone. In observe the question is
    // detached and never delays the hook, and the park below is unchanged (D11). In enforce the
    // resolver may stop the run, or refuse without parking (D5); anything else — a failure, the
    // deadline, no opinion, a redirect D5 does not allow — is the park below, as today (D1).
    if classification.decision.decision == "pending_approval"
        && is_in_flight
        && let Some(cwd) = cwd.as_deref()
        && crate::judge::resolve::park_eligible(&eligibility)
    {
        let run = RunSnapshot {
            run_id,
            lineage_root,
            shadow_decision_id,
            project_id: project_id.as_deref(),
            cwd,
        };
        let asked = resolve_asked(
            &state,
            crate::judge::resolve::Event::Park,
            &run,
            &payload,
            &classification,
        );
        match resolve_snapshot {
            crate::judge::JudgeMode::Off => {}
            crate::judge::JudgeMode::Observe => {
                crate::judge::resolve::observe(&state.pool, &state.judge, asked)
            }
            // As spec A's hook does: with the project's rules unread, a refusal nobody can read is
            // not a permission, so the resolver only observes — no redirect over it, and no stop
            // decided on a picture that is missing the project's own refusals.
            crate::judge::JudgeMode::Enforce if !rules.were_read() => {
                crate::judge::resolve::observe(&state.pool, &state.judge, asked)
            }
            crate::judge::JudgeMode::Enforce => {
                // The lineage read runs inside the question's budget here too (D10), alone or
                // carried from spec A's point; `None` is the resolver standing aside, and the park
                // below stands — never an allow.
                let row = match early_park.take() {
                    Some(carried) => carried.take(),
                    None if b_stood_aside => None,
                    None => match judge_wait(started) {
                        Some(wait) => {
                            crate::judge::resolve::ask_unless_resolution(
                                &state.pool,
                                &state.judge,
                                &asked,
                                wait,
                            )
                            .await
                        }
                        // Out of the hook's budget, as spec A's hook does: observe only, and park.
                        None => {
                            crate::judge::resolve::observe(&state.pool, &state.judge, asked);
                            None
                        }
                    },
                };
                if let Some(row) = row
                    && let Some(decision) = resolve_park(
                        &state,
                        &payload,
                        &classification,
                        &run,
                        row,
                        judge_wait(started),
                    )
                    .await
                {
                    return Json(decision);
                }
            }
        }
    }
    // Whatever was carried and not taken is recorded `moot` here at the latest (D13).
    drop(early_park);

    if classification.decision.decision == "pending_approval" {
        // Only for a genuinely in-flight run_id — an unknown/stale one must not terminate anything.
        if is_in_flight {
            // In its own task, on purpose. Terminating the run kills the CLI whose hook script owns
            // the connection this handler is answering, and that script gives up after 5s anyway
            // (`ask_daemon.py`'s `timeout=5`) — so the request can disappear mid-handler, and a
            // dropped request drops the handler future exactly the way `abort()` does. Awaiting the
            // JoinHandle keeps the response as synchronous as before; dropping a JoinHandle only
            // detaches its task, so the pause still gets recorded when the request goes away.
            let _ = tokio::spawn(pause_for_approval(
                state.clone(),
                run_id,
                payload.tool_name.clone(),
                payload.tool_input.to_string(),
                classification.reason.clone(),
            ))
            .await;
        } else {
            tracing::warn!(
                "pretooluse-decision: pending_approval for unknown/finished run_id {} — not terminating",
                run_id
            );
        }
    }

    Json(classification.decision)
}
