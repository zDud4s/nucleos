//! The owner door for declared workflow parameters (`/projects/{id}/workflow-params*` and
//! `/workflow-params/{bundle}/{name}/mode`).
//!
//! §spec workflow-auto-afinacao. These routes are in no scope table of `auth.rs`, so only the
//! control token and an Admin key reach them, like `/settings*`. They are thin handlers over
//! `workflows::param_service`, which owns every rule (manifest, bounds, mode, hand edits, path).
//! An HTTP caller is always an `Owner` actor: the tuner reaches the service in process.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::common::{project_is_on_the_roster, refusal_with_detail};
use super::settings::writable_scope;
use crate::auth::{ApiTokenLevel, Scope as Caller};
use crate::door::refusal;
use crate::state::AppState;
use crate::workflows;
use crate::workflows::param_service::{self, Actor, Ctx, ParamState, Refusal};
use crate::workflows::param_store::{Mode, ModeSource, Row};

type Refused = (StatusCode, Json<Value>);

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/workflow-params", get(list))
        .route(
            "/projects/{id}/workflow-params/{bundle}/{name}",
            put(set_value),
        )
        .route(
            "/projects/{id}/workflow-params/{bundle}/{name}/history",
            get(history),
        )
        .route(
            "/projects/{id}/workflow-params/{bundle}/{name}/rollback/{version}",
            post(rollback),
        )
        .route(
            "/projects/{id}/workflow-params/{bundle}/{name}/mode",
            put(set_project_mode),
        )
        .route(
            "/workflow-params/{bundle}/{name}/mode",
            put(set_machine_mode),
        )
}

#[derive(Deserialize)]
struct SetRequest {
    value: Value,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
struct RollbackRequest {
    #[serde(default)]
    reason: String,
}

fn bad_body(rejection: JsonRejection) -> Refused {
    refusal_with_detail(rejection.status(), "invalid", rejection.body_text())
}

/// Who asked, as the history records it: `owner:control` or `owner:admin`.
fn actor_of(caller: &Caller) -> Actor {
    let who = match caller {
        Caller::Control => "control",
        Caller::ApiToken(ApiTokenLevel::Admin) => "admin",
        _ => "other",
    };
    Actor::Owner(who.to_string())
}

fn project_scope(id: &str) -> String {
    format!("project:{id}")
}

/// A reason is required: the history is the owner's trail.
fn required_reason(reason: &str) -> Result<String, Refused> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(refusal(StatusCode::UNPROCESSABLE_ENTITY, "reason_required"));
    }
    Ok(reason.to_string())
}

fn row_json(row: &Row) -> Value {
    json!({
        "version": row.version,
        "value": row.value,
        "kind": row.kind.as_str(),
        "actor": row.actor,
        "reason": row.reason,
        "rolled_back_to": row.rolled_back_to,
        "written_at": row.written_at,
    })
}

fn mode_source_text(source: ModeSource) -> &'static str {
    match source {
        ModeSource::Project => "project",
        ModeSource::Machine => "machine",
        ModeSource::Default => "default",
    }
}

fn refused(error: Refusal) -> Refused {
    let status = match &error {
        Refusal::UnknownParam | Refusal::NoSuchVersion(_) => StatusCode::NOT_FOUND,
        Refusal::OutOfBounds(_) | Refusal::FileUnsafe(_) | Refusal::NotAScalar(_) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        Refusal::FileAbsent | Refusal::FixedByOwner | Refusal::HandEdited { .. } => {
            StatusCode::CONFLICT
        }
        Refusal::TierNotAutonomous(_) => StatusCode::FORBIDDEN,
        Refusal::Db(why) => {
            tracing::warn!(error = %why, "a workflow parameter request failed on the database");
            return refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal");
        }
    };
    let mut body = json!({ "refusal": error.code(), "detail": error.to_string() });
    if let Refusal::HandEdited {
        file_value,
        recorded,
    } = &error
    {
        body["file_value"] = file_value.clone();
        body["recorded"] = recorded.clone();
    }
    (status, Json(body))
}

