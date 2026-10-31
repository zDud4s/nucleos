//! Daemon-facing adapters for finishing the spec backlog.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::SqlitePool;

use super::audit::{self, AuditReport, SpecLineage, VerifyRunner};
use super::extract::{self, ExtractionRunner};
use super::finish::{
    FinishConfig, FinishDeps, FinishError, FinishReport, FinishScope, SpecPreparer, TreeLauncher,
    TreeOutcome, TreeRequest, derived_branch, finish_with,
};
use super::model::{Criterion, Spec};
use super::review::NoToolsReviewer;
use super::scan::ScanRoots;
use super::store::{self, StoreError};
use super::verdict::VerifyState;
use crate::execution::runner::ToolPolicy;
use crate::state::AppState;

const RUN_POLL: Duration = Duration::from_secs(1);
const RUN_CEILING: Duration = Duration::from_secs(30 * 60);
const TREE_POLL: Duration = Duration::from_secs(5);
const TREE_CEILING: Duration = Duration::from_secs(24 * 60 * 60);

/// Remembers that `run_id` extracted `spec_id`'s criteria. A repeat changes nothing.
pub async fn record_extraction_run(
    pool: &SqlitePool,
    spec_id: &str,
    run_id: &str,
) -> Result<(), StoreError> {
    sqlx::query("INSERT INTO spec_extraction_runs (spec_id, run_id, created_at) VALUES (?, ?, ?) ON CONFLICT (spec_id, run_id) DO NOTHING")
    .bind(spec_id)
    .bind(run_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// A run that starts with no tools and is waited for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoToolsRunRequest {
    pub prompt: String,
    pub project_id: Option<String>,
    pub cwd: Option<String>,
    pub tool_policy: ToolPolicy,
    pub steerable: bool,
    pub wait_for_final_text: bool,
}

/// What the real reviewer and extraction agent are started with.
pub fn no_tools_run_request(
    prompt: &str,
    project_id: Option<&str>,
    cwd: Option<&str>,
) -> NoToolsRunRequest {
    NoToolsRunRequest {
        prompt: prompt.to_owned(),
        project_id: project_id.map(str::to_owned),
        cwd: cwd.map(str::to_owned),
        tool_policy: ToolPolicy::None,
        steerable: false,
        wait_for_final_text: true,
    }
}

pub(super) async fn run_no_tools(
    state: &AppState,
    request: NoToolsRunRequest,
) -> Result<(String, String), String> {
    debug_assert_eq!(request.tool_policy, ToolPolicy::None);
    let id = crate::execution::runs::create_no_tools_run(
        state,
        request.prompt,
        request.project_id,
        request.cwd,
    )
    .await
    .map_err(|error| format!("could not start the run: {error:?}"))?;
    if !request.wait_for_final_text {
        return Ok((id.to_string(), String::new()));
    }
    let deadline = tokio::time::Instant::now() + RUN_CEILING;
    loop {
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT status, stdout FROM runs WHERE id = ?")
                .bind(id)
                .fetch_optional(&state.pool)
                .await
                .map_err(|error| error.to_string())?;
        if let Some((status, stdout)) = row
            && crate::execution::runs::TERMINAL_RUN_STATUSES.contains(&status.as_str())
        {
            if status != "completed" {
                return Err(format!("run {id} ended {status}"));
            }
            let text = stdout
                .as_deref()
                .and_then(crate::execution::runner::extract_reply)
                .ok_or_else(|| format!("run {id} ended without a final text"))?;
            return Ok((id.to_string(), text));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("run {id} did not end in time"));
        }
        tokio::time::sleep(RUN_POLL).await;
    }
}

struct RecordingExtractor<'a> {
    state: &'a AppState,
    spec_id: &'a str,
    project_id: Option<&'a str>,
    cwd: &'a str,
}

#[async_trait::async_trait]
impl ExtractionRunner for RecordingExtractor<'_> {
    async fn run(&self, prompt: String) -> Result<String, String> {
        let request = no_tools_run_request(&prompt, self.project_id, Some(self.cwd));
        let (run_id, text) = run_no_tools(self.state, request).await?;
        record_extraction_run(&self.state.pool, self.spec_id, &run_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(text)
    }
}

struct DaemonVerify {
    worktree: PathBuf,
    caller_id: i64,
}

#[async_trait::async_trait]
impl VerifyRunner for DaemonVerify {
    async fn verify(&self, _spec: &Spec, _criteria: &[Criterion]) -> VerifyState {
        use crate::gate::GateOutcome;
        use crate::verify::ScopeVerdict;

        let Some(executor) = crate::verify::installed() else {
            return VerifyState::Unknown;
        };
        match crate::verify::gate_scope(
            &executor,
            crate::verify::Caller::Job(self.caller_id),
            &self.worktree,
            crate::state::DEFAULT_GATE_TIMEOUT,
        )
        .await
        {
            ScopeVerdict::Measured(GateOutcome::Passed) => VerifyState::Green,
            ScopeVerdict::Measured(GateOutcome::Failed { .. }) => VerifyState::Red,
            _ => VerifyState::Unknown,
        }
    }
}

