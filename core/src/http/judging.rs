//! Shadow decisions, judge verdicts and the scoreboard (`/shadow-decisions/*`, `/judge-*`, `/scoreboard`, `/verify/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        // POST for both, status included: `Scope::Run` passes only literal POST paths, and a run's
        // key is the main caller of these two.
        .route("/verify", post(crate::verify::post_verify))
        .route("/verify/status", post(crate::verify::post_verify_status))
        // Control/admin only, on purpose: absent from the `auth.rs` scope tables, so a run key
        // cannot keep a worktree's verify box alive.
        .route(
            "/verify/box/beat",
            post(crate::verify::ide_box::post_box_beat),
        )
        .route("/shadow-decisions", get(get_unreviewed_shadow_decisions))
        .route("/shadow-decisions/{id}/verdict", post(post_shadow_verdict))
        .route(
            "/judge-verdicts/unreviewed",
            get(get_unreviewed_judge_verdicts),
        )
        .route("/judge-verdicts/{id}/verdict", post(post_judge_verdict))
        .route(
            "/judge-resolutions/unreviewed",
            get(get_unreviewed_judge_resolutions),
        )
        .route(
            "/judge-resolutions/{id}/outcome",
            post(post_judge_resolution_outcome),
        )
        .route(
            "/judge-verdicts/by-decision",
            get(get_judge_opinions_by_decision),
        )
        .route("/scoreboard", get(get_scoreboard))
}

#[derive(Deserialize)]
pub(super) struct VerdictRequest {
    pub(super) verdict: String,
}

