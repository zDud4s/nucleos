use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateRunRequest {
    pub prompt: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_run_mode")]
    pub mode: String,
}

fn default_run_mode() -> String {
    "real".to_owned()
}

#[derive(Serialize, Deserialize)]
pub struct CreateRunResponse {
    pub id: i64,
}

#[derive(Serialize, Deserialize)]
pub struct RunStatusResponse {
    pub id: i64,
    pub project_id: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
}

pub async fn create_run(
    State(state): State<AppState>,
    Json(req): Json<CreateRunRequest>,
) -> Result<Json<CreateRunResponse>, StatusCode> {
    let id = create_run_inner(&state, req.prompt, req.project_id, req.cwd, &req.mode)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(CreateRunResponse { id }))
}

pub async fn create_run_inner(
    state: &AppState,
    prompt: String,
    project_id: Option<String>,
    cwd: Option<String>,
    mode: &str,
) -> Result<i64, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let id = sqlx::query(
        "INSERT INTO runs (project_id, cwd, prompt, status, mode, created_at)
         VALUES (?, ?, ?, 'running', ?, ?)",
    )
    .bind(&project_id)
    .bind(&cwd)
    .bind(&prompt)
    .bind(mode)
    .bind(&now)
    .execute(&state.pool)
    .await?
    .last_insert_rowid();

    // Persist session_id the instant the runner parses it (spec §3.3) — a separate task, so it lands
    // even if the run is later terminated mid-flight.
    let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    {
        let pool = state.pool.clone();
        tokio::spawn(async move {
            if let Some(session_id) = session_rx.recv().await {
                let _ = sqlx::query("UPDATE runs SET session_id = ? WHERE id = ?")
                    .bind(&session_id)
                    .bind(id)
                    .execute(&pool)
                    .await;
            }
        });
    }

    let pool = state.pool.clone();
    let runner = state.runner.clone();
    let cwd = cwd.map(std::path::PathBuf::from);
    let plan_only = mode == "shadow";
    let feed_project_id = project_id.clone();
    let append_shadow_summary = plan_only;
    let run_timeout = state.run_timeout;
    let handles = state.run_handles.clone();
    let env = vec![
        (
            "NUCLEOS_DAEMON_URL".to_string(),
            "http://127.0.0.1:8791".to_string(),
        ),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), state.token.0.clone()),
        // run_id == runs.id (spec §3.3) — the hook echoes it back in its decision request, so the
        // core can validate it and, for a `pending_approval`, terminate the right run.
        ("NUCLEOS_RUN_ID".to_string(), id.to_string()),
    ];

    let join_handle = tokio::spawn(async move {
        let result = tokio::time::timeout(
            run_timeout,
            runner.run_prompt(&prompt, &env, cwd.as_deref(), plan_only, session_tx),
        )
        .await;
        let completed_at = chrono::Utc::now().to_rfc3339();
        match result {
            Ok(Ok(o)) => {
                let completed = sqlx::query(
                    "UPDATE runs SET status = 'completed', exit_code = ?, stdout = ?, stderr = ?, session_id = COALESCE(?, session_id), cost_usd = ?, completed_at = ? WHERE id = ?",
                )
                .bind(o.exit_code)
                .bind(&o.stdout)
                .bind(&o.stderr)
                .bind(&o.session_id)
                .bind(o.cost_usd)
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await;
                if completed.is_ok() && append_shadow_summary {
                    let _ = crate::feed::append(
                        &pool,
                        feed_project_id.as_deref(),
                        "shadow_run_completed",
                        "shadow run completed",
                        Some(id),
                    )
                    .await;
                }
            }
            Ok(Err(e)) => {
                let _ = sqlx::query(
                    "UPDATE runs SET status = 'failed', stderr = ?, completed_at = ? WHERE id = ?",
                )
                .bind(e.to_string())
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await;
            }
            Err(_elapsed) => {
                let _ = sqlx::query(
                    "UPDATE runs SET status = 'timed_out', completed_at = ? WHERE id = ?",
                )
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await;
            }
        }
        // Only reached on natural completion/timeout — an aborted task (cancellation) never gets here.
        handles.lock().unwrap().remove(&id);
    });

    state
        .run_handles
        .lock()
        .unwrap()
        .insert(id, join_handle.abort_handle());

    Ok(id)
}

pub async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<RunStatusResponse>, StatusCode> {
    let row = sqlx::query_as::<
        _,
        (i64, Option<String>, String, Option<i32>, Option<String>, Option<String>, Option<String>, Option<f64>),
    >(
        "SELECT id, project_id, status, exit_code, stdout, stderr, session_id, cost_usd FROM runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(RunStatusResponse {
        id: row.0,
        project_id: row.1,
        status: row.2,
        exit_code: row.3,
        stdout: row.4,
        stderr: row.5,
        session_id: row.6,
        cost_usd: row.7,
    }))
}

