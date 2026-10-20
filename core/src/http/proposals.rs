//! Proposals and the waiting queue (`/proposals/*`, `/waiting/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/proposals", get(get_proposals))
        // The sidebar's "waiting on you" number, read in one request instead of seven.
        .route("/waiting/count", get(get_waiting_count))
        .route("/waiting/git", get(get_waiting_git))
        // A literal at the same depth as no `{id}` sibling — `/proposals/{id}` is not a route, only
        // `/proposals/{id}/approve` and `/reject` one segment deeper — so the shadowing question
        // that `/runs/awaiting-approval` raises does not arise here.
        .route("/proposals/skipped-items", get(get_skipped_items))
        .route("/proposals/team-actions", get(get_team_action_proposals))
        .route("/proposals/recruits", get(get_recruit_proposals))
        .route("/proposals/refused-actions", get(get_refused_actions))
        .route("/proposals/{id}/approve", post(post_proposal_approve))
        .route("/proposals/{id}/reject", post(post_proposal_reject))
        .route(
            "/proposals/{id}/decline-action",
            post(post_proposal_decline_action),
        )
        .route("/proposals/{id}/dismiss", post(post_proposal_dismiss))
}

/// The things departments have asked for and nobody has answered yet.
///
/// A door of its own rather than a slice of `/proposals`, which filters to `action-approval` and
/// would need widening — and widening it would put two decisions with the same button next to each
/// other: one resumes a paused run holding a worktree, the other authorises an email from a
/// department that finished hours ago. `list_skipped_items` split off for exactly this reason.
pub(super) async fn get_team_action_proposals(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::proposals::Proposal>>, StatusCode> {
    crate::proposals::list_pending_team_actions(&state.pool)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "reading pending team actions failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// The specialists directors asked for and nobody has answered.
///
/// A fourth door, and separate for the reason the third is: the button says "Hire", not "Approve",
/// because what it does is different from the rest of the queue — and unlike every other proposal
/// in the house, this one is EDITABLE at the moment of decision. Sharing a list with things that
/// are not editable would mean one form that pretends the fields are read-only half the time.
pub(super) async fn get_recruit_proposals(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::proposals::Proposal>>, StatusCode> {
    crate::proposals::list_pending_recruits(&state.pool, None)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "reading pending recruitments failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub(super) async fn get_proposals(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::proposals::Proposal>>, StatusCode> {
    crate::proposals::list_pending(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// What the night decided not to do, and why.
///
/// The half of the skip that was missing. A job's node that hits an action needing approval marks
/// its item `skipped`, reverts the tree and lets the queue carry on — and files a `skipped-item`
/// proposal so the morning knows what was set aside. `list_pending` deliberately does not carry
/// those (approving one would resume nothing), which left the record with no door at all: measured
/// on 2026-08-08, two items skipped and the only way to read either was to open the database.
/// How many of each decision list are waiting on a person — `GET /waiting/count`.
///
/// One field per list the shell's `countWaitingDecisions` sums, counted the way that function
/// counts it. A list that fails to read is `null` rather than a 500, because the shell already
/// treats a partial count as more useful than a blank one.
#[derive(Debug, Default, serde::Serialize)]
pub(super) struct WaitingCount {
    pub(super) wheel: Option<usize>,
    pub(super) approvals: Option<usize>,
    pub(super) team_actions: Option<usize>,
    pub(super) recruits: Option<usize>,
    pub(super) merges: Option<usize>,
    pub(super) exclusions: Option<usize>,
    pub(super) git: Option<usize>,
}

/// The badge every page of the shell draws, as one read.
///
/// It used to be seven polls from every window, each shipping a whole list only for its length to
/// be taken. The lists themselves stay where they were for the pages that draw them; this is only
/// the number.
pub(super) async fn get_waiting_count(State(state): State<AppState>) -> Json<WaitingCount> {
    let pool = &state.pool;
    // Counts, not lists: this is polled every few seconds by every page, and each list used to be
    // materialised in full (joins, JSON, `output_tail`) only to take its length.
    let (wheel, approvals, team_actions, recruits, merges, exclusions, git) = tokio::join!(
        crate::browser::count_wheel_requests(pool),
        crate::proposals::count_pending(pool),
        crate::proposals::count_pending_team_actions(pool),
        crate::proposals::count_pending_recruits(pool),
        crate::integrations::contacts::count_pending_merges(pool),
        crate::governance::exclusion::count_pending_requests(pool),
        vcs::count_waiting_on_a_person(pool),
    );
    Json(WaitingCount {
        // The shell's `isWheelRequest`: an agent asked for the wheel and a proposal stands behind it.
        wheel: wheel.ok(),
        approvals: approvals.ok(),
        team_actions: team_actions.ok(),
        recruits: recruits.ok(),
        merges: merges.ok(),
        exclusions: exclusions.ok(),
        git: git.ok(),
    })
}

/// The git requests that still want a person, uncapped: the same predicate the badge counts, so the
/// Waiting page and its number cannot disagree. Admin-only by default-deny, like `/waiting/count`
/// — no scope table entry.
pub(super) async fn get_waiting_git(
    State(state): State<AppState>,
) -> Result<Json<Vec<vcs::RequestSummary>>, StatusCode> {
    vcs::waiting_on_a_person(&state.pool)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "listing the git requests that want a person failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub(super) async fn get_skipped_items(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::proposals::Proposal>>, StatusCode> {
    crate::proposals::list_skipped_items(&state.pool)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "listing skipped items failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// What the injection barrier refused, and what it was going to do.
///
/// The same shape as the listing above and for the same reason: neither kind can be approved into
/// happening, so neither belongs on `/proposals`, whose two buttons answer 409 for anything but an
/// `action-approval`. Its own route rather than sharing `/proposals/skipped-items`, because those
/// are a job's items and the shell renders them inside a job graph — a refused email has
/// no graph to sit in and would arrive there as an orphan.
pub(super) async fn get_refused_actions(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::proposals::Proposal>>, StatusCode> {
    crate::proposals::list_refused_actions(&state.pool)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "listing refused actions failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Puts a read skipped item away. Its own door, not a third arm of `/reject`.
///
/// Nothing is being refused here and nothing is released — the job let go of the item and the
/// worktree when it skipped, hours before anyone read this. Sharing `/reject` would give the two a
/// single button whose label is wrong for one of them, and `reject_proposal` guards on
/// `kind = 'action-approval'`, so that button would answer 409 half the time.
pub(super) async fn post_proposal_dismiss(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    match crate::proposals::dismiss_skipped_item(&state.pool, id).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(crate::proposals::RejectError::NotFound) => Err(StatusCode::NOT_FOUND),
        // Also the answer for a proposal of any other kind: nothing else is dismissable, and a
        // caller that aimed this at an action approval wanted `/reject`.
        Err(crate::proposals::RejectError::NotPending) => Err(StatusCode::CONFLICT),
        Err(crate::proposals::RejectError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "dismissing a skipped item failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// **Every refusal here carries a sentence, and that is the whole reason this returns a tuple.**
///
/// This route can answer 409 for two reasons that mean opposite things to the person who clicked:
/// the proposal is no longer pending (somebody already decided it, and the right response is to
/// stop) or the approval cannot resume (the run's worktree is gone, and the right response is to
/// start the work again). A status code cannot tell them apart, and the `NotResumable` arm below
/// used to say so in a comment while logging the reason server-side and sending nothing —
/// `create_job` had already settled the shape this follows.
///
/// The case that made it worth doing is real rather than hypothetical: `mode: "real"` is the API's
/// DEFAULT and creates no worktree, so approving a merge in such a run is refused by a mechanism
/// nobody can see, and the refusal is indistinguishable from a button that did not fire.
/// The body `approve` accepts, and the only kind that reads one.
///
/// `Option` and last in the argument list, so every existing caller — which sends no body at all —
/// is unaffected. A recruitment is a SUGGESTION: the director knows the name, the speciality and
/// the prompt well, and knows the engine, the model and the tool policy badly, because those are
/// what cost money per turn and what widen a surface. So the person approving may correct them, and
/// `agent::validate` runs over the correction.
#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct ApproveBody {
    pub(super) hire: Option<crate::agent::AgentRequest>,
    /// Only a browser-wheel approval reads it: `"shell"` or `"window"`.
    pub(super) seat: Option<String>,
    /// Only a `refinement` approval reads it: `agent:<id>`, `team:<id>` or `project:<id>`.
    pub(super) scope: Option<String>,
}

pub(super) async fn post_proposal_approve(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    // Last, because axum requires a body extractor to be — and optional, because four of the five
    // kinds through this door send nothing.
    body: Option<Json<ApproveBody>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (edited, seat, rescope) = match body {
        Some(Json(body)) => (body.hire, body.seat, body.scope),
        None => (None, None, None),
    };
    // Two kinds of proposal share this table and this door, and they are decided by entirely
    // different machinery: an action approval resumes a paused run, a contact merge joins two
    // people and touches no run at all. Reading the kind first is only a dispatch — the kind never
    // changes, and both paths below are compare-and-set on `status = 'pending'`, so a second
    // decision racing this one still loses there rather than here.
    let proposal = crate::proposals::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a proposal to approve failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the proposal could not be read".to_owned(),
            )
        })?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("there is no proposal {id}")))?;
    let kind = proposal.kind.clone();
    // Every kind but the browser wheel is decided by the service function. This reads the proposal
    // once to learn the kind and `decide::approve` reads it again, so an unknown id still answers
    // 404 from the read above.
    if kind != "browser-wheel" {
        return crate::proposals::approve(
            &state,
            id,
            crate::proposals::ApproveInput {
                hire: edited,
                seat,
                scope: rescope,
            },
        )
        .await
        .map(Json)
        .map_err(|error| error.into_http(id));
    }

    approve_browser_wheel(state, id, seat).await
}

/// A person took the wheel (spec §4.4, rules 1 and 3).
///
/// The `transition` below is the load-bearing line, and its position is the argument: it is a
/// compare-and-set on `status = 'pending'`, so exactly one of two concurrent approvals wins — and the
/// one that wins is the one that then hands over the browser. Handing over first and recording
/// afterwards would let both callers open a window; recording without handing over would leave a
/// proposal saying a person is driving something that was never started.
///
/// Uncancellable for the same reason as the three arms above: a request dropped in between would
/// leave the decision recorded and the window unopened.
pub(super) async fn approve_browser_wheel(
    state: AppState,
    id: i64,
    seat: Option<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let choice = match seat.as_deref() {
        None => None,
        Some("shell") => Some(crate::browser::wheel::SeatChoice::Shell),
        Some("window") => Some(crate::browser::wheel::SeatChoice::Window),
        Some(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "seat must be \"shell\" or \"window\"".to_owned(),
            ));
        }
    };
    let session_id = crate::proposals::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a wheel request failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the proposal could not be read".to_owned(),
            )
        })?
        .and_then(|proposal| proposal.tool_input)
        .and_then(|input| serde_json::from_str::<serde_json::Value>(&input).ok())
        .and_then(|input| input.get("session_id").and_then(serde_json::Value::as_i64))
        .ok_or_else(|| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "this wheel request names no session".to_owned(),
            )
        })?;

    // An explicit shell seat that is not on offer is refused BEFORE the decision is made: once the
    // proposal is approved, a refusal would strand the row in `wheel-requested`. Reads only the
    // database, so it does not depend on the pillar being on.
    if choice == Some(crate::browser::wheel::SeatChoice::Shell) {
        let offered = crate::browser::session_row(&state.pool, session_id)
            .await
            .map_err(|error| {
                tracing::warn!(proposal_id = id, %error, "reading a wheel session failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "the session could not be read".to_owned(),
                )
            })?
            .is_some_and(|row| row.closed_at.is_none() && row.shell_eligible);
        if !offered {
            return Err((
                StatusCode::CONFLICT,
                serde_json::json!({ "error": "seat_unavailable" }).to_string(),
            ));
        }
    }

    let transitioned = crate::proposals::transition(&state.pool, id, "approved", "wheel accepted")
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "accepting a wheel request failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the decision could not be recorded".to_owned(),
            )
        })?;
    if !transitioned {
        return Err((
            StatusCode::CONFLICT,
            "this proposal has already been decided".to_owned(),
        ));
    }

    let handover = state.clone();
    match uncancellable(async move {
        crate::browser::wheel::accept_with_seat(&handover, session_id, choice).await
    })
    .await
    .map_err(|status| (status, "the handover task did not finish".to_owned()))?
    {
        Ok((row, nonce)) => {
            let mut answer = serde_json::json!({ "session": row });
            if let Some(nonce) = nonce {
                answer["seat_nonce"] = serde_json::Value::String(nonce);
            }
            Ok(Json(answer))
        }
        Err(crate::browser::wheel::WheelError::NoSuchSession) => Err((
            StatusCode::NOT_FOUND,
            "the session this wheel was asked for is gone".to_owned(),
        )),
        // The window did not open (spec §4.4a). The session is recorded as a failed delivery and the
        // proposal carries the reason; it does NOT go back to the agent.
        Err(error) => Err((StatusCode::BAD_GATEWAY, error.to_string())),
    }
}

