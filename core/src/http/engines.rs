//! Tests of `GET|PUT /engines/agy`, the acknowledged opt-in of the agy engine (plan Node 2, test 15).
//!
//! RED: the handlers (`routes()`) do not exist yet; GREEN writes them above this module and merges
//! them next to `settings::routes()` in `http.rs`.

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

        for body in [json!({ "enabled": true }), json!({ "enabled": true, "acknowledged": false })]
        {
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
        assert_eq!(stored(&state).await["body"]["enabled"], false, "nothing written");
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