/// Terminates an in-flight run: aborts its task (which, via `kill_on_drop`, kills the CLI process)
/// and records `status`. Removing the entry from the handle map is the atomic arbiter when several
/// termination reasons race (user cancel, timeout, or Chunk 3's §8.4 approval-pause): whoever removes
/// it first sets the final status; a later reason finds it gone and no-ops. Returns true iff this call
/// terminated the run. Factored out so Chunk 3's `pending_approval` path reuses the same arbiter.
pub async fn finalize_termination(state: &AppState, id: i64, status: &str) -> bool {
    let handle = state.run_handles.lock().unwrap().remove(&id);
    match handle {
        Some(h) => {
            h.abort();
            let now = chrono::Utc::now().to_rfc3339();
            let _ = sqlx::query("UPDATE runs SET status = ?, completed_at = ? WHERE id = ?")
                .bind(status)
                .bind(&now)
                .bind(id)
                .execute(&state.pool)
                .await;
            true
        }
        None => false,
    }
}

pub async fn cancel_run(State(state): State<AppState>, Path(id): Path<i64>) -> StatusCode {
    if finalize_termination(&state, id, "cancelled").await {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

/// Marks every run still `"running"` as `"interrupted"` — called once at startup to recover from a
/// daemon crash that left in-flight runs' rows stuck (spec §3.2). Returns how many rows it changed.
pub async fn reconcile_orphaned_runs(pool: &sqlx::SqlitePool) -> Result<u64, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let reconciled: Vec<(i64, Option<String>)> = sqlx::query_as(
        "UPDATE runs SET status = 'interrupted', completed_at = ? WHERE status = 'running'
         RETURNING id, project_id",
    )
    .bind(&now)
    .fetch_all(pool)
    .await?;
    for (id, project_id) in &reconciled {
        let _ = crate::feed::append(
            pool,
            project_id.as_deref(),
            "run_interrupted",
            "run interrupted during startup recovery",
            Some(*id),
        )
        .await;
    }
    Ok(reconciled.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::runner::{FakeCommandRunner, RunOutcome};
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    async fn test_state_with_runner(
        delay: Option<Duration>,
        run_timeout: Duration,
    ) -> (AppState, Arc<FakeCommandRunner>) {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id TEXT,
                cwd TEXT,
                prompt TEXT NOT NULL,
                status TEXT NOT NULL,
                exit_code INTEGER,
                stdout TEXT,
                stderr TEXT,
                session_id TEXT,
                cost_usd REAL,
                mode TEXT NOT NULL DEFAULT 'real',
                created_at TEXT NOT NULL,
                completed_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE feed (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id TEXT,
                kind TEXT NOT NULL,
                summary TEXT NOT NULL,
                run_id INTEGER,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        let runner = Arc::new(FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(RunOutcome {
                exit_code: 0,
                stdout: "42".into(),
                stderr: String::new(),
                session_id: Some("fake-session-id".into()),
                cost_usd: Some(0.05),
            })),
            delay: std::sync::Mutex::new(delay),
            last_plan_only: std::sync::Mutex::new(None),
        });
        let state = AppState {
            token: Token("test-token".into()),
            pool,
            runner: runner.clone(),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_timeout,
        };
        (state, runner)
    }

    async fn test_state_with(delay: Option<Duration>, run_timeout: Duration) -> AppState {
        test_state_with_runner(delay, run_timeout).await.0
    }

    async fn test_state() -> AppState {
        test_state_with(None, crate::state::DEFAULT_RUN_TIMEOUT).await
    }

    fn test_router(state: AppState) -> Router {
        Router::new()
            .route("/runs", post(create_run))
            .route("/runs/{id}", get(get_run))
            .route("/runs/{id}/cancel", post(cancel_run))
            .with_state(state)
    }

    async fn create_run_via_http(app: &Router, prompt: &str) -> CreateRunResponse {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"prompt":"{prompt}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn get_run_status(app: &Router, id: i64) -> RunStatusResponse {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/runs/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn create_then_get_run_reaches_completed_status() {
        let app = test_router(test_state().await);
        let created = create_run_via_http(&app, "what is 6*7").await;

        let mut status = String::new();
        for _ in 0..20 {
            let parsed = get_run_status(&app, created.id).await;
            status = parsed.status.clone();
            if status == "completed" {
                assert_eq!(parsed.stdout.as_deref(), Some("42"));
                assert_eq!(parsed.session_id.as_deref(), Some("fake-session-id"));
                assert_eq!(parsed.cost_usd, Some(0.05));
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("run did not reach completed status in time, last status: {status}");
    }

    #[tokio::test]
    async fn create_run_inner_persists_mode_and_threads_plan_only_per_run() {
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;

        let shadow_id = create_run_inner(&state, "shadow".into(), None, None, "shadow")
            .await
            .unwrap();
        for _ in 0..20 {
            if runner.last_plan_only.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let shadow_mode: String = sqlx::query_scalar("SELECT mode FROM runs WHERE id = ?")
            .bind(shadow_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(shadow_mode, "shadow");
        assert_eq!(*runner.last_plan_only.lock().unwrap(), Some(true));
        let mut shadow_feed = None;
        for _ in 0..20 {
            let entries = crate::feed::list_feed(&state.pool, None, 50).await.unwrap();
            if !entries.is_empty() {
                shadow_feed = Some(entries);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let shadow_feed = shadow_feed.expect("shadow completion did not append a feed entry");
        assert_eq!(shadow_feed.len(), 1);
        assert_eq!(shadow_feed[0].kind, "shadow_run_completed");
        assert_eq!(shadow_feed[0].run_id, Some(shadow_id));

        let real_id = create_run_inner(&state, "real".into(), None, None, "real")
            .await
            .unwrap();
        for _ in 0..20 {
            if *runner.last_plan_only.lock().unwrap() == Some(false) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let real_mode: String = sqlx::query_scalar("SELECT mode FROM runs WHERE id = ?")
            .bind(real_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(real_mode, "real");
        assert_eq!(*runner.last_plan_only.lock().unwrap(), Some(false));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(
            crate::feed::list_feed(&state.pool, None, 50)
                .await
                .unwrap()
                .len(),
            1,
            "real completion must not append a feed entry"
        );

        let app = test_router(state.clone());
        let default_id = create_run_via_http(&app, "default mode").await.id;
        let default_mode: String = sqlx::query_scalar("SELECT mode FROM runs WHERE id = ?")
            .bind(default_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(default_mode, "real");
    }

    #[tokio::test]
    async fn get_unknown_run_returns_404() {
        let app = test_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/runs/999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_can_be_cancelled_mid_flight() {
        let app = test_router(
            test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await,
        );
        let created = create_run_via_http(&app, "a slow one").await;

        let cancel_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/runs/{}/cancel", created.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancel_response.status(), StatusCode::OK);

        let parsed = get_run_status(&app, created.id).await;
        assert_eq!(parsed.status, "cancelled");
    }

    #[tokio::test]
    async fn session_id_persists_even_when_cancelled_mid_flight() {
        let app = test_router(
            test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await,
        );
        let created = create_run_via_http(&app, "a slow one").await;

        let mut session_id = None;
        for _ in 0..50 {
            let parsed = get_run_status(&app, created.id).await;
            if parsed.session_id.is_some() {
                session_id = parsed.session_id;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(session_id.as_deref(), Some("fake-session-id"));

        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/runs/{}/cancel", created.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let parsed = get_run_status(&app, created.id).await;
        assert_eq!(parsed.status, "cancelled");
        assert_eq!(parsed.session_id.as_deref(), Some("fake-session-id"));
    }

    #[tokio::test]
    async fn cancelling_an_unknown_run_returns_404() {
        let app = test_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runs/999/cancel")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_times_out_when_it_outlives_the_configured_timeout() {
        let app = test_router(
            test_state_with(Some(Duration::from_millis(500)), Duration::from_millis(50)).await,
        );
        let created = create_run_via_http(&app, "a run that outlives its timeout").await;

        let mut status = String::new();
        for _ in 0..20 {
            let parsed = get_run_status(&app, created.id).await;
            status = parsed.status.clone();
            if status == "timed_out" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("run did not reach timed_out status in time, last status: {status}");
    }

    #[tokio::test]
    async fn reconcile_marks_only_running_runs_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open(&dir.path().join("nucleos.db"))
            .await
            .unwrap();

        // One run in flight when the daemon "died", one already completed.
        sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('x', 'running', '2026-07-17T00:00:00Z')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('y', 'completed', '2026-07-17T00:00:00Z')")
            .execute(&pool).await.unwrap();

        let n = reconcile_orphaned_runs(&pool).await.unwrap();
        assert_eq!(n, 1, "exactly the one running row should be reconciled");

        let statuses: Vec<(String, String)> =
            sqlx::query_as("SELECT prompt, status FROM runs ORDER BY prompt")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            statuses,
            vec![
                ("x".to_string(), "interrupted".to_string()),
                ("y".to_string(), "completed".to_string()),
            ]
        );
        let entries = crate::feed::list_feed(&pool, None, 50).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "run_interrupted");
        assert_eq!(entries[0].run_id, Some(1));
    }
}
