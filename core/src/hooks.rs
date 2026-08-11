use axum::extract::State;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

use crate::auth::Scope;
use crate::classifier;
use crate::runs::finalize_termination;
use crate::shadow;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct PreToolUsePayload {
    // The run this decision is for (spec §3.3). `run_id == runs.id` — injected into the CLI's
    // environment as NUCLEOS_RUN_ID (Step 5) and echoed back by the hook script; there is no second
    // identifier.
    pub run_id: i64,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
}

#[derive(Serialize, Deserialize)]
pub struct Decision {
    // "allow" | "deny" | "pending_approval" (spec §3.3). To the CLI, both "deny" and
    // "pending_approval" are just "block" (the hook script maps them, Step 6); the core treats them
    // differently — see below.
    pub decision: String,
    pub reason: String,
}

pub async fn pretooluse_decision(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(payload): Json<PreToolUsePayload>,
) -> Json<Decision> {
    // `run_id` arrives in the body, which makes it a claim the caller makes about itself. With a
    // scoped key the daemon can check that claim against something the caller cannot choose: a run
    // token names its own run. Without this, a run could ask for a decision under another run's id
    // — a `shadow` run borrowing a `worktree` run's rules, or an in-flight run's id being used to
    // terminate it — and every branch below reads `mode` from exactly that id.
    if let Scope::Run(id) = scope
        && id != payload.run_id
    {
        tracing::warn!(
            token_run_id = id,
            claimed_run_id = payload.run_id,
            "pretooluse-decision: a run asked for a decision under another run's id"
        );
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: "a run may only ask about itself".to_owned(),
        });
    }

    // Validate run_id against runs actually in flight before trusting anything derived from it (spec
    // §3.4 — the hook's environment sits inside the same cooperative trust model as the token, so the
    // core never blindly trusts what the hook sends).
    let is_in_flight = state
        .run_handles
        .lock()
        .unwrap()
        .contains_key(&payload.run_id);

    // `mode` is resolved for EVERY request, in flight or not, because it decides WHICH set of rules
    // applies — and a run that has left `run_handles` is exactly when defaulting to `real` is most
    // dangerous: the classifier permits `Read` there, so an email triage run would be handed the one
    // tool the pillar exists to keep away from a stranger's text. `cwd` stays behind the in-flight
    // check: it only feeds the classifier's path sensitivity, which is meaningful for a run that is
    // actually executing.
    let (cwd, mode) = match sqlx::query_as::<_, (Option<String>, String)>(
        "SELECT cwd, mode FROM runs WHERE id = ?",
    )
    .bind(payload.run_id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some((cwd, mode))) => (is_in_flight.then_some(cwd).flatten(), mode),
        Ok(None) => (None, "real".to_owned()),
        // `mode` decides WHICH set of rules applies, so an unreadable one cannot resolve to the
        // most permissive of them. `Ok(None)` above can safely default to `real` because the row is
        // genuinely absent — there is no run whose rules we are guessing at. An `Err` is different:
        // the run may well be a triage or shadow run whose barrier we would be stepping over, and
        // the pool this reads through is shared with feed appends and run-status writes, so
        // SQLITE_BUSY under contention is an ordinary event rather than a theoretical one.
        Err(error) => {
            tracing::warn!(
                run_id = payload.run_id,
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
    if mode == crate::email::TRIAGE_MODE {
        // Only a run that is actually executing can have reached a tool through barrier 1, and that
        // is the alarming case. The startup verification deliberately probes this branch with a
        // throwaway row that is NOT in flight, so warning on both would fire a false alarm on every
        // single boot — and an alarm that cries wolf at startup is one nobody reads when it matters.
        if is_in_flight {
            tracing::warn!(
                run_id = payload.run_id,
                tool = %payload.tool_name,
                "pretooluse-decision: a triage run attempted a tool — barrier 1 is not in force"
            );
        } else {
            tracing::debug!(
                run_id = payload.run_id,
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
        return council_decision(&payload);
    }

    let classification = classifier::classify(
        &payload.tool_name,
        &payload.tool_input,
        cwd.as_deref().map(Path::new),
    );
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
        if is_in_flight
            && let Err(error) = shadow::record_decision(
                &state.pool,
                payload.run_id,
                &payload.tool_name,
                &payload.tool_input,
                &classification,
            )
            .await
        {
            tracing::warn!(
                run_id = payload.run_id,
                %error,
                "pretooluse-decision: failed to record shadow decision"
            );
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

    if mode == "worktree"
        && is_in_flight
        && let Err(error) = shadow::record_decision(
            &state.pool,
            payload.run_id,
            &payload.tool_name,
            &payload.tool_input,
            &classification,
        )
        .await
    {
        tracing::warn!(
            run_id = payload.run_id,
            %error,
            "pretooluse-decision: failed to record shadow decision"
        );
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
            payload.run_id,
            &payload.tool_name,
            &payload.tool_input.to_string(),
        )
        .await
        {
            Ok(Some(request_id)) => {
                tracing::info!(
                    run_id = payload.run_id,
                    request_id,
                    "pretooluse-decision: the action is already queued — refusing the retry"
                );
                count_denial_and_stop_a_prober(&state, payload.run_id, &payload.tool_name).await;
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
                    run_id = payload.run_id,
                    %error,
                    "pretooluse-decision: could not tell whether this action is already queued"
                );
            }
        }

        match crate::proposals::grant_covers_class(
            &state.pool,
            payload.run_id,
            classification.action_class,
        )
        .await
        {
            Ok(true) => {
                tracing::info!(
                    run_id = payload.run_id,
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
                        payload.tool_name, payload.run_id
                    ),
                    Some(payload.run_id),
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
                    run_id = payload.run_id,
                    %error,
                    "pretooluse-decision: grant lookup failed; falling back to the classifier decision"
                );
            }
        }
    }

    // A denied action is answered and then counted. Answering was all this used to do, which left
    // the run free to try the next spelling immediately — and against an unbounded number of
    // attempts a lexical classifier is a puzzle with a solution rather than a boundary. The two
    // verdicts were also the wrong way round in cost: `pending_approval` stopped the run and
    // fetched a human, while `deny` — the harsher judgement — cost the run nothing at all.
    if classification.decision.decision == "deny" && is_in_flight {
        count_denial_and_stop_a_prober(&state, payload.run_id, &payload.tool_name).await;
    }

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
                payload.run_id,
                payload.tool_name.clone(),
                payload.tool_input.to_string(),
                classification.reason.clone(),
            ))
            .await;
        } else {
            tracing::warn!(
                "pretooluse-decision: pending_approval for unknown/finished run_id {} — not terminating",
                payload.run_id
            );
        }
    }

    Json(classification.decision)
}

/// The reason an orchestrator turn is refused a tool that would act. A constant because the tests
/// assert on it: every other refusal in this branch is also a `deny`, so only the reason tells
/// "the turn had read a stranger's words" apart from "the tool was not ours".
pub const UNTRUSTED_CONTEXT_DENY_REASON: &str =
    "this turn has read third-party content and can no longer act";

