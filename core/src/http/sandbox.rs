//! The shell's read-only view of the OS sandbox (`GET /sandbox/status`, spec D7). Owner-only by
//! omission from every scoped route table in `auth.rs`.
//!
//! **No route here, or anywhere, changes the switch.** The owner flips it in the config file the
//! daemon reads and nothing a caller sends can reach that file; a test pins the absence.

use super::*;
use crate::sandbox::{self, SandboxStatus};

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/sandbox/status", get(get_sandbox_status))
}

/// The status as the shell shows it: a stable tag and the banner text, `null` when armed.
fn view(status: &SandboxStatus) -> serde_json::Value {
    let state = match status {
        SandboxStatus::NoBackend => "no_backend",
        SandboxStatus::Off => "off",
        SandboxStatus::Armed => "armed",
        SandboxStatus::Refused { .. } => "refused",
    };
    serde_json::json!({ "state": state, "banner": status.banner() })
}

/// How the OS sandbox stands, with the banner the shell shows.
async fn get_sandbox_status() -> Json<serde_json::Value> {
    Json(view(&sandbox::status()))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{test_state, workflow_call};
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn the_view_names_the_state_and_the_banner() {
        let none = view(&SandboxStatus::NoBackend);
        assert_eq!(none["state"], "no_backend");
        assert_eq!(none["banner"], "sandbox por implementar");
        assert_eq!(view(&SandboxStatus::Off)["state"], "off");
        assert!(view(&SandboxStatus::Armed)["banner"].is_null());
        let refused = view(&SandboxStatus::Refused {
            reason: "cannot start".into(),
        });
        assert_eq!(
            (refused["state"].clone(), refused["banner"].clone()),
            ("refused".into(), "cannot start".into())
        );
    }

    #[tokio::test]
    async fn the_status_route_returns_no_backend_and_its_banner() {
        let state = test_state().await;
        let (code, body) = workflow_call(state, "GET", "/sandbox/status", None).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        // Whatever the machine's own switch says, the route answers what `status()` says, and
        // nothing in this tree arms the sandbox.
        assert_eq!(body, view(&sandbox::status()));
        assert_ne!(body["state"], "armed");
        if body["state"] == "no_backend" {
            assert_eq!(body["banner"], "sandbox por implementar");
        }
    }

    /// Every verb that could write, against the status path and the paths a switch route would
    /// plausibly take: none answers success.
    #[tokio::test]
    async fn there_is_no_route_that_changes_the_switch() {
        let state = test_state().await;
        let paths = [
            "/sandbox",
            "/sandbox/status",
            "/sandbox/switch",
            "/sandbox/off",
            "/sandbox/on",
            "/sandbox/arm",
            "/sandbox/enable",
            "/sandbox/disable",
            "/sandbox/config",
        ];
        for path in paths {
            for method in ["POST", "PUT", "PATCH", "DELETE"] {
                let (code, body) = workflow_call(
                    state.clone(),
                    method,
                    path,
                    Some(serde_json::json!({ "sandbox": "off" })),
                )
                .await;
                assert!(
                    !code.is_success(),
                    "{method} {path} answered {code}: {body}"
                );
            }
        }
    }
}
