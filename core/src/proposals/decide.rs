//! Deciding a proposal as a service function: `approve`, with the answers an HTTP door needs but no
//! HTTP types in the way it is called.
//!
//! Every kind is decided here: `contact-merge`, `refinement`, `calendar-event`, `github-action`,
//! `fleet-exclusion`, `agent-recruit`, `team-action`, `browser-wheel` and the default
//! `action-approval`.
//! The messages are the ones the handler has always sent: moving the code must not change what a
//! person reads.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::door::uncancellable;
use crate::state::AppState;

/// Why a decision did not happen. [`DecideError::into_http`] is the one place that turns it into a
/// status and a body.
#[derive(Debug)]
pub enum DecideError {
    /// No such proposal.
    NotFound,
    /// Somebody decided it first.
    AlreadyDecided,
    /// A refusal the person can resolve and then decide again. 409 with the sentence.
    Conflict(String),
    /// The request names something that cannot be done (an unusable scope, a proposal carrying no
    /// usable payload). 422 with the sentence.
    Invalid(String),
    /// The request itself is malformed. 400 with the sentence.
    BadRequest(String),
    /// A call out to another service failed. 502 with the sentence.
    BadGateway(String),
    /// A fault on our side. The text is the body the caller gets; empty means no body.
    Internal(String),
    /// A refusal that carries the status of its own cause. Transitional: the GitHub failure of a
    /// proposal that is already `approved` answers 503, 403, 504 or 500 depending on what failed,
    /// and none of the fixed variants above says that without changing what the door answers today.
    Status(StatusCode, String),
}

impl DecideError {
    /// The status and body the HTTP door answers with. `id` only fills in the not-found sentence.
    pub fn into_http(self, id: i64) -> (StatusCode, String) {
        match self {
            Self::NotFound => (StatusCode::NOT_FOUND, format!("there is no proposal {id}")),
            Self::AlreadyDecided => (
                StatusCode::CONFLICT,
                "this proposal has already been decided".to_owned(),
            ),
            Self::Conflict(message) => (StatusCode::CONFLICT, message),
            Self::Invalid(message) => (StatusCode::UNPROCESSABLE_ENTITY, message),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::BadGateway(message) => (StatusCode::BAD_GATEWAY, message),
            Self::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
            Self::Status(status, message) => (status, message),
        }
    }

    /// The bare status a reject door answers with. A reject never has a body, so the message of a
    /// variant that carries one goes to the log and nowhere else.
    pub fn into_status(self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::AlreadyDecided => StatusCode::CONFLICT,
            Self::Conflict(message) => {
                tracing::warn!(%message, "a rejection was refused as a conflict");
                StatusCode::CONFLICT
            }
            Self::Invalid(message) => {
                tracing::warn!(%message, "a rejection was refused as invalid");
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::BadRequest(message) => {
                tracing::warn!(%message, "a rejection was refused as a bad request");
                StatusCode::BAD_REQUEST
            }
            Self::BadGateway(message) => {
                tracing::warn!(%message, "a rejection failed on a call to another service");
                StatusCode::BAD_GATEWAY
            }
            Self::Internal(message) => {
                tracing::warn!(%message, "a rejection failed on our side");
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Status(status, message) => {
                tracing::warn!(%message, %status, "a rejection was refused with its own status");
                status
            }
        }
    }
}

/// What a person may add to an approval. Every field is optional, and each is read by one kind only.
#[derive(Default)]
pub struct ApproveInput {
    /// Only a recruitment reads it: the corrected hire.
    pub hire: Option<crate::agent::AgentRequest>,
    /// Only a browser-wheel approval reads it: `"shell"` or `"window"`.
    pub seat: Option<String>,
    /// Only a `refinement` approval reads it: `agent:<id>`, `team:<id>` or `project:<id>`.
    pub scope: Option<String>,
}

/// Approves proposal `id`.
///
/// Reads the proposal for its kind and dispatches; the kind never changes and every branch is a
/// compare-and-set on `status = 'pending'`, so a second decision racing this one loses there.
pub async fn approve(state: &AppState, id: i64, input: ApproveInput) -> Result<Value, DecideError> {
    let proposal = super::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a proposal to approve failed");
            DecideError::Internal("the proposal could not be read".to_owned())
        })?
        .ok_or(DecideError::NotFound)?;
    match proposal.kind.as_str() {
        "contact-merge" => approve_contact_merge(state, id).await,
        "refinement" => approve_refinement(state, id, input.scope.as_deref()).await,
        "calendar-event" => approve_calendar_event(state, id).await,
        "github-action" => approve_github_action(state, id).await,
        "fleet-exclusion" => approve_fleet_exclusion(state, id).await,
        "agent-recruit" => approve_agent_recruit(state, id, input.hire).await,
        "team-action" => approve_team_action(state, id).await,
        "browser-wheel" => approve_browser_wheel(state.clone(), id, input.seat).await,
        _ => approve_action_approval(state, id, &proposal).await,
    }
}

/// Rejects proposal `id`; the bare `Ok(())` is the 204 of the HTTP door.
///
/// Every kind is decided here, and the default `action-approval` discards the paused run it was
/// filed for. A failed task is `Internal`, which the door answers 500 with no body.
pub async fn reject(state: &AppState, id: i64) -> Result<(), DecideError> {
    let proposal = super::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a proposal to reject failed");
            DecideError::Internal("the proposal could not be read".to_owned())
        })?
        .ok_or(DecideError::NotFound)?;
    match proposal.kind.as_str() {
        "browser-wheel" => reject_browser_wheel(state.clone(), id).await,
        "contact-merge" => reject_contact_merge(state, id).await,
        "refinement" => reject_refinement(state, id).await,
        "calendar-event" => reject_calendar_event(state, id).await,
        "github-action" => reject_github_action(state, id).await,
        "fleet-exclusion" => reject_fleet_exclusion(state, id).await,
        "agent-recruit" => reject_agent_recruit(state, id).await,
        "team-action" => reject_team_action(state, id).await,
        _ => reject_action_approval(state, id).await,
    }
}

