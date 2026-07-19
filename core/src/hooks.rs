use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

use crate::classifier;
use crate::runs::finalize_termination;
use crate::shadow;
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
    // core never blindly trusts what the hook sends).
    let is_in_flight = state
        .run_handles
        .lock()
        .unwrap()
        .contains_key(&payload.run_id);

    let (cwd, mode) = if is_in_flight {
        match sqlx::query_as::<_, (Option<String>, String)>(
            "SELECT cwd, mode FROM runs WHERE id = ?",
        )
        .bind(payload.run_id)
        .fetch_optional(&state.pool)
        .await
        {
            Ok(Some((cwd, mode))) => (cwd, mode),
            Ok(None) => (None, "real".to_owned()),
            Err(error) => {
                tracing::warn!(
                    run_id = payload.run_id,
                    %error,
                    "pretooluse-decision: failed to resolve cwd for in-flight run"
                );
                (None, "real".to_owned())
            }
        }
    } else {
        (None, "real".to_owned())
    };

    let classification = classifier::classify(
        &payload.tool_name,
        &payload.tool_input,
        cwd.as_deref().map(Path::new),
    );
    tracing::info!(
        tool_name = %payload.tool_name,
        decision = %classification.decision.decision,
        action_class = classification.action_class,
        reason = %classification.reason,
        "pretooluse-decision: classified action"
    );

    if mode == "shadow" {
        if let Err(error) = shadow::record_decision(
            &state.pool,
            payload.run_id,
            &payload.tool_name,
            &payload.tool_input,
            &classification,
        )
        .await
        {
            tracing::warn!(
                run_id = payload.run_id,
                %error,
                "pretooluse-decision: failed to record shadow decision"
            );
        }

        let read_only = matches!(payload.tool_name.as_str(), "Read" | "Grep" | "Glob")
            || (payload.tool_name == "Bash" && classification.action_class == "read-local");
        return if read_only {
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "shadow mode permits this read-only tool".to_owned(),
            })
        } else {
            Json(Decision {
                decision: "deny".to_owned(),
                reason: "shadow mode blocks tools that are not read-only".to_owned(),
            })
        };
    }

    if classification.decision.decision == "pending_approval" {
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
    }

    Json(classification.decision)
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
                cwd TEXT,
                mode TEXT NOT NULL DEFAULT 'real',
                created_at TEXT NOT NULL,
                completed_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE shadow_decisions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL,
                tool_name TEXT NOT NULL,
                tool_input TEXT,
                decision TEXT NOT NULL,
                reason TEXT,
                action_class TEXT NOT NULL,
                classifier_version INTEGER NOT NULL,
                human_verdict TEXT,
                reviewed_at TEXT,
                created_at TEXT NOT NULL
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
        assert_eq!(decision.reason, "destructive deletion commands are denied");
    }

    #[tokio::test]
    async fn allows_safe_read_command() {
        let app = test_router(test_state().await);
        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"ls -la"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "allow");
        assert_eq!(decision.reason, "recognized non-mutating shell command");
    }

    #[tokio::test]
    async fn pends_unrecognized_command() {
        let tool_input = serde_json::json!({"command": "echo hi"});
        let classification = classifier::classify("Bash", &tool_input, None);
        assert_eq!(classification.action_class, "unrecognized");

        let app = test_router(test_state().await);
        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"echo hi"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");
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
        assert_eq!(
            decision.reason,
            "push, merge, deploy, publish, and tag actions require approval"
        );

        // The run was actively terminated into awaiting_approval, and its handle removed (spec §8.4).
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn edit_to_autopilot_config_pends_approval_and_terminates_the_run() {
        let state = test_state().await;
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, cwd, created_at) VALUES ('x', 'running', 'C:\\work\\repo', '2026-07-17T00:00:00Z')",
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
            r#"{{"run_id":{run_id},"tool_name":"Edit","tool_input":{{"file_path":".ai/autopilot.yaml"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "pending_approval");
        assert_eq!(
            decision.reason,
            "changes to autopilot governance files require approval"
        );

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn cwd_dependent_delete_outside_workspace_denies_through_the_handler() {
        let state = test_state().await;
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, cwd, created_at) VALUES ('x', 'running', 'C:\\work\\repo', '2026-07-18T00:00:00Z')",
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
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"rm ../outside/x"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "destructive deletion commands are denied");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn shadow_read_only_brake_records_would_decisions_without_terminating() {
        let state = test_state().await;
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, cwd, mode, created_at)
             VALUES ('x', 'running', 'C:\\work\\repo', 'shadow', '2026-07-18T00:00:00Z')",
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

        let edit_input = serde_json::json!({"file_path": "src/ordinary.rs"});
        let edit = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": edit_input
            })
            .to_string(),
        )
        .await;
        assert_eq!(edit.decision, "deny");

        let row: (String, String, String) = sqlx::query_as(
            "SELECT decision, action_class, tool_input FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Edit'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "allow");
        assert_eq!(row.1, "read-local");
        assert_eq!(row.2, edit_input.to_string());

        let read = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Read",
                "tool_input": {"file_path": "src/lib.rs"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(read.decision, "allow");

        let push = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(push.decision, "deny");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_ne!(status, "awaiting_approval");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }
}
