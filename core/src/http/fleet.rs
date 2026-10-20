//! Autopilot state, kill switches, quota and fleet capacity (`/autopilot/*`, `/quota`, `/fleet/*`, `/concurrency`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/autopilot/state",
            get(get_autopilot_state).post(post_autopilot_state),
        )
        .route(
            "/autopilot/judge",
            get(get_autopilot_judge).post(post_autopilot_judge),
        )
        .route(
            "/autopilot/judge-resolve",
            get(get_autopilot_judge_resolve).post(post_autopilot_judge_resolve),
        )
        .route(
            "/autopilot/kill",
            get(get_autopilot_kill).post(post_autopilot_kill),
        )
        .route(
            "/autopilot/kill/scoped",
            get(get_autopilot_kill_scoped).post(post_autopilot_kill_scoped),
        )
        .route(
            "/autopilot/budget",
            get(get_autopilot_budget).post(post_autopilot_budget),
        )
        .route(
            "/autopilot/quota-brake",
            get(get_quota_brake).post(post_quota_brake),
        )
        .route("/autopilot/attention", post(post_attention_heartbeat))
        // How much of each assistant's usage limit is gone. The notch draws this read; the
        // separate quota-brake route owns settings for acting on it.
        //
        // Registered here BEFORE anything in the shell calls it, which is the order
        // `map::seam::no_screen_in_this_repository_asks_for_a_route_the_daemon_does_not_serve`
        // enforces: a screen asking for a route the daemon does not serve fails that assertion,
        // while a route served with no caller is explicitly allowed.
        .route("/quota", get(get_quota))
        // The fleet canvas's authority: how much fits, and who is inside it. Beside `/projects`
        // because it answers about the same set — the roster — seen through capacity rather than
        // through mode.
        .route("/concurrency", get(get_concurrency))
        // The heavy-command budget the runs and verify share: who holds it, who waits. Beside
        // `/concurrency` because it is the same picture seen per command. Control/Admin by being in
        // no table in `auth.rs`.
        .route("/admission", get(get_admission))
        // A controller's wave asks here for the slots its workers will hold, keeps them by renewing,
        // and gives them back (perfil-de-velocidade spec §4.6). Beside `/concurrency` because they
        // spend what that route reads. Admin by default, by being in no table in `auth.rs`: taking
        // capacity is not something a read-only key buys.
        .route("/concurrency/waves", post(post_wave))
        .route("/concurrency/waves/{id}", delete(delete_wave))
        .route("/concurrency/waves/{id}/renew", post(post_wave_renew))
        // Beside `/concurrency` because it is about the same picture: that route says how much fits,
        // this one asks that two of the things inside it not be there at once. Admin by default, by
        // being in no table in `auth.rs` — it files a request that changes how the fleet schedules,
        // which is not something a read-only key buys.
        .route(
            "/fleet/exclusions",
            get(get_fleet_exclusions).post(post_fleet_exclusion),
        )
        // The literal ahead of `{id}`; matchit prefers it, and they are different methods besides.
        .route(
            "/fleet/exclusions/requests",
            get(get_fleet_exclusion_requests),
        )
        .route("/fleet/exclusions/{id}", delete(delete_fleet_exclusion))
}

#[derive(Deserialize)]
pub(super) struct AutopilotStateRequest {
    pub(super) project_id: String,
    pub(super) mode: String,
    pub(super) project_root: Option<String>,
}

#[derive(serde::Serialize)]
pub(super) struct AutopilotStateResponse {
    pub(super) project_id: String,
    pub(super) mode: Mode,
}

#[derive(Deserialize)]
pub(super) struct AutopilotKillRequest {
    pub(super) engaged: bool,
}

#[derive(Deserialize)]
pub(super) struct ScopedKillRequest {
    pub(super) scope_type: String,
    pub(super) scope_id: String,
    pub(super) engaged: bool,
}

#[derive(serde::Serialize)]
pub(super) struct AutopilotKillResponse {
    pub(super) engaged: bool,
}

#[derive(Deserialize)]
pub(super) struct BudgetRequest {
    pub(super) limit_usd: Option<f64>,
    pub(super) period: String,
    pub(super) hourly_limit_usd: Option<f64>,
    pub(super) per_run_reserve_usd: f64,
    pub(super) time_cost_per_hour_usd: f64,
    /// Absent leaves the relay chain ceiling as it is, so a client written before it existed
    /// keeps saving the house budget without resetting a field it never saw.
    #[serde(default)]
    pub(super) relay_chain_limit_usd: Option<f64>,
}

