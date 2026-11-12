//! The settings documents over HTTP (`/settings*`, spec `2026-10-11-configuracao-na-base-de-dados` section 5).
//!
//! These routes are in no scope table of `auth.rs`, so only the control token and an Admin key reach them. There
//! is no `DELETE`: removing a document belongs to the recovery CLI alone.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::common::{project_is_on_the_roster, refusal_with_detail};
use crate::auth::{RemoteDevice, Scope as Caller};
use crate::door::refusal;
use crate::governance::autopilot;
use crate::settings::commands::command_fields;
use crate::settings::{self as store, DocType, Kind, Scope, SettingsError, Snapshot};
use crate::state::AppState;

type Refused = (StatusCode, Json<Value>);

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings", get(index))
        .route(
            "/settings/{scope}/{name}",
            get(read_document).put(write_document),
        )
        .route("/settings/{scope}/{name}/history", get(read_history))
        .route(
            "/settings/{scope}/{name}/rollback/{version}",
            post(rollback_document),
        )
}

#[derive(Deserialize)]
struct IndexQuery {
    scope: Option<String>,
}

#[derive(Deserialize)]
struct WriteRequest {
    body: Value,
    /// The version the client read; 0 means "no document yet". Required: without it a write would be blind.
    version: i64,
}

#[derive(Deserialize)]
struct RollbackRequest {
    /// The version the client believes is current, not the one it is rolling back to.
    version: i64,
}

fn parse_scope(text: &str) -> Result<Scope, Refused> {
    text.parse::<Scope>().map_err(|error| {
        refusal_with_detail(StatusCode::BAD_REQUEST, "bad_scope", error.to_string())
    })
}

async fn on_the_roster(state: &AppState, scope: &Scope) -> Result<(), Refused> {
    match scope.project_id() {
        Some(id) => project_is_on_the_roster(state, id).await,
        None => Ok(()),
    }
}

/// The scope and document a read names, or the refusal that says which part of the path is wrong.
async fn readable_document(
    state: &AppState,
    caller: &Caller,
    scope: &str,
    name: &str,
) -> Result<(Scope, &'static DocType), Refused> {
    let scope = parse_scope(scope)?;
    on_the_roster(state, &scope).await?;
    control_only_machine_document(caller, &scope, name)?;
    let doc =
        store::doc_type(name).ok_or_else(|| refusal(StatusCode::NOT_FOUND, "unknown_document"))?;
    if doc.kind != scope.kind() {
        return Err(refusal(StatusCode::NOT_FOUND, "wrong_scope"));
    }
    Ok((scope, doc))
}

/// The first gate of every write: the stop wins over everything, even over a request it cannot parse. A stop
/// that cannot be read counts as engaged. The global stop is asked before the scope is parsed, because only the
/// stop of one project needs a valid scope.
pub(super) async fn writable_scope(state: &AppState, scope: &str) -> Result<Scope, Refused> {
    let locked = || refusal(StatusCode::LOCKED, "kill_switch");
    if autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return Err(locked());
    }
    let scope = parse_scope(scope)?;
    if let Some(id) = scope.project_id() {
        if autopilot::scoped_kill_engaged(&state.pool, "project", id)
            .await
            .unwrap_or(true)
        {
            return Err(locked());
        }
        project_is_on_the_roster(state, id).await?;
    }
    Ok(scope)
}

/// A write answers `not_ours` for an unknown name and for a known name at the wrong kind of scope alike.
fn owned_document(caller: &Caller, scope: &Scope, name: &str) -> Result<&'static DocType, Refused> {
    control_only_machine_document(caller, scope, name)?;
    store::doc_type(name)
        .filter(|doc| doc.kind == scope.kind())
        .ok_or_else(|| refusal(StatusCode::FORBIDDEN, "not_ours"))
}

/// The role is a machine-wide control decision. Its settings document stays registered for storage
/// and startup, but the generic settings door is never another way for an Admin token to change it.
fn control_only_machine_document(
    caller: &Caller,
    scope: &Scope,
    name: &str,
) -> Result<(), Refused> {
    if matches!(scope, Scope::Machine) && name == "machine" && !matches!(caller, Caller::Control) {
        return Err(refusal(StatusCode::FORBIDDEN, "control_required"));
    }
    Ok(())
}

/// The version of the stored row, readable or not; `None` when there is no row.
fn stored_version(snapshot: &Snapshot, scope: &Scope, name: &str) -> Option<i64> {
    snapshot.version(scope, name).or_else(|| {
        snapshot
            .unreadable_for(scope, name)
            .map(|record| record.version)
    })
}

/// `http:<token-id>` is not available: the control token is one shared secret and `auth::Scope` keeps only the
/// level of an API key, so neither carries an id. The name of the kind of token is what can be said, except for a
/// paired remote device, which `auth.rs` identifies: `remote:<id>`.
fn written_by(caller: &Caller, device: Option<&RemoteDevice>) -> String {
    if let Some(device) = device {
        return format!("remote:{}", device.id);
    }
    let kind = match caller {
        Caller::Control => "http:control",
        Caller::ApiToken(_) => "http:api-token",
        _ => "http:other",
    };
    kind.to_owned()
}

/// A paired device is the owner on another machine, but it never changes a command the host will run: `new` is
/// refused when any command field of `name` differs from `current`. A document that is absent or unreadable has
/// no commands, so a device may only create it without any.
/// Whether `device` is held to [`refuse_command_change`]: every device is, except an owner device whose
/// request carries a fresh step-up (`stepped_up`). An owner without the step-up keeps the refusal.
fn command_change_is_gated(device: &RemoteDevice) -> bool {
    !(device.owner && device.stepped_up)
}

fn refuse_command_change(name: &str, current: &Value, new: &Value) -> Result<(), Refused> {
    if command_fields(name, current) == command_fields(name, new) {
        return Ok(());
    }
    Err(refusal(StatusCode::FORBIDDEN, "remote_command_field"))
}

fn stale(current: i64) -> Refused {
    (
        StatusCode::CONFLICT,
        Json(json!({ "refusal": "stale", "current": current })),
    )
}

fn bad_body(rejection: JsonRejection) -> Refused {
    refusal_with_detail(rejection.status(), "invalid", rejection.body_text())
}

fn refused(error: SettingsError) -> Refused {
    match error {
        SettingsError::UnknownDocument(_) | SettingsError::WrongScope { .. } => {
            refusal(StatusCode::FORBIDDEN, "not_ours")
        }
        SettingsError::Invalid(detail) => {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "invalid", detail)
        }
        SettingsError::Stale { current } => stale(current),
        SettingsError::NotFound => refusal(StatusCode::NOT_FOUND, "no_such_version"),
        SettingsError::Unreadable { reason, .. } => {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "unreadable", reason)
        }
        SettingsError::Database(error) => {
            tracing::warn!(%error, "a settings request failed on the database");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}

