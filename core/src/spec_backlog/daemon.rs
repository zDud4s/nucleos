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
use super::model::{Criterion, Spec, SpecStatus};
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
/// How often the sweeper looks for finish intents to run.
const SWEEP_EVERY: Duration = Duration::from_secs(60);
/// How long an intent may keep coming back with specs held for disk before it is given up.
const INTENT_CEILING_HOURS: i64 = 24;

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

/// Where a spec is audited, and what its change list is taken against.
#[derive(Debug)]
struct AuditCheckout {
    path: PathBuf,
    diff_base: String,
}

/// The merge commit a finished tree landed, from its manifest (`settle_land` writes `land_sha`).
fn landed_merge_of(manifest: &crate::tree_job::model::Manifest) -> Option<String> {
    manifest
        .extra
        .get("land_sha")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|sha| !sha.is_empty())
        .map(str::to_owned)
}

/// The merge the spec's current tree landed, if it landed. A tree that cannot be read counts as
/// not landed: the audit then reads the spec branch or the integration branch.
async fn landed_merge(pool: &SqlitePool, spec: &Spec) -> Option<String> {
    let tree_id = spec.tree_id.as_deref()?;
    let job = crate::tree_job::store::load(pool, tree_id).await.ok()??;
    landed_merge_of(&job.manifest)
}

async fn branch_exists(project_root: &Path, branch: &str) -> bool {
    crate::execution::worktree::git()
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .await
        .is_ok_and(|output| output.status.success())
}

/// Picks what the audit reads. A spec tree lands into the integration branch with a `--no-ff`
/// merge, and the queue deletes `spec/<stem>` afterwards: landed work is read on the integration
/// branch, its change list taken from the merge's first parent. Work not landed is read on its
/// spec branch against the integration branch; with neither, the integration branch is what is
/// already done. Only a checkout that cannot be made falls back to the project root.
async fn audit_checkout(
    project_root: &Path,
    spec: &Spec,
    integration_branch: &str,
    landed: Option<&str>,
) -> AuditCheckout {
    let branch = derived_branch(&spec.path);
    let (target, diff_base) = if let Some(merge) = landed {
        (integration_branch.to_owned(), format!("{merge}^1"))
    } else if branch_exists(project_root, &branch).await {
        (branch, integration_branch.to_owned())
    } else {
        (integration_branch.to_owned(), integration_branch.to_owned())
    };
    match detached_audit_worktree(project_root, spec, &target).await {
        Some(path) => AuditCheckout { path, diff_base },
        None => AuditCheckout {
            path: project_root.to_path_buf(),
            diff_base: integration_branch.to_owned(),
        },
    }
}

/// The spec's audit worktree, detached at `branch`: made on first use, moved there after.
async fn detached_audit_worktree(
    project_root: &Path,
    spec: &Spec,
    branch: &str,
) -> Option<PathBuf> {
    use crate::execution::worktree as wt;

    let root = wt::worktree_root(project_root);
    let short: String = spec.id.chars().take(12).collect();
    let path = root.join(format!("spec-audit-{short}"));
    let ready = if path.join(".git").exists() {
        wt::git()
            .arg("-C")
            .arg(&path)
            .args(["checkout", "--quiet", "--detach"])
            .arg(branch)
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
                .arg(branch)
                .output()
                .await
                .is_ok_and(|output| output.status.success())
    };
    ready.then_some(path)
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
        self.audit_with(spec_id, lineage, false).await
    }

    async fn audit_before_tree(
        &self,
        spec_id: &str,
        lineage: &SpecLineage,
    ) -> Result<AuditReport, String> {
        self.audit_with(spec_id, lineage, true).await
    }
}

impl DaemonPreparer<'_> {
    /// The audit both entry points share; `before_tree` raises no owner question.
    async fn audit_with(
        &self,
        spec_id: &str,
        lineage: &SpecLineage,
        before_tree: bool,
    ) -> Result<AuditReport, String> {
        let cwd = self.project_root.to_string_lossy().into_owned();
        let spec = store::get_spec(&self.state.pool, spec_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("spec {spec_id} not found"))?;
        let landed = landed_merge(&self.state.pool, &spec).await;
        let checkout = audit_checkout(
            &self.project_root,
            &spec,
            &self.integration_branch,
            landed.as_deref(),
        )
        .await;
        let worktree = checkout.path;
        let reviewer = NoToolsReviewer {
            state: self.state,
            cwd: &cwd,
            worktree: &worktree,
            diff_base: &checkout.diff_base,
        };
        let verify = DaemonVerify {
            worktree: worktree.clone(),
            caller_id: crate::tree_job::landing::scope_caller_id(spec_id),
        };
        let pool = &self.state.pool;
        let report = if before_tree {
            audit::audit_before_tree(pool, spec_id, &worktree, lineage, &reviewer, &verify).await
        } else {
            audit::audit(pool, spec_id, &worktree, lineage, &reviewer, &verify).await
        };
        report.map_err(|error| format!("{error:?}"))
    }
}

struct DaemonLauncher<'a> {
    state: &'a AppState,
    /// The checkout the trees are built beside; its volume is the one the disk floor is read on.
    project_root: PathBuf,
}

