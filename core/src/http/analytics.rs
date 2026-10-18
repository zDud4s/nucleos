//! The analytics routes (`/analytics`, `/analytics/quota`, `/projects/{id}/analytics`): parse
//! `days`, refuse anything but 7/30/90, and hand the window to `crate::analytics`.
//!
//! Development time (spec analytics-devtime §5.3) beside them: `/analytics/devtime` and
//! `/analytics/devtime/findings`, each also under `/projects/{id}/…`, and the global
//! `POST /analytics/devtime/findings/{key}/mark`, handed to `crate::analytics::devtime`.

use axum::response::Response;

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/analytics", get(get_analytics))
        .route("/analytics/quota", get(get_analytics_quota))
        .route("/projects/{id}/analytics", get(get_project_analytics))
        .route("/analytics/devtime", get(get_devtime))
        .route("/analytics/devtime/findings", get(get_devtime_findings))
        .route(
            "/analytics/devtime/findings/{key}/mark",
            post(post_devtime_mark),
        )
        .route("/projects/{id}/analytics/devtime", get(get_project_devtime))
        .route(
            "/projects/{id}/analytics/devtime/findings",
            get(get_project_devtime_findings),
        )
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

/// A 404 for a project not on the roster (shared by the F1 and F2 project routes) — unlike
/// `/readings`, whose panel lives inside a project page that already exists; these routes are
/// reachable by URL alone.
async fn known_project(state: &AppState, id: &str, what: &'static str) -> Result<(), Refusal> {
    // Same query as `common::project_is_on_the_roster`, repeated on purpose: these routes answer a
    // plain-text 404 body, and that helper answers a JSON refusal.
    let known: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM autopilot_state WHERE project_id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| failed(error, what))?;
    known
        .map(|_| ())
        .ok_or((StatusCode::NOT_FOUND, "no such project"))
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

pub(super) async fn get_project_analytics(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DaysQuery>,
) -> Result<Json<crate::analytics::Analytics>, Refusal> {
    let now = chrono::Utc::now();
    let window = window_of(&query, now)?;
    known_project(&state, &id, "project analytics").await?;
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

/* ------------------------------------------------------------- devtime -- */

const DEFAULT_FINDINGS: i64 = 50;
const MAX_FINDINGS: i64 = 200;

/// The JSON refusals of the findings routes (`{"error": code}`), beside F1's plain-text ones.
fn refuse(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

/// The window first, then the roster — F1's order, so a bad `days` is a 400 for any project.
async fn devtime(
    state: &AppState,
    scope: crate::analytics::Scope<'_>,
    query: &DaysQuery,
) -> Result<Json<crate::analytics::devtime::DevtimeOverview>, Refusal> {
    let now = chrono::Utc::now();
    let window = window_of(query, now)?;
    if let crate::analytics::Scope::Project(id) = scope {
        known_project(state, id, "devtime analytics").await?;
    }
    // Re-read per request, so a `refresh-models.py` run needs no restart (spec §4.4).
    let models = crate::config::models_config_now();
    crate::analytics::devtime::devtime_overview(&state.pool, &models, scope, window, now)
        .await
        .map(Json)
        .map_err(|error| failed(error, "devtime analytics"))
}

pub(super) async fn get_devtime(
    State(state): State<AppState>,
    Query(query): Query<DaysQuery>,
) -> Result<Json<crate::analytics::devtime::DevtimeOverview>, Refusal> {
    devtime(&state, crate::analytics::Scope::Global, &query).await
}

pub(super) async fn get_project_devtime(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DaysQuery>,
) -> Result<Json<crate::analytics::devtime::DevtimeOverview>, Refusal> {
    devtime(&state, crate::analytics::Scope::Project(&id), &query).await
}

/// Every parameter read as text, so a bad value is this module's 400, not the extractor's.
#[derive(Debug, Deserialize)]
pub(super) struct FindingsParams {
    days: Option<String>,
    rule_id: Option<String>,
    order: Option<String>,
    limit: Option<String>,
    cursor: Option<String>,
}

async fn devtime_findings(
    state: &AppState,
    scope: crate::analytics::Scope<'_>,
    params: FindingsParams,
) -> Result<Json<crate::analytics::devtime::DevtimeFindings>, Response> {
    use crate::devtime::store::FindingOrder;
    let now = chrono::Utc::now();
    let window = window_of(&DaysQuery { days: params.days }, now)
        .map_err(IntoResponse::into_response)?;
    let order = match params.order.as_deref() {
        None | Some("cost") => FindingOrder::Cost,
        Some("recent") => FindingOrder::Recent,
        Some(_) => return Err(refuse(StatusCode::BAD_REQUEST, "bad_order")),
    };
    let limit = match params.limit.as_deref() {
        None => DEFAULT_FINDINGS,
        Some(raw) => match raw.parse::<i64>() {
            Ok(n) if (1..=MAX_FINDINGS).contains(&n) => n,
            _ => return Err(refuse(StatusCode::BAD_REQUEST, "bad_limit")),
        },
    };
    // A cursor is bound to the order it was issued under: one from `cost` is refused by `recent`.
    let cursor = match params.cursor.as_deref() {
        None => None,
        Some(raw) => Some(
            crate::analytics::devtime::decode_cursor(raw, order)
                .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "bad_cursor"))?,
        ),
    };
    if let crate::analytics::Scope::Project(id) = scope {
        known_project(state, id, "devtime findings")
            .await
            .map_err(IntoResponse::into_response)?;
    }
    let rules = devtime_rules(state);
    let query = crate::analytics::devtime::FindingsQuery {
        rule_id: params.rule_id.as_deref(),
        order,
        limit,
        cursor,
    };
    crate::analytics::devtime::devtime_findings(&state.pool, &rules, scope, window, now, &query)
        .await
        .map(Json)
        .map_err(|error| failed(error, "devtime findings").into_response())
}