/// `GET /settings[?scope=]`: the documents of one scope, in registry order. Without a scope, the machine's.
async fn index(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Query(query): Query<IndexQuery>,
) -> Result<Json<Value>, Refused> {
    let scope = match query.scope.as_deref() {
        Some(text) => parse_scope(text)?,
        None => Scope::Machine,
    };
    on_the_roster(&state, &scope).await?;

    let snapshot = store::global().snapshot();
    let rows: Vec<Value> = store::documents()
        .iter()
        .filter(|doc| doc.kind == scope.kind())
        .filter(|doc| control_only_machine_document(&caller, &scope, doc.name).is_ok())
        .map(|doc| {
            let version = stored_version(&snapshot, &scope, doc.name);
            json!({
                "scope": scope.to_string(),
                "name": doc.name,
                "kind": match doc.kind {
                    Kind::Machine => "machine",
                    Kind::Project => "project",
                },
                "takes_effect": doc.takes_effect,
                "present": version.is_some(),
                "version": version.unwrap_or(0),
                "unreadable": snapshot.is_unreadable(&scope, doc.name),
            })
        })
        .collect();
    Ok(Json(Value::Array(rows)))
}

/// `GET /settings/{scope}/{name}`: the stored body, or the default at version 0. An unreadable row is a 422 and
/// not the defaults, so the client cannot edit what is not stored; it carries the version that replaces the row.
async fn read_document(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path((scope, name)): Path<(String, String)>,
) -> Result<Json<Value>, Refused> {
    let (scope, doc) = readable_document(&state, &caller, &scope, &name).await?;
    let snapshot = store::global().snapshot();
    if let Some(record) = snapshot.unreadable_for(&scope, doc.name) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "refusal": "unreadable",
                "detail": record.reason,
                "version": record.version,
            })),
        ));
    }
    let (body, version) = match snapshot.entry(&scope, doc.name) {
        Some(entry) => (entry.body(), entry.version),
        None => ((doc.default_json)().unwrap_or(Value::Null), 0),
    };
    Ok(Json(
        json!({ "body": body, "version": version, "schema": doc.schema() }),
    ))
}

/// `GET /settings/{scope}/{name}/history`: every version, newest first, without the bodies.
async fn read_history(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path((scope, name)): Path<(String, String)>,
) -> Result<Json<Value>, Refused> {
    let (scope, doc) = readable_document(&state, &caller, &scope, &name).await?;
    let rows = store::history(&state.pool, &scope, doc.name)
        .await
        .map_err(refused)?;
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "version": row.version,
                "written_at": row.written_at,
                "written_by": row.written_by,
                "deleted": row.deleted(),
            })
        })
        .collect();
    Ok(Json(Value::Array(rows)))
}

/// `PUT /settings/{scope}/{name}`, refused in the order of the spec: 423, 403, 422, 409. The store checks the body
/// before the version and writes the `config_written` line inside its own transaction.
async fn write_document(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    device: Option<Extension<RemoteDevice>>,
    Path((scope, name)): Path<(String, String)>,
    request: Result<Json<WriteRequest>, JsonRejection>,
) -> Result<Json<Value>, Refused> {
    let scope = writable_scope(&state, &scope).await?;
    let doc = owned_document(&caller, &scope, &name)?;
    let Json(request) = request.map_err(bad_body)?;
    let device = device.map(|Extension(device)| device);
    if device.as_ref().is_some_and(command_change_is_gated) {
        let current = store::global()
            .snapshot()
            .entry(&scope, doc.name)
            .map_or(Value::Null, |entry| entry.body());
        refuse_command_change(doc.name, &current, &request.body)?;
    }
    let version = store::put(
        &state.pool,
        &scope,
        doc.name,
        &request.body,
        &written_by(&caller, device.as_ref()),
        Some(request.version),
    )
    .await
    .map_err(refused)?;
    reload_channels_after(&state, &scope, doc.name);
    void_stale_after(&state, &scope, doc.name).await;
    Ok(Json(json!({ "version": version })))
}

/// A published `devtime` document changes the thresholds the pending shadow rows were decided under:
/// those rows are voided at once, from the snapshot the store has already swapped (the ingest loop reads
/// the same `actions` from the next cycle). Rows the owner already judged are left alone. A failure is
/// logged and never fails the write, which is already committed; the startup sweep catches up on the
/// next start.
async fn void_stale_after(state: &AppState, scope: &Scope, name: &str) {
    if !matches!(scope, Scope::Machine) || name != "devtime" {
        return;
    }
    let config =
        store::get_opt::<crate::config::DevtimeConfig>(&Scope::Machine).unwrap_or_default();
    match crate::devtime::store::void_stale_thresholds(&state.pool, &config.actions).await {
        Ok(voided) if voided > 0 => {
            tracing::info!(
                voided,
                "voided the pending devtime actions after a thresholds change"
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "voiding the pending devtime actions after a thresholds change failed");
        }
    }
}

/// A published `channels/<id>` document applies at once: the hub re-reads its configuration from the snapshot,
/// which the store has already swapped by the time a write returns. `reload` cannot fail (a poisoned lock
/// keeps the old map and the next publish retries), so the write's success is never in question here. Its root
/// argument is ignored since the configuration lives in the settings store.
fn reload_channels_after(state: &AppState, scope: &Scope, name: &str) {
    if matches!(scope, Scope::Machine) && name.starts_with("channels/") {
        let root = state
            .machine_config_root
            .as_deref()
            .unwrap_or(std::path::Path::new(""));
        state.channels.reload(root);
    }
}

/// `POST /settings/{scope}/{name}/rollback/{version}`: the body of an earlier version, written as a new one.
///
/// The current version the client saw goes to the store, which checks it under its write lock: the 409 is the
/// store's `Stale`. A device is refused first when the target version carries another command than the stored one.
async fn rollback_document(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    device: Option<Extension<RemoteDevice>>,
    Path((scope, name, to_version)): Path<(String, String, i64)>,
    request: Result<Json<RollbackRequest>, JsonRejection>,
) -> Result<Json<Value>, Refused> {
    let scope = writable_scope(&state, &scope).await?;
    let doc = owned_document(&caller, &scope, &name)?;
    let Json(request) = request.map_err(bad_body)?;
    let device = device.map(|Extension(device)| device);
    if device.as_ref().is_some_and(command_change_is_gated) {
        let rows = store::history(&state.pool, &scope, doc.name)
            .await
            .map_err(refused)?;
        let target = rows
            .iter()
            .find(|row| row.version == to_version)
            .ok_or_else(|| refused(SettingsError::NotFound))?;
        let target: Value = serde_json::from_str(&target.body_json).unwrap_or(Value::Null);
        let current = store::global()
            .snapshot()
            .entry(&scope, doc.name)
            .map_or(Value::Null, |entry| entry.body());
        refuse_command_change(doc.name, &current, &target)?;
    }
    let version = store::rollback(
        &state.pool,
        &scope,
        doc.name,
        to_version,
        &written_by(&caller, device.as_ref()),
        Some(request.version),
    )
    .await
    .map_err(refused)?;
    reload_channels_after(&state, &scope, doc.name);
    void_stale_after(&state, &scope, doc.name).await;
    Ok(Json(json!({ "version": version })))
}