/// The wheel was not given (spec §4.4), and the session goes with the refusal.
///
/// Spec §4.4's diagram draws an arrow back to `agente_conduz`, and this is deliberately narrower —
/// `browser_wheel`'s module comment carries the reasoning. In short: the wall that caused the request
/// is still there, §4.5 already says the run continues without that page, and giving the wheel back
/// would need a second place where the two processes can disagree about who is driving.
pub(super) async fn reject_browser_wheel(
    state: AppState,
    id: i64,
) -> Result<StatusCode, StatusCode> {
    let session_id = crate::proposals::get(&state.pool, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .and_then(|proposal| proposal.tool_input)
        .and_then(|input| serde_json::from_str::<serde_json::Value>(&input).ok())
        .and_then(|input| input.get("session_id").and_then(serde_json::Value::as_i64));

    if !crate::proposals::transition(&state.pool, id, "rejected", "wheel refused")
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "rejecting a wheel request failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
    {
        return Err(StatusCode::CONFLICT);
    }
    if let Some(session_id) = session_id
        && let Err(error) = crate::browser::wheel::refuse(&state, session_id).await
    {
        // Logged and not returned. The refusal is recorded and that is the part the person asked
        // for; a session left open by a sidecar that did not answer is retired on its next restart.
        tracing::warn!(session = session_id, error = %error, "closing a refused wheel's session failed");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Spec B D12: refuse only the action and let the run continue without it. Only for a pending
/// `action-approval` of a run outside a job; uncancellable for `post_proposal_approve`'s reason —
/// it commits a transaction and only then launches the continued run.
pub(super) async fn post_proposal_decline_action(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let proposal = crate::proposals::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a proposal to decline failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the proposal could not be read".to_owned(),
            )
        })?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("there is no proposal {id}")))?;
    let pool = state.pool.clone();
    match uncancellable(async move { crate::execution::runs::decline_action(&state, id).await })
        .await
        .map_err(|status| (status, "the decline task did not finish".to_owned()))?
    {
        Ok(run_id) => {
            // Spec B D12: declining only the action is the label `explain` of the park it answered.
            if let Some(proposal_run_id) = proposal.run_id {
                crate::judge::resolve_review::label_from_decision(
                    &pool,
                    proposal_run_id,
                    proposal.tool_input.as_deref(),
                    crate::judge::resolve_review::PersonsDecision::Decline,
                )
                .await;
            }
            Ok(Json(serde_json::json!({ "resume_run_id": run_id })))
        }
        Err(crate::execution::runs::ResumeError::ProposalNotFound) => {
            Err((StatusCode::NOT_FOUND, format!("there is no proposal {id}")))
        }
        Err(crate::execution::runs::ResumeError::ProposalNotPending) => Err((
            StatusCode::CONFLICT,
            "this proposal has already been decided".to_owned(),
        )),
        Err(crate::execution::runs::ResumeError::BelongsToAJob) => Err((
            StatusCode::CONFLICT,
            "this run is a job's node; approve or reject it, and the job's own policy decides the rest"
                .to_owned(),
        )),
        Err(crate::execution::runs::ResumeError::NotResumable(reason)) => Err((
            StatusCode::CONFLICT,
            format!("this run cannot continue: {reason}"),
        )),
        Err(crate::execution::runs::ResumeError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "declining an action failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "the decline could not be recorded".to_owned(),
            ))
        }
    }
}