async fn audit_worktree(project_root: &Path, spec: &Spec) -> PathBuf {
    use crate::execution::worktree as wt;

    let branch = derived_branch(&spec.path);
    let exists = wt::git()
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    if !exists {
        return project_root.to_path_buf();
    }
    let root = wt::worktree_root(project_root);
    let short: String = spec.id.chars().take(12).collect();
    let path = root.join(format!("spec-audit-{short}"));
    let ready = if path.join(".git").exists() {
        wt::git()
            .arg("-C")
            .arg(&path)
            .args(["checkout", "--quiet", "--detach"])
            .arg(&branch)
            .output()
            .await
            .is_ok_and(|output| output.status.success())
    } else {
        tokio::fs::create_dir_all(&root).await.is_ok()
            && wt::hide_root_if_nested(project_root, &root).await.is_ok()
            && wt::git()
                .arg("-C")
                .arg(project_root)
                .args(["worktree", "add", "--detach"])
                .arg(&path)
                .arg(&branch)
                .output()
                .await
                .is_ok_and(|output| output.status.success())
    };
    if ready {
        path
    } else {
        project_root.to_path_buf()
    }
}

struct DaemonPreparer<'a> {
    state: &'a AppState,
    project_root: PathBuf,
    integration_branch: String,
    state_dir: Option<PathBuf>,
}

#[async_trait::async_trait]
impl SpecPreparer for DaemonPreparer<'_> {
    async fn extract(&self, spec_id: &str) -> Result<(), String> {
        let spec = store::get_spec(&self.state.pool, spec_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("spec {spec_id} not found"))?;
        let cwd = self.project_root.to_string_lossy().into_owned();
        let runner = RecordingExtractor {
            state: self.state,
            spec_id,
            project_id: spec.project_id.as_deref(),
            cwd: &cwd,
        };
        let roots = ScanRoots {
            project_root: Some(self.project_root.clone()),
            state_dir: self.state_dir.clone(),
        };
        extract::extract(&self.state.pool, &roots, spec_id, &runner)
            .await
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
    }

    async fn audit(&self, spec_id: &str, lineage: &SpecLineage) -> Result<AuditReport, String> {
        let cwd = self.project_root.to_string_lossy().into_owned();
        let spec = store::get_spec(&self.state.pool, spec_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("spec {spec_id} not found"))?;
        let worktree = audit_worktree(&self.project_root, &spec).await;
        let reviewer = NoToolsReviewer {
            state: self.state,
            cwd: &cwd,
            worktree: &worktree,
            integration_branch: &self.integration_branch,
        };
        let verify = DaemonVerify {
            worktree: worktree.clone(),
            caller_id: crate::tree_job::landing::scope_caller_id(spec_id),
        };
        audit::audit(
            &self.state.pool,
            spec_id,
            &worktree,
            lineage,
            &reviewer,
            &verify,
        )
        .await
        .map_err(|error| format!("{error:?}"))
    }
}

struct DaemonLauncher<'a> {
    state: &'a AppState,
}

#[async_trait::async_trait]
impl TreeLauncher for DaemonLauncher<'_> {
    async fn run_tree(&self, request: &TreeRequest) -> Result<TreeOutcome, String> {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        let project_id = request
            .project_id
            .as_deref()
            .ok_or_else(|| "the spec has no project, and a tree needs one".to_owned())?;
        let body = serde_json::json!({ "id": request.id, "goal": request.goal, "feature_branch": request.feature_branch, "project_root": request.project_root, "project_id": project_id });
        let http_request = Request::builder()
            .method("POST")
            .uri("/trees")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .map_err(|error| error.to_string())?;
        let response = crate::tree_job::http::routes()
            .with_state(self.state.clone())
            .oneshot(http_request)
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .map_err(|error| error.to_string())?;
        if status != StatusCode::OK {
            return Err(format!(
                "POST /trees answered {status}: {}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        let root_node_id = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("root_node_id")
                    .and_then(|id| id.as_str().map(str::to_owned))
            })
            .ok_or_else(|| "POST /trees answered without a root node".to_owned())?;
        let deadline = tokio::time::Instant::now() + TREE_CEILING;
        loop {
            let job = crate::tree_job::store::load(&self.state.pool, &request.id)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("tree {} disappeared", request.id))?;
            if job.state == crate::tree_job::model::JOB_TERMINAL_STATE {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!("tree {} did not finish in time", request.id));
            }
            tokio::time::sleep(TREE_POLL).await;
        }
        let rows: Vec<(i64, i64)> = sqlx::query_as("SELECT id, COALESCE(lineage_root_id, id) FROM runs WHERE id IN (SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE root_id = ? AND run_id IS NOT NULL)")
            .bind(&root_node_id).fetch_all(&self.state.pool).await.map_err(|error| error.to_string())?;
        let mut outcome = TreeOutcome::default();
        for (run, root) in rows {
            outcome.run_ids.push(run.to_string());
            outcome.lineage_roots.push(root.to_string());
        }
        let mut seen = std::collections::BTreeSet::new();
        outcome
            .lineage_roots
            .retain(|root| seen.insert(root.clone()));
        Ok(outcome)
    }
}

