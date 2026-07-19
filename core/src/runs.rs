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

#[derive(Debug)]
pub enum CreateRunError {
    Invalid(&'static str),
    Busy,
    Worktree(std::io::Error),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for CreateRunError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for CreateRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Busy => formatter.write_str("run creation is busy"),
            Self::Worktree(error) => write!(formatter, "worktree provisioning failed: {error}"),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

impl std::error::Error for CreateRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Worktree(error) => Some(error),
            Self::Db(error) => Some(error),
            Self::Invalid(_) | Self::Busy => None,
        }
    }
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
        .map_err(|error| crate::http::create_run_status(&error))?;

    Ok(Json(CreateRunResponse { id }))
}

pub async fn create_run_inner(
    state: &AppState,
    prompt: String,
    project_id: Option<String>,
    cwd: Option<String>,
    mode: &str,
) -> Result<i64, CreateRunError> {
    if mode == "worktree" && (project_id.is_none() || cwd.is_none()) {
        return Err(CreateRunError::Invalid(
            "worktree mode requires project_id and cwd (the project root)",
        ));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let inserted = sqlx::query(
        "INSERT INTO runs (project_id, cwd, prompt, status, mode, created_at)
         VALUES (?, ?, ?, 'running', ?, ?)",
    )
    .bind(&project_id)
    .bind(&cwd)
    .bind(&prompt)
    .bind(mode)
    .bind(&now)
    .execute(&state.pool)
    .await;
    let id = match inserted {
        Ok(result) => result.last_insert_rowid(),
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|database_error| database_error.is_unique_violation()) =>
        {
            return Err(CreateRunError::Busy);
        }
        Err(error) => return Err(CreateRunError::Db(error)),
    };

    let plan_only = mode == "shadow";
    let mut spawn_cwd = cwd.clone().map(std::path::PathBuf::from);
    let mut completion_feed = plan_only.then(|| {
        (
            "shadow_run_completed".to_owned(),
            "shadow run completed".to_owned(),
        )
    });

    if mode == "worktree" {
        let project_root = cwd.as_deref().expect("worktree cwd validated above");
        let worktree_project_id = project_id
            .as_deref()
            .expect("worktree project_id validated above");
        let info = match crate::worktree::create(std::path::Path::new(project_root), id).await {
            Ok(info) => info,
            Err(error) => {
                let completed_at = chrono::Utc::now().to_rfc3339();
                let _ =
                    sqlx::query("UPDATE runs SET status = 'failed', completed_at = ? WHERE id = ?")
                        .bind(&completed_at)
                        .bind(id)
                        .execute(&state.pool)
                        .await;
                let _ = crate::feed::append(
                    &state.pool,
                    project_id.as_deref(),
                    "worktree_provision_failed",
                    &format!("worktree provisioning failed: {error}"),
                    Some(id),
                )
                .await;
                return Err(CreateRunError::Worktree(error));
            }
        };
        let worktree_path = info.path.to_string_lossy().into_owned();
        crate::worktree::record(
            &state.pool,
            id,
            worktree_project_id,
            project_root,
            &worktree_path,
            &info.branch,
        )
        .await?;
        sqlx::query("UPDATE runs SET cwd = ? WHERE id = ?")
            .bind(&worktree_path)
            .bind(id)
            .execute(&state.pool)
            .await?;
        completion_feed = Some((
            "worktree_run_completed".to_owned(),
            format!("worktree run completed on {}", info.branch),
        ));
        spawn_cwd = Some(info.path);
    }

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
    let feed_project_id = project_id.clone();
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
            runner.run_prompt(&prompt, &env, spawn_cwd.as_deref(), plan_only, session_tx),
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
                if completed.is_ok() {
                    if let Some((kind, summary)) = completion_feed {
                        let _ = crate::feed::append(
                            &pool,
                            feed_project_id.as_deref(),
                            &kind,
                            &summary,
                            Some(id),
                        )
                        .await;
                    }
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
    use std::ffi::OsStr;
    use std::path::{Path as FsPath, PathBuf};
    use std::process::Command;
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
            "CREATE UNIQUE INDEX one_open_worktree_run_per_project
             ON runs (project_id)
             WHERE mode = 'worktree' AND status IN ('running', 'awaiting_approval')",
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
        sqlx::query(
            "CREATE TABLE worktrees (
                run_id INTEGER PRIMARY KEY,
                project_id TEXT NOT NULL,
                project_root TEXT NOT NULL,
                path TEXT NOT NULL,
                branch TEXT NOT NULL,
                created_at TEXT NOT NULL,
                removed_at TEXT
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
            last_cwd: std::sync::Mutex::new(None),
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

    fn git_ok(dir: &FsPath, args: &[&OsStr]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    fn space_free_tempdir(prefix: &str) -> tempfile::TempDir {
        let base = std::env::current_dir().expect("resolve current directory");
        assert!(
            !base.to_string_lossy().contains(' '),
            "test checkout must have a space-free path"
        );
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(base)
            .expect("create space-free tempdir")
    }

    fn initialize_repo(repo: &FsPath) {
        std::fs::create_dir_all(repo).expect("create repository directory");
        assert!(git_ok(repo, &[OsStr::new("init")]));
        assert!(git_ok(
            repo,
            &[
                OsStr::new("config"),
                OsStr::new("user.email"),
                OsStr::new("test@x"),
            ],
        ));
        assert!(git_ok(
            repo,
            &[
                OsStr::new("config"),
                OsStr::new("user.name"),
                OsStr::new("test"),
            ],
        ));
        std::fs::write(repo.join("seed.txt"), "seed\n").expect("write seed file");
        assert!(git_ok(repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            repo,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("seed"),],
        ));
    }

    fn init_contained_repo(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let container = space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        (container, repo)
    }

    async fn test_state_with(delay: Option<Duration>, run_timeout: Duration) -> AppState {
        test_state_with_runner(delay, run_timeout).await.0
    }

    async fn test_state() -> AppState {
        test_state_with(None, crate::state::DEFAULT_RUN_TIMEOUT).await
    }

    async fn advance_run_ids_past(pool: &sqlx::SqlitePool, id: i64) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, created_at)
             VALUES (?, 'sequence placeholder', 'completed', 'real', ?)",
        )
        .bind(id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn create_worktree_run(
        state: &AppState,
        prompt: &str,
        project_id: &str,
        project_root: &str,
    ) -> Result<i64, CreateRunError> {
        for attempt in 0..100 {
            let result = create_run_inner(
                state,
                prompt.to_owned(),
                Some(project_id.to_owned()),
                Some(project_root.to_owned()),
                "worktree",
            )
            .await;
            match result {
                Err(CreateRunError::Worktree(_)) if attempt < 99 => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                result => return result,
            }
        }
        unreachable!("the final retry always returns")
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

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_mode_provisions_and_runs_inside_the_worktree() {
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-provision-");
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        advance_run_ids_past(&state.pool, 40_000).await;
        let project_root = repo.to_string_lossy().into_owned();

        let id = create_worktree_run(&state, "do it", "proj", &project_root)
            .await
            .unwrap();

        let mut spawn_cwd = None;
        for _ in 0..50 {
            spawn_cwd = runner.last_cwd.lock().unwrap().clone();
            if spawn_cwd.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let spawn_cwd = spawn_cwd.expect("runner did not receive a cwd");
        assert_eq!(
            spawn_cwd.file_name(),
            Some(OsStr::new(&format!("run-{id}")))
        );
        assert_eq!(*runner.last_plan_only.lock().unwrap(), Some(false));

        let run_cwd: String = sqlx::query_scalar("SELECT cwd FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(PathBuf::from(&run_cwd), spawn_cwd);

        let (worktree_path, branch): (String, String) =
            sqlx::query_as("SELECT path, branch FROM worktrees WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(PathBuf::from(&worktree_path), spawn_cwd);
        assert_eq!(branch, format!("nucleos/run-{id}"));

        let mut completion_feed = None;
        for _ in 0..50 {
            completion_feed = sqlx::query_as::<_, (String, String)>(
                "SELECT kind, summary FROM feed WHERE run_id = ?",
            )
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .unwrap();
            if completion_feed.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let (kind, summary) = completion_feed.expect("completion feed entry was not appended");
        assert_eq!(kind, "worktree_run_completed");
        assert!(summary.contains(&branch));

        let _ = crate::worktree::remove(&repo, &spawn_cwd, &[]).await;
    }

    #[tokio::test]
    async fn worktree_mode_requires_project_id_and_cwd() {
        let state = test_state().await;
        let missing_project = create_run_inner(
            &state,
            "do it".into(),
            None,
            Some("root".into()),
            "worktree",
        )
        .await;
        assert!(matches!(missing_project, Err(CreateRunError::Invalid(_))));

        let missing_cwd = create_run_inner(
            &state,
            "do it".into(),
            Some("proj".into()),
            None,
            "worktree",
        )
        .await;
        assert!(matches!(missing_cwd, Err(CreateRunError::Invalid(_))));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn second_worktree_run_while_one_is_running_is_busy() {
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-exclusive-");
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        advance_run_ids_past(&state.pool, 10_000).await;
        let project_root = repo.to_string_lossy().into_owned();

        create_worktree_run(&state, "first", "proj", &project_root)
            .await
            .unwrap();
        let second = create_run_inner(
            &state,
            "second".into(),
            Some("proj".into()),
            Some(project_root),
            "worktree",
        )
        .await;

        assert!(matches!(second, Err(CreateRunError::Busy)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_run_for_a_different_project_is_allowed() {
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-project-");
        let (_repo2_container, repo2) = init_contained_repo("nucleos-runs-project2-");
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        advance_run_ids_past(&state.pool, 20_000).await;

        create_worktree_run(&state, "first", "proj", &repo.to_string_lossy())
            .await
            .unwrap();
        let second = create_worktree_run(&state, "second", "proj2", &repo2.to_string_lossy()).await;

        assert!(second.is_ok(), "unexpected result: {second:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_slot_is_held_while_awaiting_approval() {
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-pinned-");
        let state = test_state().await;
        let project_root = repo.to_string_lossy().into_owned();
        sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, mode, created_at)
             VALUES ('proj', ?, 'pinned', 'awaiting_approval', 'worktree', ?)",
        )
        .bind(&project_root)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();

        let result = create_run_inner(
            &state,
            "new".into(),
            Some("proj".into()),
            Some(project_root),
            "worktree",
        )
        .await;

        assert!(matches!(result, Err(CreateRunError::Busy)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_slot_frees_after_the_open_run_leaves() {
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-released-");
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        advance_run_ids_past(&state.pool, 30_000).await;
        let project_root = repo.to_string_lossy().into_owned();
        let first = create_worktree_run(&state, "first", "proj", &project_root)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();

        let second = create_worktree_run(&state, "second", "proj", &project_root).await;

        assert!(second.is_ok(), "unexpected result: {second:?}");
    }

    #[tokio::test]
    async fn shadow_and_real_runs_are_unaffected_by_the_index() {
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;

        for mode in ["shadow", "real"] {
            for prompt in ["first", "second"] {
                let result =
                    create_run_inner(&state, prompt.into(), Some("proj".into()), None, mode).await;
                assert!(result.is_ok(), "{mode} run failed: {result:?}");
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_mode_on_a_non_repo_marks_failed_and_feeds() {
        let non_repo_container = space_free_tempdir("nucleos-runs-non-repo-");
        let non_repo = non_repo_container.path().join("not-a-repo");
        std::fs::create_dir(&non_repo).expect("create non-repository directory");
        let state = test_state().await;

        let result = create_run_inner(
            &state,
            "do it".into(),
            Some("proj".into()),
            Some(non_repo.to_string_lossy().into_owned()),
            "worktree",
        )
        .await;
        assert!(
            matches!(result, Err(CreateRunError::Worktree(_))),
            "unexpected result: {result:?}"
        );

        let (id, status): (i64, String) =
            sqlx::query_as("SELECT id, status FROM runs ORDER BY id DESC LIMIT 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(status, "failed");
        let (kind, summary): (String, String) =
            sqlx::query_as("SELECT kind, summary FROM feed WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(kind, "worktree_provision_failed");
        assert!(summary.contains("worktree provisioning failed:"));
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