fn no_such_bundle() -> Refused {
    refusal(StatusCode::NOT_FOUND, "no_such_bundle")
}

/// The project's pins, or the refusal that says they cannot be read.
fn pinned(id: &str) -> Result<workflows::Pins, Refused> {
    crate::project::rules::pins(id).map_err(|error| {
        refusal_with_detail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unreadable_pins",
            error.to_string(),
        )
    })
}

/// The context of one pinned bundle of a project, read from the library copy only.
fn context_of(
    state: &AppState,
    pins: &workflows::Pins,
    id: &str,
    bundle: &str,
) -> Result<Ctx, Refused> {
    let (Some(library), Some(machine)) = (
        state.workflow_library.as_deref(),
        state.machine_config_root.as_deref(),
    ) else {
        return Err(no_such_bundle());
    };
    pins.workflows
        .iter()
        .find(|pin| pin.name == bundle)
        .and_then(|pin| Ctx::for_pin(library, machine, id, pin))
        .ok_or_else(no_such_bundle)
}

fn state_json(bundle: &str, item: &ParamState) -> Value {
    let decl = item.decl.as_ref();
    json!({
        "bundle": bundle,
        "name": item.name,
        "type": decl.map_or(Value::Null, |d| serde_json::to_value(d.ty).unwrap_or(Value::Null)),
        "min": decl.and_then(|d| d.min),
        "max": decl.and_then(|d| d.max),
        "values": decl.map(|d| d.values.clone()),
        "tier": decl.map(|d| d.tier),
        "file": decl.map(|d| d.file.clone()),
        "key": decl.map(|d| d.key.clone()),
        "default": decl.and_then(|d| d.default.clone()),
        "mode": item.mode.as_str(),
        "mode_source": mode_source_text(item.mode_source),
        "file_value": item.file_value,
        "recorded": item.recorded.as_ref().map(row_json),
        "drift": item.drift,
        "orphaned": item.orphaned,
    })
}

/// `GET /projects/{id}/workflow-params`: every declared (and orphaned) parameter of every bundle
/// the project pins.
async fn list(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Refused> {
    project_is_on_the_roster(&state, &id).await?;
    let pins = pinned(&id)?;
    let mut params = Vec::new();
    for pin in &pins.workflows {
        let Ok(ctx) = context_of(&state, &pins, &id, &pin.name) else {
            continue;
        };
        let items = param_service::state(&state.pool, &ctx)
            .await
            .map_err(refused)?;
        params.extend(items.iter().map(|item| state_json(&pin.name, item)));
    }
    Ok(Json(json!({ "params": params })))
}

/// `PUT /projects/{id}/workflow-params/{bundle}/{name}`: the owner sets a value.
async fn set_value(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path((id, bundle, name)): Path<(String, String, String)>,
    request: Result<Json<SetRequest>, JsonRejection>,
) -> Result<Json<Value>, Refused> {
    writable_scope(&state, &project_scope(&id)).await?;
    let Json(request) = request.map_err(bad_body)?;
    let reason = required_reason(&request.reason)?;
    let ctx = context_of(&state, &pinned(&id)?, &id, &bundle)?;
    let row = param_service::set_value(
        &state.pool,
        &ctx,
        &name,
        &request.value,
        &actor_of(&caller),
        &reason,
    )
    .await
    .map_err(refused)?;
    Ok(Json(row_json(&row)))
}

/// `GET /projects/{id}/workflow-params/{bundle}/{name}/history`, newest first.
async fn history(
    State(state): State<AppState>,
    Path((id, bundle, name)): Path<(String, String, String)>,
) -> Result<Json<Value>, Refused> {
    project_is_on_the_roster(&state, &id).await?;
    let ctx = context_of(&state, &pinned(&id)?, &id, &bundle)?;
    let rows = param_service::history(&state.pool, &ctx, &name)
        .await
        .map_err(refused)?;
    Ok(Json(Value::Array(rows.iter().map(row_json).collect())))
}

/// `POST /projects/{id}/workflow-params/{bundle}/{name}/rollback/{version}`.
async fn rollback(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path((id, bundle, name, version)): Path<(String, String, String, i64)>,
    request: Result<Json<RollbackRequest>, JsonRejection>,
) -> Result<Json<Value>, Refused> {
    writable_scope(&state, &project_scope(&id)).await?;
    let Json(request) = request.map_err(bad_body)?;
    let reason = required_reason(&request.reason)?;
    let ctx = context_of(&state, &pinned(&id)?, &id, &bundle)?;
    let row = param_service::rollback(
        &state.pool,
        &ctx,
        &name,
        version,
        &actor_of(&caller),
        &reason,
    )
    .await
    .map_err(refused)?;
    Ok(Json(row_json(&row)))
}

/// The body `{ "mode": "auto" | "fixed" | null }`. The key must be present: a body without it is
/// a mistake, not a request to clear.
fn mode_of(request: Result<Json<Value>, JsonRejection>) -> Result<Option<Mode>, Refused> {
    let Json(body) = request.map_err(bad_body)?;
    let invalid = |detail: &str| {
        refusal_with_detail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid",
            detail.to_string(),
        )
    };
    match body.get("mode") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Mode::from_str(text)
            .map(Some)
            .ok_or_else(|| invalid("`mode` is `auto`, `fixed` or null")),
        _ => Err(invalid("the body needs a `mode`: `auto`, `fixed` or null")),
    }
}

