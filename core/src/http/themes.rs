//! The owner's door to the themes (`/themes*`, spec 2026-10-22 section 6). Owner-only by omission from
//! every scoped route table in `auth.rs`. The detail answers with the member labels the Brain graph
//! shows (a note's first line, a knowledge title) and the synthesis column; nothing here is logged.

use super::*;
use crate::themes::{self, MemberKind, ThemeError};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/themes", get(list_themes))
        .route("/themes/{id}", get(get_theme))
        .route("/themes/{id}/accept", post(accept_theme))
        .route("/themes/{id}/synthesise", post(synthesise_theme))
        .route("/themes/{id}/refuse", post(refuse_theme))
        .route("/themes/{id}/rename", post(rename_theme))
        .route("/themes/{id}/merge", post(merge_theme))
        .route("/themes/{id}/members", post(add_theme_member))
        .route(
            "/themes/{id}/members/{kind}/{member_id}",
            delete(exclude_theme_member),
        )
}

#[derive(Deserialize)]
pub(super) struct ThemeListQuery {
    state: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct AcceptBody {
    name: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct RenameBody {
    name: String,
}

#[derive(Deserialize)]
pub(super) struct MergeBody {
    into: i64,
}

#[derive(Deserialize)]
pub(super) struct MemberBody {
    kind: String,
    id: i64,
}

/// `NotFound` is 404, a move the table or the target refuses is 409, a bad name is 422.
fn status_of(error: &ThemeError) -> StatusCode {
    match error {
        ThemeError::NotFound => StatusCode::NOT_FOUND,
        ThemeError::NotAllowed | ThemeError::BadTarget => StatusCode::CONFLICT,
        ThemeError::BadName => StatusCode::UNPROCESSABLE_ENTITY,
        ThemeError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn done(result: Result<(), ThemeError>) -> StatusCode {
    match result {
        Ok(()) => StatusCode::OK,
        Err(error) => status_of(&error),
    }
}

fn db(_: sqlx::Error) -> StatusCode {
    StatusCode::INTERNAL_SERVER_ERROR
}

/// Themes with their member counts. The list never carries the synthesis.
pub(super) async fn list_themes(
    State(state): State<AppState>,
    Query(query): Query<ThemeListQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let rows: Vec<(i64, String, String, Option<i64>, i64, String)> = sqlx::query_as(
        "SELECT t.id, t.name, t.state, t.merged_into,
                (SELECT COUNT(*) FROM theme_members m WHERE m.theme_id = t.id AND m.excluded = 0),
                t.created_at
           FROM themes t WHERE (? IS NULL OR t.state = ?) ORDER BY t.id DESC",
    )
    .bind(query.state.as_deref())
    .bind(query.state.as_deref())
    .fetch_all(&state.pool)
    .await
    .map_err(db)?;
    Ok(Json(serde_json::Value::Array(
        rows.into_iter()
            .map(|(id, name, state, merged_into, members, created_at)| {
                serde_json::json!({
                    "id": id,
                    "name": name,
                    "state": state,
                    "merged_into": merged_into,
                    "members": members,
                    "created_at": created_at,
                })
            })
            .collect(),
    )))
}

/// One theme with its members resolved. A member whose row is gone reads `missing`.
pub(super) async fn get_theme(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    type Head = (
        i64,
        String,
        String,
        Option<i64>,
        Option<String>,
        String,
        Option<String>,
    );
    let head: Head = sqlx::query_as(
        "SELECT id, name, state, merged_into, synthesis, created_at, decided_at
           FROM themes WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(db)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let members: Vec<(String, i64, String, i64)> = sqlx::query_as(
        "SELECT member_kind, member_id, origin, excluded FROM theme_members
          WHERE theme_id = ? ORDER BY member_kind, member_id",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(db)?;
    let refs: Vec<(String, String)> = members
        .iter()
        .map(|(kind, member_id, _, _)| (kind.clone(), member_id.to_string()))
        .collect();
    let wanted: Vec<(&str, &str)> = refs.iter().map(|(k, r)| (k.as_str(), r.as_str())).collect();
    let targets = crate::owner_notes::resolve_sql_labels(&state.pool, &wanted)
        .await
        .map_err(db)?;
    let members: Vec<serde_json::Value> = members
        .iter()
        .zip(targets)
        .map(|((kind, member_id, origin, excluded), target)| {
            serde_json::json!({
                "kind": kind,
                "id": member_id,
                "origin": origin,
                "excluded": *excluded != 0,
                "label": target.label,
                "missing": target.missing || MemberKind::parse(kind).is_none(),
            })
        })
        .collect();
    let (id, name, theme_state, merged_into, synthesis, created_at, decided_at) = head;
    Ok(Json(serde_json::json!({
        "id": id,
        "name": name,
        "state": theme_state,
        "merged_into": merged_into,
        "synthesis": if theme_state == "accepted" { synthesis } else { None },
        "created_at": created_at,
        "decided_at": decided_at,
        "members": members,
    })))
}

pub(super) async fn accept_theme(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: Option<Json<AcceptBody>>,
) -> StatusCode {
    let name = body.and_then(|Json(body)| body.name);
    let status = done(themes::accept(&state.pool, id, name.as_deref()).await);
    if status == StatusCode::OK {
        spawn_first_synthesis(state, id);
    }
    status
}

/// The acceptance schedules the first synthesis, but only if the distiller's model can be served now;
/// a refused route leaves it to the weekly run. The text never leaves the theme row.
fn spawn_first_synthesis(state: AppState, id: i64) {
    tokio::spawn(async move {
        let Ok(route) = crate::distill::model::route_for(&state).await else {
            return;
        };
        let asked = route.extractor(state.runner.as_ref(), &state.web.http);
        let _ = themes::synthesise(&state.pool, asked, id, chrono::Utc::now()).await;
    });
}

/// Force a synthesis now: only an `accepted` theme (409), at most once an hour (429), and only when
/// the distiller's model can be served (503, fixed text).
pub(super) async fn synthesise_theme(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> axum::response::Response {
    let row: Option<(String, Option<String>)> =
        match sqlx::query_as("SELECT state, synthesis_at FROM themes WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(row) => row,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
    let Some((theme_state, synthesis_at)) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if theme_state != "accepted" {
        return StatusCode::CONFLICT.into_response();
    }
    let now = chrono::Utc::now();
    let at = synthesis_at
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
        .map(|t| t.with_timezone(&chrono::Utc));
    if !themes::force_allowed(at, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(route) = crate::distill::model::route_for(&state).await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "the distiller model cannot be served" })),
        )
            .into_response();
    };
    let asked = route.extractor(state.runner.as_ref(), &state.web.http);
    if themes::synthesise(&state.pool, asked, id, now).await.is_err() {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    match sqlx::query_scalar::<_, Option<String>>("SELECT synthesis FROM themes WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await
    {
        Ok(synthesis) => Json(serde_json::json!({ "synthesis": synthesis })).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub(super) async fn refuse_theme(State(state): State<AppState>, Path(id): Path<i64>) -> StatusCode {
    done(themes::refuse(&state.pool, id).await)
}

pub(super) async fn rename_theme(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<RenameBody>,
) -> StatusCode {
    done(themes::rename(&state.pool, id, &body.name).await)
}

pub(super) async fn merge_theme(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<MergeBody>,
) -> StatusCode {
    done(themes::merge(&state.pool, id, body.into).await)
}

pub(super) async fn add_theme_member(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<MemberBody>,
) -> StatusCode {
    let Some(kind) = MemberKind::parse(&body.kind) else {
        return StatusCode::UNPROCESSABLE_ENTITY;
    };
    done(themes::add_member(&state.pool, id, kind, body.id).await)
}

pub(super) async fn exclude_theme_member(
    State(state): State<AppState>,
    Path((id, kind, member_id)): Path<(i64, String, i64)>,
) -> StatusCode {
    let Some(kind) = MemberKind::parse(&kind) else {
        return StatusCode::UNPROCESSABLE_ENTITY;
    };
    done(themes::exclude_member(&state.pool, id, kind, member_id).await)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    async fn call(
        state: AppState,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", "Bearer test-token");
        let body = match body {
            Some(body) => {
                request = request.header("content-type", "application/json");
                Body::from(body.to_owned())
            }
            None => Body::empty(),
        };
        let response = super::super::build_router(state)
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn themes_list_detail_and_actions_round_trip() {
        let state = super::super::tests::test_state().await;
        let t = sqlx::query_scalar::<_, i64>(
            "INSERT INTO themes (name, state, model, dim, created_at) VALUES ('T', 'proposed', 'm', 3, 't') RETURNING id",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO theme_members (theme_id, member_kind, member_id, added_at) VALUES (?, 'note', 9999, 't')",
        )
        .bind(t)
        .execute(&state.pool)
        .await
        .unwrap();
        let (s, list) = call(state.clone(), "GET", "/themes?state=proposed", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(list[0]["id"], t);
        assert!(
            list[0].get("synthesis").is_none(),
            "the list carries counts, never the synthesis"
        );
        let (_, detail) = call(state.clone(), "GET", &format!("/themes/{t}"), None).await;
        assert_eq!(detail["members"][0]["missing"], true);
        let (s, _) = call(
            state.clone(),
            "POST",
            &format!("/themes/{t}/accept"),
            Some(r#"{"name":"Sleep"}"#),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(
            state.clone(),
            "POST",
            &format!("/themes/{t}/accept"),
            Some("{}"),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "accepted -> accepted is not in the table");
        let (s, _) = call(state, "POST", "/themes/999/refuse", Some("{}")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    fn canned_runner(stdout: &str) -> std::sync::Arc<crate::execution::runner::FakeCommandRunner> {
        std::sync::Arc::new(crate::execution::runner::FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(crate::execution::runner::RunOutcome {
                exit_code: 0,
                stdout: stdout.to_owned(),
                stderr: String::new(),
                session_id: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
                model_requested: false,
            })),
            ..Default::default()
        })
    }

    async fn theme_in(state: &AppState, theme_state: &str, synthesis_at: Option<&str>) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO themes (name, state, model, dim, synthesis, synthesis_at, created_at)
             VALUES ('T', ?, 'm', 3, 'old', ?, 't') RETURNING id",
        )
        .bind(theme_state)
        .bind(synthesis_at)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn forcing_a_synthesis_is_for_accepted_themes_and_once_an_hour() {
        let mut state = super::super::tests::test_state().await;
        state.runner = canned_runner("A fresh synthesis.");
        let proposed = theme_in(&state, "proposed", None).await;
        let (s, _) = call(
            state.clone(),
            "POST",
            &format!("/themes/{proposed}/synthesise"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);
        let recent = (chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339();
        let busy = theme_in(&state, "accepted", Some(&recent)).await;
        let (s, _) = call(
            state.clone(),
            "POST",
            &format!("/themes/{busy}/synthesise"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
        let old = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        let due = theme_in(&state, "accepted", Some(&old)).await;
        let (s, body) = call(
            state.clone(),
            "POST",
            &format!("/themes/{due}/synthesise"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["synthesis"], "A fresh synthesis.");
        let (_, detail) = call(state.clone(), "GET", &format!("/themes/{proposed}"), None).await;
        assert!(detail["synthesis"].is_null(), "only an accepted theme shows it");
        let (_, list) = call(state, "GET", "/themes", None).await;
        assert!(list.as_array().unwrap().iter().all(|t| t.get("synthesis").is_none()));
    }
}
