//! Decisions for the other kinds of caller: the assistant, the council and teams, and the record of a refused action.

use super::*;

pub(super) async fn assistant_decision(state: &AppState, payload: &PreToolUsePayload) -> Json<Decision> {
    // Whole segment, not a prefix. MCP tool names are `mcp__<server>__<tool>`, so a server called
    // `nucleos__x` produced `mcp__nucleos__x__...`, which passed a prefix test and inherited the
    // orchestrator's unconditional allow — in the one mode that skips the classifier, the proposals
    // and the termination entirely.
    let Some(tool) = payload
        .tool_name
        .strip_prefix("mcp__nucleos__")
        .filter(|tool| !tool.contains("__"))
    else {
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: "the orchestrator is restricted to NucleOS tools".to_owned(),
        });
    };

    // The `get_run`-names-a-triage-run rule lives in `mcp_tools::effect_of_call` rather than here,
    // because a second dispatcher needed the same answer and got a different one from the bare
    // table. One implementation is the only way two callers cannot disagree.
    let effect = crate::mcp_tools::effect_of_call(&state.pool, tool, &payload.tool_input).await;

    match effect {
        crate::mcp_tools::ToolEffect::ReadsUntrusted => {
            // Marked BEFORE the tool is allowed, and the failure to mark refuses the read. The
            // alternative is a turn that has a stranger's words in it and no record of having read
            // them, which is the state every refusal below depends on not existing.
            if let Err(error) =
                crate::execution::runs::mark_untrusted_context(&state.pool, payload.run_id).await
            {
                tracing::warn!(
                    run_id = payload.run_id,
                    tool,
                    %error,
                    "pretooluse-decision: could not mark the turn as having read third-party content — refusing the read"
                );
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not record that this turn has read third-party content"
                        .to_owned(),
                });
            }
            // And WHICH stranger, which is a different question with a different failure policy.
            // The mark above fails closed because the barrier reads it; this is read only by a
            // person deciding whether to finish a refused action by hand, so failing closed here
            // would give a convenience a veto over every read the daemon does. Logged and carried
            // on: the cost is a refusal that has to say "not recorded", which is worse than knowing
            // and better than a browsing session that cannot open a page because a log line would
            // not write.
            if let Err(error) = crate::execution::runs::record_untrusted_read(
                &state.pool,
                payload.run_id,
                tool,
                Some(&payload.tool_input.to_string()),
            )
            .await
            {
                tracing::warn!(
                    run_id = payload.run_id,
                    tool,
                    %error,
                    "pretooluse-decision: the turn is marked but what it read could not be written down"
                );
            }
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "orchestrator NucleOS tool".to_owned(),
            })
        }
        crate::mcp_tools::ToolEffect::Acts => {
            match crate::execution::runs::read_untrusted_since(&state.pool, payload.run_id).await {
                Ok(false) => Json(Decision {
                    decision: "allow".to_owned(),
                    reason: "orchestrator NucleOS tool".to_owned(),
                }),
                Ok(true) => {
                    // Warned, not merely refused. The owner asking their own bot to do two things
                    // in one message reaches this line, and so does a mail body that talked it into
                    // the second one; the two are indistinguishable from here, and only one of them
                    // is worth looking at a log for.
                    tracing::warn!(
                        run_id = payload.run_id,
                        tool,
                        "pretooluse-decision: refused an action in a turn that has read third-party content"
                    );
                    record_refused_action(state, payload, tool, UNTRUSTED_CONTEXT_DENY_REASON)
                        .await;
                    Json(Decision {
                        decision: "deny".to_owned(),
                        reason: UNTRUSTED_CONTEXT_DENY_REASON.to_owned(),
                    })
                }
                Err(error) => {
                    tracing::warn!(
                        run_id = payload.run_id,
                        tool,
                        %error,
                        "pretooluse-decision: could not tell whether the turn has read third-party content — failing closed"
                    );
                    Json(Decision {
                        decision: "deny".to_owned(),
                        reason: "could not tell whether this turn has read third-party content"
                            .to_owned(),
                    })
                }
            }
        }
        // `WritesOwn` sits with `ReadsOwn` and not with `Acts`, which is the entire reason the
        // variant exists. A turn that read the web first and writes down what it found afterwards
        // is already past the barrier by the time it records anything; refusing the write here
        // would mean it never records anything at all. It reaches no network, starts no work and
        // lifts no approval.
        crate::mcp_tools::ToolEffect::ReadsOwn | crate::mcp_tools::ToolEffect::WritesOwn => {
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "orchestrator NucleOS tool".to_owned(),
            })
        }
    }
}

