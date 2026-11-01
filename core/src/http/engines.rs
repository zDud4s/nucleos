//! The acknowledged opt-in of the agy engine over HTTP (`/engines/agy`, plan Node 2 section 5).
//!
//! Like `/settings`, the route is in no scope table of `auth.rs`: only the control token and an Admin key reach it.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::common::refusal_with_detail;
use crate::auth::Scope as Caller;
use crate::door::refusal;
use crate::engines::agy::{self, AgyDoc, WARNING, WARNING_VERSION};
use crate::governance::autopilot;
use crate::settings::{self as store, Scope, SettingsError};
use crate::state::AppState;

type Refused = (StatusCode, Json<Value>);

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/engines/agy", get(read).put(write))
}

#[derive(Deserialize)]
struct WriteRequest {
    enabled: bool,
    /// The owner accepts [`WARNING`]. Needed to enable, ignored to disable.
    #[serde(default)]
    acknowledged: bool,
}

/// `GET /engines/agy`: the state, the warning to show and whether the binary resolves, on or off.
async fn read() -> Json<Value> {
    let doc = store::get::<AgyDoc>(&Scope::Machine);
    Json(json!({
        // The gate, not the raw field: a stored row is not run through `finish`, so `enabled: true`
        // without the current acknowledgement is not an open gate.
        "enabled": agy::enabled(),
        "acknowledged": doc.acknowledged_warning_version == Some(WARNING_VERSION),
        "warning": WARNING,
        "warning_version": WARNING_VERSION,
        "installed": agy::installed(),
    }))
}