/// The precision thresholds, read from `~/.nucleos/devtime.yaml` per request — the file `main.rs`
/// loads at startup, from the same root. Not an `AppState` field: that struct has ~70 literal
/// constructors, and only these routes need it. No root (every test state) is the defaults.
fn devtime_rules(state: &AppState) -> crate::config::DevtimeRulesConfig {
    state
        .machine_config_root
        .as_deref()
        .map(|root| {
            crate::config::load_devtime_config(&root.join(crate::machine_config::DEVTIME_FILE))
                .rules
        })
        .unwrap_or_default()
}

pub(super) async fn get_devtime_findings(
    State(state): State<AppState>,
    Query(params): Query<FindingsParams>,
) -> Result<Json<crate::analytics::devtime::DevtimeFindings>, Response> {
    devtime_findings(&state, crate::analytics::Scope::Global, params).await
}

pub(super) async fn get_project_devtime_findings(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<FindingsParams>,
) -> Result<Json<crate::analytics::devtime::DevtimeFindings>, Response> {
    devtime_findings(&state, crate::analytics::Scope::Project(&id), params).await
}

#[derive(Debug, Deserialize)]
pub(super) struct MarkBody {
    verdict: String,
    #[serde(default)]
    cause: Option<String>,
}

/// Global: a finding's key names it in every project, so there is no per-project variant
/// (spec §5.3). `?days=` only chooses the window the returned rule precision is computed over (the
/// list's).
pub(super) async fn post_devtime_mark(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<DaysQuery>,
    Json(body): Json<MarkBody>,
) -> Result<Json<crate::analytics::devtime::MarkOutcome>, Response> {
    use crate::devtime::precision::MarkError;
    let window = window_of(&query, chrono::Utc::now())
        .map_err(IntoResponse::into_response)?;
    let rules = devtime_rules(&state);
    let verdict = body.verdict.as_str();
    let cause = body.cause.as_deref();
    crate::analytics::devtime::mark_finding(&state.pool, &rules, window, &key, verdict, cause)
        .await
        .map(Json)
        .map_err(|error| match error {
            MarkError::BadVerdict => refuse(StatusCode::BAD_REQUEST, "bad_verdict"),
            MarkError::BadCause => refuse(StatusCode::BAD_REQUEST, "bad_cause"),
            MarkError::UnknownFinding => refuse(StatusCode::NOT_FOUND, "unknown_finding"),
            // Generic, no SQL echoed (spec §6.8).
            MarkError::Db(error) => failed(error, "devtime mark").into_response(),
        })
}