async fn reject_fleet_exclusion(state: &AppState, id: i64) -> Result<(), DecideError> {
    // Refusing an edge leaves nothing behind, because drawing it changed nothing: no rule was
    // written, no run was paused, no worktree is held. It is the one refusal with no second half
    // to undo.
    use crate::governance::exclusion::DecisionError as ExclusionError;
    let state = state.clone();
    let rejected =
        uncancellable(async move { crate::governance::exclusion::reject(&state.pool, id).await })
            .await
            .map_err(|_| DecideError::Internal("the rejection task did not finish".to_owned()))?;
    match rejected {
        Ok(()) => Ok(()),
        Err(ExclusionError::NotFound) => Err(DecideError::NotFound),
        Err(ExclusionError::NotPending) => Err(DecideError::AlreadyDecided),
        Err(ExclusionError::Malformed) => Err(DecideError::Invalid(
            "this request names no usable pair of jobs".to_owned(),
        )),
        Err(ExclusionError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "rejecting an exclusion failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

async fn reject_agent_recruit(state: &AppState, id: i64) -> Result<(), DecideError> {
    // Refusing leaves nothing behind, because nothing was created: no agent, no roster row, no run
    // held. And it is deliberately not a permanent no — the NEXT run of that department meets the
    // same gap and may ask again, which is right. Nobody said the director should stop asking; they
    // said not this one.
    let state = state.clone();
    let rejected =
        uncancellable(
            async move { super::transition(&state.pool, id, "rejected", "not hired").await },
        )
        .await
        .map_err(|_| DecideError::Internal("the rejection task did not finish".to_owned()))?;
    match rejected {
        Ok(true) => Ok(()),
        Ok(false) => Err(DecideError::AlreadyDecided),
        Err(error) => {
            tracing::warn!(proposal_id = id, %error, "refusing a recruitment failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

async fn reject_team_action(state: &AppState, id: i64) -> Result<(), DecideError> {
    // Refusing closes the action as well as the proposal, in that order: the proposal is the
    // decision and the action is what is left to do about it, and leaving the second `pending`
    // would keep it in the department's queue ceiling forever, blocking the next request over a
    // question already answered.
    let state = state.clone();
    let rejected = uncancellable(async move {
        let decided = super::transition(&state.pool, id, "rejected", "rejected by user").await?;
        if decided {
            crate::team::refuse_action(&state.pool, id).await?;
        }
        Ok::<bool, sqlx::Error>(decided)
    })
    .await
    .map_err(|_| DecideError::Internal("the rejection task did not finish".to_owned()))?;
    match rejected {
        Ok(true) => Ok(()),
        Ok(false) => Err(DecideError::AlreadyDecided),
        Err(error) => {
            tracing::warn!(proposal_id = id, %error, "rejecting a team action failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

async fn reject_action_approval(state: &AppState, id: i64) -> Result<(), DecideError> {
    // Uncancellable: rejecting flips the proposal first and discards the paused run second, and the
    // first half cannot be replayed — a retry finds the proposal no longer `pending` and answers 409.
    let state = state.clone();
    let rejected = uncancellable(async move { super::reject_proposal(&state.pool, id).await })
        .await
        .map_err(|_| DecideError::Internal("the rejection task did not finish".to_owned()))?;
    match rejected {
        Ok(()) => Ok(()),
        Err(super::RejectError::NotFound) => Err(DecideError::NotFound),
        Err(super::RejectError::NotPending) => Err(DecideError::AlreadyDecided),
        Err(super::RejectError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "rejecting a proposal failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

/// The wheel was not given (spec §4.4), and the session goes with the refusal.
///
/// Spec §4.4's diagram draws an arrow back to `agente_conduz`, and this is deliberately narrower —
/// `browser_wheel`'s module comment carries the reasoning. In short: the wall that caused the request
/// is still there, §4.5 already says the run continues without that page, and giving the wheel back
/// would need a second place where the two processes can disagree about who is driving.
async fn reject_browser_wheel(state: AppState, id: i64) -> Result<(), DecideError> {
    let session_id = super::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a wheel request to refuse failed");
            DecideError::Internal("the proposal could not be read".to_owned())
        })?
        .and_then(|proposal| proposal.tool_input)
        .and_then(|input| serde_json::from_str::<Value>(&input).ok())
        .and_then(|input| input.get("session_id").and_then(Value::as_i64));

    if !super::transition(&state.pool, id, "rejected", "wheel refused")
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "rejecting a wheel request failed");
            DecideError::Internal("the decision could not be recorded".to_owned())
        })?
    {
        return Err(DecideError::AlreadyDecided);
    }
    if let Some(session_id) = session_id
        && let Err(error) = crate::browser::wheel::refuse(&state, session_id).await
    {
        // Logged and not returned. The refusal is recorded and that is the part the person asked
        // for; a session left open by a sidecar that did not answer is retired on its next restart.
        tracing::warn!(session = session_id, error = %error, "closing a refused wheel's session failed");
    }
    Ok(())
}

async fn reject_contact_merge(state: &AppState, id: i64) -> Result<(), DecideError> {
    // `reject_merge` records the refused pair in the same transaction as the status, which is what
    // stops the heuristic asking the identical question forever. Wiring the approval without this
    // would have been worse than wiring neither: the suggestion you refused would come back on every
    // sweep, and a suggestion that ignores your answer is not a suggestion.
    let state = state.clone();
    let rejected =
        uncancellable(
            async move { crate::integrations::contacts::reject_merge(&state.pool, id).await },
        )
        .await
        .map_err(|_| DecideError::Internal("the rejection task did not finish".to_owned()))?;
    match rejected {
        Ok(()) => Ok(()),
        // The guarded SELECT and the compare-and-set both report a proposal that is no longer
        // pending this way; either means another decision got there first.
        Err(sqlx::Error::RowNotFound) => Err(DecideError::AlreadyDecided),
        Err(error) => {
            tracing::warn!(proposal_id = id, %error, "rejecting a contact merge failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

async fn reject_refinement(state: &AppState, id: i64) -> Result<(), DecideError> {
    // Unlike the calendar's refusal, this one leaves a row: `refine::reject` marks the refinement
    // `rejected` rather than dropping it, because what the agent kept trying to learn and was told
    // no to is the record the layer's history exists to keep.
    let state = state.clone();
    let refused = uncancellable(async move { crate::knowledge::reject(&state.pool, id).await })
        .await
        .map_err(|_| DecideError::Internal("the rejection task did not finish".to_owned()))?;
    match refused {
        Ok(_) => Ok(()),
        Err(crate::knowledge::DecisionError::NotFound) => Err(DecideError::NotFound),
        Err(crate::knowledge::DecisionError::NotPending) => Err(DecideError::AlreadyDecided),
        Err(crate::knowledge::DecisionError::Malformed) => Err(DecideError::Invalid(
            "this proposal does not name a refinement".to_owned(),
        )),
    }
}

async fn reject_calendar_event(state: &AppState, id: i64) -> Result<(), DecideError> {
    // Refusing a suggested block leaves nothing behind: it is a suggestion declined, not a meeting
    // cancelled, so the calendar never learns it was offered.
    use crate::integrations::calendar::DecisionError;
    let state = state.clone();
    let rejected = uncancellable(async move {
        crate::integrations::calendar::reject_proposed_event(&state.pool, id).await
    })
    .await
    .map_err(|_| DecideError::Internal("the rejection task did not finish".to_owned()))?;
    match rejected {
        Ok(()) => Ok(()),
        Err(DecisionError::NotFound) => Err(DecideError::NotFound),
        Err(DecisionError::NotPending) => Err(DecideError::AlreadyDecided),
        Err(DecisionError::Malformed) => Err(DecideError::Invalid(
            "this proposal carries no usable event".to_owned(),
        )),
        Err(DecisionError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "rejecting a calendar proposal failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

async fn reject_github_action(state: &AppState, id: i64) -> Result<(), DecideError> {
    // Refusing leaves nothing behind, because nothing was done: `github::submit` files the proposal
    // and returns, and the operation reaches `gh` only on the approval path. So this is a status
    // flip with no second half — the opposite of the approve side, which claims the row BEFORE
    // spawning precisely because there IS one there.
    //
    // Its absence was invisible until somebody refused one by hand: the fallback knows only
    // `action-approval`, so a `github-action` came back 409 about a proposal sitting there pending.
    let state = state.clone();
    let rejected = uncancellable(async move {
        super::transition(&state.pool, id, "rejected", "rejected by user").await
    })
    .await
    .map_err(|_| DecideError::Internal("the rejection task did not finish".to_owned()))?;
    match rejected {
        Ok(true) => Ok(()),
        Ok(false) => Err(DecideError::AlreadyDecided),
        Err(error) => {
            tracing::warn!(proposal_id = id, %error, "refusing a github action failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

/// A person took the wheel (spec §4.4, rules 1 and 3).
///
/// The `transition` below is the load-bearing line, and its position is the argument: it is a
/// compare-and-set on `status = 'pending'`, so exactly one of two concurrent approvals wins — and the
/// one that wins is the one that then hands over the browser. Handing over first and recording
/// afterwards would let both callers open a window; recording without handing over would leave a
/// proposal saying a person is driving something that was never started.
///
/// Uncancellable for the same reason as the other arms: a request dropped in between would leave the
/// decision recorded and the window unopened.
async fn approve_browser_wheel(
    state: AppState,
    id: i64,
    seat: Option<String>,
) -> Result<Value, DecideError> {
    let choice = match seat.as_deref() {
        None => None,
        Some("shell") => Some(crate::browser::wheel::SeatChoice::Shell),
        Some("window") => Some(crate::browser::wheel::SeatChoice::Window),
        Some(_) => {
            return Err(DecideError::BadRequest(
                "seat must be \"shell\" or \"window\"".to_owned(),
            ));
        }
    };
    let session_id = super::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a wheel request failed");
            DecideError::Internal("the proposal could not be read".to_owned())
        })?
        .and_then(|proposal| proposal.tool_input)
        .and_then(|input| serde_json::from_str::<Value>(&input).ok())
        .and_then(|input| input.get("session_id").and_then(Value::as_i64))
        .ok_or_else(|| DecideError::Invalid("this wheel request names no session".to_owned()))?;

    // An explicit shell seat that is not on offer is refused BEFORE the decision is made: once the
    // proposal is approved, a refusal would strand the row in `wheel-requested`. Reads only the
    // database, so it does not depend on the pillar being on.
    if choice == Some(crate::browser::wheel::SeatChoice::Shell) {
        let offered = crate::browser::session_row(&state.pool, session_id)
            .await
            .map_err(|error| {
                tracing::warn!(proposal_id = id, %error, "reading a wheel session failed");
                DecideError::Internal("the session could not be read".to_owned())
            })?
            .is_some_and(|row| row.closed_at.is_none() && row.shell_eligible);
        if !offered {
            return Err(DecideError::Conflict(
                json!({ "error": "seat_unavailable" }).to_string(),
            ));
        }
    }

    let transitioned = super::transition(&state.pool, id, "approved", "wheel accepted")
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "accepting a wheel request failed");
            DecideError::Internal("the decision could not be recorded".to_owned())
        })?;
    if !transitioned {
        return Err(DecideError::AlreadyDecided);
    }

    let handover = state.clone();
    match uncancellable(async move {
        crate::browser::wheel::accept_with_seat(&handover, session_id, choice).await
    })
    .await
    .map_err(|_| DecideError::Internal("the handover task did not finish".to_owned()))?
    {
        Ok((row, nonce)) => {
            let mut answer = json!({ "session": row });
            if let Some(nonce) = nonce {
                answer["seat_nonce"] = Value::String(nonce);
            }
            Ok(answer)
        }
        // `NotFound` would say "there is no proposal"; the sentence here is about the session.
        Err(crate::browser::wheel::WheelError::NoSuchSession) => Err(DecideError::Status(
            StatusCode::NOT_FOUND,
            "the session this wheel was asked for is gone".to_owned(),
        )),
        // The window did not open (spec §4.4a). The session is recorded as a failed delivery and the
        // proposal carries the reason; it does NOT go back to the agent.
        Err(error) => Err(DecideError::BadGateway(error.to_string())),
    }
}

/// The default branch: an action approval resumes the paused run it was filed for.
async fn approve_action_approval(
    state: &AppState,
    id: i64,
    proposal: &super::Proposal,
) -> Result<Value, DecideError> {
    // Uncancellable: the approval commits a transaction and only then spawns the resumed run, so a
    // request dropped in between leaves a `running` run nothing will ever drive.
    use crate::execution::runs::ResumeError;
    let state = state.clone();
    let pool = state.pool.clone();
    let resumed =
        uncancellable(async move { crate::execution::runs::resume_approved_run(&state, id).await })
            .await
            .map_err(|_| DecideError::Internal("the approval task did not finish".to_owned()))?;
    match resumed {
        Ok(resume_id) => {
            // Spec B D12: a person's approval is the label of the park the resolver observed.
            if let Some(run_id) = proposal.run_id {
                crate::judge::resolve_review::label_from_decision(
                    &pool,
                    run_id,
                    proposal.tool_input.as_deref(),
                    crate::judge::resolve_review::PersonsDecision::Approve,
                )
                .await;
            }
            Ok(json!({ "resume_run_id": resume_id }))
        }
        Err(ResumeError::ProposalNotFound) => Err(DecideError::NotFound),
        Err(ResumeError::ProposalNotPending) => Err(DecideError::AlreadyDecided),
        // The two 409s mean opposite things — "somebody already answered this" and "this can never
        // be answered" — and the reason is the only thing that separates them.
        Err(ResumeError::NotResumable(reason)) => {
            tracing::warn!(
                proposal_id = id,
                reason,
                "approved proposal is not resumable"
            );
            Err(DecideError::Conflict(format!(
                "this approval cannot resume the run: {reason}"
            )))
        }
        // The approval never returns this, but the `match` is exhaustive.
        Err(ResumeError::BelongsToAJob) => Err(DecideError::Conflict(
            "this run is a job's node, and the job's own policy decides what happens to it"
                .to_owned(),
        )),
        Err(ResumeError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "approving a proposal failed");
            Err(DecideError::Internal(
                "the approval could not be recorded".to_owned(),
            ))
        }
    }
}

async fn approve_contact_merge(state: &AppState, id: i64) -> Result<Value, DecideError> {
    // Uncancellable for the same reason the resume is: the decision commits, and a request dropped
    // mid-flight must not leave the record disagreeing with what happened.
    let state = state.clone();
    let outcome = uncancellable(async move {
        crate::integrations::contacts::approve_merge(&state.pool, id).await
    })
    .await
    .map_err(|_| DecideError::Internal("the merge task did not finish".to_owned()))?;
    merge_decision_response(outcome)
}

async fn approve_refinement(
    state: &AppState,
    id: i64,
    rescope: Option<&str>,
) -> Result<Value, DecideError> {
    // The third kind that starts no run: approving activates the refinement and decides the
    // proposal in one transaction. Uncancellable for the reason the others give — a dropped request
    // must not leave the proposal and the layer disagreeing about whether the agent was allowed to
    // learn something.
    let target = rescope
        .map(|raw| {
            refinement_scope(raw).ok_or_else(|| {
                DecideError::Invalid(format!(
                    "`{raw}` is not a scope a refinement can be approved into: agent:<id>, team:<id> or project:<id>"
                ))
            })
        })
        .transpose()?;
    // A scope naming an agent or a team must name one that exists; and a scope equal to the
    // declared one is the plain approval, so a refusal below can only be about the move.
    let target = match target {
        Some(scope) => {
            refuse_unknown_owner(&state.pool, &scope).await?;
            match crate::knowledge::declared_in(&state.pool, id, &scope).await {
                Ok(true) => None,
                Ok(false) => Some(scope),
                Err(error) => return Err(refinement_refusal(error, id, false)),
            }
        }
        None => None,
    };
    let was_rescoped = target.is_some();
    let state = state.clone();
    let activated = uncancellable(async move {
        crate::knowledge::approve_as(&state.pool, id, target.as_ref()).await
    })
    .await
    .map_err(|_| DecideError::Internal("the approval task did not finish".to_owned()))?;
    match activated {
        Ok(refinement_id) => Ok(json!({ "refinement_id": refinement_id })),
        Err(error) => Err(refinement_refusal(error, id, was_rescoped)),
    }
}

async fn approve_calendar_event(state: &AppState, id: i64) -> Result<Value, DecideError> {
    // Approving writes the event and the decision in one transaction. Uncancellable for the same
    // reason as the others — a dropped request must not leave the proposal and the calendar
    // disagreeing.
    use crate::integrations::calendar::DecisionError;
    let state = state.clone();
    let created = uncancellable(async move {
        crate::integrations::calendar::approve_proposed_event(&state.pool, id).await
    })
    .await
    .map_err(|_| DecideError::Internal("the approval task did not finish".to_owned()))?;
    match created {
        Ok(event_id) => Ok(json!({ "event_id": event_id })),
        Err(DecisionError::NotFound) => Err(DecideError::NotFound),
        Err(DecisionError::NotPending) => Err(DecideError::AlreadyDecided),
        Err(DecisionError::Malformed) => {
            tracing::warn!(
                proposal_id = id,
                "a calendar proposal carried no usable event"
            );
            Err(DecideError::Invalid(
                "this proposal carries no usable event, so there is nothing to create".to_owned(),
            ))
        }
        Err(DecisionError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "approving a calendar proposal failed");
            Err(DecideError::Internal(
                "the event could not be written".to_owned(),
            ))
        }
    }
}

async fn approve_fleet_exclusion(state: &AppState, id: i64) -> Result<Value, DecideError> {
    // Starts no run. Uncancellable for the reason the others are: the rule and the decision that
    // authorised it commit together, and a request dropped mid-flight must not leave a job parked
    // by a rule whose proposal still reads `pending` beside it.
    use crate::governance::exclusion::{Approved, DecisionError as ExclusionError};
    let state = state.clone();
    let decided =
        uncancellable(async move { crate::governance::exclusion::approve(&state.pool, id).await })
            .await
            .map_err(|_| DecideError::Internal("the approval task did not finish".to_owned()))?;
    match decided {
        Ok(Approved::Written(exclusion_id)) => Ok(json!({ "exclusion_id": exclusion_id })),
        // 200 and not an error: the person answered, and the answer was recorded. What changed is
        // that the question had stopped mattering while it waited, and a screen that showed this
        // as a failure would send them looking for a rule that was right not to be written.
        Ok(Approved::Stale) => Ok(json!({
            "exclusion_id": Value::Null,
            "closed": "the jobs it named have ended, so no rule was written",
        })),
        Err(ExclusionError::NotFound) => Err(DecideError::NotFound),
        Err(ExclusionError::NotPending) => Err(DecideError::AlreadyDecided),
        Err(ExclusionError::Malformed) => {
            tracing::warn!(
                proposal_id = id,
                "an exclusion request carried no usable pair"
            );
            Err(DecideError::Invalid(
                "this request names no usable pair of jobs".to_owned(),
            ))
        }
        Err(ExclusionError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "approving an exclusion failed");
            Err(DecideError::Internal(
                "the rule could not be written".to_owned(),
            ))
        }
    }
}

async fn approve_agent_recruit(
    state: &AppState,
    id: i64,
    edited: Option<crate::agent::AgentRequest>,
) -> Result<Value, DecideError> {
    // The only kind that grows the house rather than releasing something: approving writes an
    // `agents` row and a `team_members` row in one transaction with the decision. Uncancellable so
    // a dropped request cannot leave an agent hired into a team nobody agreed to.
    use crate::team::HireError;
    let state = state.clone();
    let hired =
        uncancellable(async move { crate::team::approve_recruit(&state.pool, id, edited).await })
            .await
            .map_err(|_| DecideError::Internal("the approval task did not finish".to_owned()))?;
    match hired {
        Ok(agent_id) => Ok(json!({ "agent_id": agent_id })),
        Err(HireError::NotFound) => Err(DecideError::NotFound),
        Err(HireError::NotPending) => Err(DecideError::AlreadyDecided),
        Err(HireError::Malformed) => Err(DecideError::Invalid(
            "this request does not describe an agent this daemon can create".to_owned(),
        )),
        // 409 and a sentence: the team went away while the request waited, and the person can
        // still dismiss the proposal or recreate the team.
        Err(HireError::NoSuchTeam(team_id)) => Err(DecideError::Conflict(format!(
            "`{team_id}` no longer exists, so there is no team to hire them into"
        ))),
        Err(HireError::Refused(why)) => Err(DecideError::Conflict(why)),
        Err(HireError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "hiring an agent failed");
            Err(DecideError::Internal(
                "the agent could not be written".to_owned(),
            ))
        }
    }
}

async fn approve_team_action(state: &AppState, id: i64) -> Result<Value, DecideError> {
    // The only kind where approving DOES NOTHING but say yes: `team::execute_due_actions` carries
    // the action out on the next tick. A `pending` row survives a restart; an `await` in a handler
    // does not. Uncancellable all the same: the decision commits.
    let state = state.clone();
    let decided = uncancellable(async move {
        crate::proposals::transition(&state.pool, id, "approved", "approved by user").await
    })
    .await
    .map_err(|_| DecideError::Internal("the approval task did not finish".to_owned()))?;
    match decided {
        Ok(true) => Ok(json!({
            "queued": "the department's action will be carried out shortly",
        })),
        // The compare-and-set lost: somebody decided this while the request was in flight.
        Ok(false) => Err(DecideError::AlreadyDecided),
        Err(error) => {
            tracing::warn!(proposal_id = id, %error, "approving a team action failed");
            Err(DecideError::Internal(
                "the approval could not be recorded".to_owned(),
            ))
        }
    }
}

async fn approve_github_action(state: &AppState, id: i64) -> Result<Value, DecideError> {
    // The kind that ACTS on approval, where `team-action` waits for a tick. A pillar answering
    // synchronously has no later pass to be picked up on, so without this the button would approve
    // nothing.
    //
    // Uncancellable for the reason all of its neighbours are, and here it carries more: the work
    // between the claim and the note is a live call to GitHub, and a request dropped across it
    // would leave a comment published and the row saying only that somebody said yes.
    use crate::integrations::github::DecisionError;
    let state = state.clone();
    let ran = uncancellable(async move {
        crate::integrations::github::approve_proposed_operation(&state.pool, &state.github, id)
            .await
    })
    .await
    .map_err(|_| DecideError::Internal("the approval task did not finish".to_owned()))?;
    match ran {
        Ok(outcome) => Ok(json!({
            "operation": outcome.kind,
            "exit_code": outcome.exit_code,
            "stdout": outcome.stdout,
            "output_tail": outcome.output_tail,
        })),
        Err(DecisionError::NotFound) => Err(DecideError::NotFound),
        Err(DecisionError::NotPending) => Err(DecideError::AlreadyDecided),
        Err(DecisionError::Malformed) => {
            tracing::warn!(
                proposal_id = id,
                "a github proposal carried no usable operation"
            );
            Err(DecideError::Invalid(
                "this proposal carries no usable operation, so there is nothing to run".to_owned(),
            ))
        }
        // The proposal is already `approved` and the note beneath it says what happened. The
        // status is the failure's own, so a missing token does not read as a broken daemon.
        Err(DecisionError::Failed(failure)) => Err(DecideError::Status(
            github_failure_status(&failure),
            format!("approved, and it did not run: {failure}"),
        )),
        Err(DecisionError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "approving a github proposal failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

/// The same table `http::common::github_failure_status` holds; that one is private to the HTTP
/// module, which this file must not depend on.
fn github_failure_status(failure: &crate::integrations::github::Failure) -> StatusCode {
    match failure {
        crate::integrations::github::Failure::NotConfigured
        | crate::integrations::github::Failure::MissingCli => StatusCode::SERVICE_UNAVAILABLE,
        crate::integrations::github::Failure::MissingToken => StatusCode::FORBIDDEN,
        crate::integrations::github::Failure::TimedOut => StatusCode::GATEWAY_TIMEOUT,
        crate::integrations::github::Failure::Unknown(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// `agent:<id>`, `team:<id>` or `project:<id>`; the same grammar as `http::common::refinement_target`.
fn refinement_scope(raw: &str) -> Option<crate::knowledge::Scope> {
    let (kind, id) = raw.split_once(':')?;
    if id.is_empty() {
        return None;
    }
    match kind {
        "project" => Some(crate::knowledge::Scope::Project(id.to_owned())),
        "team" => Some(crate::knowledge::Scope::Team(id.to_owned())),
        "agent" => Some(crate::knowledge::Scope::Agent(id.to_owned())),
        _ => None,
    }
}

/// Turns a merge decision's outcome into the answer the caller gets.
///
/// The conflict is a 409 with a body, not a bare status: it is the one refusal here that names
/// something the person can go and change, and a status code cannot say which two instructions
/// disagree.
fn merge_decision_response(
    outcome: Result<
        crate::integrations::contacts::MergeOutcome,
        crate::integrations::contacts::DecisionError,
    >,
) -> Result<Value, DecideError> {
    match outcome {
        Ok(crate::integrations::contacts::MergeOutcome::Merged) => Ok(json!({ "merged": true })),
        Ok(crate::integrations::contacts::MergeOutcome::RefusedConflictingVerdicts {
            keep,
            absorb,
        }) => {
            tracing::info!(%keep, %absorb, "refused a contact merge with conflicting verdicts");
            Err(DecideError::Conflict(format!(
                "these two people carry standing decisions that disagree — {keep} against {absorb}; settle one of them and decide this again"
            )))
        }
        Err(crate::integrations::contacts::DecisionError::NotPending) => Err(
            DecideError::Conflict("this suggestion has already been decided".to_owned()),
        ),
        Err(crate::integrations::contacts::DecisionError::Db(error)) => {
            tracing::warn!(%error, "deciding a contact merge failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

/// 422 when `scope` names an agent or a team the núcleo does not have. A project is not checked:
/// a project id is whatever a run was started with, and has no table of its own to be missing from.
async fn refuse_unknown_owner(
    pool: &sqlx::SqlitePool,
    scope: &crate::knowledge::Scope,
) -> Result<(), DecideError> {
    let (sql, noun, owner) = match scope {
        crate::knowledge::Scope::Agent(owner) => {
            ("SELECT 1 FROM agents WHERE id = ?", "agent", owner)
        }
        crate::knowledge::Scope::Team(owner) => ("SELECT 1 FROM teams WHERE id = ?", "team", owner),
        _ => return Ok(()),
    };
    let found: Option<i64> = sqlx::query_scalar(sql)
        .bind(owner)
        .fetch_optional(pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "checking the scope an approval names failed");
            DecideError::Internal("the scope could not be checked".to_owned())
        })?;
    match found {
        Some(_) => Ok(()),
        None => Err(DecideError::Invalid(format!(
            "there is no {noun} `{owner}` to keep this refinement"
        ))),
    }
}

/// What the refinement door says when deciding refuses. `Malformed` after a real move can only be
/// the move itself (a successor stays where its predecessor lived); otherwise the proposal names
/// no row.
fn refinement_refusal(
    error: crate::knowledge::DecisionError,
    _id: i64,
    was_rescoped: bool,
) -> DecideError {
    match error {
        crate::knowledge::DecisionError::NotFound => DecideError::NotFound,
        crate::knowledge::DecisionError::NotPending => DecideError::AlreadyDecided,
        crate::knowledge::DecisionError::Malformed => DecideError::Invalid(if was_rescoped {
            "this refinement replaces an earlier one, so it can only be approved in the scope \
             it was declared in"
                .to_owned()
        } else {
            "this proposal does not name a refinement".to_owned()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistants::NoAssistants;
    use crate::auth::Token;
    use crate::execution::runner::FakeCommandRunner;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    async fn test_state() -> AppState {
        AppState {
            startup_config: Default::default(),
            token: Token("t".into()),
            pool: crate::testdb::fresh_pool().await,
            channels: Default::default(),
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: Arc::new(NoAssistants),
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_messages: Arc::new(Mutex::new(HashMap::new())),
            run_tails: Default::default(),
            fleet: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: Arc::new(crate::secrets::InMemorySecrets::default()),
            email: Arc::new(crate::state::EmailRuntime::default()),
            voice: Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
            council: Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
            utility_model: None,
        }
    }

    async fn a_contact(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query("INSERT INTO contacts (display_name, created_at) VALUES (NULL, ?)")
            .bind("2026-10-19T08:00:00+00:00")
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
    }

    #[tokio::test]
    async fn decide_approve_unknown_proposal_is_not_found() {
        let state = test_state().await;
        let result = approve(&state, 999_999, ApproveInput::default()).await;
        assert!(matches!(result, Err(DecideError::NotFound)), "{result:?}");
    }

    #[tokio::test]
    async fn decide_approve_contact_merge_twice_is_already_decided() {
        let state = test_state().await;
        let keep = a_contact(&state.pool).await;
        let absorb = a_contact(&state.pool).await;
        let id = crate::proposals::create_contact_merge(&state.pool, keep, absorb, "same name")
            .await
            .unwrap();

        let first = approve(&state, id, ApproveInput::default()).await.unwrap();
        assert_eq!(first, json!({ "merged": true }));

        // The second decision finds the proposal no longer pending; the contact-merge door says
        // so with its own sentence, a 409 either way.
        let second = approve(&state, id, ApproveInput::default()).await;
        let error = second.expect_err("a second approval must be refused");
        let (status, body) = error.into_http(id);
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, "this suggestion has already been decided");
    }

    #[tokio::test]
    async fn decide_approve_team_action_queues_and_second_call_is_already_decided() {
        let state = test_state().await;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO proposals (kind, status, reasoning, created_at) VALUES ('team-action', 'pending', 'send the summary', ?) RETURNING id",
        )
        .bind("2026-10-19T08:00:00+00:00")
        .fetch_one(&state.pool)
        .await
        .unwrap();

        let first = approve(&state, id, ApproveInput::default()).await.unwrap();
        assert_eq!(
            first,
            json!({ "queued": "the department's action will be carried out shortly" })
        );

        let second = approve(&state, id, ApproveInput::default()).await;
        assert!(
            matches!(second, Err(DecideError::AlreadyDecided)),
            "{second:?}"
        );
    }

    #[tokio::test]
    async fn decide_approve_action_approval_unknown_is_not_found() {
        let state = test_state().await;
        // No proposal 424242 exists, so the read at the top of `approve` answers before any kind
        // is chosen; the default branch is never reached with a missing row.
        let result = approve(&state, 424_242, ApproveInput::default()).await;
        assert!(matches!(result, Err(DecideError::NotFound)), "{result:?}");
    }

    #[tokio::test]
    async fn decide_approve_action_approval_twice_is_already_decided() {
        let state = test_state().await;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO proposals (kind, status, reasoning, created_at) VALUES ('action-approval', 'approved', 'run the tests', ?) RETURNING id",
        )
        .bind("2026-10-19T08:00:00+00:00")
        .fetch_one(&state.pool)
        .await
        .unwrap();

        let result = approve(&state, id, ApproveInput::default()).await;
        assert!(
            matches!(result, Err(DecideError::AlreadyDecided)),
            "{result:?}"
        );
    }

    /// A pending wheel request whose payload names `session_id` (or nothing, with `None`).
    async fn a_wheel_request(pool: &sqlx::SqlitePool, session_id: Option<i64>) -> i64 {
        let tool_input = session_id.map(|session| json!({ "session_id": session }).to_string());
        sqlx::query_scalar(
            "INSERT INTO proposals (kind, status, reasoning, tool_input, created_at) \
             VALUES ('browser-wheel', 'pending', 'login', ?, ?) RETURNING id",
        )
        .bind(tool_input)
        .bind("2026-10-19T08:00:00+00:00")
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// A `wheel-requested` session row, not shell-eligible.
    async fn a_wheel_session(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query(
            "INSERT INTO browser_sessions \
                (sidecar_id, run_id, project_id, profile_kind, profile_id, requested_url, \
                 final_url, rule, mode, opened_at) \
             VALUES ('s1', 7, 'acme', 'ephemeral', 'run-7', 'https://jira.example.org/login', \
                     'https://jira.example.org/login', 'project-site', 'wheel-requested', \
                     '2026-08-16T10:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn status_of(pool: &sqlx::SqlitePool, id: i64) -> String {
        super::super::get(pool, id).await.unwrap().unwrap().status
    }

    #[tokio::test]
    async fn decide_approve_browser_wheel_with_unknown_seat_is_invalid() {
        let state = test_state().await;
        let session = a_wheel_session(&state.pool).await;
        let id = a_wheel_request(&state.pool, Some(session)).await;
        let input = ApproveInput {
            seat: Some("sofa".to_owned()),
            ..Default::default()
        };

        let result = approve(&state, id, input).await;
        assert!(
            matches!(result, Err(DecideError::BadRequest(_))),
            "{result:?}"
        );
        assert_eq!(status_of(&state.pool, id).await, "pending");
    }

    #[tokio::test]
    async fn decide_approve_browser_wheel_bad_request_is_400() {
        let state = test_state().await;
        let id = a_wheel_request(&state.pool, Some(1)).await;
        let input = ApproveInput {
            seat: Some("sofa".to_owned()),
            ..Default::default()
        };

        let error = approve(&state, id, input).await.unwrap_err();
        assert_eq!(
            error.into_http(id),
            (
                StatusCode::BAD_REQUEST,
                "seat must be \"shell\" or \"window\"".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn decide_approve_browser_wheel_naming_no_session_is_422() {
        let state = test_state().await;
        let id = a_wheel_request(&state.pool, None).await;

        let error = approve(&state, id, ApproveInput::default())
            .await
            .unwrap_err();
        assert_eq!(
            error.into_http(id),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "this wheel request names no session".to_owned()
            )
        );
        assert_eq!(status_of(&state.pool, id).await, "pending");
    }

    #[tokio::test]
    async fn decide_approve_browser_wheel_shell_seat_not_offered_is_409() {
        let state = test_state().await;
        let session = a_wheel_session(&state.pool).await;
        let id = a_wheel_request(&state.pool, Some(session)).await;
        let input = ApproveInput {
            seat: Some("shell".to_owned()),
            ..Default::default()
        };

        let error = approve(&state, id, input).await.unwrap_err();
        assert_eq!(
            error.into_http(id),
            (
                StatusCode::CONFLICT,
                json!({ "error": "seat_unavailable" }).to_string()
            )
        );
        assert_eq!(
            status_of(&state.pool, id).await,
            "pending",
            "the decision must not have been made"
        );
    }

    #[tokio::test]
    async fn decide_approve_browser_wheel_gateway_failure_is_502() {
        // The pillar is off in the test state, so the handover fails after the decision is made.
        let state = test_state().await;
        let session = a_wheel_session(&state.pool).await;
        let id = a_wheel_request(&state.pool, Some(session)).await;

        let error = approve(&state, id, ApproveInput::default())
            .await
            .unwrap_err();
        assert_eq!(
            error.into_http(id),
            (
                StatusCode::BAD_GATEWAY,
                "the browser pillar is off".to_owned()
            )
        );
        assert_eq!(status_of(&state.pool, id).await, "approved");
    }

    #[tokio::test]
    async fn decide_approve_browser_wheel_session_gone_is_404() {
        let mut state = test_state().await;
        state.browser = Arc::new(crate::browser::BrowserRuntime {
            enabled: true,
            ..crate::browser::BrowserRuntime::disabled()
        });
        let id = a_wheel_request(&state.pool, Some(999_999)).await;

        let error = approve(&state, id, ApproveInput::default())
            .await
            .unwrap_err();
        assert_eq!(
            error.into_http(id),
            (
                StatusCode::NOT_FOUND,
                "the session this wheel was asked for is gone".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn decide_approve_browser_wheel_twice_is_already_decided() {
        let state = test_state().await;
        let session = a_wheel_session(&state.pool).await;
        let id = a_wheel_request(&state.pool, Some(session)).await;

        // The first call decides it (and fails to hand over, the pillar being off).
        let _ = approve(&state, id, ApproveInput::default()).await;
        let second = approve(&state, id, ApproveInput::default()).await;
        assert!(
            matches!(second, Err(DecideError::AlreadyDecided)),
            "{second:?}"
        );
    }

    #[tokio::test]
    async fn decide_reject_unknown_proposal_is_not_found() {
        let state = test_state().await;
        let result = reject(&state, 999_999).await;
        assert!(matches!(result, Err(DecideError::NotFound)), "{result:?}");
    }

    #[tokio::test]
    async fn decide_reject_contact_merge_twice_is_already_decided() {
        let state = test_state().await;
        let keep = a_contact(&state.pool).await;
        let absorb = a_contact(&state.pool).await;
        let id = crate::proposals::create_contact_merge(&state.pool, keep, absorb, "same name")
            .await
            .unwrap();

        reject(&state, id).await.unwrap();
        assert_eq!(status_of(&state.pool, id).await, "rejected");

        let second = reject(&state, id).await;
        assert!(
            matches!(second, Err(DecideError::AlreadyDecided)),
            "{second:?}"
        );
    }

    #[tokio::test]
    async fn decide_reject_twice_is_already_decided() {
        let state = test_state().await;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO proposals (kind, status, reasoning, created_at) VALUES ('team-action', 'pending', 'send the summary', ?) RETURNING id",
        )
        .bind("2026-10-19T08:00:00+00:00")
        .fetch_one(&state.pool)
        .await
        .unwrap();

        reject(&state, id).await.unwrap();
        assert_eq!(status_of(&state.pool, id).await, "rejected");

        let second = reject(&state, id).await;
        assert!(
            matches!(second, Err(DecideError::AlreadyDecided)),
            "{second:?}"
        );
    }

    #[tokio::test]
    async fn decide_reject_action_approval_unknown_is_not_found() {
        let state = test_state().await;
        // The read at the top of `reject` answers before any kind is chosen, so the default branch
        // is never reached with a missing row.
        let result = reject(&state, 424_242).await;
        assert!(matches!(result, Err(DecideError::NotFound)), "{result:?}");
    }

    #[test]
    fn decide_error_into_status_maps_each_variant_and_carries_no_body() {
        // `into_status` returns a bare `StatusCode`: the type is what guarantees no body, and the
        // messages below are given only to show they do not change the answer.
        let cases = [
            (DecideError::NotFound, StatusCode::NOT_FOUND),
            (DecideError::AlreadyDecided, StatusCode::CONFLICT),
            (DecideError::Conflict("c".to_owned()), StatusCode::CONFLICT),
            (
                DecideError::Invalid("i".to_owned()),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                DecideError::BadRequest("b".to_owned()),
                StatusCode::BAD_REQUEST,
            ),
            (
                DecideError::BadGateway("g".to_owned()),
                StatusCode::BAD_GATEWAY,
            ),
            (
                DecideError::Internal("x".to_owned()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                DecideError::Internal(String::new()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                DecideError::Status(StatusCode::FORBIDDEN, "s".to_owned()),
                StatusCode::FORBIDDEN,
            ),
        ];
        for (error, status) in cases {
            let label = format!("{error:?}");
            let answered: StatusCode = error.into_status();
            assert_eq!(answered, status, "{label}");
        }
    }

    #[test]
    fn decide_error_into_http_maps_each_variant_to_its_status() {
        let cases = [
            (
                DecideError::NotFound,
                StatusCode::NOT_FOUND,
                "there is no proposal 7",
            ),
            (
                DecideError::AlreadyDecided,
                StatusCode::CONFLICT,
                "this proposal has already been decided",
            ),
            (
                DecideError::Conflict("c".to_owned()),
                StatusCode::CONFLICT,
                "c",
            ),
            (
                DecideError::Invalid("i".to_owned()),
                StatusCode::UNPROCESSABLE_ENTITY,
                "i",
            ),
            (
                DecideError::BadRequest("b".to_owned()),
                StatusCode::BAD_REQUEST,
                "b",
            ),
            (
                DecideError::BadGateway("g".to_owned()),
                StatusCode::BAD_GATEWAY,
                "g",
            ),
            (
                DecideError::Internal("x".to_owned()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "x",
            ),
            (
                DecideError::Internal(String::new()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "",
            ),
            (
                DecideError::Status(StatusCode::FORBIDDEN, "s".to_owned()),
                StatusCode::FORBIDDEN,
                "s",
            ),
        ];
        for (error, status, body) in cases {
            let label = format!("{error:?}");
            assert_eq!(error.into_http(7), (status, body.to_owned()), "{label}");
        }
    }
}