/// `PUT /engines/agy`: the kill switch first, as for every write. Enabling stores the current warning version
/// and needs `acknowledged: true`; disabling needs nothing. A chat reads the switch at once; a
/// `primary_runner: agy` was built at startup and changes when the daemon restarts.
async fn write(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    request: Result<Json<WriteRequest>, JsonRejection>,
) -> Result<Json<Value>, Refused> {
    if autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }
    let Json(request) = request.map_err(|rejection| {
        refusal_with_detail(rejection.status(), "invalid", rejection.body_text())
    })?;
    if request.enabled && !request.acknowledged {
        return Err(refusal_with_detail(
            StatusCode::BAD_REQUEST,
            "warning_not_acknowledged",
            WARNING.to_owned(),
        ));
    }
    let by = match caller {
        Caller::Control => "http:control",
        Caller::ApiToken(_) => "http:api-token",
        _ => "http:other",
    };
    let version = store::update::<AgyDoc, _>(&state.pool, &Scope::Machine, by, |doc| {
        doc.enabled = request.enabled;
        if request.enabled {
            doc.acknowledged_warning_version = Some(WARNING_VERSION);
        }
    })
    .await
    .map_err(|error| match error {
        SettingsError::Invalid(detail) => {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "invalid", detail)
        }
        SettingsError::Stale { .. } => refusal(StatusCode::CONFLICT, "stale"),
        SettingsError::Unreadable { reason, .. } => {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "unreadable", reason)
        }
        other => {
            tracing::warn!(error = %other, "the agy opt-in could not be written");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    })?;
    Ok(Json(json!({
        "version": version,
        "takes_effect": "a chat reads the switch at once; a primary_runner of agy takes effect when the daemon restarts",
    })))
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, StatusCode};
    use serde_json::{Value, json};

    use super::super::tests::{test_state, workflow_call};
    use crate::auth::{ApiTokenLevel, Scope as Caller, permits};
    use crate::engines::agy::{WARNING, WARNING_VERSION};
    use crate::governance::autopilot::set_kill_switch;
    use crate::project::rules::testing::Home;
    use crate::state::AppState;

    /// An empty settings snapshot owned by this test (these tests publish a document).
    async fn fixture() -> (Home, AppState) {
        let home = Home::new();
        let state = test_state().await;
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

    /// The stored `agy` document as `GET /settings/machine/agy` serves it.
    async fn stored(state: &AppState) -> Value {
        let (status, answer) = call(state, "GET", "/settings/machine/agy", None).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        answer
    }

    #[tokio::test]
    async fn put_engines_agy_enable_without_ack_is_400_with_warning() {
        let (_home, state) = fixture().await;

        for body in [
            json!({ "enabled": true }),
            json!({ "enabled": true, "acknowledged": false }),
        ] {
            let (status, answer) = call(&state, "PUT", "/engines/agy", Some(body.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {answer}");
            assert!(
                answer.to_string().contains(WARNING),
                "the refusal carries the warning text: {answer}"
            );
        }
        let document = stored(&state).await;
        assert_eq!(document["body"]["enabled"], false, "nothing was written");
        assert_eq!(document["version"], 0, "nothing was written");
    }

    #[tokio::test]
    async fn put_engines_agy_enable_with_ack_stores_version() {
        let (_home, state) = fixture().await;

        let body = json!({ "enabled": true, "acknowledged": true });
        let (status, answer) = call(&state, "PUT", "/engines/agy", Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");

        let document = stored(&state).await;
        assert_eq!(document["body"]["enabled"], true);
        assert_eq!(
            document["body"]["acknowledged_warning_version"],
            json!(WARNING_VERSION),
            "the acknowledgement is stored as the current warning version"
        );
        assert_eq!(document["version"], 1);

        let (status, answer) = call(&state, "GET", "/engines/agy", None).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["enabled"], true);
        assert_eq!(answer["acknowledged"], true);
    }

    #[tokio::test]
    async fn put_engines_agy_disable_needs_no_ack() {
        let (_home, state) = fixture().await;
        let off = json!({ "enabled": false });

        // Disabling something that was never enabled is accepted.
        let (status, answer) = call(&state, "PUT", "/engines/agy", Some(off.clone())).await;
        assert_eq!(status, StatusCode::OK, "{answer}");

        let enable = json!({ "enabled": true, "acknowledged": true });
        let (status, answer) = call(&state, "PUT", "/engines/agy", Some(enable)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(stored(&state).await["body"]["enabled"], true);

        let (status, answer) = call(&state, "PUT", "/engines/agy", Some(off)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(stored(&state).await["body"]["enabled"], false);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn get_engines_agy_returns_warning_text_and_installed() {
        // The probe resolves `NUCLEOS_AGY_BIN` first, as given, so the answer does not depend on the
        // machine's PATH. The env lock is taken before the settings fixture (lock order).
        let _env = crate::execution::worktree::test_env_lock();
        let previous = std::env::var_os("NUCLEOS_AGY_BIN");
        unsafe { std::env::set_var("NUCLEOS_AGY_BIN", "fake-agy-for-the-probe") };
        let (_home, state) = fixture().await;

        let (status, answer) = call(&state, "GET", "/engines/agy", None).await;

        unsafe {
            match previous {
                Some(value) => std::env::set_var("NUCLEOS_AGY_BIN", value),
                None => std::env::remove_var("NUCLEOS_AGY_BIN"),
            }
        }
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["enabled"], false);
        assert_eq!(answer["acknowledged"], false);
        assert_eq!(answer["warning"], WARNING);
        assert_eq!(answer["warning_version"], json!(WARNING_VERSION));
        assert_eq!(
            answer["installed"], true,
            "installed is reported while the engine is off"
        );
    }

    /// E15 F2: a stored row is not run through `finish` on load, so GET reports the gate (`enabled()`),
    /// not the raw field.
    #[tokio::test]
    async fn get_engines_agy_reports_the_gate_not_the_raw_field() {
        use crate::engines::agy::AgyDoc;
        use crate::settings::{Scope, Settings, Snapshot, set_global};

        let (_home, state) = fixture().await;
        for acknowledged in [None, Some(0), Some(WARNING_VERSION + 1)] {
            // Published raw, as the production load decodes it: no `finish`.
            let snapshot = Snapshot::default().with_document(
                &Scope::Machine,
                AgyDoc {
                    enabled: true,
                    acknowledged_warning_version: acknowledged,
                },
            );
            let _ = set_global(Settings::from_snapshot(snapshot));
            let (status, answer) = call(&state, "GET", "/engines/agy", None).await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            assert_eq!(
                answer["enabled"], false,
                "enabled:true with acknowledgement {acknowledged:?} is not an open gate: {answer}"
            );
            assert_eq!(answer["acknowledged"], false, "{answer}");
        }
        let snapshot = Snapshot::default().with_document(
            &Scope::Machine,
            AgyDoc {
                enabled: true,
                acknowledged_warning_version: Some(WARNING_VERSION),
            },
        );
        let _ = set_global(Settings::from_snapshot(snapshot));
        let (_, answer) = call(&state, "GET", "/engines/agy", None).await;
        assert_eq!(answer["enabled"], true, "{answer}");
        assert_eq!(answer["acknowledged"], true, "{answer}");
    }

    /// E16 F8: a chat reads the switch at once; only a `primary_runner: agy` waits for a restart.
    #[tokio::test]
    async fn put_engines_agy_takes_effect_names_chats_and_restart() {
        let (_home, state) = fixture().await;
        let body = json!({ "enabled": true, "acknowledged": true });
        let (status, answer) = call(&state, "PUT", "/engines/agy", Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let text = answer["takes_effect"].as_str().expect("a sentence").to_lowercase();
        assert!(text.contains("chat"), "{text}");
        assert!(text.contains("restart"), "{text}");
    }

    #[tokio::test]
    async fn generic_settings_put_cannot_enable_agy_without_ack() {
        let (_home, state) = fixture().await;

        // The bypass this closes: the generic door with `enabled: true` and no acknowledgement.
        let uri = "/settings/machine/agy";
        let request = json!({
            "body": { "enabled": true, "acknowledged_warning_version": null },
            "version": 0,
        });
        let (status, answer) = call(&state, "PUT", uri, Some(request)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
        assert_eq!(answer["refusal"], "invalid");
        assert_eq!(stored(&state).await["body"]["enabled"], false);

        // A stale acknowledgement is no better.
        let request = json!({
            "body": { "enabled": true, "acknowledged_warning_version": WARNING_VERSION + 1 },
            "version": 0,
        });
        let (status, answer) = call(&state, "PUT", uri, Some(request)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");

        // The document is self-consistent through this door too once the version is current.
        let request = json!({
            "body": { "enabled": true, "acknowledged_warning_version": WARNING_VERSION },
            "version": 0,
        });
        let (status, answer) = call(&state, "PUT", uri, Some(request)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
    }

    #[tokio::test]
    async fn engines_agy_put_is_423_under_kill_switch() {
        let (_home, state) = fixture().await;
        set_kill_switch(&state.pool, true).await.unwrap();

        let body = json!({ "enabled": true, "acknowledged": true });
        let (status, answer) = call(&state, "PUT", "/engines/agy", Some(body)).await;
        assert_eq!(status, StatusCode::LOCKED, "{answer}");
        assert_eq!(answer["refusal"], "kill_switch");

        set_kill_switch(&state.pool, false).await.unwrap();
        assert_eq!(
            stored(&state).await["body"]["enabled"],
            false,
            "nothing written"
        );
    }

    /// Like `/settings`, the route is in no scope table: only the control token and an Admin key
    /// reach it, reads included.
    #[test]
    fn engines_agy_route_needs_control_or_admin() {
        for method in [Method::GET, Method::PUT] {
            for caller in [
                Caller::Run(7),
                Caller::ApiToken(ApiTokenLevel::ReadOnly),
                Caller::ApiToken(ApiTokenLevel::RunCreating),
            ] {
                assert!(
                    !permits(&caller, &method, "/engines/agy"),
                    "{caller:?} must not reach {method} /engines/agy"
                );
            }
            for caller in [Caller::ApiToken(ApiTokenLevel::Admin), Caller::Control] {
                assert!(
                    permits(&caller, &method, "/engines/agy"),
                    "{caller:?} must reach {method} /engines/agy"
                );
            }
        }
    }
}