/// `PUT /projects/{id}/workflow-params/{bundle}/{name}/mode`: the project override.
async fn set_project_mode(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path((id, bundle, name)): Path<(String, String, String)>,
    request: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, Refused> {
    let scope = writable_scope(&state, &project_scope(&id)).await?;
    let mode = mode_of(request)?;
    let ctx = context_of(&state, &pinned(&id)?, &id, &bundle)?;
    let known = param_service::state(&state.pool, &ctx)
        .await
        .map_err(refused)?;
    if !known.iter().any(|item| item.name == name) {
        return Err(refused(Refusal::UnknownParam));
    }
    let by = actor_of(&caller).text();
    param_service::set_mode(&state.pool, &bundle, &scope, &name, mode, &by)
        .await
        .map_err(refused)?;
    Ok(Json(json!({ "mode": mode.map(Mode::as_str) })))
}

/// `PUT /workflow-params/{bundle}/{name}/mode`: the machine default.
async fn set_machine_mode(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path((bundle, name)): Path<(String, String)>,
    request: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, Refused> {
    let scope = writable_scope(&state, "machine").await?;
    let mode = mode_of(request)?;
    if !workflows::valid_name(&bundle) || name.trim().is_empty() {
        return Err(no_such_bundle());
    }
    let by = actor_of(&caller).text();
    param_service::set_mode(&state.pool, &bundle, &scope, &name, mode, &by)
        .await
        .map_err(refused)?;
    Ok(Json(json!({ "mode": mode.map(Mode::as_str) })))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::{Value, json};

    use super::super::tests::{library_with, test_state, with_project_home, workflow_call};
    use crate::governance::autopilot::{set_kill_switch, set_scoped_kill};
    use crate::project::rules::testing::{Home, publish_pins};
    use crate::state::AppState;

    const MANIFEST: &str = "description: synthetic
params:
  - name: alpha.size
    type: int
    min: 1
    max: 10
    tier: 1
    file: p.yaml
    key: alpha.size
  - name: alpha.mode
    type: enum
    values: [fast, slow]
    default: fast
    tier: 1
    file: p.yaml
    key: alpha.mode
";

    const STATE_FILE: &str = "# Synthetic state file.\nalpha:\n  size: 3\n  other: keep\n";

    const BASE: &str = "/projects/p1/workflow-params";

    struct Fixture {
        _home: Home,
        _library: tempfile::TempDir,
        _machine: tempfile::TempDir,
        state: AppState,
        file: std::path::PathBuf,
    }

    impl Fixture {
        async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
            workflow_call(self.state.clone(), method, uri, body).await
        }

        fn text(&self) -> String {
            std::fs::read_to_string(&self.file).unwrap()
        }

        /// The listed parameter `alpha.size` of the pinned bundle.
        async fn size(&self) -> Value {
            let (status, listing) = self.call("GET", BASE, None).await;
            assert_eq!(status, StatusCode::OK, "{listing}");
            listing["params"]
                .as_array()
                .expect("a list of parameters")
                .iter()
                .find(|item| item["name"] == "alpha.size")
                .unwrap_or_else(|| panic!("alpha.size is not listed in {listing}"))
                .clone()
        }

        async fn put_size(&self, value: Value) -> (StatusCode, Value) {
            let uri = format!("{BASE}/synth/alpha.size");
            self.call(
                "PUT",
                &uri,
                Some(json!({ "value": value, "reason": "tune" })),
            )
            .await
        }
    }

    /// Project `p1` on the roster pinning `synth@1.0`, whose manifest declares two parameters in
    /// `p.yaml` under the project's state directory.
    async fn fixture() -> Fixture {
        let home = Home::new();
        let mut state = test_state().await;
        let (library, path) = library_with(&[("synth", "1.0", MANIFEST)]);
        state.workflow_library = Some(path);
        let machine = with_project_home(&mut state);
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('p1', 'active')")
            .execute(&state.pool)
            .await
            .unwrap();
        publish_pins(
            "p1",
            "workflows:\n  - name: synth\n    version: '1.0'\n    origin: library\n    hash: abc\n",
        );
        let dir = crate::project::state::dir(machine.path(), "p1").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("p.yaml");
        std::fs::write(&file, STATE_FILE).unwrap();
        Fixture {
            _home: home,
            _library: library,
            _machine: machine,
            state,
            file,
        }
    }

    #[tokio::test]
    async fn get_lists_the_declared_parameters_of_the_pinned_bundle_with_mode_and_drift() {
        let fixture = fixture().await;

        let (status, listing) = fixture.call("GET", BASE, None).await;
        assert_eq!(status, StatusCode::OK, "{listing}");
        assert_eq!(listing["params"].as_array().map(Vec::len), Some(2));

        let size = fixture.size().await;
        assert_eq!(size["bundle"], "synth");
        assert_eq!(size["type"], "int");
        assert_eq!(size["min"].as_f64(), Some(1.0));
        assert_eq!(size["max"].as_f64(), Some(10.0));
        assert_eq!(size["tier"], 1);
        assert_eq!(size["file"], "p.yaml");
        assert_eq!(size["key"], "alpha.size");
        assert_eq!(size["file_value"], 3);
        assert_eq!(size["mode"], "auto");
        assert_eq!(size["mode_source"], "default");
        assert_eq!(size["recorded"], Value::Null);
        assert_eq!(size["drift"], false);
        assert_eq!(size["orphaned"], false);
    }

    #[tokio::test]
    async fn put_sets_a_value_and_the_file_changes() {
        let fixture = fixture().await;

        let (status, row) = fixture.put_size(json!(5)).await;
        assert_eq!(status, StatusCode::OK, "{row}");
        assert_eq!(row["value"], 5);
        assert_eq!(row["kind"], "set");
        assert_eq!(row["actor"], "owner:control");
        assert_eq!(row["reason"], "tune");

        let text = fixture.text();
        assert!(text.contains("size: 5"), "{text}");
        assert!(text.contains("other: keep"), "{text}");
        assert!(text.starts_with("# Synthetic state file."), "{text}");
        assert_eq!(fixture.size().await["recorded"]["value"], 5);
    }

    #[tokio::test]
    async fn put_flips_the_project_mode_to_fixed_and_get_shows_it() {
        let fixture = fixture().await;
        assert_eq!(fixture.size().await["mode"], "auto");

        let (status, row) = fixture.put_size(json!(4)).await;
        assert_eq!(status, StatusCode::OK, "{row}");
        let size = fixture.size().await;
        assert_eq!(size["mode"], "fixed");
        assert_eq!(size["mode_source"], "project");

        let uri = format!("{BASE}/synth/alpha.size/mode");
        let (status, answer) = fixture
            .call("PUT", &uri, Some(json!({ "mode": "auto" })))
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let size = fixture.size().await;
        assert_eq!(size["mode"], "auto");
        assert_eq!(size["mode_source"], "project");
    }

    #[tokio::test]
    async fn put_out_of_bounds_is_422_and_the_file_is_untouched() {
        let fixture = fixture().await;

        for bad in [json!(0), json!(11), json!(-1), json!("4"), json!(true)] {
            let (status, answer) = fixture.put_size(bad.clone()).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {answer}");
            assert_eq!(answer["refusal"], "out_of_bounds", "{bad}");
        }
        assert_eq!(fixture.text(), STATE_FILE);
        let (_, rows) = fixture
            .call("GET", &format!("{BASE}/synth/alpha.size/history"), None)
            .await;
        assert_eq!(rows.as_array().map(Vec::len), Some(0), "{rows}");
    }

    #[tokio::test]
    async fn put_without_a_reason_is_422() {
        let fixture = fixture().await;
        let uri = format!("{BASE}/synth/alpha.size");

        for body in [
            json!({ "value": 5 }),
            json!({ "value": 5, "reason": "   " }),
        ] {
            let (status, answer) = fixture.call("PUT", &uri, Some(body)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
        }
        assert_eq!(fixture.text(), STATE_FILE);
    }

    #[tokio::test]
    async fn history_and_rollback_round_trip() {
        let fixture = fixture().await;
        assert_eq!(fixture.put_size(json!(5)).await.0, StatusCode::OK);
        assert_eq!(fixture.put_size(json!(7)).await.0, StatusCode::OK);
        assert!(fixture.text().contains("size: 7"));

        let (status, rows) = fixture
            .call("GET", &format!("{BASE}/synth/alpha.size/history"), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{rows}");
        let rows = rows.as_array().expect("an array of rows");
        assert_eq!(rows[0]["value"], 7, "newest first");
        let five = rows
            .iter()
            .find(|row| row["value"] == 5 && row["kind"] == "set")
            .expect("the first set is in the history");
        let version = five["version"].as_i64().unwrap();

        let uri = format!("{BASE}/synth/alpha.size/rollback/{version}");
        let (status, row) = fixture
            .call("POST", &uri, Some(json!({ "reason": "back" })))
            .await;
        assert_eq!(status, StatusCode::OK, "{row}");
        assert_eq!(row["kind"], "rollback");
        assert_eq!(row["value"], 5);
        assert_eq!(row["rolled_back_to"], version);
        assert!(fixture.text().contains("size: 5"));

        let uri = format!("{BASE}/synth/alpha.size/rollback/9999");
        let (status, answer) = fixture
            .call("POST", &uri, Some(json!({ "reason": "back" })))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "no_such_version");
    }

    #[tokio::test]
    async fn project_mode_overrides_machine_mode_and_null_clears_it() {
        let fixture = fixture().await;
        let project = format!("{BASE}/synth/alpha.size/mode");
        let machine = "/workflow-params/synth/alpha.size/mode";

        let (status, answer) = fixture
            .call("PUT", machine, Some(json!({ "mode": "fixed" })))
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let size = fixture.size().await;
        assert_eq!(size["mode"], "fixed");
        assert_eq!(size["mode_source"], "machine");

        let (status, answer) = fixture
            .call("PUT", &project, Some(json!({ "mode": "auto" })))
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let size = fixture.size().await;
        assert_eq!(size["mode"], "auto");
        assert_eq!(size["mode_source"], "project");

        let (status, answer) = fixture
            .call("PUT", &project, Some(json!({ "mode": null })))
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let size = fixture.size().await;
        assert_eq!(size["mode"], "fixed");
        assert_eq!(size["mode_source"], "machine");

        let (status, answer) = fixture.call("PUT", &project, Some(json!({}))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
        let (status, answer) = fixture
            .call("PUT", &project, Some(json!({ "mode": "maybe" })))
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
    }

    #[tokio::test]
    async fn the_kill_switch_answers_423_before_anything_is_read() {
        let fixture = fixture().await;
        set_kill_switch(&fixture.state.pool, true).await.unwrap();

        // A bundle that is not pinned and a body that is not valid: the stop still wins.
        let uri = format!("{BASE}/nonsense/alpha.size");
        let (status, answer) = fixture.call("PUT", &uri, Some(json!({ "nope": 1 }))).await;
        assert_eq!(status, StatusCode::LOCKED, "{answer}");
        assert_eq!(answer["refusal"], "kill_switch");
        let (status, _) = fixture.put_size(json!(5)).await;
        assert_eq!(status, StatusCode::LOCKED);
        let (status, _) = fixture
            .call(
                "PUT",
                "/workflow-params/synth/alpha.size/mode",
                Some(json!({ "mode": "fixed" })),
            )
            .await;
        assert_eq!(status, StatusCode::LOCKED);
        assert_eq!(fixture.text(), STATE_FILE);

        set_kill_switch(&fixture.state.pool, false).await.unwrap();
        set_scoped_kill(&fixture.state.pool, "project", "p1", true)
            .await
            .unwrap();
        let (status, _) = fixture.put_size(json!(5)).await;
        assert_eq!(status, StatusCode::LOCKED, "the stop of the project");
    }

    #[tokio::test]
    async fn a_bundle_the_project_does_not_pin_is_404() {
        let fixture = fixture().await;

        let uri = format!("{BASE}/other/alpha.size");
        let (status, answer) = fixture
            .call("PUT", &uri, Some(json!({ "value": 5, "reason": "x" })))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "no_such_bundle");
        let (status, answer) = fixture.call("GET", &format!("{uri}/history"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "no_such_bundle");

        let uri = format!("{BASE}/synth/nonsense");
        let (status, answer) = fixture
            .call("PUT", &uri, Some(json!({ "value": 5, "reason": "x" })))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "unknown_param");

        let (status, answer) = fixture
            .call("GET", "/projects/ghost/workflow-params", None)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "no_such_project");
    }

    #[tokio::test]
    async fn a_non_admin_api_key_cannot_reach_the_routes() {
        use crate::auth::{ApiTokenLevel, mint_api_token};
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let fixture = fixture().await;
        let mut tokens = Vec::new();
        for (name, level) in [
            ("reader", ApiTokenLevel::ReadOnly),
            ("runner", ApiTokenLevel::RunCreating),
        ] {
            let (token, secret) = mint_api_token(name);
            sqlx::query(
                "INSERT INTO api_tokens (name, token, access_level, created_at)
                 VALUES (?, ?, ?, '2026-07-29T12:00:00Z')",
            )
            .bind(name)
            .bind(secret)
            .bind(level.as_str())
            .execute(&fixture.state.pool)
            .await
            .unwrap();
            tokens.push(token);
        }

        for token in &tokens {
            for (method, uri) in [
                ("GET", BASE.to_string()),
                ("PUT", format!("{BASE}/synth/alpha.size")),
                ("GET", format!("{BASE}/synth/alpha.size/history")),
                ("POST", format!("{BASE}/synth/alpha.size/rollback/1")),
                ("PUT", format!("{BASE}/synth/alpha.size/mode")),
                ("PUT", "/workflow-params/synth/alpha.size/mode".to_string()),
            ] {
                let response = crate::http::build_router(fixture.state.clone())
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(&uri)
                            .header("Authorization", format!("Bearer {token}"))
                            .header("Content-Type", "application/json")
                            .body(Body::from(r#"{"value":5,"reason":"x","mode":"fixed"}"#))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri}");
            }
        }
        assert_eq!(fixture.text(), STATE_FILE);
    }
}
