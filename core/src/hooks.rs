use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runs::finalize_termination;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct PreToolUsePayload {
    // The run this decision is for (spec §3.3). `run_id == runs.id` — injected into the CLI's
    // environment as NUCLEOS_RUN_ID (Step 5) and echoed back by the hook script; there is no second
    // identifier.
    pub run_id: i64,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
}

#[derive(Serialize, Deserialize)]
pub struct Decision {
    // "allow" | "deny" | "pending_approval" (spec §3.3). To the CLI, both "deny" and
    // "pending_approval" are just "block" (the hook script maps them, Step 6); the core treats them
    // differently — see below.
    pub decision: String,
    pub reason: String,
}

pub async fn pretooluse_decision(
    State(state): State<AppState>,
    Json(payload): Json<PreToolUsePayload>,
) -> Json<Decision> {
    // Validate run_id against runs actually in flight before trusting anything derived from it (spec
    // §3.4 — the hook's environment sits inside the same cooperative trust model as the token, so the
    // core never blindly trusts what the hook sends). In the Foundation this only gates the
    // active-termination path below; the richer per-project/mode resolution run_id enables is
    // Autopilot-plan scope.
    let is_in_flight = state
        .run_handles
        .lock()
        .unwrap()
        .contains_key(&payload.run_id);

    let command = payload
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Stub policy — NOT the real §8.2 risk taxonomy (Autopilot-plan scope). Two illustrative rules,
    // one per non-allow decision, so the whole mechanism is exercised end-to-end:
    //   * `rm -rf`   -> "deny": a definitive refusal; the run continues/fails on its own.
    //   * `git push` -> "pending_approval": high-risk, needs a human — the core actively terminates
    //                   the run into `awaiting_approval` (spec §8.4) via the same path cancellation
    //                   uses. (The real classifier deciding *which* actions pend approval, plus the
    //                   resume-on-approval and single-use-authorization flow, are Autopilot-plan
    //                   scope; this proves the Foundation mechanism they build on.)
    if payload.tool_name == "Bash" && command.contains("rm -rf") {
        return Json(Decision {
            decision: "deny".into(),
            reason: "stub policy (Foundation PoC only): rm -rf is denied".into(),
        });
    }

    if payload.tool_name == "Bash" && command.contains("git push") {
        // Active termination (spec §8.4 steps 2–3): drive the run to `awaiting_approval` via the same
        // atomic-handle-removal arbiter cancellation uses (`finalize_termination`, Chunk 2 Task 4).
        // Only for a genuinely in-flight run_id — an unknown/stale one must not terminate anything.
        if is_in_flight {
            finalize_termination(&state, payload.run_id, "awaiting_approval").await;
        } else {
            tracing::warn!(
                "pretooluse-decision: pending_approval for unknown/finished run_id {} — not terminating",
                payload.run_id
            );
        }
        return Json(Decision {
            decision: "pending_approval".into(),
            reason: "stub policy (Foundation PoC only): git push needs approval".into(),
        });
    }

    Json(Decision {
        decision: "allow".into(),
        reason: "stub policy: default allow".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::runner::FakeCommandRunner;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn test_state() -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        // The active-termination path (`git push` -> awaiting_approval) writes the runs table, so the
        // test pool needs it. A minimal shape is enough for these tests.
        sqlx::query(
            "CREATE TABLE runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                prompt TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                completed_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        AppState {
            token: Token("test-token".into()),
            pool,
            runner: Arc::new(FakeCommandRunner::default()),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    fn test_router(state: AppState) -> Router {
        Router::new()
            .route("/hooks/pretooluse-decision", post(pretooluse_decision))
            .with_state(state)
    }

    async fn decide(app: &Router, body: &str) -> Decision {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/pretooluse-decision")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn denies_rm_rf() {
        let app = test_router(test_state().await);
        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"rm -rf /tmp/x"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "deny");
    }

    #[tokio::test]
    async fn allows_everything_else() {
        let app = test_router(test_state().await);
        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"echo hi"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "allow");
    }

    #[tokio::test]
    async fn git_push_pends_approval_and_terminates_the_run() {
        let state = test_state().await;

        // Stand up a fake in-flight run: a runs row plus a live task whose abort handle is registered
        // under the same id (that's the `run_id` the hook will send).
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, created_at) VALUES ('x', 'running', '2026-07-17T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let task =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });
        state
            .run_handles
            .lock()
            .unwrap()
            .insert(run_id, task.abort_handle());

        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"git push origin main"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "pending_approval");

        // The run was actively terminated into awaiting_approval, and its handle removed (spec §8.4).
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));
    }
}