/// The orchestrator turn's decision, and the only barrier standing between a mail body and the
/// daemon's controls.
///
/// Everything else in this file gets a second look from the classifier. This branch does not, by
/// design — a turn holds no worktree and cannot satisfy an approval — so an unconditional allow
/// here is genuinely unconditional. What made that dangerous is the tool set: `get_email` returns a
/// stranger's body verbatim into the same context that reaches `approve_proposal`, `set_kill` and
/// `create_run`, and the turn carries the control token, so a body asking for a proposal to be
/// approved was read by the one agent able to approve it.
///
/// The rule is an ordering rule rather than a policy on any single tool: read what you like, and
/// act while nothing third-party has entered the turn — but not both, and not in that order. It is
/// deliberately not a hard split of the tool set, because reading mail from a phone is the feature,
/// and the ordering costs the owner one extra message rather than the tool.
async fn assistant_decision(state: &AppState, payload: &PreToolUsePayload) -> Json<Decision> {
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

    let effect = match crate::mcp_tools::tool_effect(tool) {
        // `get_run` is `ReadsOwn` by name and not always by content: a triage run's stdout is a
        // model's answer over mail a stranger wrote. The parse that bounds a verdict to a class and
        // 200 stripped characters runs AFTER the raw stream is stored, so what comes back through
        // this tool was never put through it.
        crate::mcp_tools::ToolEffect::ReadsOwn
            if tool == "get_run"
                && get_run_names_a_triage_run(state, &payload.tool_input).await =>
        {
            crate::mcp_tools::ToolEffect::ReadsUntrusted
        }
        effect => effect,
    };

    match effect {
        crate::mcp_tools::ToolEffect::ReadsUntrusted => {
            // Marked BEFORE the tool is allowed, and the failure to mark refuses the read. The
            // alternative is a turn that has a stranger's words in it and no record of having read
            // them, which is the state every refusal below depends on not existing.
            if let Err(error) =
                crate::runs::mark_untrusted_context(&state.pool, payload.run_id).await
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
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "orchestrator NucleOS tool".to_owned(),
            })
        }
        crate::mcp_tools::ToolEffect::Acts => {
            match crate::runs::read_untrusted_context(&state.pool, payload.run_id).await {
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
        crate::mcp_tools::ToolEffect::ReadsOwn => Json(Decision {
            decision: "allow".to_owned(),
            reason: "orchestrator NucleOS tool".to_owned(),
        }),
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
/// PURE — no state is read, so there is nothing to fail closed ABOUT. The orchestrator's branch has
/// to ask the database whether the turn has read third-party text, and every way that read can fail
/// is a way its answer can be wrong; this one is a list membership test and cannot be.
fn council_decision(payload: &PreToolUsePayload) -> Json<Decision> {
    // Whole segment, not a prefix, for the reason `assistant_decision` records: an MCP server named
    // `nucleos__x` produces `mcp__nucleos__x__…`, which passes a prefix test.
    let permitted = payload
        .tool_name
        .strip_prefix("mcp__nucleos__")
        .filter(|tool| !tool.contains("__"))
        .is_some_and(|tool| crate::mcp_tools::COUNCIL_TOOLS.contains(&tool));

    if permitted {
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

/// Whether a `get_run` call names a triage run.
///
/// Fails closed on every shape it cannot read — an absent id, an id that is not a number, a
/// database that will not answer — because the question being decided is whether a stranger's words
/// are about to enter the turn, and "I could not tell" is not "no". A run that does not exist is
/// the one honest `false`: the tool returns an error and nothing is read.
async fn get_run_names_a_triage_run(state: &AppState, tool_input: &Value) -> bool {
    let Some(id) = tool_input.get("id").and_then(Value::as_i64) else {
        return true;
    };
    match sqlx::query_scalar::<_, String>("SELECT mode FROM runs WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
    {
        Ok(Some(mode)) => mode == crate::email::TRIAGE_MODE,
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(
                run_id = id,
                %error,
                "pretooluse-decision: could not resolve the mode of the run being read — treating it as third-party content"
            );
            true
        }
    }
}

/// How many denied actions a run may attempt before it is stopped.
///
/// Not one: a run that reaches for something destructive, is told no, and takes a different route
/// is behaving exactly as the gate intends, and killing it there would turn the gate into a
/// tripwire. Not unbounded either, which is what it was. Three leaves room for an honest mistake
/// and a retry, and far too little to search a grammar with.
const DENIAL_LIMIT: i64 = 3;

/// Records a denied attempt and, once a run has spent its allowance, stops it.
///
/// Terminated to `failed` rather than `awaiting_approval`: a denied action is destructive by
/// classification, and the pause path exists to make an action approvable. Offering a human an
/// "approve" button here would launder precisely the verdict that is supposed to be final — the
/// class-scoped grant deliberately only ever lifts a `pending_approval`.
async fn count_denial_and_stop_a_prober(state: &AppState, run_id: i64, tool_name: &str) {
    let denials: i64 = match sqlx::query_scalar(
        "UPDATE runs SET denials = denials + 1 WHERE id = ? RETURNING denials",
    )
    .bind(run_id)
    .fetch_one(&state.pool)
    .await
    {
        Ok(count) => count,
        // The action is still denied — that part never depended on this write. Only the allowance
        // does, and the pool this shares with feed appends and status writes makes SQLITE_BUSY an
        // ordinary event here. Sustained probing is many calls, of which lost counts are a
        // minority; treating one lost count as a reason to kill the run would make a busy database
        // indistinguishable from an attack.
        Err(error) => {
            tracing::warn!(run_id, %error, "could not count a denied action against the run");
            return;
        }
    };

    tracing::warn!(
        run_id,
        tool = %tool_name,
        denials,
        "pretooluse-decision: denied action {denials}/{DENIAL_LIMIT} for this run"
    );
    if denials < DENIAL_LIMIT {
        return;
    }

    // Spawned and then awaited, for the same reason `pause_for_approval` is: terminating the run
    // kills the CLI whose hook script owns the connection this handler is answering, so the request
    // can vanish mid-handler and take an inline continuation with it.
    let state = state.clone();
    let tool_name = tool_name.to_owned();
    let _ = tokio::spawn(async move {
        if !finalize_termination(&state, run_id, "failed").await {
            return;
        }
        let _ = crate::feed::append(
            &state.pool,
            None,
            "run_stopped_probing",
            &format!("run {run_id} was stopped after {denials} denied actions (last: {tool_name})"),
            Some(run_id),
        )
        .await;
    })
    .await;
}

/// The whole `pending_approval` act: terminate the run, then record the proposal that makes the
/// pause actionable. These two belong together — a run parked in `awaiting_approval` with no
/// proposal can be neither approved nor rejected, and it goes on holding one of the project's
/// concurrency slots, permanently: the sweep spares `awaiting_approval`, and startup recovery only
/// reconciles rows left `running`. Hence the caller runs this as a detachable task rather than
/// inline in a request that may not survive its own side effects.
async fn pause_for_approval(
    state: AppState,
    run_id: i64,
    tool_name: String,
    tool_input: String,
    reason: String,
) {
    // Active termination (spec §8.4 steps 2–3): drive the run to `awaiting_approval` via the same
    // atomic-handle-removal arbiter cancellation uses (`finalize_termination`, Chunk 2 Task 4).
    if !finalize_termination(&state, run_id, "awaiting_approval").await {
        return;
    }

    let (session_id, project_id, job_id, stage) =
        sqlx::query_as::<_, (Option<String>, Option<String>, Option<i64>, Option<String>)>(
            "SELECT session_id, project_id, job_id, stage FROM runs WHERE id = ?",
        )
        .bind(run_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .unwrap_or((None, None, None, None));

    // A node of a job takes the other road entirely, and three shapes of node take three roads.
    //
    // An **item's** node is put down and the job carries on to the next item. A **review** node owns
    // no item and does not need one: its verdict is advisory — §5.5 gives ship/no-ship to the gate —
    // and everything it was going to read is already written, gated and checkpointed. A **replan**
    // node owns none either, and ending it hands the job to `stop_after_replan`, which stops it
    // `Stopped` rather than `Failed` precisely so the rounds that DID run stay worth looking at.
    //
    // The rule underneath all three: **a node that asks already has an ending written for it**, and
    // the job's own handler picks it. What parking adds is not safety — it is a job that reads live,
    // holds a concurrency slot and does nothing, with no sign anywhere until a person goes looking.
    //
    // Measured, not reasoned into. Chunk 2 put review and plan on one road with a single sentence,
    // and four jobs falsified it in a day: 12 parked its review on `git reflog`, 13 parked its own
    // on a `for` loop, and 14 and 16 both parked their REPLAN nodes after every item had passed.
    //
    // The **plan** node is the one that still parks, and the argument for it is the one Chunk 2
    // made: the queue is what it produces, so there is nothing partial to preserve and nothing to
    // carry on to. Ending it would fail a job that a single answer turns into a night's work, to
    // save a slot that is now bounded by a ceiling rather than blocking the project.
    //
    // The mark is what decides for an item, and it is also step (2) of the skip — see
    // `skip_the_item`. Doing it here rather than inside keeps "did this run own an item?" and "put
    // it down" as one write: asking first and marking after would be a race with the same
    // cancel/reconcile the mark is already guarded against.
    //
    // Read HERE and not before the termination above, deliberately: `job_id` does not change when a
    // run ends, and moving the read earlier would put an extra query on the hot path of every
    // ordinary run's hook, which is nearly all of them.
    let road = match job_id {
        Some(job_id) if put_the_item_down(&state.pool, job_id, run_id).await => {
            Some((job_id, true))
        }
        Some(job_id) if NODES_THAT_GIVE_UP.contains(&stage.as_deref().unwrap_or_default()) => {
            Some((job_id, false))
        }
        _ => None,
    };
    if let Some((job_id, had_an_item)) = road {
        skip_the_item(
            state,
            SkippedItem {
                run_id,
                job_id,
                session_id,
                project_id,
                tool_name,
                tool_input,
                reason,
                had_an_item,
            },
        )
        .await;
        return;
    }

    if let Err(error) = crate::proposals::create_action_approval(
        &state.pool,
        run_id,
        session_id.as_deref(),
        project_id.as_deref(),
        &tool_name,
        &reason,
        Some(&tool_input),
    )
    .await
    {
        tracing::warn!(
            run_id,
            %error,
            "pretooluse-decision: failed to record action-approval proposal"
        );
        let _ = crate::feed::append(
            &state.pool,
            project_id.as_deref(),
            "proposal_record_failed",
            &format!("failed to record action-approval proposal: {error}"),
            Some(run_id),
        )
        .await;

        // Warning alone left the run parked in `awaiting_approval` with nothing to approve or
        // reject, holding one of the project's concurrency slots for good: the sweep spares that
        // status on purpose, because a run with a PENDING proposal is resumable.
        // `reconcile_stranded_approvals` is what tells the two apart — but only at startup, so the
        // project ran one slot narrower until someone restarted the daemon.
        //
        // Undo the pause instead. `interrupted` is the status startup recovery already uses for
        // exactly this shape, so a run that ends here reads the same either way, and the slot is
        // free immediately. Guarded on the status this function set, so a cancel that arrived in
        // the meantime keeps the last word.
        let rolled_back = sqlx::query(
            "UPDATE runs SET status = 'interrupted', completed_at = ?
             WHERE id = ? AND status = 'awaiting_approval'",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(run_id)
        .execute(&state.pool)
        .await;
        match rolled_back {
            Ok(result) if result.rows_affected() == 1 => tracing::warn!(
                run_id,
                "pretooluse-decision: rolled the unapprovable pause back to interrupted"
            ),
            Ok(_) => {}
            Err(rollback_error) => tracing::error!(
                run_id,
                %rollback_error,
                "pretooluse-decision: could not roll back an unapprovable pause — this project is blocked until restart"
            ),
        }
    }
}

/// A job's node asked for a decision, so the job puts the item down and carries on without it.
///
/// The other half of `pause_for_approval`, and it exists because parking is the wrong answer for a
/// job. A run belongs to a person who is going to come back to it; a job is the thing that was
/// supposed to work while nobody was watching, and stopping it dead on the first unrecognised shell
/// command is what "work through the night" met in practice — measured on 2026-08-07, where a
/// two-item job in a four-file repository parked seven times.
///
/// **The order of the three writes below is the whole safety argument, and it is not arbitrary.**
///
/// 1. The run is already terminated by the caller. That `.await` is the one `core/AGENTS.md` names
///    as biting hardest: it kills the CLI whose hook script owns the connection being answered, so
///    everything after it runs on borrowed time.
/// 2. **Mark the item.** First, and before anything that can fail or block — done by the caller in
///    `put_the_item_down`, because whether the mark landed is also what decides that this road is
///    the right one at all. An item left `running` in a job nobody is driving is a job that answers
///    `Wait` for ever — `next_step` sees a live node, there is no live node, and no later pass
///    rescues it. Every other loss here is recoverable; that one is not.
/// 3. **Take the pause off the run.** The caller parked it to ask; the asking is over. A run left
///    `awaiting_approval` parks the whole job through `job::node_awaiting_approval`, which is the
///    same stop by another door — the item reads `skipped` and the job waits on it anyway.
///    Recoverable, unlike (2): `reconcile_stranded_approvals` writes the same `interrupted` at the
///    next startup, which is why it goes second and not first.
/// 4. Revert the tree, then record the proposal. Both may fail, and neither failure is allowed to
///    take the mark with it: an item skipped without a proposal is work nobody will be reminded of,
///    which is bad and survivable, where an item stuck `running` is a dead job.
struct SkippedItem {
    run_id: i64,
    job_id: i64,
    session_id: Option<String>,
    project_id: Option<String>,
    tool_name: String,
    tool_input: String,
    reason: String,
    /// Whether this run owned an item whose half-written edits have to be undone.
    ///
    /// False for a review node, the other thing that takes this road, and the difference is not
    /// cosmetic. A review node owns no item, so `footing_for_run` would answer for whichever item
    /// ran last — and reverting to that footing would throw away the checkpoint of the job's final
    /// item, which is the one thing the review existed to look at. A node that changes nothing by
    /// design has nothing to revert.
    had_an_item: bool,
}

/// The job stages that give their node up rather than parking the job on it.
///
/// Both own no item, and both already have an ending written for the case where their run does not
/// come back: `ingest_replan`'s failure arm hands the job to `stop_after_replan` (`Stopped`, with
/// the rounds that ran still on the branch), and `load_view` reads any terminal review as `Done`
/// ("a review that failed is still a review that happened").
///
/// `plan` is deliberately absent. Its ending exists too — `Outcome::Failed` — but it is the only one
/// that throws away a whole job to save a slot: nothing has been done yet, so there is no partial
/// work to preserve, and a single answer turns that same job into a night's work.
const NODES_THAT_GIVE_UP: [&str; 2] = ["review", "replan"];

/// Step (2) of the skip, and the question that decides whether there is a skip at all: mark this
/// run's item `skipped`, and say whether there was one.
///
/// Scoped to the item this run owns, so a job whose other items are in flight is untouched. A
/// `false` answer has two shapes and the caller treats them alike, because the right move is the
/// same for both — park and ask:
///
/// - **No item exists.** A plan or review node. There is nothing to put down, and skipping a job's
///   plan would leave it with no queue to carry on with.
/// - **The item is no longer `running`.** A cancel or a reconcile got there first and its verdict is
///   the newer one; overwriting it would be this handler talking over a decision already made.
///
/// A database error also answers `false`, which parks the run rather than skipping an item that may
/// still be `running`. Loud, and recoverable by hand — where a job stalled on a live item is not.
async fn put_the_item_down(pool: &sqlx::SqlitePool, job_id: i64, run_id: i64) -> bool {
    let marked = sqlx::query(
        "UPDATE job_items SET status = ? WHERE job_id = ? AND run_id = ? AND status = 'running'",
    )
    .bind(crate::job::STATUS_SKIPPED)
    .bind(job_id)
    .bind(run_id)
    .execute(pool)
    .await;
    match marked {
        Ok(result) if result.rows_affected() == 1 => true,
        Ok(_) => {
            tracing::info!(
                run_id,
                job_id,
                "pretooluse-decision: no running job item for this run — parking it instead"
            );
            false
        }
        Err(error) => {
            tracing::error!(
                run_id,
                job_id,
                %error,
                "pretooluse-decision: could not mark a job item skipped — parking the run instead"
            );
            false
        }
    }
}

async fn skip_the_item(state: AppState, item: SkippedItem) {
    let SkippedItem {
        run_id,
        job_id,
        session_id,
        project_id,
        tool_name,
        tool_input,
        reason,
        had_an_item,
    } = item;

    // (3) Take the pause off the RUN. The caller terminated it to `awaiting_approval`, because at
    // that point the answer was still "ask a person". It is not any more — the answer was "skip it",
    // and it has already been given.
    //
    // Without this the job stops anyway, one step later and for a different reason:
    // `job::node_awaiting_approval` asks whether ANY run of the job is `awaiting_approval` and parks
    // the whole job when one is, and `node_in_flight` counts that status as a node still to wait
    // for. The item would read `skipped` while the job sat on a node nobody would ever answer —
    // measured on 2026-08-08, job 3: item 0 `skipped`, job `awaiting_approval`, nothing pending.
    //
    // `interrupted` and not something new: it is what `reconcile_stranded_approvals` writes at
    // startup for precisely this row (an `awaiting_approval` run with no `action-approval` proposal),
    // and what `pause_for_approval` rolls back to when a pause turns out not to be one. Writing it
    // here is the same verdict without waiting for a restart.
    //
    // AFTER the item's mark, never before, for two reasons. If this write is lost the damage is
    // recoverable — that same startup reconciler writes it — where an item left `running` is a job
    // that answers `Wait` for ever and no pass rescues. And the window between the two writes is
    // only safe in this order: `job::reconcile_nodes` folds back every item that is still `running`
    // whose run has left flight, and would read this one as `failed` and stop the chain. Marked
    // first, the item is no longer `running` and that query cannot see it at all.
    //
    // Guarded on the status the caller set, so a cancel that landed in between keeps the last word.
    let unpaused = sqlx::query(
        "UPDATE runs SET status = 'interrupted', completed_at = ?
         WHERE id = ? AND status = 'awaiting_approval'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(run_id)
    .execute(&state.pool)
    .await;
    if let Err(error) = unpaused {
        tracing::error!(
            run_id,
            job_id,
            %error,
            "pretooluse-decision: could not lift the pause off a skipped item's run — the job will park until restart"
        );
    }

    // (4a) Put the tree back. The item wrote whatever it wrote before it asked, and the next item
    // must not build on a half-done change nobody approved.
    //
    // Only for an item. A review node reaches this function too and must NOT come through here:
    // it owns no item, so `footing_for_run` would answer for whichever one ran last, and reverting
    // to that footing would discard the checkpoint of the job's final item — the very work the
    // review was there to read. A node that changes nothing by design has nothing to put back.
    if had_an_item {
        let footing = crate::job::footing_for_run(&state.pool, job_id, run_id).await;
        match (
            crate::job::job_worktree_path(&state.pool, job_id).await,
            footing,
        ) {
            (Some(worktree), Some(sha)) => {
                if let Err(error) = crate::worktree::revert_to(&worktree, &sha).await {
                    tracing::warn!(run_id, job_id, %error, "pretooluse-decision: could not revert a skipped item");
                }
            }
            _ => tracing::warn!(
                run_id,
                job_id,
                "pretooluse-decision: no worktree or no footing to revert a skipped item to"
            ),
        }
    }

    // (4b) The proposal. A different `kind` from an action approval, and `wip.rs` counts only the
    // other one: this is work NOT YET DONE waiting on a decision, where the WIP limit exists to cap
    // work already done waiting to be looked at. Conflating them closes the autonomy this change
    // just opened, at the third skipped item of the night.
    if let Err(error) = crate::proposals::create_skipped_item(
        &state.pool,
        run_id,
        session_id.as_deref(),
        project_id.as_deref(),
        &tool_name,
        &reason,
        Some(&tool_input),
    )
    .await
    {
        tracing::warn!(run_id, job_id, %error, "pretooluse-decision: failed to record a skipped-item proposal");
        let _ = crate::feed::append(
            &state.pool,
            project_id.as_deref(),
            "proposal_record_failed",
            &format!("failed to record skipped-item proposal: {error}"),
            Some(run_id),
        )
        .await;
        // Deliberately NOT rolled back, where the action-approval path above rolls its pause back.
        // There the run is stuck `awaiting_approval` with nothing to approve, holding an index that
        // blocks the project. Here the item is `skipped`, the job moves on, and what is lost is the
        // reminder — a worse outcome than having it, and a far better one than a job that stalls.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::proposals;
    use crate::runner::FakeCommandRunner;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn test_state() -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        AppState {
            token: Token("test-token".into()),
            pool,
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            local_assistant: None,
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// The real middleware, not a stand-in that inserts the extension directly: the handler now
    /// reads a `Scope` that only `require_token` puts there, so a test router without it would pass
    /// while every hook call in production returned 500.
    fn test_router(state: AppState) -> Router {
        Router::new()
            .route("/hooks/pretooluse-decision", post(pretooluse_decision))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::auth::require_token,
            ))
            .with_state(state)
    }

    /// As the control token, which is what these tests were written against — they are about what
    /// the classifier decides, not about who is allowed to ask. `decide_as` is for the latter.
    async fn decide(app: &Router, body: &str) -> Decision {
        decide_as(app, "test-token", body).await
    }

    async fn decide_as(app: &Router, bearer: &str, body: &str) -> Decision {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/pretooluse-decision")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {bearer}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Insert a `runs` row and spawn a long-sleeping task whose abort handle is registered under
    /// the row's id, making that run look in-flight to `pretooluse_decision` the same way a real
    /// governed run does. `project_id`, `cwd`, and `session_id` are `None` for tests that don't
    /// care about them; `created_at` is a fixed placeholder since no test ever asserts on it.
    async fn in_flight_run(
        state: &AppState,
        mode: &str,
        project_id: Option<&str>,
        cwd: Option<&str>,
        session_id: Option<&str>,
    ) -> i64 {
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, project_id, cwd, session_id, created_at)
             VALUES ('x', 'running', ?, ?, ?, ?, '2026-07-17T00:00:00Z')",
        )
        .bind(mode)
        .bind(project_id)
        .bind(cwd)
        .bind(session_id)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let task =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });
        state
            .run_handles
            .lock()
            .unwrap()
            .insert(run_id, task.abort_handle());

        run_id
    }

    /// A live implement node of a live job: the job row, one `running` item, the run that owns it,
    /// and a worktree row for the job so the revert has somewhere to point.
    ///
    /// Returns `(job_id, run_id)`.
    async fn in_flight_job_node(state: &AppState) -> (i64, i64) {
        let job_id = sqlx::query(
            "INSERT INTO jobs
             (project_id, project_root, prompt, status, max_items, gate_each, review, created_at)
             VALUES ('proj', 'C:\\work\\repo', 'advance the backlog', 'implementing', 5, 1, 1,
                     '2026-08-07T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let run_id = in_flight_run(
            state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-x"),
        )
        .await;
        sqlx::query("UPDATE runs SET job_id = ?, stage = 'implement' WHERE id = ?")
            .bind(job_id)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, run_id)
             VALUES (?, 0, 'the first thing', 'running', ?)",
        )
        .bind(job_id)
        .bind(run_id)
        .execute(&state.pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO worktrees (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
             VALUES ('job', ?, 'proj', 'C:\\work\\repo', 'C:\\work\\wt', 'nucleos/job-x',
                     '2026-08-07T00:00:00Z')",
        )
        .bind(job_id)
        .execute(&state.pool)
        .await
        .unwrap();

        (job_id, run_id)
    }

    /// A `runs` row without an abort handle: the run exists and its mode is on record, but nothing
    /// is executing under it. Every barrier that reads `mode` has to hold here too, because this is
    /// the state a run passes through on its way out — and the state a forged request would claim.
    async fn out_of_flight_run(state: &AppState, mode: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'completed', ?, '2026-07-28T00:00:00Z')",
        )
        .bind(mode)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// Mints and stores a run's own key, returning what its CLI would find in the environment.
    async fn key_for(state: &AppState, run_id: i64) -> String {
        let (token, secret) = crate::auth::mint_run_token(run_id);
        sqlx::query("UPDATE runs SET token = ? WHERE id = ?")
            .bind(&secret)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        token
    }

    /// `run_id` comes from the request body, so it is a claim the caller makes about itself. Every
    /// branch in this handler reads `mode` from that id, so a run able to name another run's id
    /// picks which rules it is judged by — a `shadow` run could ask under a `worktree` run's id and
    /// be handed the worktree ruleset, and an in-flight run's id could be used to terminate it.
    #[tokio::test]
    async fn a_run_may_only_ask_the_gate_about_itself() {
        let state = test_state().await;
        let mine = in_flight_run(&state, "shadow", None, None, None).await;
        let other = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;
        let my_key = key_for(&state, mine).await;
        let app = test_router(state.clone());

        let decision = decide_as(
            &app,
            &my_key,
            &format!(r#"{{"run_id":{other},"tool_name":"Read","tool_input":{{}}}}"#),
        )
        .await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "a run may only ask about itself");

        // And the same key asking about its own run is answered normally — the check is about the
        // id, not about run tokens being second-class.
        let own = decide_as(
            &app,
            &my_key,
            &format!(r#"{{"run_id":{mine},"tool_name":"Read","tool_input":{{}}}}"#),
        )
        .await;
        assert_eq!(own.decision, "allow");
    }

    /// A `deny` used to cost the run nothing, so a lexical classifier could be searched: try a
    /// spelling, get told no, try the next, forever, with nothing counting the attempts and nothing
    /// watching. The allowance leaves room for an honest mistake and stops a search.
    #[tokio::test]
    async fn a_run_that_keeps_reaching_for_denied_actions_is_stopped() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;
        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"rm -rf /"}}}}"#
        );

        for attempt in 1..DENIAL_LIMIT {
            assert_eq!(decide(&app, &body).await.decision, "deny");
            assert!(
                state.run_handles.lock().unwrap().contains_key(&run_id),
                "attempt {attempt} is within the allowance and must not stop the run"
            );
        }

        // The one that spends it. The action is still denied — being stopped is on top of the
        // refusal, never instead of it.
        assert_eq!(decide(&app, &body).await.decision, "deny");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        // `failed`, not `awaiting_approval`: a denied action must never acquire an approve button.
        assert_eq!(status, "failed");

        let proposals = proposals::list_pending(&state.pool).await.unwrap();
        assert!(
            proposals.is_empty(),
            "stopping a prober must not mint something a human can approve"
        );
    }

    /// A run that tries again the action the queue took over is TOLD so, not paused.
    ///
    /// The loop this closes: the approval queues the merge and mints no grant, so the resumed run's
    /// retry used to be an ordinary `pending_approval` — pausing the run and putting a second
    /// proposal in front of a person, whose approval would queue the same merge a second time. The
    /// prompt asks the run not to retry; this is what happens when it does anyway.
    ///
    /// Three things are asserted because getting any one of them wrong reopens the loop: the verdict
    /// names the request, the run is still in flight, and nothing was minted for a person to read.
    #[tokio::test]
    async fn an_action_the_queue_already_has_is_refused_rather_than_asked_about_again() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;
        let app = test_router(state.clone());
        let input = r#"{"command":"git merge feature/x"}"#;

        sqlx::query(
            "INSERT INTO action_grants
             (run_id, tool_name, tool_input, proposal_id, created_at, consumed_at, queued_request_id)
             VALUES (?, 'Bash', ?, 1, '2026-01-01T00:00:00Z', NULL, 77)",
        )
        .bind(run_id)
        .bind(input)
        .execute(&state.pool)
        .await
        .unwrap();

        let body = format!(r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{input}}}"#);
        let decision = decide(&app, &body).await;

        assert_eq!(decision.decision, "deny");
        assert!(
            decision.reason.contains("request 77"),
            "the run has to be told WHICH request has its work: {}",
            decision.reason
        );
        assert!(
            state.run_handles.lock().unwrap().contains_key(&run_id),
            "a refusal that names the queue must not also stop the run"
        );
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty(),
            "asking a person again about a merge already queued would queue it twice"
        );
    }

    /// The refusal is COUNTED, and that is the half that keeps the answer from being free.
    ///
    /// Without it the verdict would be the one thing in the system that neither pauses the run nor
    /// spends anything: a run that ignores both the resume prompt and the reason string could retry
    /// for ever, burning tokens against a merge that is already on its way. The test that asserts
    /// the run survives ONE refusal cannot see that — it passes whether or not anything is counted.
    #[tokio::test]
    async fn a_run_that_keeps_retrying_a_queued_action_is_stopped() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;
        let app = test_router(state.clone());
        let input = r#"{"command":"git merge feature/x"}"#;

        sqlx::query(
            "INSERT INTO action_grants
             (run_id, tool_name, tool_input, proposal_id, created_at, consumed_at, queued_request_id)
             VALUES (?, 'Bash', ?, 1, '2026-01-01T00:00:00Z', NULL, 77)",
        )
        .bind(run_id)
        .bind(input)
        .execute(&state.pool)
        .await
        .unwrap();

        let body = format!(r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{input}}}"#);
        for _ in 0..DENIAL_LIMIT {
            assert_eq!(decide(&app, &body).await.decision, "deny");
        }

        assert!(
            !state.run_handles.lock().unwrap().contains_key(&run_id),
            "a run told {DENIAL_LIMIT} times where its work went, that asks again, is not obeying"
        );
    }

    /// The allowance is per run, so one run spending it does not shorten another's.
    #[tokio::test]
    async fn each_run_gets_its_own_allowance() {
        let state = test_state().await;
        let spender = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;
        let bystander = in_flight_run(&state, "worktree", Some("proj-2"), None, None).await;
        let app = test_router(state.clone());

        for _ in 0..DENIAL_LIMIT {
            let body = format!(
                r#"{{"run_id":{spender},"tool_name":"Bash","tool_input":{{"command":"rm -rf /"}}}}"#
            );
            decide(&app, &body).await;
        }

        assert!(!state.run_handles.lock().unwrap().contains_key(&spender));
        assert!(state.run_handles.lock().unwrap().contains_key(&bystander));
        let untouched: i64 = sqlx::query_scalar("SELECT denials FROM runs WHERE id = ?")
            .bind(bystander)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(untouched, 0);
    }

    /// Shadow mode denies everything that is not read-only — that is its whole job, not evidence of
    /// a run probing the gate. It returns before the classifier's verdict is ever counted.
    #[tokio::test]
    async fn a_shadow_runs_ordinary_refusals_do_not_spend_an_allowance() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "shadow", Some("proj-1"), None, None).await;
        let app = test_router(state.clone());

        for _ in 0..DENIAL_LIMIT + 2 {
            let body = format!(
                r#"{{"run_id":{run_id},"tool_name":"Edit","tool_input":{{"file_path":"a.txt"}}}}"#
            );
            assert_eq!(decide(&app, &body).await.decision, "deny");
        }

        assert!(
            state.run_handles.lock().unwrap().contains_key(&run_id),
            "a shadow run must survive doing exactly what shadow mode expects of it"
        );
    }

    /// The single most important assertion in this pillar: a triage run gets NO tool, of any kind.
    /// Barrier 1 means the CLI should never offer one — this is what happens if it does.
    #[tokio::test]
    async fn a_triage_run_is_denied_every_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::email::TRIAGE_MODE, None, None, None).await;
        let app = test_router(state);

        for (tool, input) in [
            ("Bash", serde_json::json!({"command": "ls -la"})),
            ("Read", serde_json::json!({"file_path": "/etc/passwd"})),
            (
                "Edit",
                serde_json::json!({"file_path": "a", "new_string": "b"}),
            ),
            (
                "Write",
                serde_json::json!({"file_path": "a", "content": "b"}),
            ),
            ("Grep", serde_json::json!({"pattern": "secret"})),
            ("Glob", serde_json::json!({"pattern": "**/*.env"})),
            ("mcp__nucleos__create_run", serde_json::json!({})),
            (
                "mcp__claude_ai_Google_Drive__create_file",
                serde_json::json!({}),
            ),
        ] {
            let body = serde_json::json!({
                "run_id": run_id,
                "tool_name": tool,
                "tool_input": input,
            })
            .to_string();
            let decision = decide(&app, &body).await;
            assert_eq!(decision.decision, "deny", "{tool} must be denied");
            assert_eq!(decision.reason, "email triage runs have no tools");
        }
    }

    /// `ls -la` is the case that proves the fallthrough was real: the classifier calls it
    /// read-local and ALLOWS it, so before `mode` was resolved for out-of-flight runs, a triage run
    /// that had left `run_handles` was handed a shell.
    #[tokio::test]
    async fn a_triage_run_stays_denied_once_it_leaves_the_handle_map() {
        let state = test_state().await;
        let run_id = out_of_flight_run(&state, crate::email::TRIAGE_MODE).await;
        let app = test_router(state);

        let body = serde_json::json!({
            "run_id": run_id,
            "tool_name": "Bash",
            "tool_input": {"command": "ls -la"},
        })
        .to_string();
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "email triage runs have no tools");
    }

    #[tokio::test]
    async fn an_out_of_flight_orchestrator_turn_is_denied_rather_than_classified() {
        let state = test_state().await;
        let run_id = out_of_flight_run(&state, "assistant").await;
        let app = test_router(state);

        let body = serde_json::json!({
            "run_id": run_id,
            "tool_name": "Read",
            "tool_input": {"file_path": "/etc/passwd"},
        })
        .to_string();
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(
            decision.reason,
            "the orchestrator is restricted to NucleOS tools"
        );
    }

    /// The no-regression half of the lift: resolving `mode` outside the in-flight check must not
    /// have narrowed what a shadow run may do.
    #[tokio::test]
    async fn an_out_of_flight_shadow_run_still_allows_read_only_tools() {
        let state = test_state().await;
        let run_id = out_of_flight_run(&state, "shadow").await;
        let app = test_router(state);

        for tool in ["Read", "Grep", "Glob"] {
            let body = serde_json::json!({
                "run_id": run_id,
                "tool_name": tool,
                "tool_input": {"file_path": "src/main.rs", "pattern": "fn"},
            })
            .to_string();
            let decision = decide(&app, &body).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
    }

    /// The scoreboard records decisions taken over live runs. A call naming a run that is no longer
    /// executing is not one, and counting it would quietly inflate the promotion gate's evidence.
    #[tokio::test]
    async fn an_out_of_flight_run_records_no_shadow_decision() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let shadow_id = out_of_flight_run(&state, "shadow").await;
        let worktree_id = out_of_flight_run(&state, "worktree").await;
        let app = test_router(state);

        for run_id in [shadow_id, worktree_id] {
            let body = serde_json::json!({
                "run_id": run_id,
                "tool_name": "Read",
                "tool_input": {"file_path": "src/main.rs"},
            })
            .to_string();
            decide(&app, &body).await;
        }

        let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shadow_decisions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(recorded, 0);
    }

    #[tokio::test]
    async fn denies_rm_rf() {
        let app = test_router(test_state().await);
        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"rm -rf /tmp/x"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "destructive deletion commands are denied");
    }

    #[tokio::test]
    async fn allows_safe_read_command() {
        let app = test_router(test_state().await);
        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"ls -la"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "allow");
        assert_eq!(decision.reason, "recognized non-mutating shell command");
    }

    /// The hook's pending path, and the classifier's verdict that reaches it, asserted together so
    /// the two cannot drift apart. `echo hi` stood here until `classifier.rs` learned that saying
    /// something is not doing something; the command changed, the path under test did not.
    #[tokio::test]
    async fn pends_unrecognized_command() {
        let tool_input = serde_json::json!({"command": "frobnicate --hard"});
        let classification = classifier::classify("Bash", &tool_input, None);
        assert_eq!(classification.action_class, "unrecognized");

        let app = test_router(test_state().await);
        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"frobnicate --hard"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");
    }

    #[tokio::test]
    async fn git_push_pends_approval_and_terminates_the_run() {
        let state = test_state().await;

        // Stand up a fake in-flight run: a runs row plus a live task whose abort handle is registered
        // under the same id (that's the `run_id` the hook will send).
        let run_id = in_flight_run(&state, "real", None, None, None).await;

        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"git push origin main"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "pending_approval");
        assert_eq!(
            decision.reason,
            "push, merge, deploy, publish, and tag actions require approval"
        );

        // The run was actively terminated into awaiting_approval, and its handle removed (spec §8.4).
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    /// The trap in spec §7's cancellation sweep, made a test rather than a comment. `pause_for_approval`
    /// drives the run through the same `finalize_termination` a cancel uses, but the run it produces
    /// **resumes**: sweeping its queued requests would cancel the very merge it paused to have approved,
    /// and the human would then approve a request that no longer exists.
    ///
    /// It has to be driven through the handler rather than asserted on the predicate, because a
    /// predicate test cannot see a wrong argument at the call site — which is the only place this can
    /// actually go wrong.
    #[tokio::test]
    async fn a_run_paused_for_approval_keeps_the_merge_it_asked_for() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "real", None, None, None).await;
        let request = crate::vcs::submit(
            &state.pool,
            &crate::vcs::ResolvedRepo::synthetic("proj-1", "C:/repo", "proj-1"),
            &crate::vcs::Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            crate::vcs::Origin::Run(run_id),
        )
        .await
        .unwrap();

        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"git push origin main"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "pending_approval");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");

        let request_status: String =
            sqlx::query_scalar("SELECT status FROM vcs_requests WHERE id = ?")
                .bind(request)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            request_status, "awaiting_approval",
            "the run resumes, so the merge it is pausing to have approved must still be there"
        );
    }

    #[tokio::test]
    async fn edit_to_autopilot_config_pends_approval_and_terminates_the_run() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "real", None, Some("C:\\work\\repo"), None).await;

        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Edit","tool_input":{{"file_path":".ai/autopilot.yaml"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "pending_approval");
        assert_eq!(
            decision.reason,
            "changes to autopilot governance files require approval"
        );

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    /// Terminating the run kills the CLI whose hook script owns the very connection this handler is
    /// serving, and that script gives up after 5s anyway (`ask_daemon.py`'s `timeout=5`). Either way
    /// the request can vanish mid-handler, and a dropped request drops the handler future exactly the
    /// way `abort()` does — so everything sequenced after the termination is lost.
    ///
    /// The loss is unrecoverable, not merely untidy: a run parked in `awaiting_approval` with no
    /// proposal can be neither approved nor rejected, and it holds one of the project's concurrency
    /// slots for as long as it sits there — the sweep spares that status, because a run with a
    /// pending proposal is resumable. Startup recovery does not help either: it only reconciles rows
    /// left `running`.
    #[tokio::test]
    async fn a_dropped_hook_request_still_records_the_approval_proposal() {
        use std::future::Future;

        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;

        let mut handler = Box::pin(pretooluse_decision(
            State(state.clone()),
            // What the middleware would have inserted for this run's own key.
            Extension(Scope::Run(run_id)),
            Json(PreToolUsePayload {
                run_id,
                tool_name: "Bash".to_owned(),
                tool_input: serde_json::json!({"command": "git push origin main"}),
            }),
        ));

        // Drive the handler by hand so the request can be dropped at a chosen point: the instant the
        // irreversible half is done. Removing the abort handle is that point of no return — it is the
        // arbiter that decides this call owns the termination, and it runs before any recording work.
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut terminated = false;
        for _ in 0..10_000 {
            assert!(
                handler.as_mut().poll(&mut context).is_pending(),
                "the handler ran to completion before the request could be dropped"
            );
            if !state.run_handles.lock().unwrap().contains_key(&run_id) {
                terminated = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(terminated, "the handler never terminated the run");
        drop(handler);

        for _ in 0..100 {
            if !proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(
            pending.len(),
            1,
            "a paused run with no proposal is stuck forever and blocks its project"
        );
        assert_eq!(pending[0].run_id, Some(run_id));
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
    }

    #[tokio::test]
    async fn cwd_dependent_delete_outside_workspace_denies_through_the_handler() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "real", None, Some("C:\\work\\repo"), None).await;

        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"rm ../outside/x"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "destructive deletion commands are denied");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn worktree_allows_and_records_an_ordinary_edit() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let tool_input = serde_json::json!({"file_path": "src/ordinary.rs"});
        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": tool_input
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "allow");

        let row: (String, String, String) = sqlx::query_as(
            "SELECT decision, action_class, tool_input FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Edit'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "allow");
        assert_eq!(row.1, "read-local");
        assert_eq!(row.2, tool_input.to_string());

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn worktree_denies_and_records_a_destructive_delete_without_terminating() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "rm -rf target"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "deny");

        let row: (String, String) = sqlx::query_as(
            "SELECT decision, action_class FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Bash'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "deny");
        assert_eq!(row.1, "destructive");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn worktree_pends_and_terminates_on_git_push_and_records_it() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let row: (String, String) = sqlx::query_as(
            "SELECT decision, action_class FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Bash'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "pending_approval");
        assert_eq!(row.1, "push-merge-deploy");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn shadow_read_only_brake_records_would_decisions_without_terminating() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "shadow", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let edit_input = serde_json::json!({"file_path": "src/ordinary.rs"});
        let edit = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": edit_input
            })
            .to_string(),
        )
        .await;
        assert_eq!(edit.decision, "deny");

        let row: (String, String, String) = sqlx::query_as(
            "SELECT decision, action_class, tool_input FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Edit'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "allow");
        assert_eq!(row.1, "read-local");
        assert_eq!(row.2, edit_input.to_string());

        let read = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Read",
                "tool_input": {"file_path": "src/lib.rs"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(read.decision, "allow");

        let push = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(push.decision, "deny");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_ne!(status, "awaiting_approval");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn git_push_pause_creates_a_pending_action_approval_proposal() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-x"),
        )
        .await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        let proposal = &pending[0];
        assert_eq!(proposal.run_id, Some(run_id));
        assert_eq!(proposal.tool_name.as_deref(), Some("Bash"));
        assert_eq!(proposal.session_id.as_deref(), Some("sess-x"));
        assert_eq!(proposal.project_id.as_deref(), Some("proj"));
        assert_eq!(proposal.status, "pending");
        assert!(!proposal.reasoning.is_empty());
    }

    /// A job's node takes the other road out of the same door.
    ///
    /// The item is put down, the job is left alone to carry on, and the record is a `skipped-item`
    /// rather than an `action-approval` — which is what stops `list_pending` offering it as
    /// something to approve, since approving it would resume nothing.
    #[tokio::test]
    async fn a_jobs_node_skips_its_item_instead_of_parking_the_job() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let item: (String, i64) =
            sqlx::query_as("SELECT status, ordinal FROM job_items WHERE job_id = ? AND run_id = ?")
                .bind(job_id)
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(item.0, "skipped", "the item must not be left running");

        let job_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            job_status, "implementing",
            "the job itself is untouched — it has a queue to get on with"
        );

        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(kinds, vec!["skipped-item"]);
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty(),
            "a skipped item is a note, not something to approve — approving it resumes nothing"
        );

        // And the half the first version of this test did not ask about, which is what let the
        // whole thing stop anyway. `job::node_awaiting_approval` parks a job when ANY of its runs
        // is `awaiting_approval`, so a skipped item whose run keeps that status trades one stop for
        // another: the item reads `skipped` and the job waits on it for ever.
        let run_status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            run_status, "interrupted",
            "the run must not keep asking after the answer was given"
        );
        let still_parked: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM runs WHERE job_id = ? AND status = 'awaiting_approval' LIMIT 1",
        )
        .bind(job_id)
        .fetch_optional(&state.pool)
        .await
        .unwrap();
        assert!(
            still_parked.is_none(),
            "this is the query `job::node_awaiting_approval` runs; a hit here parks the whole job"
        );
    }

    /// And the counterpart, which is the one that would go wrong quietly: a run with no `job_id`
    /// keeps the behaviour it has always had. This is the test that fails if the branch above is
    /// ever widened past the condition it was written for.
    #[tokio::test]
    async fn a_run_that_belongs_to_no_job_still_parks_and_asks() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-x"),
        )
        .await;
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(kinds, vec!["action-approval"]);
    }

    /// The node of a job that owns no item: the plan, which runs before the queue exists.
    ///
    /// Found by dogfooding the change above on 2026-08-07 — the plan node asked to run `find`, took
    /// the job road because its run has a `job_id`, matched no item, and returned before writing any
    /// proposal. The run sat in `awaiting_approval` with nothing to approve, which is precisely the
    /// state `pause_for_approval`'s rollback exists to prevent, and which holds one of the
    /// project's concurrency slots until a restart notices.
    ///
    /// The queue is what the plan produces, so there is nothing to skip and nothing to carry on to.
    /// It parks and asks, like any other run — and it is the ONLY node that still does. A review
    /// node owns no item either and stopped taking this road; see the test below for why.
    #[tokio::test]
    async fn a_jobs_plan_node_owns_no_item_so_it_parks_and_asks() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        // What makes it a plan node: the queue does not exist yet.
        sqlx::query("DELETE FROM job_items WHERE job_id = ?")
            .bind(job_id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET stage = 'plan' WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(
            pending.len(),
            1,
            "a parked node with no proposal can be neither approved nor rejected, and blocks the \
             project until the daemon restarts"
        );
        assert_eq!(pending[0].kind, "action-approval");
        assert_eq!(pending[0].run_id, Some(run_id));
    }

    /// A review node abandons the review rather than parking the job on it.
    ///
    /// It owns no item, so it used to take the plan node's road and park — and the cost of that is
    /// the whole night, not one opinion. Its verdict is advisory (§5.5 gives ship/no-ship to the
    /// gate), it runs after every item is written, gated and checkpointed, and it changes nothing
    /// itself. So the job loses a review and finishes, which is the trade the right way round.
    ///
    /// Measured twice before it was changed: job 12 on 2026-08-08 parked its review node on
    /// `git reflog` and job 13 parked its own on a `for` loop, each with all the real work already
    /// done. Both sat until a person cancelled them.
    ///
    /// The run must end TERMINAL, and that is the whole mechanism — `load_view` already reads any
    /// finished review as `ReviewState::Done` ("a review that failed is still a review that
    /// happened"), so the round closes with no change to the state machine at all.
    /// The replan node too, and for the same reason with a different ending behind it.
    ///
    /// Its run ending non-successfully hands the job to `stop_after_replan`, which stops it
    /// `Stopped` and not `Failed` — precisely so the rounds that already ran stay worth looking at.
    /// That ending existed before this; all that was missing is the node reaching it.
    ///
    /// Measured on 2026-08-08: jobs 14 and 16 both passed every item, closed their round, spawned
    /// their replan node — and both parked it, one on a `for` loop and one on `find`. Two jobs that
    /// had done all their work sat holding a slot each.
    #[tokio::test]
    async fn a_jobs_replan_node_gives_up_instead_of_parking_the_job() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        sqlx::query("UPDATE job_items SET status = 'passed', run_id = NULL WHERE job_id = ?")
            .bind(job_id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET stage = 'replan' WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                // A shell loop, which the classifier reads as a program rather than a line and so
                // does not recognise. NOT `find`, which it allows — an allowed command never
                // reaches this road, and the run would read terminal for the wrong reason.
                "tool_input": {"command": "for f in *.py; do cat \"$f\"; done"}
            })
            .to_string(),
        )
        .await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_ne!(
            status, "awaiting_approval",
            "a parked replan leaves a job that finished all its work reading live and doing nothing"
        );
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(kinds, vec!["skipped-item"]);
        // The rounds that ran are what `stop_after_replan` keeps, so nothing may be reverted here.
        let item_statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert!(item_statuses.iter().all(|status| status == "passed"));
    }

    #[tokio::test]
    async fn a_jobs_review_node_gives_up_the_review_instead_of_parking_the_job() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        // What makes it a review node: it owns no running item, and its stage says so.
        sqlx::query("UPDATE job_items SET status = 'passed', run_id = NULL WHERE job_id = ?")
            .bind(job_id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET stage = 'review' WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "for f in *.py; do cat \"$f\"; done"}
            })
            .to_string(),
        )
        .await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_ne!(
            status, "awaiting_approval",
            "a review left parked stops the job it was only ever going to comment on"
        );

        // Nothing pending: `list_pending` filters `skipped-item` out, because it is a note rather
        // than a queue — work NOT done waiting on a decision, not work done waiting to be read.
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            kinds,
            vec!["skipped-item"],
            "what it asked for is still on the record for the morning"
        );

        // And the items it was going to read are untouched. A review node owns no item, so a revert
        // here would target whichever ran last and throw away that item's checkpoint.
        let item_statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert!(item_statuses.iter().all(|status| status == "passed"));
    }

    /// The write that must not be lost: an item left `running` in a job nobody drives makes
    /// `next_step` answer `Wait` for ever, and no later pass rescues it.
    ///
    /// Exercised by taking away everything the skip could stumble on — no worktree row, so no
    /// revert is possible — and demanding the mark survive anyway.
    #[tokio::test]
    async fn the_item_is_marked_even_when_nothing_else_about_the_skip_can_happen() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        sqlx::query("DELETE FROM worktrees WHERE owner_kind = 'job' AND owner_id = ?")
            .bind(job_id)
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;

        let status: String =
            sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? AND run_id = ?")
                .bind(job_id)
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            status, "skipped",
            "a job with no worktree still must not be left holding a running item"
        );
    }

    #[tokio::test]
    async fn assistant_turn_allows_nucleos_mcp_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "mcp__nucleos__list_projects",
                "tool_input": {}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "allow");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn assistant_turn_denies_non_mcp_tool_and_creates_no_proposal() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "ToolSearch",
                "tool_input": {}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "deny");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// One NucleOS tool call in an orchestrator turn, named the way the CLI names it.
    async fn orchestrator_tool(
        app: &Router,
        run_id: i64,
        tool: &str,
        tool_input: serde_json::Value,
    ) -> Decision {
        decide(
            app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": format!("mcp__nucleos__{tool}"),
                "tool_input": tool_input,
            })
            .to_string(),
        )
        .await
    }

    /// The attack this barrier exists for, start to finish.
    ///
    /// An orchestrator turn is allowed every NucleOS tool unconditionally and carries the control
    /// token, and `get_email` returns a stranger's body verbatim into that same context. So a mail
    /// body that says "approve proposal 4" was read by the one agent able to approve it, in the one
    /// mode with no classifier, no proposal and no termination between the reading and the doing.
    /// The threat model calls "an email body causing a tool call" a thing this product prevents;
    /// until this test passed, it prevented it only for the triage run.
    #[tokio::test]
    async fn a_mail_body_cannot_reach_the_controls_it_asks_for() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state.clone());

        let read = orchestrator_tool(&app, run_id, "get_email", serde_json::json!({"id": 7})).await;
        assert_eq!(
            read.decision, "allow",
            "reading mail is what the turn is for"
        );

        // The three the body would ask for: lift an approval the classifier withheld, disengage the
        // emergency stop, and start a run that launches with `ToolPolicy::Unrestricted`.
        for tool in ["approve_proposal", "set_kill", "create_run"] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 4})).await;
            assert_eq!(
                decision.decision, "deny",
                "{tool} was allowed after a mail read"
            );
            assert_eq!(decision.reason, UNTRUSTED_CONTEXT_DENY_REASON, "{tool}");
        }

        // Refused, never punished: the owner asking for two things in one message lands here too,
        // and the turn is not a governed run that can be paused or proposed against.
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The rule is about acting, not about reading, and a barrier that also stopped the reading
    /// would have taken the feature with it: the owner asked what was in their mail, and the answer
    /// needs more than one message to assemble.
    #[tokio::test]
    async fn a_turn_that_has_read_mail_may_keep_reading() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state);

        orchestrator_tool(&app, run_id, "get_email_queue", serde_json::json!({})).await;

        for tool in ["get_email", "get_email_queue", "list_files"] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
        // NucleOS's own state, which changes nothing and travels to the owner's own chat.
        for tool in ["list_proposals", "get_budget", "get_kill", "list_projects"] {
            let decision = orchestrator_tool(&app, run_id, tool, serde_json::json!({})).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
    }

    /// The ordinary case has to keep working, or the barrier is just an outage: a turn that has read
    /// nothing third-party is the one the owner uses to approve and to work the kill switch.
    #[tokio::test]
    async fn a_turn_that_has_read_no_mail_still_acts() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state);

        for tool in ["approve_proposal", "set_kill", "create_run", "cancel_run"] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
    }

    /// Marked on the run, not on the process. A flag held in a module-level set would be shared by
    /// every turn the daemon is running, so one chat asking about its mail would quietly disarm the
    /// controls in another — and a test suite whose in-memory databases all start numbering at 1
    /// would not notice, because it would look like the barrier working.
    #[tokio::test]
    async fn one_turn_reading_mail_does_not_disarm_the_next() {
        let state = test_state().await;
        let reader = in_flight_run(&state, "assistant", None, None, None).await;
        let other = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state);

        orchestrator_tool(&app, reader, "get_email", serde_json::json!({"id": 1})).await;

        assert_eq!(
            orchestrator_tool(
                &app,
                reader,
                "set_kill",
                serde_json::json!({"engaged": false})
            )
            .await
            .decision,
            "deny"
        );
        assert_eq!(
            orchestrator_tool(
                &app,
                other,
                "set_kill",
                serde_json::json!({"engaged": false})
            )
            .await
            .decision,
            "allow",
            "a turn that read nothing must not inherit another turn's refusal"
        );
    }

    /// `get_run` reads a run's own output, which is NucleOS's account of the owner's work — except
    /// for a triage run, where it is a model's answer over mail a stranger wrote. The parse that
    /// bounds a verdict to a class and 200 stripped characters runs after the raw stream is stored,
    /// so this tool is the one way that text gets back out unbounded.
    #[tokio::test]
    async fn reading_a_triage_run_counts_as_reading_the_mail_it_triaged() {
        let state = test_state().await;
        let turn = in_flight_run(&state, "assistant", None, None, None).await;
        let triage = in_flight_run(&state, crate::email::TRIAGE_MODE, None, None, None).await;
        let worktree = in_flight_run(&state, "worktree", None, None, None).await;
        let app = test_router(state);

        // A worktree run's output is the owner's own work and leaves the turn able to act.
        orchestrator_tool(&app, turn, "get_run", serde_json::json!({"id": worktree})).await;
        assert_eq!(
            orchestrator_tool(
                &app,
                turn,
                "set_kill",
                serde_json::json!({"engaged": false})
            )
            .await
            .decision,
            "allow"
        );

        orchestrator_tool(&app, turn, "get_run", serde_json::json!({"id": triage})).await;
        assert_eq!(
            orchestrator_tool(
                &app,
                turn,
                "set_kill",
                serde_json::json!({"engaged": false})
            )
            .await
            .decision,
            "deny",
            "a triage run's stdout is a stranger's words at one remove"
        );
    }

    /// The question being answered is whether third-party text is about to enter the turn, and "I
    /// cannot tell which run you mean" is not "no". An id that is missing or is not a number would
    /// otherwise be the cheapest way to read a triage run without being counted as having done so.
    #[tokio::test]
    async fn a_get_run_naming_nothing_readable_is_treated_as_third_party_text() {
        for tool_input in [
            serde_json::json!({}),
            serde_json::json!({"id": "12"}),
            serde_json::json!({"id": null}),
        ] {
            let state = test_state().await;
            let turn = in_flight_run(&state, "assistant", None, None, None).await;
            let app = test_router(state);

            orchestrator_tool(&app, turn, "get_run", tool_input.clone()).await;

            assert_eq!(
                orchestrator_tool(
                    &app,
                    turn,
                    "set_kill",
                    serde_json::json!({"engaged": false})
                )
                .await
                .decision,
                "deny",
                "{tool_input}"
            );
        }
    }

    /// A name this server does not expose reaches the same unconditional allow as one it does, so it
    /// has to land on the fail-closed side of the rule rather than on neither side of it.
    #[tokio::test]
    async fn an_unrecognised_nucleos_tool_is_refused_after_mail_is_read() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state);

        assert_eq!(
            orchestrator_tool(&app, run_id, "send_email", serde_json::json!({}))
                .await
                .decision,
            "allow",
            "an unknown name behaves as it did before a turn has read anything"
        );

        orchestrator_tool(&app, run_id, "get_email", serde_json::json!({"id": 1})).await;

        assert_eq!(
            orchestrator_tool(&app, run_id, "send_email", serde_json::json!({}))
                .await
                .decision,
            "deny"
        );
    }

    #[tokio::test]
    async fn self_governing_edit_pause_creates_a_proposal_with_edit_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-e"),
        )
        .await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": {"file_path": ".ai/autopilot.yaml"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name.as_deref(), Some("Edit"));
        assert_eq!(pending[0].run_id, Some(run_id));
    }

    /// What the human actually agreed to is a KIND of action, not one spelling of it. Approving
    /// `git push origin main` and then parking the resume on `git push origin other` asked the same
    /// question twice about the same decision — and every re-ask is a chance to answer it wearily.
    /// The grant therefore covers its class for the rest of the run rather than a single call.
    #[tokio::test]
    async fn a_grant_authorizes_every_action_of_its_class_for_the_rest_of_the_run() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        proposals::grant_action(&state.pool, run_id, "Bash", Some("push-merge-deploy"), 1)
            .await
            .unwrap();
        let app = test_router(state.clone());

        // Two DIFFERENT pushes: same class, different input. The second is the one the old
        // single-use, input-matched grant sent back for a second approval.
        for command in ["git push origin main", "git push origin other"] {
            let decision = decide(
                &app,
                &serde_json::json!({
                    "run_id": run_id,
                    "tool_name": "Bash",
                    "tool_input": {"command": command}
                })
                .to_string(),
            )
            .await;
            assert_eq!(decision.decision, "allow", "{command}");

            // Never parks, and is never terminated on the way to parking: an authorized action
            // that still killed the run would be an allow in name only.
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(status, "running", "{command}");
            assert!(
                state.run_handles.lock().unwrap().contains_key(&run_id),
                "{command}"
            );
        }
    }

    /// The other half of the class rule: covering a class for the rest of the run is only safe if
    /// the class is a real boundary. A push approval must not reach an edit to the file that
    /// governs what this run is allowed to do at all.
    #[tokio::test]
    async fn a_grant_does_not_authorize_a_different_action_class() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        proposals::grant_action(&state.pool, run_id, "Bash", Some("push-merge-deploy"), 1)
            .await
            .unwrap();
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": {"file_path": ".ai/autopilot.yaml"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");

        // The grant survives whole: an action it does not cover must neither spend it nor be
        // recorded as having used it.
        let grant = sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT action_class, consumed_at FROM action_grants WHERE run_id = ?",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            grant,
            (Some("push-merge-deploy".to_owned()), None),
            "the grant must survive an action class it does not authorize"
        );
    }

    #[tokio::test]
    async fn a_database_error_resolving_the_mode_denies() {
        // `mode` decides WHICH rules apply, so failing to read it is not a reason to pick the most
        // permissive one. This is the `Err` twin of the `Ok(None)` case: an email-triage run whose
        // mode read fails must not be handed `Read`, the one tool the pillar exists to keep away
        // from a stranger's text. A gate that cannot be read is not permission.
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::email::TRIAGE_MODE, None, None, None).await;
        let app = test_router(state.clone());

        // A closed pool makes every query error — the cheapest faithful stand-in for the SQLITE_BUSY
        // this handler shares a pool with feed appends and run-status writes to earn.
        state.pool.close().await;

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Read",
                "tool_input": {"file_path": "README.md"}
            })
            .to_string(),
        )
        .await;

        assert_eq!(decision.decision, "deny");
    }

    #[tokio::test]
    async fn deny_still_denies_even_with_a_matching_grant() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        // The grant names the very class of the action attempted below, so this proves `deny`
        // outranks a grant that covers it rather than merely one that failed to match. No approval
        // flow can mint such a grant — `deny` never becomes a proposal — which is exactly why the
        // check has to hold against one conjured directly in the table.
        proposals::grant_action(&state.pool, run_id, "Bash", Some("destructive"), 1)
            .await
            .unwrap();
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "rm -rf target"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "deny");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));

        let consumed_at: Option<String> =
            sqlx::query_scalar("SELECT consumed_at FROM action_grants WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(consumed_at.is_none());
    }

    /// A seat's whole job is to read this machine's state and answer, so the reads have to work or
    /// the tools are decoration.
    #[tokio::test]
    async fn a_council_turn_allows_a_reads_own_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::council::COUNCIL_MODE, None, None, None).await;
        let app = test_router(state);

        for tool in [
            "list_projects",
            "list_proposals",
            "get_budget",
            "get_kill",
            "get_run",
        ] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
    }

    /// The half that makes a seat worth asking. Half the questions somebody puts to a council are
    /// about what arrived, and a seat that cannot read mail answers those from what it half-recalls.
    ///
    /// Safe here in a way it is not for the orchestrator, and for a structural reason rather than a
    /// hopeful one: the taint rule exists to stop a stranger's words from reaching a tool that ACTS,
    /// and no tool a seat may call acts.
    #[tokio::test]
    async fn a_council_turn_allows_a_reads_untrusted_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::council::COUNCIL_MODE, None, None, None).await;
        let app = test_router(state.clone());

        for tool in ["get_email_queue", "get_email", "list_files"] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }

        // And having read them changes nothing afterwards, because there was never anything to
        // withdraw: a seat could not act before the mail and cannot act after it.
        assert_eq!(
            orchestrator_tool(&app, run_id, "create_run", serde_json::json!({}))
                .await
                .decision,
            "deny"
        );
    }

    /// The brake itself. Every `Acts` tool refused, the run left alone, and no proposal minted —
    /// a council has no worktree to resume into, so an approval for one could never be satisfied.
    #[tokio::test]
    async fn a_council_turn_denies_an_acts_tool_and_creates_no_proposal() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::council::COUNCIL_MODE, None, None, None).await;
        let app = test_router(state.clone());

        for tool in [
            "create_run",
            "create_job",
            "approve_proposal",
            "reject_proposal",
            "cancel_run",
            "set_kill",
            "triage_email",
            "vcs_request",
            // Not an action, and refused all the same: `web_search` and `web_read` reach off this
            // machine, and a roster of local seats holding either would stop being a local council.
            "web_search",
            "web_read",
            // The read-back half of `vcs_request`. A seat that cannot queue an operation has
            // nothing of its own to read back.
            "vcs_ticket",
        ] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_eq!(decision.decision, "deny", "{tool}");
        }

        // Tools outside this server too: a seat is not an ordinary run and does not get Bash by
        // falling through to the classifier.
        for tool in ["Bash", "Write", "Read", "mcp__other__anything"] {
            let decision = decide(
                &app,
                &serde_json::json!({
                    "run_id": run_id,
                    "tool_name": tool,
                    "tool_input": {"command": "git push"}
                })
                .to_string(),
            )
            .await;
            assert_eq!(decision.decision, "deny", "{tool}");
        }

        // The prefix trap `assistant_decision` records: a server called `nucleos__x` would produce
        // this name, and a prefix test would have inherited the council's allow.
        assert_eq!(
            decide(
                &app,
                &serde_json::json!({
                    "run_id": run_id,
                    "tool_name": "mcp__nucleos__x__get_budget",
                    "tool_input": {}
                })
                .to_string(),
            )
            .await
            .decision,
            "deny"
        );

        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running", "a refusal must not terminate the seat");
    }

    /// The property the mode branches are written to preserve: a mode nobody has written a branch
    /// for does not inherit an unconditional allow.
    ///
    /// It falls through to the classifier, where an MCP tool name is `unrecognized` and therefore
    /// `pending_approval` — which is a stop, not a grant. Asserted as "not allow" rather than as the
    /// exact verdict, because the value of this test is the direction and pinning the spelling would
    /// make a later refinement of the classifier read as a regression here.
    #[tokio::test]
    async fn an_unknown_mode_gets_no_acts_by_default() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "a-mode-invented-later", None, None, None).await;
        let app = test_router(state);

        for tool in ["create_run", "set_kill", "vcs_request"] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_ne!(decision.decision, "allow", "{tool}");
        }
    }

    #[tokio::test]
    async fn no_proposal_created_when_run_is_not_in_flight() {
        let state = test_state().await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert!(pending.is_empty());
    }
}