impl DaemonLauncher<'_> {
    /// Waits until the tree's durable job is terminal, then collects the runs it made and their
    /// lineage roots. A tree with no job does not exist; one still live at the ceiling is reported
    /// as such.
    async fn wait_for_job(&self, tree_id: &str) -> Result<TreeOutcome, String> {
        let deadline = tokio::time::Instant::now() + TREE_CEILING;
        let root_node_id = loop {
            let job = crate::tree_job::store::load(&self.state.pool, tree_id)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("tree {tree_id} does not exist"))?;
            if job.state == crate::tree_job::model::JOB_TERMINAL_STATE {
                break job.root_node_id;
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(TreeOutcome {
                    still_running: true,
                    ..Default::default()
                });
            }
            tokio::time::sleep(TREE_POLL).await;
        };
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

/// The landing mode of every spec tree. Owner, 2026-11-04: "once a spec enters, the tree must
/// guarantee everything is done until the end" — so the tree lands its work into the project's
/// integration branch instead of leaving it held on `spec/<stem>`, and a Done audit means the work
/// is on the integration branch.
const SPEC_TREE_LANDING: &str = "integration";

/// The `POST /trees` body for one spec tree.
fn spec_tree_body(request: &TreeRequest) -> Result<serde_json::Value, String> {
    let project_id = request
        .project_id
        .as_deref()
        .ok_or_else(|| "the spec has no project, and a tree needs one".to_owned())?;
    Ok(serde_json::json!({
        "id": request.id,
        "goal": request.goal,
        "feature_branch": request.feature_branch,
        "project_root": request.project_root,
        "project_id": project_id,
        "land": SPEC_TREE_LANDING,
    }))
}

/// Launches the spec's tree through the daemon's own `POST /trees` route.
async fn post_spec_tree(state: &AppState, request: &TreeRequest) -> Result<(), String> {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let body = spec_tree_body(request)?;
    let http_request = Request::builder()
        .method("POST")
        .uri("/trees")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .map_err(|error| error.to_string())?;
    let response = crate::tree_job::http::routes()
        .with_state(state.clone())
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
    Ok(())
}

/// Marks the spec's tree as launched once the daemon has accepted it, before the wait for its end.
async fn accepted(pool: &SqlitePool, request: &TreeRequest) -> Result<(), String> {
    store::set_tree_launched(pool, &request.spec_id)
        .await
        .map_err(|error| error.to_string())
}

#[async_trait::async_trait]
impl TreeLauncher for DaemonLauncher<'_> {
    async fn run_tree(&self, request: &TreeRequest) -> Result<TreeOutcome, String> {
        post_spec_tree(self.state, request).await?;
        // The tree exists from here on: record it now, so a restart mid-tree is not read as "never launched".
        accepted(&self.state.pool, request).await?;
        self.wait_for_job(&request.id).await
    }

    async fn wait_tree(&self, tree_id: &str) -> Result<TreeOutcome, String> {
        self.wait_for_job(tree_id).await
    }

    fn free_disk_bytes(&self) -> u64 {
        // A volume that cannot be read counts as full: the spec waits, it is not failed.
        crate::health::available_space(&self.project_root).unwrap_or(0)
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

/// One finish pass. A spec held back for want of disk is reported in `deferred`, never failed and
/// never put to the owner: the caller keeps its finish intent, and the sweeper runs another pass.
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
    let (tree_cap, min_free_disk_bytes) = tree_speed(&paths.project_root);
    let config = FinishConfig {
        max_parallel: tree_cap,
        max_rounds: 2,
        integration_branch: paths.integration_branch.clone(),
        owned_paths: owned_paths_by_spec(&state.pool, &project_id).await,
        min_free_disk_bytes,
        worktree: PathBuf::from(&paths.project_root),
        state_dir: paths.state_dir.clone(),
        project_root: paths.project_root.clone(),
    };
    let launcher = DaemonLauncher {
        state,
        project_root: PathBuf::from(&paths.project_root),
    };
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

/// `tree.max_live_branches` and `tree.floor_gb` (as bytes) of the project's `speed.yaml`: the
/// limits tree admission applies, else its defaults (4 branches, 15 GB).
fn tree_speed(project_root: &str) -> (usize, u64) {
    const BYTES_PER_GB: f64 = 1_073_741_824.0;
    let doc = std::fs::read_to_string(Path::new(project_root).join(".nucleos/workflow/speed.yaml"))
        .ok()
        .and_then(|text| serde_yaml::from_str::<serde_yaml::Value>(&text).ok());
    let tree = doc.as_ref().and_then(|doc| doc.get("tree"));
    let cap = tree
        .and_then(|tree| tree.get("max_live_branches"))
        .and_then(serde_yaml::Value::as_u64)
        .and_then(|cap| usize::try_from(cap).ok())
        .unwrap_or(4);
    let floor_gb = tree
        .and_then(|tree| tree.get("floor_gb"))
        .and_then(serde_yaml::Value::as_f64)
        .unwrap_or(15.0);
    (cap, (floor_gb.max(0.0) * BYTES_PER_GB) as u64)
}

/// The files each unfinished spec's criteria name, by spec id. A spec naming none, or one whose
/// criteria are not read yet, has an unknown scope. Best effort: a failed read means no paths.
async fn owned_paths_by_spec(pool: &SqlitePool, project_id: &str) -> BTreeMap<String, Vec<String>> {
    let specs = store::list_specs_by_project(pool, Some(project_id))
        .await
        .unwrap_or_default();
    let mut owned = BTreeMap::new();
    for spec in specs.iter().filter(|spec| spec.status != SpecStatus::Done) {
        let criteria = store::list_criteria(pool, &spec.id)
            .await
            .unwrap_or_default();
        let named = super::review::paths_named_by(&criteria);
        if !named.is_empty() {
            owned.insert(spec.id.clone(), named);
        }
    }
    owned
}

/// The projects with an `in_progress` spec: a restart cut their finish off. A spec with no tree id
/// is included: a crash between its status write and its tree id write left it half-recorded, and
/// the finish runs its round again (no tree exists yet, as the id is recorded before the launch).
async fn resume_candidates(pool: &SqlitePool) -> Result<Vec<String>, StoreError> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT project_id FROM specs WHERE status = 'in_progress' \
         AND project_id IS NOT NULL ORDER BY project_id",
    )
    .fetch_all(pool)
    .await?)
}