pub(super) async fn post_proposal_reject(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    let kind = crate::proposals::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a proposal to reject failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?
        .kind;
    if kind == "browser-wheel" {
        return reject_browser_wheel(state, id).await;
    }
    if kind == "contact-merge" {
        // `reject_merge` records the refused pair in the same transaction as the status, which is
        // what stops the heuristic asking the identical question forever. Wiring the approval
        // without this would have been worse than wiring neither: the suggestion you refused would
        // come back on every sweep, and a suggestion that ignores your answer is not a suggestion.
        let state = state.clone();
        let rejected = uncancellable(async move {
            crate::integrations::contacts::reject_merge(&state.pool, id).await
        })
        .await?;
        return match rejected {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            // The guarded SELECT and the compare-and-set both report a proposal that is no longer
            // pending this way; either means another decision got there first.
            Err(sqlx::Error::RowNotFound) => Err(StatusCode::CONFLICT),
            Err(error) => {
                tracing::warn!(proposal_id = id, %error, "rejecting a contact merge failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    if kind == "refinement" {
        // Unlike the calendar's refusal below, this one leaves a row: `refine::reject` marks the
        // refinement `rejected` rather than dropping it, because what the agent kept trying to
        // learn and was told no to is the record the layer's history exists to keep.
        let state = state.clone();
        let refused = uncancellable(async move { crate::knowledge::reject(&state.pool, id).await })
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return match refused {
            Ok(_) => Ok(StatusCode::NO_CONTENT),
            Err(crate::knowledge::DecisionError::NotFound) => Err(StatusCode::NOT_FOUND),
            Err(crate::knowledge::DecisionError::NotPending) => Err(StatusCode::CONFLICT),
            Err(crate::knowledge::DecisionError::Malformed) => {
                Err(StatusCode::UNPROCESSABLE_ENTITY)
            }
        };
    }

    if kind == "calendar-event" {
        // Refusing a suggested block leaves nothing behind: it is a suggestion declined, not a
        // meeting cancelled, so the calendar never learns it was offered.
        let state = state.clone();
        let rejected = uncancellable(async move {
            crate::integrations::calendar::reject_proposed_event(&state.pool, id).await
        })
        .await?;
        return match rejected {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(crate::integrations::calendar::DecisionError::NotFound) => {
                Err(StatusCode::NOT_FOUND)
            }
            Err(crate::integrations::calendar::DecisionError::NotPending) => {
                Err(StatusCode::CONFLICT)
            }
            Err(crate::integrations::calendar::DecisionError::Malformed) => {
                Err(StatusCode::UNPROCESSABLE_ENTITY)
            }
            Err(crate::integrations::calendar::DecisionError::Db(error)) => {
                tracing::warn!(proposal_id = id, %error, "rejecting a calendar proposal failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    if kind == "fleet-exclusion" {
        // Refusing an edge leaves nothing behind, because drawing it changed nothing: no rule was
        // written, no run was paused, no worktree is held. It is the one refusal on this route with
        // no second half to undo.
        let state = state.clone();
        let rejected =
            uncancellable(
                async move { crate::governance::exclusion::reject(&state.pool, id).await },
            )
            .await?;
        return match rejected {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(crate::governance::exclusion::DecisionError::NotFound) => {
                Err(StatusCode::NOT_FOUND)
            }
            Err(crate::governance::exclusion::DecisionError::NotPending) => {
                Err(StatusCode::CONFLICT)
            }
            Err(crate::governance::exclusion::DecisionError::Malformed) => {
                Err(StatusCode::UNPROCESSABLE_ENTITY)
            }
            Err(crate::governance::exclusion::DecisionError::Db(error)) => {
                tracing::warn!(proposal_id = id, %error, "rejecting an exclusion failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    if kind == "agent-recruit" {
        // Refusing leaves nothing behind, because nothing was created: no agent, no roster row, no
        // run held. And it is deliberately not a permanent no — the NEXT run of that department
        // meets the same gap and may ask again, which is right. Nobody said the director should
        // stop asking; they said not this one.
        let state = state.clone();
        let rejected = uncancellable(async move {
            crate::proposals::transition(&state.pool, id, "rejected", "not hired").await
        })
        .await?;
        return match rejected {
            Ok(true) => Ok(StatusCode::NO_CONTENT),
            Ok(false) => Err(StatusCode::CONFLICT),
            Err(error) => {
                tracing::warn!(proposal_id = id, %error, "refusing a recruitment failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    if kind == "github-action" {
        // Refusing leaves nothing behind, because nothing was done: `github::submit` files the
        // proposal and returns, and the operation reaches `gh` only on the approval path. So this is
        // a status flip with no second half — the opposite of the approve side, which claims the row
        // BEFORE spawning precisely because there IS one there.
        //
        // Its absence was invisible until somebody refused one by hand. The fallback below knows
        // only `action-approval`, so a `github-action` fell through to it and came back 409 saying
        // NotPending about a proposal sitting there pending; `/dismiss`, which knows only
        // `skipped-item`, answered the same. An operation the owner could approve and could not
        // refuse waited for a decision that had no way to arrive.
        let state = state.clone();
        let rejected = uncancellable(async move {
            crate::proposals::transition(&state.pool, id, "rejected", "rejected by user").await
        })
        .await?;
        return match rejected {
            Ok(true) => Ok(StatusCode::NO_CONTENT),
            Ok(false) => Err(StatusCode::CONFLICT),
            Err(error) => {
                tracing::warn!(proposal_id = id, %error, "refusing a github action failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    if kind == "team-action" {
        // Refusing closes the action as well as the proposal, in that order: the proposal is the
        // decision and the action is what is left to do about it, and leaving the second `pending`
        // would keep it in the department's queue ceiling forever, blocking the next request over a
        // question already answered.
        let state = state.clone();
        let rejected = uncancellable(async move {
            let decided =
                crate::proposals::transition(&state.pool, id, "rejected", "rejected by user")
                    .await?;
            if decided {
                crate::team::refuse_action(&state.pool, id).await?;
            }
            Ok::<bool, sqlx::Error>(decided)
        })
        .await?;
        return match rejected {
            Ok(true) => Ok(StatusCode::NO_CONTENT),
            Ok(false) => Err(StatusCode::CONFLICT),
            Err(error) => {
                tracing::warn!(proposal_id = id, %error, "rejecting a team action failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    // Uncancellable: rejecting flips the proposal first and discards the paused run second, and the
    // first half cannot be replayed — a retry finds the proposal no longer `pending` and answers 409.
    match uncancellable(async move { crate::proposals::reject_proposal(&state.pool, id).await })
        .await?
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(crate::proposals::RejectError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(crate::proposals::RejectError::NotPending) => Err(StatusCode::CONFLICT),
        Err(crate::proposals::RejectError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "rejecting a proposal failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
