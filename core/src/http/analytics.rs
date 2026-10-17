//! The analytics routes (`/analytics`, `/analytics/quota`, `/projects/{id}/analytics`): parse
//! `days`, refuse anything but 7/30/90, and hand the window to `crate::analytics`.

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/analytics", get(get_analytics))
        .route("/analytics/quota", get(get_analytics_quota))
        .route("/projects/{id}/analytics", get(get_project_analytics))
}

/// `days` read as text, so `days=abc` gets this module's 400 and not the extractor's.
#[derive(Debug, Deserialize)]
pub(super) struct DaysQuery {
    days: Option<String>,
}

type Refusal = (StatusCode, &'static str);

const BAD_DAYS: Refusal = (StatusCode::BAD_REQUEST, "days must be 7, 30 or 90");

/// No clamp, unlike `/readings`: an unexpected window here is a client bug, and clamping it would
/// hide it (spec §4.1).
fn window_of(
    query: &DaysQuery,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<crate::analytics::Window, Refusal> {
    let days = match query.days.as_deref() {
        None => crate::analytics::DEFAULT_DAYS,
        Some(raw) => raw.parse::<i64>().map_err(|_| BAD_DAYS)?,
    };
    crate::analytics::Window::new(days, now).ok_or(BAD_DAYS)
}

fn failed(error: sqlx::Error, what: &'static str) -> Refusal {
    tracing::warn!(%error, "{what} failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "analytics failed")
}

pub(super) async fn get_analytics(
    State(state): State<AppState>,
    Query(query): Query<DaysQuery>,
) -> Result<Json<crate::analytics::Analytics>, Refusal> {
    let now = chrono::Utc::now();
    let window = window_of(&query, now)?;
    crate::analytics::report(&state.pool, crate::analytics::Scope::Global, window, now)
        .await
        .map(Json)
        .map_err(|error| failed(error, "analytics"))
}

/// A 404 for a project not on the roster — unlike `/readings`, whose panel lives inside a project
/// page that already exists; this route is reachable by URL alone.
pub(super) async fn get_project_analytics(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DaysQuery>,
) -> Result<Json<crate::analytics::Analytics>, Refusal> {
    let now = chrono::Utc::now();
    let window = window_of(&query, now)?;
    // Same query as `common::project_is_on_the_roster`, repeated on purpose: the plan wants a
    // plain-text 404 body here, and that helper answers a JSON refusal.
    let known: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM autopilot_state WHERE project_id = ?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| failed(error, "project analytics"))?;
    if known.is_none() {
        return Err((StatusCode::NOT_FOUND, "no such project"));
    }
    crate::analytics::report(
        &state.pool,
        crate::analytics::Scope::Project(&id),
        window,
        now,
    )
    .await
    .map(Json)
    .map_err(|error| failed(error, "project analytics"))
}

pub(super) async fn get_analytics_quota(
    State(state): State<AppState>,
    Query(query): Query<DaysQuery>,
) -> Result<Json<crate::analytics::QuotaAnalytics>, Refusal> {
    let window = window_of(&query, chrono::Utc::now())?;
    crate::analytics::quota_history(&state.pool, window)
        .await
        .map(Json)
        .map_err(|error| failed(error, "quota analytics"))
}