#[derive(serde::Serialize)]
pub(super) struct QuotaBrakeResponse {
    pub(super) enabled: bool,
    pub(super) pause_above_percent_5h: i64,
    pub(super) pause_above_percent_7d: i64,
    pub(super) provider: String,
}

#[derive(Deserialize)]
pub(super) struct QuotaBrakeRequest {
    pub(super) enabled: bool,
    pub(super) pause_above_percent_5h: i64,
    pub(super) pause_above_percent_7d: i64,
}

#[derive(Deserialize)]
pub(super) struct AttentionHeartbeatRequest {
    pub(super) project_id: Option<String>,
}

pub(super) async fn get_autopilot_state(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<AutopilotStateResponse>, StatusCode> {
    let mode = autopilot::project_mode(&state.pool, &query.project_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(AutopilotStateResponse {
        project_id: query.project_id,
        mode,
    }))
}

pub(super) async fn post_autopilot_state(
    State(state): State<AppState>,
    Json(body): Json<AutopilotStateRequest>,
) -> Result<Json<AutopilotStateResponse>, axum::response::Response> {
    let mode = Mode::from_db_str(&body.mode).ok_or(StatusCode::BAD_REQUEST.into_response())?;
    let project_root = body.project_root.as_deref().map(std::path::Path::new);
    autopilot::set_project_mode(
        &state.pool,
        state.machine_config_root.as_deref(),
        &body.project_id,
        mode,
        project_root,
    )
    .await
    .map_err(activation_status)?;
    let mode = autopilot::project_mode(&state.pool, &body.project_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;

    Ok(Json(AutopilotStateResponse {
        project_id: body.project_id,
        mode,
    }))
}

#[derive(Deserialize)]
pub(super) struct AutopilotJudgeRequest {
    pub(super) project_id: String,
    pub(super) judge: String,
}

#[derive(Serialize)]
pub(super) struct AutopilotJudgeResponse {
    pub(super) project_id: String,
    pub(super) judge: crate::judge::JudgeMode,
    /// Review item G: `Some` when this project's `autopilot.yaml` cannot be read, which leaves
    /// the judge without effect (every call falls back to the classifier, D7/D10).
    pub(super) rules_error: Option<String>,
    /// D11: how far the project is from being allowed to enforce, with the classes beside it.
    pub(super) readiness: crate::judge::JudgeReadiness,
}

/// One project's judge, as both handlers answer it. A readiness that cannot be read is a 500, like
/// the mode: a panel shown a made-up zero would read "nothing reviewed yet".
pub(super) async fn judge_status(
    state: &AppState,
    project_id: String,
) -> Result<AutopilotJudgeResponse, StatusCode> {
    let judge = autopilot::autopilot_judge_mode(&state.pool, &project_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let readiness = crate::judge::readiness(&state.pool, &project_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let rules_error =
        crate::judge::rules_problem(state.machine_config_root.clone(), &project_id).await;
    Ok(AutopilotJudgeResponse {
        project_id,
        judge,
        rules_error,
        readiness,
    })
}

/// Spec A: one project's judge setting. Its own route beside `/autopilot/state` rather than a
/// field on the roster: the panel reads one project at a time, and a field on `ProjectSummary`
/// would touch nine literals and the CLI's deserialiser for nothing the roster shows.
///
/// Admin-only, by appearing in no table in `auth.rs` (not in `READ_ONLY_ROUTES`): it reveals and
/// changes what a model may decide for a project, and default-deny is the safe side of that.
pub(super) async fn get_autopilot_judge(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<AutopilotJudgeResponse>, StatusCode> {
    judge_status(&state, query.project_id).await.map(Json)
}

pub(super) async fn post_autopilot_judge(
    State(state): State<AppState>,
    Json(body): Json<AutopilotJudgeRequest>,
) -> Result<Json<AutopilotJudgeResponse>, (StatusCode, Json<serde_json::Value>)> {
    let judge = crate::judge::JudgeMode::from_db_str(&body.judge)
        .ok_or_else(|| refusal(StatusCode::BAD_REQUEST, "invalid"))?;
    match autopilot::set_project_judge(&state.pool, &body.project_id, judge).await {
        Ok(_) => judge_status(&state, body.project_id)
            .await
            .map(Json)
            .map_err(|status| refusal(status, "internal")),
        Err(autopilot::JudgeActivationError::UnknownProject) => {
            Err(refusal(StatusCode::NOT_FOUND, "unknown_project"))
        }
        Err(autopilot::JudgeActivationError::NotActive) => {
            Err(refusal(StatusCode::CONFLICT, "not_active"))
        }
        Err(autopilot::JudgeActivationError::NotReady { reviewed, agree }) => {
            tracing::info!(
                reviewed,
                agree,
                "enforce refused: the judge's bar is not met"
            );
            Err(refusal(StatusCode::CONFLICT, "not_ready"))
        }
        Err(autopilot::JudgeActivationError::Database(error)) => {
            tracing::warn!(%error, "setting a project's judge failed");
            Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}

#[derive(Deserialize)]
pub(super) struct JudgeResolveRequest {
    pub(super) project_id: String,
    pub(super) judge_resolve: String,
}

#[derive(Serialize)]
pub(super) struct JudgeResolveResponse {
    pub(super) project_id: String,
    pub(super) judge_resolve: crate::judge::JudgeMode,
    pub(super) readiness: crate::judge::resolve_review::ResolveReadiness,
}

/// Spec B D11: one project's resolver setting. Admin-only like `/autopilot/judge`, by appearing
/// in no table in `auth.rs`.
pub(super) async fn get_autopilot_judge_resolve(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<JudgeResolveResponse>, StatusCode> {
    let judge_resolve = autopilot::autopilot_judge_resolve_mode(&state.pool, &query.project_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let readiness = crate::judge::resolve_review::readiness(&state.pool, &query.project_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(JudgeResolveResponse {
        project_id: query.project_id,
        judge_resolve,
        readiness,
    }))
}

pub(super) async fn post_autopilot_judge_resolve(
    State(state): State<AppState>,
    Json(body): Json<JudgeResolveRequest>,
) -> Result<Json<JudgeResolveResponse>, (StatusCode, Json<serde_json::Value>)> {
    let mode = crate::judge::JudgeMode::from_db_str(&body.judge_resolve)
        .ok_or_else(|| refusal(StatusCode::BAD_REQUEST, "invalid"))?;
    // `enforce` is accepted now that the shell shows its residual risk (spec D11). The bar is
    // enforced by `set_project_judge_resolve`, whatever door asks.
    match autopilot::set_project_judge_resolve(&state.pool, &body.project_id, mode).await {
        Ok(judge_resolve) => {
            let readiness = crate::judge::resolve_review::readiness(&state.pool, &body.project_id)
                .await
                .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?;
            Ok(Json(JudgeResolveResponse {
                project_id: body.project_id,
                judge_resolve,
                readiness,
            }))
        }
        Err(autopilot::ResolveActivationError::UnknownProject) => {
            Err(refusal(StatusCode::NOT_FOUND, "unknown_project"))
        }
        Err(autopilot::ResolveActivationError::NotActive) => {
            Err(refusal(StatusCode::CONFLICT, "not_active"))
        }
        Err(autopilot::ResolveActivationError::NotReady(readiness)) => {
            tracing::info!(
                reviewed = readiness.reviewed,
                agree = readiness.agree,
                less_cautious = readiness.less_cautious,
                "enforce refused: the resolver's bar is not met"
            );
            Err(refusal(StatusCode::CONFLICT, "not_ready"))
        }
        Err(autopilot::ResolveActivationError::Database(error)) => {
            tracing::error!(%error, project_id = %body.project_id, "setting judge_resolve failed");
            Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}

pub(super) async fn get_autopilot_kill(
    State(state): State<AppState>,
) -> Result<Json<AutopilotKillResponse>, StatusCode> {
    let engaged = autopilot::kill_switch_engaged(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(AutopilotKillResponse { engaged }))
}

pub(super) async fn post_autopilot_kill(
    State(state): State<AppState>,
    Json(body): Json<AutopilotKillRequest>,
) -> Result<StatusCode, StatusCode> {
    autopilot::set_kill_switch(&state.pool, body.engaged)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) async fn get_autopilot_kill_scoped(
    State(state): State<AppState>,
) -> Result<Json<Vec<ScopedKill>>, StatusCode> {
    autopilot::list_scoped_kills(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) async fn post_autopilot_kill_scoped(
    State(state): State<AppState>,
    Json(body): Json<ScopedKillRequest>,
) -> Result<StatusCode, StatusCode> {
    if body.scope_type != "project" && body.scope_type != "trigger" {
        return Err(StatusCode::BAD_REQUEST);
    }
    autopilot::set_scoped_kill(&state.pool, &body.scope_type, &body.scope_id, body.engaged)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// The quota readings, live from the sidecar or — when it cannot be reached — from the table.
///
/// Never an error: a sidecar that is down is an answer about the sidecar, and `quota::report`
/// carries it in the body as `source: "stored"` plus the reason. Returning a 5xx would have the
/// shell draw nothing, which looks exactly like a quota that has not been touched.
pub(super) async fn get_quota(State(state): State<AppState>) -> Json<crate::quota::QuotaReport> {
    Json(crate::quota::report(&state.quota, &state.pool, chrono::Utc::now()).await)
}

pub(super) async fn get_autopilot_budget(
    State(state): State<AppState>,
) -> Result<Json<budget::BudgetSnapshot>, StatusCode> {
    budget_response(&state).await.map(Json)
}

pub(super) async fn post_autopilot_budget(
    State(state): State<AppState>,
    Json(body): Json<BudgetRequest>,
) -> Result<Json<budget::BudgetSnapshot>, StatusCode> {
    let period = budget::BudgetPeriod::from_db_str(&body.period).ok_or(StatusCode::BAD_REQUEST)?;
    // Refused before anything is written, so a bad ceiling never lands half a request. Zero is a
    // valid answer — it refuses every relay — and a negative or non-finite one is not a ceiling.
    if body
        .relay_chain_limit_usd
        .is_some_and(|limit| !limit.is_finite() || limit < 0.0)
    {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let config = budget::BudgetConfig {
        limit_usd: body.limit_usd,
        period,
        hourly_limit_usd: body.hourly_limit_usd,
        per_run_reserve_usd: body.per_run_reserve_usd,
        time_cost_per_hour_usd: body.time_cost_per_hour_usd,
    };
    budget::set_budget_config(&state.pool, &config)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Some(limit) = body.relay_chain_limit_usd {
        budget::set_relay_chain_limit(&state.pool, limit)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    budget_response(&state).await.map(Json)
}

pub(super) async fn get_quota_brake(
    State(state): State<AppState>,
) -> Result<Json<QuotaBrakeResponse>, StatusCode> {
    quota_brake_response(&state).await.map(Json)
}

pub(super) async fn post_quota_brake(
    State(state): State<AppState>,
    Json(body): Json<QuotaBrakeRequest>,
) -> Result<Json<QuotaBrakeResponse>, StatusCode> {
    if !(1..=100).contains(&body.pause_above_percent_5h)
        || !(1..=100).contains(&body.pause_above_percent_7d)
    {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    crate::quota::set_brake_policy(
        &state.pool,
        crate::quota::BrakePolicy {
            enabled: body.enabled,
            pause_above_percent_5h: body.pause_above_percent_5h,
            pause_above_percent_7d: body.pause_above_percent_7d,
        },
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    quota_brake_response(&state).await.map(Json)
}

pub(super) async fn quota_brake_response(
    state: &AppState,
) -> Result<QuotaBrakeResponse, StatusCode> {
    let policy = crate::quota::load_brake_policy(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(QuotaBrakeResponse {
        enabled: policy.enabled,
        pause_above_percent_5h: policy.pause_above_percent_5h,
        pause_above_percent_7d: policy.pause_above_percent_7d,
        provider: state.quota.provider.clone(),
    })
}

pub(super) async fn post_attention_heartbeat(
    State(state): State<AppState>,
    Json(body): Json<AttentionHeartbeatRequest>,
) -> Result<StatusCode, StatusCode> {
    let scope = match body.project_id {
        None => AttentionScope::Global,
        Some(project_id) if project_id.trim().is_empty() => return Err(StatusCode::BAD_REQUEST),
        Some(project_id) => AttentionScope::Project(project_id),
    };
    attention::record_heartbeat(&state.pool, &scope, chrono::Utc::now())
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) async fn budget_response(
    state: &AppState,
) -> Result<budget::BudgetSnapshot, StatusCode> {
    budget::snapshot(&state.pool, chrono::Utc::now())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// The mode door's refusals on the wire.
///
/// Three of the four prerequisites are still the bare 422 they always were, and the shell's copy
/// for that answer names all of them. `NotOnboarded` alone carries a name, `not_onboarded`, because
/// it is the one refusal the shell can resolve in place — by running onboarding
/// (`POST /projects/{id}/onboard`) — and a status shared with three other causes could not tell it
/// to. The name is opaque on purpose: it says what is missing, never where a file would have been.
pub(super) fn activation_status(error: ActivationError) -> axum::response::Response {
    match error {
        ActivationError::NotOnboarded => {
            refusal(StatusCode::UNPROCESSABLE_ENTITY, "not_onboarded").into_response()
        }
        ActivationError::NotAGitRepo
        | ActivationError::ProjectRootRequired
        | ActivationError::HookNotRegistered => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
        ActivationError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct ExclusionRequest {
    pub(super) job_a: i64,
    pub(super) job_b: i64,
    /// The files that motivated the request, as `collision.rs` reported them. Optional, and kept
    /// rather than acted on — see the migration.
    #[serde(default)]
    pub(super) paths: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct ExclusionQuery {
    /// Absent means every project, which is what the canvas asks for.
    pub(super) project_id: Option<String>,
}

/// The rules in force, for the canvas to draw.
///
/// Only the live ones. A revoked rule is kept in the table so the decision stays readable, but a
/// screen that drew it would be showing a constraint that is not constraining anything.
pub(super) async fn get_fleet_exclusions(
    State(state): State<AppState>,
    Query(query): Query<ExclusionQuery>,
) -> Result<Json<Vec<crate::governance::exclusion::Exclusion>>, StatusCode> {
    crate::governance::exclusion::live(&state.pool, query.project_id.as_deref())
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "reading the fleet exclusions failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// The requests waiting on an answer.
///
/// `/proposals` cannot carry these: it serves `action-approval` alone, deliberately, because
/// approving one of those resumes a paused run and approving one of these resumes nothing — the
/// argument `list_pending` and `get_contact_merges` both make at length. So this follows the door
/// `contact-merge` opened, which also puts the question where the context is: whether two jobs
/// should be serialised is decided while looking at the fleet.
pub(super) async fn get_fleet_exclusion_requests(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::proposals::Proposal>>, StatusCode> {
    crate::governance::exclusion::pending_requests(&state.pool)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "reading the pending exclusion requests failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Lifts a rule.
///
/// 409 and not 404 when it is already revoked: the row is there, and the difference between "no such
/// rule" and "somebody lifted this before you" is the difference between a stale screen and a wrong
/// id.
pub(super) async fn delete_fleet_exclusion(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    match crate::governance::exclusion::revoke(&state.pool, id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::CONFLICT),
        Err(error) => {
            tracing::warn!(exclusion_id = id, %error, "revoking an exclusion failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Asks that two jobs of one project not run at the same time.
///
/// It answers 201 with a PROPOSAL id, not with a rule id, and the difference is the design. Drawing
/// this edge changes nothing about how the fleet schedules until somebody approves it in the same
/// queue every other decision passes through. An edge that took effect on being drawn would be a way
/// to change scheduling without passing through approval, which is exactly the property this
/// pillar's canvas was meant not to copy from october.dev.
///
/// Every refusal carries a sentence, following `post_proposal_approve`: three of the five mean
/// different things a person can act on — a pair already asked about, a pair already excluded, and
/// two jobs that share no project — and a bare 409 tells them apart from nothing.
pub(super) async fn post_fleet_exclusion(
    State(state): State<AppState>,
    Json(body): Json<ExclusionRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    use crate::governance::exclusion::ProposeError;
    match crate::governance::exclusion::propose(&state.pool, body.job_a, body.job_b, &body.paths)
        .await
    {
        Ok(proposal_id) => Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({ "proposal_id": proposal_id })),
        )),
        Err(ProposeError::SameJob) => Err((
            StatusCode::BAD_REQUEST,
            "a job cannot be excluded from itself".to_owned(),
        )),
        Err(ProposeError::UnknownJob(id)) => {
            Err((StatusCode::NOT_FOUND, format!("there is no job {id}")))
        }
        Err(ProposeError::DifferentProjects) => Err((
            StatusCode::BAD_REQUEST,
            "these jobs belong to different projects, so they share no slots to serialise"
                .to_owned(),
        )),
        Err(ProposeError::AlreadyAsked) => Err((
            StatusCode::CONFLICT,
            "these two jobs already have a request waiting for a decision".to_owned(),
        )),
        Err(ProposeError::AlreadyExcluded) => Err((
            StatusCode::CONFLICT,
            "these two jobs are already excluded from running at the same time".to_owned(),
        )),
        Err(ProposeError::Db(error)) => {
            tracing::warn!(%error, "asking for a fleet exclusion failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "the request could not be recorded".to_owned(),
            ))
        }
    }
}

pub(super) async fn get_concurrency(
    State(state): State<AppState>,
) -> Result<Json<crate::execution::concurrency::Readout>, StatusCode> {
    crate::execution::concurrency::readout(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// The readout of the heavy-command admission book: `None` (none installed) answers a disabled
/// readout with no capacity, so a caller never has to tell "off" from "absent".
pub(super) fn admission_readout(
    book: Option<&crate::execution::admission::AdmissionBook>,
) -> crate::execution::admission::Snapshot {
    match book {
        Some(book) => book.snapshot(),
        None => crate::execution::admission::Snapshot {
            enabled: false,
            capacity: 0,
            used: 0,
            verify_demand: false,
            holders: Vec::new(),
            waiters: Vec::new(),
        },
    }
}

pub(super) async fn get_admission() -> Json<crate::execution::admission::Snapshot> {
    let book = crate::execution::admission::installed();
    Json(admission_readout(book.as_deref()))
}

#[derive(Deserialize)]
pub(super) struct WaveRequest {
    /// Where the controller stands. The project is resolved from it the way the session hook
    /// resolves one, so a controller never has to know an id the daemon made up.
    pub(super) cwd: String,
    pub(super) workers: i64,
}

pub(super) async fn post_wave(
    State(state): State<AppState>,
    Json(body): Json<WaveRequest>,
) -> Result<Json<crate::execution::wave::Grant>, (StatusCode, String)> {
    if !(1..=crate::execution::wave::MAX_WORKERS).contains(&body.workers) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "a wave asks for 1 to {} workers, not {}",
                crate::execution::wave::MAX_WORKERS,
                body.workers
            ),
        ));
    }
    let deadline = std::time::Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;
    let root = crate::execution::git_exec::toplevel(std::path::Path::new(&body.cwd), deadline)
        .await
        .map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("{} is not inside a git checkout: {error}", body.cwd),
            )
        })?;
    // 404 and not 400: the request is well formed, and the answer is that this daemon cannot vouch
    // for that repository's capacity — not on its roster, or the roster could not be read. Both fail
    // the safe way: the controller reads either as `no_capacity_authority` and runs one worker.
    let project_id = vcs::project_for_worktree(&state.pool, &root, deadline)
        .await
        .map_err(|error| (StatusCode::NOT_FOUND, error))?;
    crate::execution::wave::grant(&state.pool, &project_id, body.workers, chrono::Utc::now())
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%project_id, %error, "granting a wave its slots failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not grant the wave its slots".to_owned(),
            )
        })
}

/// 204 renewed; 410 lapsed or released, and its slots are given back; 404 no such wave.
pub(super) async fn post_wave_renew(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    match crate::execution::wave::renew(&state.pool, id, chrono::Utc::now()).await {
        Ok(crate::execution::wave::Renewal::Renewed) => Ok(StatusCode::NO_CONTENT),
        Ok(crate::execution::wave::Renewal::Lapsed) => Err(StatusCode::GONE),
        Ok(crate::execution::wave::Renewal::Unknown) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(wave_id = id, %error, "renewing a wave's lease failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// 204 released, including a second time; 404 no such wave.
pub(super) async fn delete_wave(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    match crate::execution::wave::release(&state.pool, id, chrono::Utc::now()).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(wave_id = id, %error, "releasing a wave's slots failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::admission_readout;
    use crate::execution::admission::{AdmissionBook, Poll};
    use std::time::Duration;

    #[tokio::test]
    async fn admission_readout_lists_holders_and_waiters() {
        // A private book: the process-wide one is never installed by a test.
        let book = AdmissionBook::new(1, Duration::from_secs(900), Duration::from_secs(7200));
        assert!(matches!(
            book.request(1, "holder", 1),
            Poll::Granted { weight: 1 }
        ));
        assert!(matches!(book.request(2, "waiter", 1), Poll::Waiting { .. }));

        let readout = admission_readout(Some(&book));
        assert!(readout.enabled);
        assert_eq!(readout.capacity, 1);
        assert_eq!(readout.used, 1);
        assert_eq!(readout.holders.len(), 1);
        assert_eq!(readout.holders[0].run_id, Some(1));
        assert_eq!(readout.waiters.len(), 1);
        assert_eq!(readout.waiters[0].run_id, 2);

        // No book installed: a disabled, empty readout with no capacity.
        let off = admission_readout(None);
        assert!(!off.enabled);
        assert_eq!(off.capacity, 0);
        assert!(off.holders.is_empty() && off.waiters.is_empty());
    }
}