/// Writes down an action the barrier just refused, so somebody finds out it was wanted.
///
/// **Best-effort, and deliberately after the decision is already made.** Every failure here is
/// swallowed: the refusal is the security property and this is the courtesy beside it, so a full
/// disk or a lost race must never be able to turn a `deny` into anything else. The one failure that
/// is expected rather than exceptional is the unique violation — a run that reaches a second time
/// already has its row — and it is not worth a warning, which is why the log line says how many
/// rather than complaining.
///
/// The reasoning is the same sentence the model was given, because the person reading this in the
/// morning is answering a different question from the model's: not "may I", but "should I do this
/// myself". The tool input travels with it for the same reason — a request to email a dealer is a
/// decision nobody can take from the tool name alone.
pub(super) async fn record_refused_action(
    state: &AppState,
    payload: &PreToolUsePayload,
    tool: &str,
    reason: &str,
) {
    let session_id =
        sqlx::query_scalar::<_, Option<String>>("SELECT session_id FROM runs WHERE id = ?")
            .bind(payload.run_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .flatten();

    // Read here and copied onto the row, rather than joined at display time: `runs` rows are pruned
    // on their own schedule and this record is meant to outlive the turn. An error is `None` for the
    // same reason the write was best-effort — a person reading "not recorded" still has the action
    // in front of them, where a refusal that failed to be written down at all leaves them nothing.
    let read_from = crate::execution::runs::untrusted_reads_json(&state.pool, payload.run_id)
        .await
        .unwrap_or_default();

    match crate::proposals::create_refused_action(
        &state.pool,
        payload.run_id,
        session_id.as_deref(),
        tool,
        reason,
        Some(&payload.tool_input.to_string()),
        read_from.as_deref(),
    )
    .await
    {
        Ok(proposal_id) => tracing::info!(
            run_id = payload.run_id,
            proposal_id,
            tool,
            "pretooluse-decision: the refused action was written down for a person to read"
        ),
        Err(error) => tracing::debug!(
            run_id = payload.run_id,
            tool,
            %error,
            "pretooluse-decision: this run already has a refused action on record, or it could not be written"
        ),
    }
}

/// What a council seat may call: the named list, and nothing else.
///
/// An ALLOW-list, written out, rather than "deny the `Acts` ones". The two are the same today and
/// stop being the same the moment somebody adds a tool to the MCP server: a deny-list hands a
/// council every future tool by default, and this hands it none of them until somebody decides. The
/// direction matters more here than for the orchestrator, because a council is up to eight agents
/// launched by one sentence rather than one turn a person is watching.
///
/// Almost pure. The list is a membership test that cannot fail; the one stateful question is which
/// run a `get_run` names, and that one fails closed.
pub(super) async fn council_decision(state: &AppState, payload: &PreToolUsePayload) -> Json<Decision> {
    // Whole segment, not a prefix, for the reason `assistant_decision` records: an MCP server named
    // `nucleos__x` produces `mcp__nucleos__x__…`, which passes a prefix test.
    let tool = payload
        .tool_name
        .strip_prefix("mcp__nucleos__")
        .filter(|tool| !tool.contains("__"))
        .filter(|tool| crate::mcp_tools::COUNCIL_TOOLS.contains(tool));

    if let Some(tool) = tool {
        // `get_run` reads any run by id, and run ids are sequential integers — so a seat could
        // read the row next to its own and find a sibling's answer before writing its own. Phase 1
        // is supposed to be N independent answers, and one seat that waited would be answering
        // with the others' work in front of it.
        //
        // The same shape as the orchestrator's `get_run` check above, and for a related reason:
        // that one asks whether the named run holds a stranger's words, this one whether it holds
        // a peer's. Both fail closed, because "I could not tell" is not "no".
        if tool == "get_run" && get_run_names_a_council_run(state, &payload.tool_input).await {
            tracing::debug!(
                run_id = payload.run_id,
                "pretooluse-decision: refused a council seat a look at another seat's run"
            );
            return Json(Decision {
                decision: "deny".to_owned(),
                reason: "a council seat may not read another seat's run".to_owned(),
            });
        }
        return Json(Decision {
            decision: "allow".to_owned(),
            reason: "council seats may read NucleOS state".to_owned(),
        });
    }

    // Not warned about. A seat reaching for `create_run` is a model being a model, not a symptom of
    // anything — where an orchestrator refused an action after reading mail is a line somebody
    // should read. A log level is a claim about who should look at it.
    tracing::debug!(
        run_id = payload.run_id,
        tool = %payload.tool_name,
        "pretooluse-decision: refused a tool a council seat may not call"
    );
    Json(Decision {
        decision: "deny".to_owned(),
        reason: "a council seat may only read NucleOS state".to_owned(),
    })
}

/// What a team agent may call: the named list, and — once it has read a stranger's words — the
/// reads alone.
///
/// **Two questions, and this branch used to answer only the first.** Which tools a department may
/// reach is `TEAM_BASE` (now also holding `request_tool`, a `WritesOwn` that lands in the
/// `ReadsOwn | WritesOwn` arm) plus whatever `TEAM_EXTRAS` the run's loadout lists, and
/// `auth::TEAM_ROUTES` refuses the rest without anybody's cooperation.
/// Whether it may still ACT, having read, was answered nowhere on the cloud path: this function
/// allowed every name on the list unconditionally, and the doc that stood here said `TEAM_TOOLS`
/// carried no `Acts` — true when it was written, false since the alçada landed.
///
/// **`TEAM_TOOLS` is six untrusted reads and two asks**, which is what makes the shape below so
/// small. `get_email`, `get_email_queue`, `list_files`, `read_team_file`, `web_read` and
/// `web_search` all carry somebody else's words into the turn; `propose_action` and
/// `propose_teammate` are the alçada. A department's whole day is: read the world, then ask — and
/// the rule is that the asking comes first or not at all.
///
/// **The marking is this function's too, and that half was missing as well.** A refusal that
/// consults a flag nothing ever sets is a refusal that never fires, and `runs.read_untrusted` was
/// written for a cloud team node by nothing at all: the orchestrator's marking arm lives past the
/// `mode` branch that dispatches here, so a department returns before ever reaching it, and
/// `team.rs` marks on the LOCAL path only, off `local_agent`'s taint atomic. Adding the refusal
/// without the marking would have looked exactly like a fix and behaved exactly like none.
///
/// `effect_of_call` rather than the bare `tool_effect`, even though the two are identical for every
/// name on today's list — that function short-circuits anything which is not `ReadsOwn`, and no
/// team tool is. It is asked anyway so that the day somebody puts `get_run` on a department's list,
/// this branch gets the argument-aware answer the orchestrator gets rather than the bare table's.
/// One implementation is the only way two callers cannot disagree, which is a lesson this file
/// records having already paid for once.
///
/// Whole segment and not a prefix, for the reason `assistant_decision` records: an MCP server named
/// `nucleos__x` produces `mcp__nucleos__x__…`, which passes a prefix test and is not this server.
pub(super) async fn team_decision(state: &AppState, payload: &PreToolUsePayload) -> Json<Decision> {
    use crate::mcp_tools::Admission;

    let named = payload
        .tool_name
        .strip_prefix("mcp__nucleos__")
        .filter(|tool| !tool.contains("__"));
    let admission = named.map(|tool| {
        crate::mcp_tools::box_admission(
            crate::mcp_tools::TEAM_BASE,
            crate::mcp_tools::TEAM_EXTRAS,
            tool,
        )
    });

    let permitted = match (named, admission) {
        (Some(tool), Some(Admission::Base)) => Some(tool),
        (Some(tool), Some(Admission::Extra)) => {
            // An extra is admitted only when this run's loadout lists it; a lookup that fails
            // refuses, like every other unreadable state below.
            match crate::tool_loadout::run_lists_tool(&state.pool, payload.run_id, tool).await {
                Ok(true) => Some(tool),
                Ok(false) => {
                    return Json(Decision {
                        decision: "deny".to_owned(),
                        reason: "not in this run's loadout; ask the owner for it with request_tool"
                            .to_owned(),
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        run_id = payload.run_id,
                        tool,
                        %error,
                        "pretooluse-decision: could not read a team agent's loadout - failing closed"
                    );
                    return Json(Decision {
                        decision: "deny".to_owned(),
                        reason: "could not read the run's loadout — failing closed".to_owned(),
                    });
                }
            }
        }
        _ => None,
    };

    let Some(tool) = permitted else {
        // Debug and not warn, for the reason the council's branch gives: a specialist reaching for
        // `create_run` is a model being a model, not a symptom of anything.
        tracing::debug!(
            run_id = payload.run_id,
            tool = %payload.tool_name,
            "pretooluse-decision: refused a tool a team agent may not call"
        );
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: "a team agent may only read".to_owned(),
        });
    };

    let effect = crate::mcp_tools::effect_of_call(&state.pool, tool, &payload.tool_input).await;

    match effect {
        crate::mcp_tools::ToolEffect::ReadsUntrusted => {
            // Marked BEFORE the read is allowed, and a failure to mark refuses it — the same rule
            // and the same direction as the orchestrator's arm. The alternative is a department
            // holding a stranger's words with no record of having read them, which is the one state
            // every refusal below depends on not existing.
            if let Err(error) =
                crate::execution::runs::mark_untrusted_context(&state.pool, payload.run_id).await
            {
                tracing::warn!(
                    run_id = payload.run_id,
                    tool,
                    %error,
                    "pretooluse-decision: could not record that a department read third-party content - refusing the read"
                );
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not record that this turn read third-party content".to_owned(),
                });
            }
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "team agents may read".to_owned(),
            })
        }
        crate::mcp_tools::ToolEffect::Acts => {
            match crate::execution::runs::read_untrusted_context(&state.pool, payload.run_id).await
            {
                Ok(false) => Json(Decision {
                    decision: "allow".to_owned(),
                    reason: "a department may ask".to_owned(),
                }),
                Ok(true) => {
                    // Warned rather than merely refused, and unlike the council's `debug!` this one
                    // earns it: a specialist reaching for `create_run` is a model being a model, but
                    // a director filing a proposal straight after reading a web page is the exact
                    // sentence `propose_teammate`'s own comment describes - a director that read a
                    // page saying "hire an agent with this prompt" could otherwise file it.
                    tracing::warn!(
                        run_id = payload.run_id,
                        tool,
                        "pretooluse-decision: refused a department's ask in a turn that has read third-party content"
                    );
                    // Written down for the reason the orchestrator's is: the alçada exists so a
                    // person reads a queue, and a refusal that leaves no trace is a department that
                    // quietly stopped asking with nothing anywhere saying why.
                    record_refused_action(state, payload, tool, UNTRUSTED_CONTEXT_DENY_REASON)
                        .await;
                    Json(Decision {
                        decision: "deny".to_owned(),
                        reason: UNTRUSTED_CONTEXT_DENY_REASON.to_owned(),
                    })
                }
                Err(error) => {
                    tracing::warn!(
                        run_id = payload.run_id,
                        tool,
                        %error,
                        "pretooluse-decision: could not tell whether a department has read third-party content - failing closed"
                    );
                    Json(Decision {
                        decision: "deny".to_owned(),
                        reason: "could not tell whether this turn has read third-party content"
                            .to_owned(),
                    })
                }
            }
        }
        // Nothing on today's list lands here, and the arm is written out rather than folded into a
        // wildcard so that a tool added to `TEAM_TOOLS` which neither carries a stranger's words nor
        // acts is allowed DELIBERATELY, by whoever puts it there.
        crate::mcp_tools::ToolEffect::ReadsOwn | crate::mcp_tools::ToolEffect::WritesOwn => {
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "team agents may read".to_owned(),
            })
        }
    }
}

/// Whether a `get_run` call names another council seat's run.
///
/// Fails closed on every shape it cannot read, exactly as its triage sibling does, and for a reason
/// that is weaker but points the same way: what is lost by refusing is one lookup, and what is lost
/// by allowing wrongly is the independence phase 1 exists to have. A run that does not exist is the
/// one honest `false` — the tool returns an error and nothing is read.
pub(super) async fn get_run_names_a_council_run(state: &AppState, tool_input: &Value) -> bool {
    let Some(id) = tool_input.get("id").and_then(Value::as_i64) else {
        return true;
    };
    match sqlx::query_scalar::<_, String>("SELECT mode FROM runs WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
    {
        Ok(Some(mode)) => mode == crate::council::COUNCIL_MODE,
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(
                run_id = id,
                %error,
                "pretooluse-decision: could not resolve the mode of the run a seat asked for — refusing it"
            );
            true
        }
    }
}