pub(super) async fn get_unreviewed_shadow_decisions(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<Vec<ShadowDecision>>, StatusCode> {
    shadow::list_unreviewed(&state.pool, &query.project_id)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) async fn post_shadow_verdict(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<VerdictRequest>,
) -> Result<StatusCode, StatusCode> {
    if !matches!(body.verdict.as_str(), "approve" | "reject") {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Readiness only ever moves when a human reviews a decision, so this is the one place a project
    // can cross the promotion bar — sample it either side of the verdict to catch the crossing.
    let project = shadow::project_of_decision(&state.pool, id)
        .await
        .unwrap_or(None);
    let was_promotable = match &project {
        Some(project_id) => shadow::project_readiness(&state.pool, project_id)
            .await
            .map(|(ready, total, withheld)| shadow::promotable(ready, total, withheld))
            .unwrap_or(false),
        None => false,
    };

    // Uncancellable: the verdict lands first and the crossing is announced second, and a verdict is
    // recorded once — replaying it answers 404, so an announcement dropped in between is lost for
    // good, and the promotion bar is the one thing this endpoint exists to surface.
    let pool = state.pool.clone();
    let verdict = body.verdict.clone();
    uncancellable(async move {
        match shadow::set_verdict(&pool, id, &verdict).await {
            Ok(true) => {
                if let Some(project_id) = project {
                    announce_promotable(&pool, &project_id, was_promotable).await;
                }
                Ok(StatusCode::NO_CONTENT)
            }
            Ok(false) => Err(StatusCode::NOT_FOUND),
            Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    })
    .await?
}

pub(super) async fn get_unreviewed_judge_verdicts(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<Vec<crate::judge::JudgeVerdictView>>, StatusCode> {
    crate::judge::list_unreviewed(&state.pool, &query.project_id)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Deserialize)]
pub(super) struct DecisionIdsQuery {
    /// Comma-separated `shadow_decisions.id`s - the page's own rows.
    pub(super) ids: String,
}

/// Admin-only like the rest of the judge's routes (in no table in `auth.rs`).
pub(super) async fn get_judge_opinions_by_decision(
    State(state): State<AppState>,
    Query(query): Query<DecisionIdsQuery>,
) -> Result<Json<Vec<crate::judge::JudgeOpinion>>, StatusCode> {
    let ids = query
        .ids
        .split(',')
        .filter(|part| !part.is_empty())
        .map(|part| part.trim().parse::<i64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    crate::judge::opinions_for_decisions(&state.pool, &ids)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Accepts `approve|reject` once, like `post_shadow_verdict`: 400 for any other word, 404 for an
/// unknown id, a verdict already given, or a verdict that was not in a deciding band.
pub(super) async fn post_judge_verdict(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<VerdictRequest>,
) -> Result<StatusCode, StatusCode> {
    if !matches!(body.verdict.as_str(), "approve" | "reject") {
        return Err(StatusCode::BAD_REQUEST);
    }
    // The verdict and the demotion it may cause are ONE write: a verdict is the only thing that
    // moves readiness, so it is where the bar is held, and a failure anywhere records nothing.
    let recorded = async {
        let mut tx = state.pool.begin().await?;
        if !crate::judge::set_verdict(&mut tx, id, &body.verdict).await? {
            return Ok(false);
        }
        if let Some(project_id) = crate::judge::project_of_verdict(&mut tx, id).await? {
            autopilot::hold_judge_to_the_bar(&mut tx, &project_id).await?;
        }
        tx.commit().await?;
        Ok::<_, sqlx::Error>(true)
    }
    .await;
    match recorded {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, id, "could not record the judge verdict");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(Deserialize)]
pub(super) struct OutcomeRequest {
    pub(super) outcome: String,
}

/// Spec B D11: the resolver's review queue, one entry per unit.
pub(super) async fn get_unreviewed_judge_resolutions(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<Vec<crate::judge::resolve_review::ResolutionView>>, StatusCode> {
    crate::judge::resolve_review::list_unreviewed(&state.pool, &query.project_id)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// 204 once; 404 for an unknown id, an outcome the event cannot have, or one already given.
pub(super) async fn post_judge_resolution_outcome(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<OutcomeRequest>,
) -> Result<StatusCode, StatusCode> {
    // The review and the demotion it may cause are ONE write: a review is what moves readiness, so
    // it is where the bar is held, as `post_judge_verdict` does for spec A.
    let recorded = async {
        let mut tx = state.pool.begin().await?;
        if !crate::judge::resolve_review::set_outcome(&mut tx, id, &body.outcome).await? {
            return Ok(false);
        }
        if let Some(project_id) =
            crate::judge::resolve_review::project_of_resolution(&mut tx, id).await?
        {
            autopilot::hold_judge_resolve_to_the_bar(&mut tx, &project_id).await?;
        }
        tx.commit().await?;
        Ok::<_, sqlx::Error>(true)
    }
    .await;
    match recorded {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, id, "could not record the resolver's review");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// The §8.2 promotion nudge: a feed entry the moment a project's last outstanding action class
/// clears the bar. Without it the gate solves promotion-by-impatience but leaves the opposite
/// failure — a project that quietly became promotable and nobody noticed.
///
/// Best-effort and idempotent by construction: it fires only on the false→true crossing, so a
/// project already promotable before the verdict stays silent. Feed failures never fail the verdict.
pub(super) async fn announce_promotable(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    was_promotable: bool,
) {
    if was_promotable {
        return;
    }
    let Ok((ready, total, withheld)) = shadow::project_readiness(pool, project_id).await else {
        return;
    };
    if !shadow::promotable(ready, total, withheld) {
        return;
    }

    let summary = format!(
        "{project_id} is ready for promotion — all {total} reviewed action classes clear the bar \
         ({}+ reviews, {}%+ agreement). Promotion is still yours to make.",
        shadow::READINESS_MIN_REVIEWED,
        shadow::READINESS_MIN_AGREE_PERCENT,
    );
    let _ = feed::append(
        pool,
        Some(project_id),
        "promotion_ready",
        &summary,
        None,
        None,
    )
    .await;
}

pub(super) async fn get_scoreboard(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<Vec<ClassTally>>, StatusCode> {
    shadow::scoreboard(&state.pool, &query.project_id)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