#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub project_root: String,
    pub integration_branch: String,
    pub state_dir: Option<PathBuf>,
}

pub async fn resolve_project(
    state: &AppState,
    project_id: &str,
) -> Result<ProjectPaths, FinishError> {
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT project_root, integration_branch FROM autopilot_state WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(StoreError::from)?;
    let (root, branch) = row.unwrap_or((None, None));
    let project_root = root
        .filter(|root| !root.trim().is_empty())
        .ok_or_else(|| FinishError::Config(format!("project {project_id} has no recorded root")))?;
    let state_dir = state
        .machine_config_root
        .as_deref()
        .and_then(|root| crate::project::state::dir(root, project_id));
    Ok(ProjectPaths {
        project_root,
        integration_branch: branch
            .filter(|branch| !branch.trim().is_empty())
            .unwrap_or_else(|| "master".to_owned()),
        state_dir,
    })
}

fn comparable_root(root: &str) -> String {
    let unified = root.trim().replace('\\', "/");
    let trimmed = unified.trim_end_matches('/');
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_owned()
    }
}

pub async fn project_id_for_root(
    pool: &SqlitePool,
    root: &str,
) -> Result<Option<String>, StoreError> {
    let wanted = comparable_root(root);
    if wanted.is_empty() {
        return Ok(None);
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT project_id, project_root FROM autopilot_state WHERE project_root IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .find(|(_, stored)| comparable_root(stored) == wanted)
        .map(|(project_id, _)| project_id))
}

fn daemon_preparer<'a>(state: &'a AppState, paths: &ProjectPaths) -> DaemonPreparer<'a> {
    DaemonPreparer {
        state,
        project_root: PathBuf::from(&paths.project_root),
        integration_branch: paths.integration_branch.clone(),
        state_dir: paths.state_dir.clone(),
    }
}

async fn paths_of_spec(state: &AppState, spec_id: &str) -> Result<ProjectPaths, String> {
    let spec = store::get_spec(&state.pool, spec_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("spec {spec_id} not found"))?;
    let project_id = spec
        .project_id
        .ok_or_else(|| format!("spec {spec_id} has no project"))?;
    resolve_project(state, &project_id)
        .await
        .map_err(|error| error.to_string())
}

pub async fn extract_one(state: &AppState, spec_id: &str) -> Result<(), String> {
    let paths = paths_of_spec(state, spec_id).await?;
    daemon_preparer(state, &paths).extract(spec_id).await
}

pub async fn audit_one(state: &AppState, spec_id: &str) -> Result<AuditReport, String> {
    let paths = paths_of_spec(state, spec_id).await?;
    let lineage = super::finish::lineage_for(&state.pool, spec_id, &[])
        .await
        .map_err(|error| error.to_string())?;
    daemon_preparer(state, &paths)
        .audit(spec_id, &lineage)
        .await
}

pub async fn finish(state: &AppState, scope: FinishScope) -> Result<FinishReport, FinishError> {
    let project_id = match &scope {
        FinishScope::All { project_id } => project_id.clone(),
        FinishScope::One { spec_id } => {
            store::get_spec(&state.pool, spec_id)
                .await?
                .ok_or_else(|| StoreError::NotFound(format!("spec {spec_id}")))?
                .project_id
        }
    };
    let project_id =
        project_id.ok_or_else(|| FinishError::Config("a project is required".to_owned()))?;
    let paths = resolve_project(state, &project_id).await?;
    let config = FinishConfig {
        max_parallel: 2,
        max_rounds: 2,
        integration_branch: paths.integration_branch.clone(),
        owned_paths: BTreeMap::new(),
        worktree: PathBuf::from(&paths.project_root),
        state_dir: paths.state_dir.clone(),
        project_root: paths.project_root.clone(),
    };
    let launcher = DaemonLauncher { state };
    let preparer = daemon_preparer(state, &paths);
    finish_with(
        &state.pool,
        &FinishDeps {
            launcher: &launcher,
            preparer: &preparer,
        },
        &config,
        scope,
    )
    .await
}