#[cfg(test)]
mod devtime_void_tests;

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::{Value, json};

    use super::super::tests::{test_state, workflow_call};
    use crate::config::{AutopilotRules, VerifyConfig};
    use crate::governance::autopilot::{set_kill_switch, set_scoped_kill};
    use crate::project::rules::testing::Home;
    use crate::settings::{self as store, Scope};
    use crate::state::AppState;

    /// An empty snapshot owned by this test (these tests publish, so the shared hold of `test_state` is not
    /// enough) and two projects on the roster.
    async fn fixture() -> (Home, AppState) {
        let home = Home::new();
        let state = test_state().await;
        for project in ["p1", "p2"] {
            sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, 'active')")
                .bind(project)
                .execute(&state.pool)
                .await
                .unwrap();
        }
        (home, state)
    }

    async fn call(
        state: &AppState,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        workflow_call(state.clone(), method, uri, body).await
    }

    /// `PUT /settings/{scope}/{name}` with `{body, version}`.
    async fn put_doc(
        state: &AppState,
        scope: &str,
        name: &str,
        body: Value,
        version: i64,
    ) -> (StatusCode, Value) {
        let uri = format!("/settings/{scope}/{name}");
        let request = json!({ "body": body, "version": version });
        call(state, "PUT", &uri, Some(request)).await
    }

    /// Write `verify` past the HTTP door, as version `expected + 1`.
    async fn store_verify(state: &AppState, capacity: u32, expected: i64) {
        store::put(
            &state.pool,
            &Scope::Machine,
            "verify",
            &json!({ "capacity": capacity }),
            "test",
            Some(expected),
        )
        .await
        .expect("seed the verify document");
    }

    /// A row the struct cannot decode, loaded into the snapshot as unreadable.
    async fn store_unreadable_verify(state: &AppState, version: i64) {
        sqlx::query(
            "INSERT INTO settings (scope, name, body_json, version, updated_at, updated_by) \
             VALUES ('machine', 'verify', '{\"capacity\":\"many\"}', ?, '2026-10-11T00:00:00Z', 'test')",
        )
        .bind(version)
        .execute(&state.pool)
        .await
        .unwrap();
        store::load(&state.pool).await.unwrap();
    }

    async fn verify_history(state: &AppState) -> Vec<store::HistoryRow> {
        store::history(&state.pool, &Scope::Machine, "verify")
            .await
            .unwrap()
    }

    fn verify_capacity() -> u32 {
        store::get::<VerifyConfig>(&Scope::Machine).capacity
    }

    fn autopilot_body() -> Value {
        serde_json::to_value(AutopilotRules::default()).unwrap()
    }

    /// A hub that already holds the `channels/telegram` document as it stands in the snapshot, the way
    /// `main.rs` builds it at startup. The reads of a running turn go through `state.channels.configs`.
    fn seed_channel_hub(state: &mut AppState) {
        state.channels = std::sync::Arc::new(crate::channels::ChannelsRuntime::load(Some(
            std::path::Path::new("."),
        )));
    }

    fn row<'a>(rows: &'a Value, name: &str) -> &'a Value {
        rows.as_array()
            .expect("an array of documents")
            .iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("`{name}` is not listed in {rows}"))
    }

    /// Task 5.3: the reads.
    mod get {
        use super::*;

        #[tokio::test]
        async fn index_lists_the_machine_documents_by_default() {
            let (_home, state) = fixture().await;
            store_verify(&state, 9, 0).await;

            let (status, rows) = call(&state, "GET", "/settings", None).await;
            assert_eq!(status, StatusCode::OK, "{rows}");

            let machine: Vec<&str> = store::documents()
                .iter()
                .filter(|doc| doc.kind == store::Kind::Machine)
                .map(|doc| doc.name)
                .collect();
            let listed: Vec<&str> = rows
                .as_array()
                .expect("an array of documents")
                .iter()
                .map(|row| row["name"].as_str().expect("a name"))
                .collect();
            assert_eq!(listed, machine, "machine documents only, in registry order");

            let telegram = row(&rows, "channels/telegram");
            assert_eq!(telegram["scope"], "machine");
            assert_eq!(telegram["kind"], "machine");
            assert_eq!(telegram["present"], false, "never written");
            assert_eq!(telegram["version"], 0);
            assert!(
                telegram["takes_effect"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
                "{telegram}"
            );

            let verify = row(&rows, "verify");
            assert_eq!(verify["present"], true);
            assert_eq!(verify["version"], 1);
        }

        #[tokio::test]
        async fn index_with_a_project_scope_lists_that_projects_documents() {
            let (_home, state) = fixture().await;
            sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('7', 'active')")
                .execute(&state.pool)
                .await
                .unwrap();

            let (status, rows) = call(&state, "GET", "/settings?scope=project:7", None).await;
            assert_eq!(status, StatusCode::OK, "{rows}");
            assert_eq!(rows.as_array().map(Vec::len), Some(2), "{rows}");
            for name in ["autopilot", "workflow_pins"] {
                let document = row(&rows, name);
                assert_eq!(document["scope"], "project:7");
                assert_eq!(document["kind"], "project");
                assert_eq!(document["present"], false);
                assert_eq!(document["version"], 0);
            }
        }

        #[tokio::test]
        async fn a_document_returns_body_version_and_schema() {
            let (_home, state) = fixture().await;
            store_verify(&state, 9, 0).await;

            let (status, answer) = call(&state, "GET", "/settings/machine/verify", None).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["body"]["capacity"], 9);
            assert_eq!(answer["version"], 1);
            assert_eq!(
                answer["schema"],
                store::doc_type("verify").unwrap().schema(),
                "the schema of the registry row"
            );
        }

        #[tokio::test]
        async fn a_missing_document_returns_the_default_body_and_version_0() {
            let (_home, state) = fixture().await;

            let (status, answer) = call(&state, "GET", "/settings/machine/verify", None).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["version"], 0);
            assert_eq!(
                answer["body"]["capacity"],
                VerifyConfig::default().capacity,
                "the shipped default"
            );
            assert!(answer["schema"].is_object(), "{answer}");

            // `council` has no default: an empty roster is a mistake, not a configuration.
            let (status, answer) = call(&state, "GET", "/settings/machine/council", None).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["version"], 0);
            assert_eq!(answer["body"], Value::Null);
            assert!(answer["schema"].is_object(), "{answer}");
        }

        /// Not a 200 with the defaults: the client has to see that what it would edit is not what is stored.
        #[tokio::test]
        async fn an_unreadable_document_is_422_unreadable() {
            let (_home, state) = fixture().await;
            store_unreadable_verify(&state, 3).await;

            let (status, answer) = call(&state, "GET", "/settings/machine/verify", None).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
            assert_eq!(answer["refusal"], "unreadable");
        }

        #[tokio::test]
        async fn an_unknown_name_is_404_unknown_document() {
            let (_home, state) = fixture().await;

            let (status, answer) = call(&state, "GET", "/settings/machine/nonsense", None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
            assert_eq!(answer["refusal"], "unknown_document");
        }

        #[tokio::test]
        async fn a_wrong_scope_is_404_wrong_scope() {
            let (_home, state) = fixture().await;

            for uri in ["/settings/project:p1/verify", "/settings/machine/autopilot"] {
                let (status, answer) = call(&state, "GET", uri, None).await;
                assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {answer}");
                assert_eq!(answer["refusal"], "wrong_scope", "{uri}");
            }
        }

        #[tokio::test]
        async fn a_project_not_on_the_roster_is_404_no_such_project() {
            let (_home, state) = fixture().await;

            for uri in [
                "/settings?scope=project:ghost",
                "/settings/project:ghost/autopilot",
            ] {
                let (status, answer) = call(&state, "GET", uri, None).await;
                assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {answer}");
                assert_eq!(answer["refusal"], "no_such_project", "{uri}");
            }
        }

        #[tokio::test]
        async fn history_is_newest_first_and_flags_deletions() {
            let (_home, state) = fixture().await;
            store_verify(&state, 5, 0).await;
            store_verify(&state, 6, 1).await;
            store::delete(&state.pool, &Scope::Machine, "verify", "test", Some(2))
                .await
                .unwrap();

            let (status, rows) =
                call(&state, "GET", "/settings/machine/verify/history", None).await;
            assert_eq!(status, StatusCode::OK, "{rows}");
            let rows = rows.as_array().expect("an array of versions");
            let shape: Vec<(i64, bool)> = rows
                .iter()
                .map(|row| {
                    (
                        row["version"].as_i64().expect("a version"),
                        row["deleted"].as_bool().expect("a flag"),
                    )
                })
                .collect();
            assert_eq!(shape, vec![(3, true), (2, false), (1, false)]);
            assert_eq!(rows[0]["written_by"], "test");
            assert!(rows[0]["written_at"].is_string(), "{}", rows[0]);
        }

        /// The shell encodes scope and name with `encodeURIComponent`, so the channel family arrives as one segment.
        ///
        /// The plain `/settings/{scope}/{name}` route with `Path<(String, String)>` is the form that works: axum
        /// matches the raw path, where `%2F` is not a separator, and `Path` decodes the segment afterwards. The
        /// fallback the plan allowed, a second `/settings/{scope}/channels/{id}` route, was not needed.
        #[tokio::test]
        async fn an_encoded_slash_in_the_name_reaches_the_handler() {
            let (_home, state) = fixture().await;

            let uri = "/settings/machine/channels%2Ftelegram";
            let (status, answer) = call(&state, "GET", uri, None).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["version"], 0);
            assert!(answer["schema"].is_object(), "{answer}");

            let (status, rows) = call(&state, "GET", &format!("{uri}/history"), None).await;
            assert_eq!(status, StatusCode::OK, "{rows}");
            assert_eq!(rows, json!([]));
        }

        #[tokio::test]
        async fn an_encoded_colon_in_the_scope_reaches_the_handler() {
            let (_home, state) = fixture().await;

            let uri = "/settings/project%3Ap1/autopilot";
            let (status, answer) = call(&state, "GET", uri, None).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["version"], 0);
        }

        #[tokio::test]
        async fn a_malformed_scope_is_400_bad_scope() {
            let (_home, state) = fixture().await;

            for uri in [
                "/settings?scope=nonsense",
                "/settings/garbage/verify",
                "/settings/garbage/verify/history",
            ] {
                let (status, answer) = call(&state, "GET", uri, None).await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {answer}");
                assert_eq!(answer["refusal"], "bad_scope", "{uri}");
            }
        }

        /// The index says what is stored: an unreadable row is present, at its real version, and flagged.
        #[tokio::test]
        async fn an_unreadable_document_is_flagged_in_the_index_at_its_real_version() {
            let (_home, state) = fixture().await;
            store_unreadable_verify(&state, 5).await;

            let (status, rows) = call(&state, "GET", "/settings", None).await;
            assert_eq!(status, StatusCode::OK, "{rows}");
            let verify = row(&rows, "verify");
            assert_eq!(verify["present"], true);
            assert_eq!(verify["version"], 5);
            assert_eq!(verify["unreadable"], true);

            let email = row(&rows, "email");
            assert_eq!(email["present"], false);
            assert_eq!(email["unreadable"], false);
        }

        /// The client can only replace the row if the refusal tells it which version to send.
        #[tokio::test]
        async fn an_unreadable_refusal_carries_the_version_to_replace() {
            let (_home, state) = fixture().await;
            store_unreadable_verify(&state, 3).await;

            let (status, answer) = call(&state, "GET", "/settings/machine/verify", None).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
            assert_eq!(answer["version"], 3);

            let body = json!({ "capacity": 9 });
            let (status, answer) = put_doc(&state, "machine", "verify", body, 3).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
        }
    }

    /// Task 5.4: the optimistic write, refused in the order of spec section 5 (423, 403, 422, 409).
    mod put {
        use super::*;

        /// The registry promises the hub re-reads a channel document when it is published.
        #[tokio::test]
        async fn put_of_a_channel_document_reloads_the_channel_config() {
            let (_home, mut state) = fixture().await;
            seed_channel_hub(&mut state);
            assert!(
                state.channels.config("telegram").unwrap().stream_edits(),
                "the default is on"
            );

            let (status, answer) = put_doc(
                &state,
                "machine",
                "channels%2Ftelegram",
                json!({ "stream_edits": false }),
                0,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{answer}");

            let config = state
                .channels
                .config("telegram")
                .expect("telegram is loaded");
            assert!(
                !config.stream_edits(),
                "the hub still serves the old configuration: {config:?}"
            );
        }

        #[tokio::test]
        async fn put_during_the_kill_switch_is_423() {
            let (_home, state) = fixture().await;

            set_kill_switch(&state.pool, true).await.unwrap();
            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capacity": 9 }), 0).await;
            assert_eq!(status, StatusCode::LOCKED, "{answer}");
            assert_eq!(answer["refusal"], "kill_switch");
            let (status, answer) =
                put_doc(&state, "project:p1", "autopilot", autopilot_body(), 0).await;
            assert_eq!(status, StatusCode::LOCKED, "{answer}");
            assert!(verify_history(&state).await.is_empty(), "nothing written");
            set_kill_switch(&state.pool, false).await.unwrap();

            // The stop of one project holds that project's documents and nothing else.
            set_scoped_kill(&state.pool, "project", "p1", true)
                .await
                .unwrap();
            let (status, answer) =
                put_doc(&state, "project:p1", "autopilot", autopilot_body(), 0).await;
            assert_eq!(status, StatusCode::LOCKED, "{answer}");
            assert_eq!(answer["refusal"], "kill_switch");
            let (status, answer) =
                put_doc(&state, "project:p2", "autopilot", autopilot_body(), 0).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capacity": 9 }), 0).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
        }

        /// The stop cannot be read, so it counts as engaged.
        #[tokio::test]
        async fn put_with_an_unreadable_kill_switch_is_423() {
            let (_home, state) = fixture().await;
            sqlx::query("DROP TABLE autopilot_global")
                .execute(&state.pool)
                .await
                .unwrap();

            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capacity": 9 }), 0).await;
            assert_eq!(status, StatusCode::LOCKED, "{answer}");
            assert_eq!(answer["refusal"], "kill_switch");
        }

        #[tokio::test]
        async fn put_to_a_document_the_registry_does_not_own_is_403_not_ours() {
            let (_home, state) = fixture().await;

            for (scope, name, body) in [
                ("machine", "nonsense", json!({})),
                ("project:p1", "verify", json!({ "capacity": 9 })),
                ("machine", "autopilot", autopilot_body()),
            ] {
                let (status, answer) = put_doc(&state, scope, name, body, 0).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{scope}/{name}: {answer}");
                assert_eq!(answer["refusal"], "not_ours", "{scope}/{name}");
            }
            assert!(verify_history(&state).await.is_empty(), "nothing written");
        }

        #[tokio::test]
        async fn put_of_an_invalid_body_is_422_invalid_with_detail() {
            let (_home, state) = fixture().await;

            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capcity": 9 }), 0).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
            assert_eq!(answer["refusal"], "invalid");
            assert!(
                answer["detail"]
                    .as_str()
                    .is_some_and(|detail| detail.contains("capcity")),
                "the detail points at the field: {answer}"
            );
            assert!(verify_history(&state).await.is_empty(), "nothing written");
        }

        /// A refused write leaves the document standing: same value, same version, and no line in
        /// the history or the feed.
        #[tokio::test]
        async fn a_422_over_an_existing_document_leaves_it_intact() {
            async fn config_written_lines(state: &AppState) -> usize {
                crate::feed::list_feed(&state.pool, None, 10)
                    .await
                    .unwrap()
                    .iter()
                    .filter(|line| line.kind == "config_written")
                    .count()
            }

            let (_home, state) = fixture().await;
            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capacity": 5 }), 0).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(config_written_lines(&state).await, 1);

            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capcity": 9 }), 1).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
            assert_eq!(answer["refusal"], "invalid");

            let (status, doc) = call(&state, "GET", "/settings/machine/verify", None).await;
            assert_eq!(status, StatusCode::OK, "{doc}");
            assert_eq!(doc["version"], 1);
            assert_eq!(doc["body"]["capacity"], 5);
            assert_eq!(verify_capacity(), 5);
            assert_eq!(verify_history(&state).await.len(), 1);
            assert_eq!(config_written_lines(&state).await, 1);
        }

        /// A project on the roster with no folder still takes its documents: they live in the
        /// database, and the folder is no part of writing one.
        #[tokio::test]
        async fn put_to_a_project_with_no_folder_is_accepted_and_reads_back() {
            let (_home, state) = fixture().await;
            let mut body = autopilot_body();
            body["gate_command"] = json!("cargo test");

            let (status, answer) = put_doc(&state, "project:p1", "autopilot", body, 0).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer, json!({ "version": 1 }));

            let (status, doc) = call(&state, "GET", "/settings/project:p1/autopilot", None).await;
            assert_eq!(status, StatusCode::OK, "{doc}");
            assert_eq!(doc["version"], 1);
            assert_eq!(doc["body"]["gate_command"], "cargo test");
        }

        /// A request that does not say which version it read must not fall through to a blind write.
        #[tokio::test]
        async fn put_without_a_version_is_refused() {
            let (_home, state) = fixture().await;

            let request = json!({ "body": { "capacity": 9 } });
            let (status, _) = call(&state, "PUT", "/settings/machine/verify", Some(request)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert!(verify_history(&state).await.is_empty(), "nothing written");
        }

        #[tokio::test]
        async fn put_with_a_stale_version_is_409_stale_with_current() {
            let (_home, state) = fixture().await;
            store_verify(&state, 5, 0).await;

            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capacity": 9 }), 7).await;
            assert_eq!(status, StatusCode::CONFLICT, "{answer}");
            assert_eq!(answer["refusal"], "stale");
            assert_eq!(answer["current"], 1);
            assert_eq!(verify_capacity(), 5, "the snapshot did not move");
            assert_eq!(verify_history(&state).await.len(), 1, "nothing written");
        }

        #[tokio::test]
        async fn put_with_version_0_creates_and_a_second_one_is_409() {
            let (_home, state) = fixture().await;

            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capacity": 5 }), 0).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["version"], 1);

            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capacity": 6 }), 0).await;
            assert_eq!(status, StatusCode::CONFLICT, "{answer}");
            assert_eq!(answer["refusal"], "stale");
            assert_eq!(answer["current"], 1);
            assert_eq!(verify_capacity(), 5);
        }

        #[tokio::test]
        async fn put_returns_200_with_the_new_version_and_writes_history_and_a_config_written_line()
        {
            let (_home, state) = fixture().await;

            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capacity": 5 }), 0).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer, json!({ "version": 1 }));
            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capacity": 9 }), 1).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer, json!({ "version": 2 }));
            assert_eq!(verify_capacity(), 9, "visible straight after the commit");

            let history = verify_history(&state).await;
            assert_eq!(history.len(), 2);
            assert!(
                history[0].written_by.starts_with("http:"),
                "{}",
                history[0].written_by
            );

            let feed = crate::feed::list_feed(&state.pool, None, 10).await.unwrap();
            let written: Vec<_> = feed
                .iter()
                .filter(|line| line.kind == "config_written")
                .collect();
            assert_eq!(written.len(), 2, "one line per write, not two");
            assert!(
                written
                    .iter()
                    .all(|line| !line.summary.contains("capacity")),
                "the feed never carries a body"
            );
        }

        /// Spec section 11: the client's version predates a write by the daemon itself.
        #[tokio::test]
        async fn a_put_after_an_internal_write_with_the_old_version_is_409() {
            let (_home, state) = fixture().await;

            let uri = "/settings/project:p1/workflow_pins";
            let (status, read) = call(&state, "GET", uri, None).await;
            assert_eq!(status, StatusCode::OK, "{read}");
            let old_version = read["version"].as_i64().expect("a version");
            assert_eq!(old_version, 0);

            let bundle = crate::workflows::Bundle {
                name: "alpha-flow".to_string(),
                version: "1".to_string(),
                description: None,
                origin: "library:alpha-flow".to_string(),
                owns: Vec::new(),
                agents_block: None,
                hash: "hash-1".to_string(),
                path: "unused".to_string(),
            };
            assert!(
                crate::workflows::install(&state.pool, "p1", &bundle)
                    .await
                    .is_ok(),
                "the internal write"
            );

            let (status, answer) = put_doc(
                &state,
                "project:p1",
                "workflow_pins",
                json!({ "workflows": [] }),
                old_version,
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{answer}");
            assert_eq!(answer["refusal"], "stale");
            assert_eq!(answer["current"], 1);

            let (_, read) = call(&state, "GET", uri, None).await;
            assert_eq!(read["version"], 1);
            assert_eq!(read["body"]["workflows"][0]["name"], "alpha-flow");
        }

        #[tokio::test]
        async fn put_cannot_write_an_unreadable_document_without_a_matching_version() {
            let (_home, state) = fixture().await;
            store_unreadable_verify(&state, 5).await;
            let body = json!({ "capacity": 9 });

            for wrong in [0, 4] {
                let (status, answer) =
                    put_doc(&state, "machine", "verify", body.clone(), wrong).await;
                assert_eq!(status, StatusCode::CONFLICT, "version {wrong}: {answer}");
                assert_eq!(answer["refusal"], "stale");
                assert_eq!(answer["current"], 5);
            }

            let (status, answer) = put_doc(&state, "machine", "verify", body, 5).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["version"], 6);

            let (status, read) = call(&state, "GET", "/settings/machine/verify", None).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "replaced, so readable again: {read}"
            );
            assert_eq!(read["body"]["capacity"], 9);
        }

        /// Each refusal wins over the ones after it in spec section 5.
        #[tokio::test]
        async fn put_refuses_in_the_order_of_the_spec() {
            let (_home, state) = fixture().await;
            store_verify(&state, 5, 0).await;

            // 422 before 409: an invalid body with a stale version.
            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capcity": 9 }), 7).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
            assert_eq!(answer["refusal"], "invalid");

            // 403 before 422: a document the registry does not own, whatever the body.
            let (status, answer) =
                put_doc(&state, "machine", "nonsense", json!({ "capcity": 9 }), 7).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
            assert_eq!(answer["refusal"], "not_ours");

            // 423 before everything.
            set_kill_switch(&state.pool, true).await.unwrap();
            let (status, answer) =
                put_doc(&state, "machine", "nonsense", json!({ "capcity": 9 }), 7).await;
            assert_eq!(status, StatusCode::LOCKED, "{answer}");
            assert_eq!(answer["refusal"], "kill_switch");
        }

        /// Not even a request that cannot be parsed gets a different answer while the stop is on.
        #[tokio::test]
        async fn the_kill_switch_wins_over_a_malformed_request() {
            let (_home, state) = fixture().await;
            let no_version = json!({ "body": {} });

            set_kill_switch(&state.pool, true).await.unwrap();
            let (status, answer) = call(
                &state,
                "PUT",
                "/settings/garbage/verify",
                Some(no_version.clone()),
            )
            .await;
            assert_eq!(status, StatusCode::LOCKED, "{answer}");
            set_kill_switch(&state.pool, false).await.unwrap();

            set_scoped_kill(&state.pool, "project", "p1", true)
                .await
                .unwrap();
            let (status, answer) = call(
                &state,
                "PUT",
                "/settings/project:p1/autopilot",
                Some(no_version),
            )
            .await;
            assert_eq!(status, StatusCode::LOCKED, "{answer}");
        }

        #[tokio::test]
        async fn put_to_a_project_off_the_roster_is_404_no_such_project() {
            let (_home, state) = fixture().await;

            for (name, body) in [
                ("autopilot", autopilot_body()),
                ("verify", json!({ "capacity": 9 })),
            ] {
                let (status, answer) = put_doc(&state, "project:ghost", name, body, 0).await;
                assert_eq!(status, StatusCode::NOT_FOUND, "{name}: {answer}");
                assert_eq!(answer["refusal"], "no_such_project", "{name}");
            }
            let written = store::history(&state.pool, &Scope::project("ghost"), "autopilot")
                .await
                .unwrap();
            assert!(written.is_empty(), "nothing written");
        }

        #[tokio::test]
        async fn put_to_a_malformed_scope_is_400_bad_scope() {
            let (_home, state) = fixture().await;

            let (status, answer) =
                put_doc(&state, "garbage", "verify", json!({ "capacity": 9 }), 0).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
            assert_eq!(answer["refusal"], "bad_scope");
        }

        /// Neither token kind carries an id, so the history names the kind.
        #[tokio::test]
        async fn a_write_is_signed_with_the_kind_of_token() {
            let (_home, state) = fixture().await;

            let (status, answer) =
                put_doc(&state, "machine", "verify", json!({ "capacity": 5 }), 0).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(verify_history(&state).await[0].written_by, "http:control");

            let admin = crate::auth::Scope::ApiToken(crate::auth::ApiTokenLevel::Admin);
            assert_eq!(super::super::written_by(&admin, None), "http:api-token");
        }
    }

    /// Task 5.5: rollback, and no deletion over HTTP.
    mod rollback {
        use super::*;

        /// Versions 1 (`capacity` 5) and 2 (`capacity` 6).
        async fn two_versions(state: &AppState) {
            store_verify(state, 5, 0).await;
            store_verify(state, 6, 1).await;
        }

        #[tokio::test]
        async fn rollback_restores_an_old_body_as_a_new_version() {
            let (_home, state) = fixture().await;
            two_versions(&state).await;

            let (status, answer) = call(
                &state,
                "POST",
                "/settings/machine/verify/rollback/1",
                Some(json!({ "version": 2 })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["version"], 3);

            let (status, read) = call(&state, "GET", "/settings/machine/verify", None).await;
            assert_eq!(status, StatusCode::OK, "{read}");
            assert_eq!(read["body"]["capacity"], 5);
            assert_eq!(read["version"], 3);
            assert_eq!(verify_capacity(), 5);
            assert_eq!(
                verify_history(&state).await.len(),
                3,
                "it is in the history too"
            );
        }

        #[tokio::test]
        async fn rollback_of_a_channel_document_reloads_the_channel_config() {
            let (_home, mut state) = fixture().await;
            for (stream_edits, expected) in [(false, 0), (true, 1)] {
                store::put(
                    &state.pool,
                    &Scope::Machine,
                    "channels/telegram",
                    &json!({ "stream_edits": stream_edits }),
                    "test",
                    Some(expected),
                )
                .await
                .expect("seed the channel document");
            }
            seed_channel_hub(&mut state);
            assert!(state.channels.config("telegram").unwrap().stream_edits());

            let (status, answer) = call(
                &state,
                "POST",
                "/settings/machine/channels%2Ftelegram/rollback/1",
                Some(json!({ "version": 2 })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{answer}");

            let config = state
                .channels
                .config("telegram")
                .expect("telegram is loaded");
            assert!(
                !config.stream_edits(),
                "the hub still serves the old configuration: {config:?}"
            );
        }

        #[tokio::test]
        async fn rollback_needs_the_current_version_in_the_body() {
            let (_home, state) = fixture().await;
            two_versions(&state).await;
            let uri = "/settings/machine/verify/rollback/1";

            let (status, answer) = call(&state, "POST", uri, Some(json!({ "version": 1 }))).await;
            assert_eq!(status, StatusCode::CONFLICT, "{answer}");
            assert_eq!(answer["refusal"], "stale");
            assert_eq!(answer["current"], 2);

            let (status, _) = call(&state, "POST", uri, Some(json!({}))).await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "the version is not optional"
            );

            assert_eq!(verify_capacity(), 6, "still the current body");
            assert_eq!(verify_history(&state).await.len(), 2, "nothing written");
        }

        #[tokio::test]
        async fn rollback_to_a_missing_version_is_404() {
            let (_home, state) = fixture().await;
            two_versions(&state).await;

            let (status, answer) = call(
                &state,
                "POST",
                "/settings/machine/verify/rollback/99",
                Some(json!({ "version": 2 })),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
            // The handler's refusal, not the router's empty 404 for a route that does not exist.
            assert!(answer["refusal"].is_string(), "{answer}");
            assert_eq!(verify_history(&state).await.len(), 2, "nothing written");
        }

        #[tokio::test]
        async fn rollback_during_the_kill_switch_is_423() {
            let (_home, state) = fixture().await;
            two_versions(&state).await;
            let request = json!({ "version": 2 });

            set_kill_switch(&state.pool, true).await.unwrap();
            let (status, answer) = call(
                &state,
                "POST",
                "/settings/machine/verify/rollback/1",
                Some(request.clone()),
            )
            .await;
            assert_eq!(status, StatusCode::LOCKED, "{answer}");
            assert_eq!(answer["refusal"], "kill_switch");
            assert_eq!(verify_history(&state).await.len(), 2, "nothing written");
            set_kill_switch(&state.pool, false).await.unwrap();

            // The stop of a project holds a rollback of that project's documents.
            store::put(
                &state.pool,
                &Scope::project("p1"),
                "autopilot",
                &autopilot_body(),
                "test",
                Some(0),
            )
            .await
            .unwrap();
            set_scoped_kill(&state.pool, "project", "p1", true)
                .await
                .unwrap();
            let (status, answer) = call(
                &state,
                "POST",
                "/settings/project:p1/autopilot/rollback/1",
                Some(json!({ "version": 1 })),
            )
            .await;
            assert_eq!(status, StatusCode::LOCKED, "{answer}");
            assert_eq!(answer["refusal"], "kill_switch");
        }

        /// Deleting a document is for the recovery CLI alone.
        #[tokio::test]
        async fn delete_is_not_exposed_over_http() {
            let (_home, state) = fixture().await;
            store_verify(&state, 5, 0).await;

            let (status, _) = call(&state, "DELETE", "/settings/machine/verify", None).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(verify_capacity(), 5, "still there");
            assert_eq!(verify_history(&state).await.len(), 1, "nothing written");
        }

        #[tokio::test]
        async fn rollback_to_a_missing_version_is_named_no_such_version() {
            let (_home, state) = fixture().await;
            two_versions(&state).await;

            let (status, answer) = call(
                &state,
                "POST",
                "/settings/machine/verify/rollback/99",
                Some(json!({ "version": 2 })),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
            assert_eq!(answer["refusal"], "no_such_version");
        }

        #[tokio::test]
        async fn rollback_in_a_project_off_the_roster_is_404_no_such_project() {
            let (_home, state) = fixture().await;

            let (status, answer) = call(
                &state,
                "POST",
                "/settings/project:ghost/autopilot/rollback/1",
                Some(json!({ "version": 1 })),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
            assert_eq!(answer["refusal"], "no_such_project");
        }

        #[tokio::test]
        async fn rollback_in_a_malformed_scope_is_400_bad_scope() {
            let (_home, state) = fixture().await;

            let (status, answer) = call(
                &state,
                "POST",
                "/settings/garbage/verify/rollback/1",
                Some(json!({ "version": 1 })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
            assert_eq!(answer["refusal"], "bad_scope");
        }

        /// The same map as a `PUT`: an unknown name and a wrong scope both answer `not_ours`.
        #[tokio::test]
        async fn rollback_of_a_document_the_registry_does_not_own_is_403_not_ours() {
            let (_home, state) = fixture().await;

            for uri in [
                "/settings/machine/nonsense/rollback/1",
                "/settings/machine/autopilot/rollback/1",
            ] {
                let (status, answer) =
                    call(&state, "POST", uri, Some(json!({ "version": 0 }))).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {answer}");
                assert_eq!(answer["refusal"], "not_ours", "{uri}");
            }
        }
    }

    /// A paired remote device is the owner on another machine, but it never changes a command the host will
    /// run: the request carries the `RemoteDevice` extension that `auth.rs` inserts after the token check.
    mod remote {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        use super::*;
        use crate::auth::{ApiTokenLevel, RemoteDevice, Scope as Caller};

        const DEVICE: i64 = 7;

        /// The settings routes with the extensions the auth layer would have put on a device's request.
        async fn remote_call(
            state: &AppState,
            method: &str,
            uri: &str,
            body: Value,
        ) -> (StatusCode, Value) {
            let device = RemoteDevice {
                id: DEVICE,
                owner: false,
                stepped_up: false,
            };
            remote_call_as(state, device, method, uri, body).await
        }

        async fn remote_call_as(
            state: &AppState,
            device: RemoteDevice,
            method: &str,
            uri: &str,
            body: Value,
        ) -> (StatusCode, Value) {
            let app = super::super::routes()
                .with_state(state.clone())
                .layer(axum::Extension(Caller::ApiToken(ApiTokenLevel::Admin)))
                .layer(axum::Extension(device));
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }

        async fn remote_put(
            state: &AppState,
            scope: &str,
            name: &str,
            body: Value,
            version: i64,
        ) -> (StatusCode, Value) {
            let uri = format!("/settings/{scope}/{name}");
            let request = json!({ "body": body, "version": version });
            remote_call(state, "PUT", &uri, request).await
        }

        async fn remote_rollback(
            state: &AppState,
            scope: &str,
            name: &str,
            to_version: i64,
            current: i64,
        ) -> (StatusCode, Value) {
            let uri = format!("/settings/{scope}/{name}/rollback/{to_version}");
            remote_call(state, "POST", &uri, json!({ "version": current })).await
        }

        #[tokio::test]
        async fn a_device_cannot_change_the_role_through_the_settings_door() {
            let (_home, state) = fixture().await;

            let (status, answer) = remote_put(
                &state,
                "machine",
                "machine",
                json!({ "role": "secondary" }),
                0,
            )
            .await;

            assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
            assert_eq!(answer["refusal"], "control_required");
            assert_eq!(
                store::get::<crate::config::MachineConfig>(&Scope::Machine).role,
                crate::config::MachineRole::Primary,
                "the refused write did not change the stored role"
            );
        }

        async fn seed(state: &AppState, scope: &str, name: &str, body: &Value, expected: i64) {
            store::put(
                &state.pool,
                &scope.parse::<Scope>().unwrap(),
                name,
                body,
                "test",
                Some(expected),
            )
            .await
            .expect("seed the document");
        }

        async fn versions(state: &AppState, scope: &str, name: &str) -> Vec<store::HistoryRow> {
            store::history(&state.pool, &scope.parse::<Scope>().unwrap(), name)
                .await
                .unwrap()
        }

        /// The document is stored as `old`; a device that sends `new` is refused with the named reason and the
        /// document stays at version 1; the same `new` from the local caller goes through.
        async fn remote_put_changing_a_command_is_refused(
            scope: &str,
            name: &str,
            old: Value,
            new: Value,
        ) {
            let (_home, state) = fixture().await;
            seed(&state, scope, name, &old, 0).await;

            let (status, answer) = remote_put(&state, scope, name, new.clone(), 1).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{scope}/{name}: {answer}");
            assert_eq!(answer["refusal"], "remote_command_field", "{scope}/{name}");
            let rows = versions(&state, scope, name).await;
            assert_eq!(rows.len(), 1, "{scope}/{name}: nothing written");
            assert_eq!(rows[0].written_by, "test");

            let (status, answer) = put_doc(&state, scope, name, new, 1).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "local caller, {scope}/{name}: {answer}"
            );
            assert_eq!(answer["version"], 2);
        }

        #[tokio::test]
        async fn a_remote_put_changing_the_verify_broker_prefix_is_403() {
            remote_put_changing_a_command_is_refused(
                "machine",
                "verify",
                json!({ "capacity": 2, "broker_prefix": ["python", "heavy.py"] }),
                json!({ "capacity": 2, "broker_prefix": ["python", "evil.py"] }),
            )
            .await;
        }

        /// Only an owner device with a step-up passes the command-field refusal; an owner without it, and
        /// a stepped-up device that is not an owner, keep it.
        #[tokio::test]
        async fn only_a_stepped_up_owner_may_change_a_command_field() {
            let old = json!({ "capacity": 2, "broker_prefix": ["python", "heavy.py"] });
            let new = json!({ "capacity": 2, "broker_prefix": ["python", "other.py"] });
            let device = |owner, stepped_up| RemoteDevice {
                id: DEVICE,
                owner,
                stepped_up,
            };
            let (_home, state) = fixture().await;
            seed(&state, "machine", "verify", &old, 0).await;
            let uri = "/settings/machine/verify";
            for (owner, stepped_up) in [(false, false), (false, true), (true, false)] {
                let request = json!({ "body": new, "version": 1 });
                let (status, answer) =
                    remote_call_as(&state, device(owner, stepped_up), "PUT", uri, request).await;
                assert_eq!(
                    status,
                    StatusCode::FORBIDDEN,
                    "{owner} {stepped_up}: {answer}"
                );
                assert_eq!(answer["refusal"], "remote_command_field");
            }
            let request = json!({ "body": new, "version": 1 });
            let (status, answer) =
                remote_call_as(&state, device(true, true), "PUT", uri, request).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["version"], 2);
        }

        #[tokio::test]
        async fn a_remote_put_adding_a_verify_broker_prefix_is_403() {
            remote_put_changing_a_command_is_refused(
                "machine",
                "verify",
                json!({ "capacity": 2 }),
                json!({ "capacity": 2, "broker_prefix": ["python", "evil.py"] }),
            )
            .await;
        }

        #[tokio::test]
        async fn a_remote_put_changing_the_voice_stt_command_is_403() {
            remote_put_changing_a_command_is_refused(
                "machine",
                "voice",
                json!({ "stt_command": "whisper-cli -m model.bin" }),
                json!({ "stt_command": "evil.exe" }),
            )
            .await;
        }

        #[tokio::test]
        async fn a_remote_put_changing_the_autopilot_gate_command_is_403() {
            let mut old = autopilot_body();
            old["gate_command"] = json!("cargo test");
            let mut new = old.clone();
            new["gate_command"] = json!("cargo test && evil");
            remote_put_changing_a_command_is_refused("project:p1", "autopilot", old, new).await;
        }

        #[tokio::test]
        async fn a_remote_put_changing_the_autopilot_postgate_idle_command_is_403() {
            let mut old = autopilot_body();
            old["postgate_idle_command"] = json!("python scripts/heavy.py idle");
            let mut new = old.clone();
            new["postgate_idle_command"] = json!("python evil.py");
            remote_put_changing_a_command_is_refused("project:p1", "autopilot", old, new).await;
        }

        #[tokio::test]
        async fn a_remote_put_changing_an_autopilot_schedule_command_is_403() {
            let mut old = autopilot_body();
            old["schedules"] = json!([
                { "name": "r1", "at": "2026-10-22T09:00:00Z", "command": "git --version" }
            ]);
            let mut new = old.clone();
            new["schedules"][0]["command"] = json!("git --help");
            remote_put_changing_a_command_is_refused("project:p1", "autopilot", old, new).await;
        }

        /// Changing a schedule's `cwd` moves where a relative schedule command runs, even when the new
        /// folder is still inside the project.
        #[tokio::test]
        async fn a_remote_put_changing_a_schedule_cwd_is_403() {
            let mut old = autopilot_body();
            old["schedules"] = json!([
                { "name": "r1", "at": "2026-10-22T09:00:00Z", "command": "git --version", "cwd": "tools" }
            ]);
            let mut new = old.clone();
            new["schedules"][0]["cwd"] = json!("scripts");
            remote_put_changing_a_command_is_refused("project:p1", "autopilot", old, new).await;
        }

        #[tokio::test]
        async fn a_remote_put_changing_a_workflow_pin_node_command_is_403() {
            let pins = |command: &str| {
                json!({ "workflows": [{
                    "name": "alpha",
                    "version": "1",
                    "origin": "library:alpha",
                    "hash": "h",
                    "nodes": { "build": { "command": command } },
                }] })
            };
            remote_put_changing_a_command_is_refused(
                "project:p1",
                "workflow_pins",
                pins("make"),
                pins("make evil"),
            )
            .await;
        }

        #[tokio::test]
        async fn a_remote_put_changing_a_workflow_pin_path_is_403() {
            let pins = |path: &str| {
                json!({ "workflows": [{
                    "name": "alpha",
                    "version": "1",
                    "origin": "library:alpha",
                    "hash": "h",
                    "path": path,
                }] })
            };
            remote_put_changing_a_command_is_refused(
                "project:p1",
                "workflow_pins",
                pins("bundles/alpha"),
                pins("bundles/evil"),
            )
            .await;
        }

        #[tokio::test]
        async fn a_remote_put_that_leaves_every_command_alone_is_200() {
            let (_home, state) = fixture().await;
            let body = |capacity: u32| json!({ "capacity": capacity, "broker_prefix": ["python", "heavy.py"] });
            seed(&state, "machine", "verify", &body(2), 0).await;

            let (status, answer) = remote_put(&state, "machine", "verify", body(9), 1).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["version"], 2);
            assert_eq!(verify_capacity(), 9);
        }

        #[tokio::test]
        async fn a_remote_rollback_to_a_version_with_another_command_is_403() {
            let (_home, state) = fixture().await;
            seed(
                &state,
                "machine",
                "verify",
                &json!({ "broker_prefix": ["python", "a.py"] }),
                0,
            )
            .await;
            seed(
                &state,
                "machine",
                "verify",
                &json!({ "broker_prefix": ["python", "b.py"] }),
                1,
            )
            .await;

            let (status, answer) = remote_rollback(&state, "machine", "verify", 1, 2).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
            assert_eq!(answer["refusal"], "remote_command_field");
            assert_eq!(versions(&state, "machine", "verify").await.len(), 2);
            assert_eq!(
                store::get::<VerifyConfig>(&Scope::Machine).broker_prefix,
                vec!["python", "b.py"],
                "the snapshot did not move"
            );
        }

        #[tokio::test]
        async fn a_remote_rollback_that_changes_no_command_is_200() {
            let (_home, state) = fixture().await;
            let body = |capacity: u32| json!({ "capacity": capacity, "broker_prefix": ["python", "heavy.py"] });
            seed(&state, "machine", "verify", &body(2), 0).await;
            seed(&state, "machine", "verify", &body(3), 1).await;

            let (status, answer) = remote_rollback(&state, "machine", "verify", 1, 2).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(answer["version"], 3);
            assert_eq!(verify_capacity(), 2);
        }

        #[tokio::test]
        async fn a_remote_write_is_signed_with_the_device_id() {
            let (_home, state) = fixture().await;
            let expected = format!("remote:{DEVICE}");

            let (status, answer) =
                remote_put(&state, "machine", "verify", json!({ "capacity": 5 }), 0).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(
                versions(&state, "machine", "verify").await[0].written_by,
                expected
            );

            let (status, answer) = remote_rollback(&state, "machine", "verify", 1, 1).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            let rows = versions(&state, "machine", "verify").await;
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].written_by, expected, "the rollback is signed too");
        }
    }
}
