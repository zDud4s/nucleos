//! The pipeline routes (`/analytics/pipeline`, `/projects/{id}/analytics/pipeline`, spec
//! analytics-pipeline §5.2): `/analytics`' `days` rules and errors, one JSON per request whose
//! blocks carry their own errors (D5), so the only refusals here are the 400 and the 404.

use super::analytics::{DaysQuery, Refusal, known_project, window_of};
use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/analytics/pipeline", get(get_pipeline))
        .route(
            "/projects/{id}/analytics/pipeline",
            get(get_project_pipeline),
        )
}

async fn get_pipeline(
    State(state): State<AppState>,
    Query(query): Query<DaysQuery>,
) -> Result<Json<crate::analytics_pipeline::PipelineReport>, Refusal> {
    let now = chrono::Utc::now();
    let window = window_of(&query, now)?;
    Ok(Json(
        crate::analytics_pipeline::build(&state.pool, crate::analytics::Scope::Global, window, now)
            .await,
    ))
}

/// The window is checked before the project, as in `/projects/{id}/analytics`: a bad `days` is
/// a 400 even for a project that does not exist.
async fn get_project_pipeline(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DaysQuery>,
) -> Result<Json<crate::analytics_pipeline::PipelineReport>, Refusal> {
    let now = chrono::Utc::now();
    let window = window_of(&query, now)?;
    known_project(&state, &id, "project analytics").await?;
    Ok(Json(
        crate::analytics_pipeline::build(
            &state.pool,
            crate::analytics::Scope::Project(&id),
            window,
            now,
        )
        .await,
    ))
}