/// Writes, once per boot, a finish intent for each project a restart interrupted. It starts
/// nothing: the sweep that follows runs the intents, and the finish adopts the trees its specs
/// already have instead of starting others. An intent already there keeps its age.
pub async fn resume_finishes(state: &AppState) {
    let projects = match resume_candidates(&state.pool).await {
        Ok(projects) => projects,
        Err(error) => {
            tracing::warn!(%error, "could not look for spec finishes to resume");
            return;
        }
    };
    let pending: Vec<String> = match store::finish_intents(&state.pool).await {
        Ok(intents) => intents
            .into_iter()
            .map(|(project_id, _)| project_id)
            .collect(),
        Err(error) => {
            tracing::warn!(%error, "could not read the spec finish intents");
            return;
        }
    };
    for project_id in projects.iter().filter(|id| !pending.contains(id)) {
        if let Err(error) = store::write_finish_intent(&state.pool, project_id).await {
            tracing::warn!(%error, %project_id, "could not record a spec finish to resume");
        }
    }
}

/// Settles a finish intent from the pass that served it. A pass with nothing deferred, or one that
/// failed (the feed says so), deletes it; a pass that held specs for disk keeps it for the next
/// sweep, until it is `INTENT_CEILING_HOURS` old, when it is announced once and dropped. Only the
/// intent as it was when the pass began is deleted: one re-written since is a newer request.
///
/// The intent is the project's, not one spec's. A pass for one spec never deletes it: it only
/// writes one (keeping the age of any that stands) when it deferred something, so a spec held back
/// for disk is not forgotten. Only a whole-project pass settles an intent away.
pub async fn settle_intent(
    pool: &SqlitePool,
    project_id: &str,
    created_at: &str,
    scope: &FinishScope,
    result: &Result<FinishReport, FinishError>,
) {
    let whole_project = matches!(scope, FinishScope::All { .. });
    let settled = match result {
        Ok(report) if report.deferred.is_empty() => whole_project,
        Ok(_) if !whole_project => {
            if let Err(error) = store::ensure_finish_intent(pool, project_id, created_at).await {
                tracing::warn!(%error, %project_id, "could not keep a spec finish intent");
            }
            false
        }
        Ok(report) => {
            let age = chrono::DateTime::parse_from_rfc3339(created_at)
                .map(|at| chrono::Utc::now() - at.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Duration::zero());
            if age > chrono::Duration::hours(INTENT_CEILING_HOURS) {
                // A line in the feed, never a decision: answering cannot make the disk bigger.
                super::finish::announce_failure(
                    pool,
                    project_id,
                    &format!(
                        "the disk has stayed short for {INTENT_CEILING_HOURS} hours; {} spec(s) \
                         could not start for it ({}). Free some disk and ask for the finish again",
                        report.deferred.len(),
                        report.deferred.join(", ")
                    ),
                )
                .await;
                true
            } else {
                false
            }
        }
        Err(error) => {
            tracing::warn!(%error, %project_id, "the spec finish failed");
            super::finish::announce_failure(pool, project_id, &error.to_string()).await;
            whole_project
        }
    };
    if settled && let Err(error) = store::delete_finish_intent(pool, project_id, created_at).await {
        tracing::warn!(%error, %project_id, "could not delete a spec finish intent");
    }
}

/// Runs one pass for each project with a finish intent and no finish under way, then settles the
/// intent from the pass. A claimed project is skipped with its intent untouched. Returns when every
/// pass it started has ended; passes of different projects run together.
async fn sweep_with<P, Fut>(state: &AppState, pass: P)
where
    P: Fn(AppState, String) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = Result<FinishReport, FinishError>> + Send + 'static,
{
    let intents = match store::finish_intents(&state.pool).await {
        Ok(intents) => intents,
        Err(error) => {
            tracing::warn!(%error, "could not read the spec finish intents");
            return;
        }
    };
    let mut passes = tokio::task::JoinSet::new();
    for (project_id, created_at) in intents {
        let Some(claim) = super::finish::claim_project(&project_id) else {
            continue;
        };
        let state = state.clone();
        let pass = pass.clone();
        passes.spawn(async move {
            // Held until the pass is settled, then free for an owner request to take.
            let _claim = claim;
            let result = pass(state.clone(), project_id.clone()).await;
            // The sweep always runs a whole-project pass.
            let scope = FinishScope::All {
                project_id: Some(project_id.clone()),
            };
            settle_intent(&state.pool, &project_id, &created_at, &scope, &result).await;
        });
    }
    while let Some(ended) = passes.join_next().await {
        if let Err(error) = ended {
            tracing::warn!(%error, "a spec finish pass panicked");
        }
    }
}

/// One sweep over the finish intents: a whole-project pass for each.
pub async fn sweep_finishes(state: &AppState) {
    sweep_with(state, |state, project_id| async move {
        finish(
            &state,
            FinishScope::All {
                project_id: Some(project_id),
            },
        )
        .await
    })
    .await;
}

/// Sweeps the finish intents every `SWEEP_EVERY`, the first time one period after it starts.
pub async fn run_sweep_loop(state: AppState) {
    let mut ticker =
        tokio::time::interval_at(tokio::time::Instant::now() + SWEEP_EVERY, SWEEP_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        sweep_finishes(&state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_resumes_each_project_with_in_progress_specs_once() {
        // The durable restart entry point is the contract under test. It must find every
        // project with an in-progress spec, claim it once, and schedule one finish per project.
        let _resume_finishes = resume_finishes;
    }

    #[tokio::test]
    async fn resume_candidates_lists_each_project_with_in_progress_specs_once() {
        let pool = crate::testdb::fresh_pool().await;
        // Two projects have an in-progress spec with a tree (one of them two); a third has none.
        for (project, name) in [
            ("proj-a", "first"),
            ("proj-a", "second"),
            ("proj-b", "only"),
        ] {
            let path = format!("<state_dir>/specs/2026-01-01-{name}.md");
            let spec = store::upsert_by_path(&pool, Some(project), &path, name, "sha-1")
                .await
                .unwrap();
            store::set_status(&pool, &spec.id, SpecStatus::InProgress, None)
                .await
                .unwrap();
            store::set_tree_id(&pool, &spec.id, &format!("tree-{project}-{name}"))
                .await
                .unwrap();
        }
        let idle = store::upsert_by_path(
            &pool,
            Some("proj-c"),
            "<state_dir>/specs/2026-01-01-idle.md",
            "idle",
            "sha-1",
        )
        .await
        .unwrap();
        store::set_status(&pool, &idle.id, SpecStatus::Extracted, None)
            .await
            .unwrap();

        let projects = resume_candidates(&pool).await.unwrap();

        assert_eq!(projects, vec!["proj-a".to_owned(), "proj-b".to_owned()]);
    }

    #[tokio::test]
    async fn resume_candidates_includes_a_spec_with_no_tree_id() {
        let pool = crate::testdb::fresh_pool().await;
        // A crash between the status write and the tree id write left this one half-recorded.
        let spec = store::upsert_by_path(
            &pool,
            Some("proj-null-tree"),
            "<state_dir>/specs/2026-01-01-null-tree.md",
            "null-tree",
            "sha-1",
        )
        .await
        .unwrap();
        store::set_status(&pool, &spec.id, SpecStatus::InProgress, None)
            .await
            .unwrap();
        assert_eq!(
            store::get_spec(&pool, &spec.id)
                .await
                .unwrap()
                .unwrap()
                .tree_id,
            None
        );

        let projects = resume_candidates(&pool).await.unwrap();

        assert_eq!(projects, vec!["proj-null-tree".to_owned()]);
    }

    #[tokio::test]
    async fn the_launch_is_recorded_while_the_tree_still_runs() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = store::upsert_by_path(
            &pool,
            Some("launch-recorded"),
            "<state_dir>/specs/2026-01-01-launch-recorded.md",
            "launch-recorded",
            "sha-1",
        )
        .await
        .unwrap();
        let request = TreeRequest {
            id: "tree-launch-recorded".to_owned(),
            spec_id: spec.id.clone(),
            goal: "Keep running".to_owned(),
            feature_branch: "feat/launch-recorded".to_owned(),
            project_root: "C:/repo".to_owned(),
            project_id: Some("launch-recorded".to_owned()),
            round: 1,
        };

        accepted(&pool, &request).await.unwrap();

        assert!(
            store::get_spec(&pool, &spec.id)
                .await
                .unwrap()
                .unwrap()
                .tree_launched
        );
    }

    // ---- A spec tree lands, and its audit reads what landed -----------------------------------------

    fn spec_tree_request(id: &str, project_root: &str) -> TreeRequest {
        TreeRequest {
            id: id.to_owned(),
            spec_id: "spec-lands".to_owned(),
            goal: "Finish the spec".to_owned(),
            feature_branch: "spec/2026-01-01-lands".to_owned(),
            project_root: project_root.to_owned(),
            project_id: Some("proj-lands".to_owned()),
            round: 1,
        }
    }

    /// Owner, 2026-11-04: once a spec enters, its tree carries the work to the end — the body asks
    /// the daemon to land into the integration branch, never the route's `hold` default.
    #[test]
    fn a_spec_tree_is_launched_to_land_into_the_integration_branch() {
        let body = spec_tree_body(&spec_tree_request("tree-lands", "C:/repo")).unwrap();

        assert_eq!(body["land"], serde_json::json!("integration"));
        assert_eq!(body["id"], serde_json::json!("tree-lands"));
        assert_eq!(
            body["feature_branch"],
            serde_json::json!("spec/2026-01-01-lands")
        );
        assert_eq!(body["project_root"], serde_json::json!("C:/repo"));
        assert_eq!(body["project_id"], serde_json::json!("proj-lands"));
    }

    #[test]
    fn a_spec_tree_without_a_project_is_not_launched() {
        let mut request = spec_tree_request("tree-no-project", "C:/repo");
        request.project_id = None;

        assert!(spec_tree_body(&request).is_err());
    }

    /// Through the real route: the stored job is marked to land, which is what the driver reads.
    #[tokio::test]
    async fn a_spec_tree_posted_to_the_daemon_is_stored_to_land() {
        let (state, _runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        let root = std::env::temp_dir().to_string_lossy().replace('\\', "/");

        post_spec_tree(&state, &spec_tree_request("tree-posted", &root))
            .await
            .unwrap();

        let job = crate::tree_job::store::load(&state.pool, "tree-posted")
            .await
            .unwrap()
            .expect("the job is stored");
        assert_eq!(
            job.manifest.extra.get("landing"),
            Some(&serde_json::json!("integration"))
        );
    }

    #[test]
    fn the_landed_merge_is_the_manifest_s_land_sha() {
        let mut manifest = crate::tree_job::model::Manifest::new(
            "tree-x",
            "spec/2026-01-01-x",
            "goal",
            "C:/repo",
            &[],
        );
        assert_eq!(landed_merge_of(&manifest), None);
        manifest
            .extra
            .insert("land_sha".to_owned(), serde_json::json!("abc123"));
        assert_eq!(landed_merge_of(&manifest).as_deref(), Some("abc123"));
    }

    /// A repository with `master` at one commit and the main checkout parked on `elsewhere`, which
    /// carries a commit of its own, so that reading the main checkout is told apart from reading
    /// the integration branch.
    struct AuditRepo {
        _container: tempfile::TempDir,
        path: PathBuf,
    }

    impl AuditRepo {
        fn new() -> AuditRepo {
            use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
            let container = space_free_tempdir("spec-audit-");
            let path = container.path().join("repo");
            initialize_repo(&path);
            let repo = AuditRepo {
                _container: container,
                path,
            };
            repo.git(&["checkout", "-B", "master"]);
            repo.commit("base.txt", "base\n");
            repo
        }

        fn git(&self, args: &[&str]) -> String {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&self.path)
                .args(args)
                .output()
                .expect("git should start");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }

        fn commit(&self, file: &str, text: &str) {
            std::fs::write(self.path.join(file), text).unwrap();
            self.git(&["add", file]);
            self.git(&["commit", "-q", "-m", file]);
        }

        fn park_elsewhere(&self) {
            self.git(&["checkout", "-q", "-b", "elsewhere"]);
            self.commit("noise.txt", "not the integration branch\n");
        }
    }

    fn head_of(path: &Path) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    async fn audit_spec(pool: &SqlitePool, name: &str) -> Spec {
        store::upsert_by_path(
            pool,
            Some("proj-audit"),
            &format!("<state_dir>/specs/2026-01-01-{name}.md"),
            name,
            "sha-1",
        )
        .await
        .unwrap()
    }

    async fn changed_paths(checkout: &AuditCheckout) -> Vec<String> {
        super::super::review::gather_material(&checkout.path, &checkout.diff_base, &[])
            .await
            .files
            .into_iter()
            .map(|file| file.path)
            .collect()
    }

    /// The landing merged `spec/<stem>` with `--no-ff` and deleted it. The audit reads the
    /// integration branch, not the owner's checkout, and its change list is what the merge brought.
    #[tokio::test]
    async fn after_a_landing_the_audit_reads_the_integration_branch_and_what_landed() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = audit_spec(&pool, "landed").await;
        let repo = AuditRepo::new();
        let branch = derived_branch(&spec.path);
        repo.git(&["checkout", "-q", "-b", &branch]);
        repo.commit("landed.txt", "the spec's work\n");
        repo.git(&["checkout", "-q", "master"]);
        repo.git(&["merge", "-q", "--no-ff", "-m", "merge spec", &branch]);
        let merge = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&["branch", "-q", "-D", &branch]);
        repo.park_elsewhere();

        let checkout = audit_checkout(&repo.path, &spec, "master", Some(merge.as_str())).await;

        assert_ne!(checkout.path, repo.path, "the owner's checkout was audited");
        assert_eq!(head_of(&checkout.path), repo.git(&["rev-parse", "master"]));
        assert_eq!(
            changed_paths(&checkout).await,
            vec!["landed.txt".to_owned()]
        );
    }

    /// Unchanged: a spec branch that has not landed is audited against the integration branch.
    #[tokio::test]
    async fn before_a_landing_the_audit_reads_the_spec_branch() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = audit_spec(&pool, "held").await;
        let repo = AuditRepo::new();
        let branch = derived_branch(&spec.path);
        repo.git(&["checkout", "-q", "-b", &branch]);
        repo.commit("held.txt", "the spec's work\n");
        repo.git(&["checkout", "-q", "master"]);

        let checkout = audit_checkout(&repo.path, &spec, "master", None).await;

        assert_eq!(head_of(&checkout.path), repo.git(&["rev-parse", &branch]));
        assert_eq!(checkout.diff_base, "master");
        assert_eq!(changed_paths(&checkout).await, vec!["held.txt".to_owned()]);
    }

    /// No spec branch and nothing landed (the audit before the first tree): the integration
    /// branch is what is already done, whatever the owner's checkout stands on.
    #[tokio::test]
    async fn with_no_spec_branch_the_audit_reads_the_integration_branch() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = audit_spec(&pool, "fresh").await;
        let repo = AuditRepo::new();
        repo.park_elsewhere();

        let checkout = audit_checkout(&repo.path, &spec, "master", None).await;

        assert_eq!(head_of(&checkout.path), repo.git(&["rev-parse", "master"]));
    }

    // ---- Fix C: durable finish intents and the sweeper ---------------------------------------------
    //
    // Assumed seams (the implementer follows these exactly):
    //   store::write_finish_intent(pool, project_id) -> Result<(), StoreError>
    //       upsert; created_at = now, so re-writing bumps it.
    //   store::write_finish_intent_at(pool, project_id, created_at: &str) -> Result<(), StoreError>
    //       the same upsert with an explicit RFC3339 created_at.
    //   store::finish_intents(pool) -> Result<Vec<(String, String)>, StoreError>
    //       (project_id, created_at), ordered by project_id.
    //   async fn sweep_with<P, Fut>(state: &AppState, pass: P)
    //       where P: Fn(AppState, String) -> Fut + Clone + Send + 'static,
    //             Fut: Future<Output = Result<FinishReport, FinishError>> + Send + 'static
    //       Claims each intent's unclaimed project, runs `pass(state.clone(), project_id)` for it,
    //       settles the intent from the result, releases the claim, and returns only when every pass
    //       it started has ended. `sweep_finishes(state)` is `sweep_with` over `finish` of
    //       `FinishScope::All`, run in a spawned task by its callers.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::spec_backlog::finish::{FinishError, FinishReport, claim_project};

    async fn sweep_state() -> AppState {
        crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await
        .0
    }

    async fn intents(state: &AppState) -> Vec<(String, String)> {
        store::finish_intents(&state.pool).await.unwrap()
    }

    async fn intent_projects(state: &AppState) -> Vec<String> {
        intents(state).await.into_iter().map(|(p, _)| p).collect()
    }

    async fn feed_lines(state: &AppState, subject: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed WHERE subject = ? ORDER BY rowid")
            .bind(subject)
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    fn hours_ago(hours: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::hours(hours)).to_rfc3339()
    }

    /// Projects each pass was run for, in the order the passes started.
    type PassLog = Arc<std::sync::Mutex<Vec<String>>>;

    fn logged(log: &PassLog) -> Vec<String> {
        let mut ran = log.lock().unwrap().clone();
        ran.sort();
        ran
    }

    #[tokio::test]
    async fn an_answer_writes_its_intent_in_the_same_transaction() {
        let state = sweep_state().await;
        let pool = &state.pool;
        let waiting = store::upsert_by_path(
            pool,
            Some("proj-ans-a"),
            "<state_dir>/specs/2026-01-01-ans-a.md",
            "ans-a",
            "sha-1",
        )
        .await
        .unwrap();
        store::set_status(pool, &waiting.id, SpecStatus::NeedsOwner, Some("asked"))
            .await
            .unwrap();
        let question = store::add_question(pool, &waiting.id, "which way?")
            .await
            .unwrap();
        // A spec that is not waiting on the owner: its answer reopens nothing.
        let running = store::upsert_by_path(
            pool,
            Some("proj-ans-b"),
            "<state_dir>/specs/2026-01-01-ans-b.md",
            "ans-b",
            "sha-1",
        )
        .await
        .unwrap();
        store::set_status(pool, &running.id, SpecStatus::InProgress, None)
            .await
            .unwrap();
        let other = store::add_question(pool, &running.id, "and this?")
            .await
            .unwrap();
        assert!(intents(&state).await.is_empty());

        let (_, reopened) = store::answer_question(pool, &question, "this way")
            .await
            .unwrap();
        assert!(reopened);
        assert_eq!(intent_projects(&state).await, vec!["proj-ans-a".to_owned()]);

        // An answer that reopens nothing, and an answer refused, leave no intent behind.
        let (_, reopened) = store::answer_question(pool, &other, "that").await.unwrap();
        assert!(!reopened);
        assert!(
            store::answer_question(pool, &question, "again")
                .await
                .is_err()
        );
        assert_eq!(
            intent_projects(&state).await,
            vec!["proj-ans-a".to_owned()],
            "exactly the reopening answer wrote an intent"
        );
    }

    #[tokio::test]
    async fn sweep_runs_one_pass_per_project_and_deletes_a_finished_intent() {
        let state = sweep_state().await;
        for project in ["sweep-one-a", "sweep-one-b"] {
            store::write_finish_intent(&state.pool, project)
                .await
                .unwrap();
        }
        let ran = PassLog::default();
        let pass = {
            let ran = ran.clone();
            move |_state: AppState, project: String| {
                let ran = ran.clone();
                async move {
                    ran.lock().unwrap().push(project);
                    Ok::<_, FinishError>(FinishReport::default())
                }
            }
        };

        sweep_with(&state, pass.clone()).await;

        assert_eq!(
            logged(&ran),
            vec!["sweep-one-a".to_owned(), "sweep-one-b".to_owned()],
            "one pass per project, no more"
        );
        assert!(
            intents(&state).await.is_empty(),
            "a finished pass deletes its intent"
        );
        assert!(
            claim_project("sweep-one-a").is_some(),
            "the claim is released"
        );
        assert!(claim_project("sweep-one-b").is_some());
        assert!(feed_lines(&state, "sweep-one-a").await.is_empty());

        // Nothing left: a second sweep runs no pass.
        sweep_with(&state, pass).await;
        assert_eq!(ran.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sweep_skips_a_claimed_project_and_keeps_its_intent() {
        let state = sweep_state().await;
        store::write_finish_intent_at(&state.pool, "sweep-busy", &hours_ago(1))
            .await
            .unwrap();
        store::write_finish_intent(&state.pool, "sweep-free")
            .await
            .unwrap();
        let busy_stamp = intents(&state)
            .await
            .into_iter()
            .find(|(project, _)| project == "sweep-busy")
            .map(|(_, stamp)| stamp)
            .unwrap();
        let ran = PassLog::default();
        let pass = {
            let ran = ran.clone();
            move |_state: AppState, project: String| {
                let ran = ran.clone();
                async move {
                    ran.lock().unwrap().push(project);
                    Ok::<_, FinishError>(FinishReport::default())
                }
            }
        };
        let claim = claim_project("sweep-busy").expect("nothing else holds the claim");

        sweep_with(&state, pass.clone()).await;

        assert_eq!(logged(&ran), vec!["sweep-free".to_owned()]);
        assert_eq!(
            intents(&state).await,
            vec![("sweep-busy".to_owned(), busy_stamp)],
            "the busy project's intent is kept untouched"
        );
        assert!(
            feed_lines(&state, "sweep-busy").await.is_empty(),
            "nothing is announced"
        );

        // Once the claim is free, the next sweep runs it.
        drop(claim);
        sweep_with(&state, pass).await;
        assert_eq!(
            logged(&ran),
            vec!["sweep-busy".to_owned(), "sweep-free".to_owned()]
        );
        assert!(intents(&state).await.is_empty());
    }

    // Fix H seam, assumed: `settle_intent(pool, project_id, created_at, &scope, &result)` takes the
    // scope of the pass. A `FinishScope::One` pass writes no intent before it starts; afterwards a
    // deferral writes the project intent if none exists (an existing one keeps its age), and nothing
    // deferred leaves any intent alone. Only a `FinishScope::All` pass deletes one.
    #[tokio::test]
    async fn a_one_spec_pass_that_deferred_writes_the_project_intent() {
        let state = sweep_state().await;
        assert!(intents(&state).await.is_empty());
        let scope = FinishScope::One {
            spec_id: "spec-held".to_owned(),
        };
        let deferred = Ok::<_, FinishError>(FinishReport {
            deferred: vec!["spec-held".to_owned()],
            ..Default::default()
        });

        settle_intent(
            &state.pool,
            "one-deferred",
            &hours_ago(0),
            &scope,
            &deferred,
        )
        .await;

        assert_eq!(
            intent_projects(&state).await,
            vec!["one-deferred".to_owned()]
        );
    }

    #[tokio::test]
    async fn a_one_spec_pass_that_deferred_keeps_the_age_of_an_existing_intent() {
        let state = sweep_state().await;
        store::write_finish_intent_at(&state.pool, "one-aged", &hours_ago(3))
            .await
            .unwrap();
        let stamp = intents(&state).await[0].1.clone();
        let scope = FinishScope::One {
            spec_id: "spec-held".to_owned(),
        };
        let deferred = Ok::<_, FinishError>(FinishReport {
            deferred: vec!["spec-held".to_owned()],
            ..Default::default()
        });

        settle_intent(&state.pool, "one-aged", &hours_ago(0), &scope, &deferred).await;

        assert_eq!(intents(&state).await, vec![("one-aged".to_owned(), stamp)]);
    }

    #[tokio::test]
    async fn a_one_spec_pass_with_nothing_deferred_leaves_an_intent_alone() {
        let state = sweep_state().await;
        store::write_finish_intent_at(&state.pool, "one-clean", &hours_ago(1))
            .await
            .unwrap();
        let stamp = intents(&state).await[0].1.clone();
        let scope = FinishScope::One {
            spec_id: "spec-fine".to_owned(),
        };
        let clean = Ok::<_, FinishError>(FinishReport::default());

        // Both the stamp the intent has and a stamp it never had must leave it standing.
        settle_intent(&state.pool, "one-clean", &stamp, &scope, &clean).await;
        settle_intent(&state.pool, "one-clean", &hours_ago(0), &scope, &clean).await;

        assert_eq!(intents(&state).await, vec![("one-clean".to_owned(), stamp)]);
    }

    #[tokio::test]
    async fn a_whole_project_pass_with_nothing_deferred_deletes_the_intent() {
        let state = sweep_state().await;
        store::write_finish_intent_at(&state.pool, "all-clean", &hours_ago(1))
            .await
            .unwrap();
        let stamp = intents(&state).await[0].1.clone();
        let scope = FinishScope::All {
            project_id: Some("all-clean".to_owned()),
        };

        settle_intent(
            &state.pool,
            "all-clean",
            &stamp,
            &scope,
            &Ok::<_, FinishError>(FinishReport::default()),
        )
        .await;

        assert!(intents(&state).await.is_empty());
    }

    #[tokio::test]
    async fn a_deferred_pass_keeps_the_intent_and_releases_the_claim() {
        let state = sweep_state().await;
        store::write_finish_intent_at(&state.pool, "sweep-deferred", &hours_ago(1))
            .await
            .unwrap();
        let stamp = intents(&state).await[0].1.clone();
        let passes = Arc::new(AtomicUsize::new(0));
        let pass = {
            let passes = passes.clone();
            move |_state: AppState, _project: String| {
                let passes = passes.clone();
                async move {
                    passes.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, FinishError>(FinishReport {
                        deferred: vec!["spec-short-of-disk".to_owned()],
                        ..Default::default()
                    })
                }
            }
        };

        sweep_with(&state, pass.clone()).await;

        assert_eq!(passes.load(Ordering::SeqCst), 1);
        assert_eq!(
            intents(&state).await,
            vec![("sweep-deferred".to_owned(), stamp)],
            "the intent is kept, with its original created_at"
        );
        assert!(
            claim_project("sweep-deferred").is_some(),
            "the claim is free, so an owner request can run meanwhile"
        );
        assert!(
            feed_lines(&state, "sweep-deferred").await.is_empty(),
            "inside the ceiling nothing is announced"
        );

        // The next sweep retries it.
        sweep_with(&state, pass).await;
        assert_eq!(passes.load(Ordering::SeqCst), 2);
        assert_eq!(
            intent_projects(&state).await,
            vec!["sweep-deferred".to_owned()]
        );
    }

    #[tokio::test]
    async fn an_intent_rewritten_during_a_pass_survives_it() {
        let state = sweep_state().await;
        store::write_finish_intent_at(&state.pool, "sweep-rewrite", &hours_ago(1))
            .await
            .unwrap();
        let first_stamp = intents(&state).await[0].1.clone();
        let passes = Arc::new(AtomicUsize::new(0));
        let pass = {
            let passes = passes.clone();
            move |state: AppState, project: String| {
                let passes = passes.clone();
                async move {
                    // The first pass is still running when an owner request arrives.
                    if passes.fetch_add(1, Ordering::SeqCst) == 0 {
                        store::write_finish_intent(&state.pool, &project)
                            .await
                            .unwrap();
                    }
                    Ok::<_, FinishError>(FinishReport::default())
                }
            }
        };

        sweep_with(&state, pass.clone()).await;

        assert_eq!(passes.load(Ordering::SeqCst), 1);
        let left = intents(&state).await;
        assert_eq!(
            left.len(),
            1,
            "the request that arrived during the pass is not lost"
        );
        assert_eq!(left[0].0, "sweep-rewrite");
        assert_ne!(left[0].1, first_stamp, "re-writing bumped created_at");

        // It gets its own pass, and that one deletes it.
        sweep_with(&state, pass).await;
        assert_eq!(passes.load(Ordering::SeqCst), 2);
        assert!(intents(&state).await.is_empty());
    }

    #[tokio::test]
    async fn an_intent_past_the_ceiling_is_announced_once_and_deleted() {
        let state = sweep_state().await;
        store::write_finish_intent_at(&state.pool, "sweep-stale", &hours_ago(25))
            .await
            .unwrap();
        // Inside the ceiling the same deferral just waits.
        store::write_finish_intent_at(&state.pool, "sweep-young", &hours_ago(23))
            .await
            .unwrap();
        let passes = Arc::new(AtomicUsize::new(0));
        let pass = {
            let passes = passes.clone();
            move |_state: AppState, _project: String| {
                let passes = passes.clone();
                async move {
                    passes.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, FinishError>(FinishReport {
                        deferred: vec!["spec-short-of-disk".to_owned()],
                        ..Default::default()
                    })
                }
            }
        };

        sweep_with(&state, pass.clone()).await;

        assert_eq!(passes.load(Ordering::SeqCst), 2);
        assert_eq!(
            intent_projects(&state).await,
            vec!["sweep-young".to_owned()],
            "only the stale intent is deleted"
        );
        let lines = feed_lines(&state, "sweep-stale").await;
        assert_eq!(lines.len(), 1, "announced once: {lines:?}");
        assert!(
            lines[0].contains("disk"),
            "the line says the disk stayed short: {:?}",
            lines[0]
        );
        assert!(feed_lines(&state, "sweep-young").await.is_empty());

        // Later sweeps never revisit it or announce again.
        sweep_with(&state, pass).await;
        assert_eq!(
            passes.load(Ordering::SeqCst),
            3,
            "only the young intent ran again"
        );
        assert_eq!(feed_lines(&state, "sweep-stale").await.len(), 1);
        let owner_decisions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM decisions")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(owner_decisions, 0, "never a new owner decision");
    }

    #[tokio::test]
    async fn a_failed_pass_deletes_its_intent_and_announces_the_failure() {
        let state = sweep_state().await;
        store::write_finish_intent(&state.pool, "sweep-failing")
            .await
            .unwrap();
        let pass = |_state: AppState, _project: String| async {
            Err::<FinishReport, _>(FinishError::Config("no root".to_owned()))
        };

        sweep_with(&state, pass).await;

        assert!(intents(&state).await.is_empty());
        let lines = feed_lines(&state, "sweep-failing").await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("no root"));
        assert!(claim_project("sweep-failing").is_some());
    }

    #[tokio::test]
    async fn boot_writes_an_intent_for_each_project_with_a_spec_in_progress() {
        // Assumes `resume_finishes(state: &AppState)` keeps its signature but only writes the
        // intents (it no longer spawns a finish): `main.rs` sweeps right after it.
        let state = sweep_state().await;
        let spec = store::upsert_by_path(
            &state.pool,
            Some("proj-boot-intent"),
            "<state_dir>/specs/2026-01-01-boot-intent.md",
            "boot-intent",
            "sha-1",
        )
        .await
        .unwrap();
        store::set_status(&state.pool, &spec.id, SpecStatus::InProgress, None)
            .await
            .unwrap();
        store::set_tree_id(&state.pool, &spec.id, "tree-boot-intent")
            .await
            .unwrap();

        resume_finishes(&state).await;

        assert_eq!(
            intent_projects(&state).await,
            vec!["proj-boot-intent".to_owned()]
        );
    }

    #[test]
    fn finish_config_takes_its_limit_from_speed_tree_cap() {
        let source = include_str!("daemon.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        let finish_body = production
            .split("pub async fn finish")
            .nth(1)
            .expect("the finish adapter exists");

        assert!(
            finish_body.contains("max_live_branches"),
            "the finish configuration reads tree.max_live_branches from speed.yaml"
        );
        assert!(
            !finish_body.contains("max_parallel: 2"),
            "the finish configuration does not keep a literal parallelism cap"
        );
    }
}
