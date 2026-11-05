//! Tree job routes (`/trees*`): the local RPC through which nodes record their state (spec R4).
//!
//! Every refusal is `{error}`, and a field sent as `null` reads as an absent one, the conventions
//! of `http/nodes.rs`. A transition is idempotent per node, transition and key. A manifest is
//! written whole, so every handler that changes one holds a lock on its job.
//!
//! **Who reaches them.** No route here is in any `auth.rs` table, so, as for `/nodes*`, only the
//! control token and an Admin API key get in.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path as FsPath;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::execution::admission::{AdmissionBook, default_capacity, installed};
use crate::execution::runs::CreateRunError;
use crate::state::AppState;
use crate::tree_job::budget::{Admission, check_admission, layer_cost_gb};
use crate::tree_job::docs_paths::is_allowed_docs_path;
use crate::tree_job::engine::{apply_transition, overlapping_branch};
use crate::tree_job::model::{BRANCH_STATES, Branch, JOB_TERMINAL_STATE, Manifest};
use crate::tree_job::node_run::{self, NodeRunError};
use crate::tree_job::root::ROOT_BRANCH;
use crate::tree_job::store::{self, StoreError, TreeJob, launch_recovering};
use crate::work_nodes::{self, NodeError, NodeLimits, OpenArgs};

const DOCS_LAYER: &str = "docs";
/// The feed kind of the line that says a new job's root run did not start.
const ROOT_START_FAILED_KIND: &str = "work_tree_root_start_failed";
/// The states of a branch that hold a slot and grow on disk (`OPEN_STATES` in `tree_resources.py`).
/// `queued` and `parked` are not among them.
const LIVE_STATES: [&str; 7] = [
    "open",
    "building",
    "gating",
    "green",
    "red",
    "reviewing",
    "fixing",
];
/// The defaults of `tree:` in `speed.yaml`, used when a project has no config.
const DEFAULT_FLOOR_GB: f64 = 15.0;
const DEFAULT_MAX_LIVE_BRANCHES: usize = 4;
const BYTES_PER_GB: f64 = 1_073_741_824.0;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/trees", post(post_tree))
        .route("/trees/{id}", get(get_tree))
        .route("/trees/{id}/branches", post(post_branch))
        .route("/trees/{id}/nodes/{node}/transition", post(post_transition))
        .merge(super::branch_start::routes())
}

type Refusal = (StatusCode, Json<Value>);

pub(super) fn refuse(status: StatusCode, message: impl Into<String>) -> Refusal {
    (status, Json(json!({ "error": message.into() })))
}

fn bad_body(rejection: JsonRejection) -> Refusal {
    refuse(StatusCode::BAD_REQUEST, rejection.body_text())
}

fn required(field: &str, value: Option<String>) -> Result<String, Refusal> {
    value
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, format!("{field} is required")))
}

fn db_refusal(error: sqlx::Error) -> Refusal {
    tracing::warn!(%error, "tree job storage failed");
    refuse(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("database error: {error}"),
    )
}

fn store_refusal(error: StoreError) -> Refusal {
    let status = match &error {
        StoreError::NotFound(_) => StatusCode::NOT_FOUND,
        _ => {
            tracing::warn!(%error, "tree job storage failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    refuse(status, error.to_string())
}

fn node_refusal(error: NodeError) -> Refusal {
    refuse(error.status_code(), error.to_string())
}

fn run_refusal(error: NodeRunError) -> Refusal {
    let status = match &error {
        NodeRunError::NodeNotFound(_) => StatusCode::NOT_FOUND,
        NodeRunError::Unresolved { .. } => StatusCode::CONFLICT,
        NodeRunError::Create(CreateRunError::Invalid(_)) => StatusCode::UNPROCESSABLE_ENTITY,
        NodeRunError::Create(CreateRunError::Busy | CreateRunError::NoRoomOnDisk(_)) => {
            StatusCode::CONFLICT
        }
        _ => {
            tracing::warn!(%error, "tree job node run failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    refuse(status, error.to_string())
}

/// One lock per job: a manifest is loaded and written whole, so two handlers that change the same
/// job must not interleave.
pub(super) fn job_lock(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.entry(id.to_owned()).or_default().clone()
}

async fn load_job(state: &AppState, id: &str) -> Result<TreeJob, Refusal> {
    store::load(&state.pool, id)
        .await
        .map_err(store_refusal)?
        .ok_or_else(|| refuse(StatusCode::NOT_FOUND, format!("tree job {id} not found")))
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ----- budget -----

fn live_code_branches(manifest: &Manifest) -> impl Iterator<Item = &Branch> {
    manifest
        .branches
        .values()
        .filter(|branch| branch.layer != DOCS_LAYER && LIVE_STATES.contains(&branch.state.as_str()))
}

/// Decides whether a new branch of `layer` may start. Growth first: every live code branch will
/// still grow to its layer's cost, so what it has yet to take is subtracted from the free disk
/// (an absent size counts as 0). Then the cap on live branches, then [`check_admission`]. The root
/// branch only plans and delegates, so it is charged no growth, though it still takes a slot.
pub fn branch_admission(
    book: &AdmissionBook,
    manifest: &Manifest,
    layer: &str,
    free_disk_gb: f64,
    floor_gb: f64,
    max_live_branches: usize,
    live_sizes_gb: &BTreeMap<String, f64>,
) -> Admission {
    if layer == DOCS_LAYER {
        return check_admission(book, manifest, layer, free_disk_gb, floor_gb);
    }
    let growth_gb: f64 = live_code_branches(manifest)
        .filter(|branch| branch.name != ROOT_BRANCH)
        .map(|branch| {
            let size = live_sizes_gb.get(&branch.name).copied().unwrap_or(0.0);
            (layer_cost_gb(manifest, &branch.layer) - size).max(0.0)
        })
        .sum();
    let live = live_code_branches(manifest).count();
    let mut reasons = Vec::new();
    let capped = live >= max_live_branches;
    if capped {
        reasons.push(format!(
            "max_live_branches: {live} branches are live, the limit is {max_live_branches}"
        ));
    }
    let checked = check_admission(book, manifest, layer, free_disk_gb - growth_gb, floor_gb);
    let disk_only = checked.disk_only && !capped;
    reasons.extend(checked.reasons);
    if !reasons.is_empty() && growth_gb > 0.0 {
        reasons.push(format!(
            "{growth_gb:.1} GB is still to grow on the live branches and was counted against the disk"
        ));
    }
    Admission {
        admit: reasons.is_empty(),
        cost_gb: checked.cost_gb,
        reasons,
        disk_only,
    }
}

/// The first branch, by name, outside the new branch's base chain that still holds paths
/// overlapping `paths`. The chain is the base, its base, and so on, and is cut where a name repeats
/// or names no branch. It is filtered out before the search, since `overlapping_branch` answers
/// only the first overlap by name and that one may belong to the chain.
pub fn overlap_outside_base_chain(
    manifest: &Manifest,
    paths: &[String],
    base: Option<&str>,
) -> Option<String> {
    let mut chain = BTreeSet::new();
    let mut next = base;
    while let Some(name) = next {
        let Some(branch) = manifest.branches.get(name) else {
            break;
        };
        if !chain.insert(name) {
            break;
        }
        next = branch.base.as_deref();
    }
    let mut rest = manifest.clone();
    rest.branches
        .retain(|name, _| !chain.contains(name.as_str()));
    overlapping_branch(&rest, paths)
}

/// `tree.floor_gb` and `tree.max_live_branches` of the project's `speed.yaml`, else the defaults.
pub(super) fn tree_limits(project_root: &str) -> (f64, usize) {
    let doc =
        std::fs::read_to_string(FsPath::new(project_root).join(".nucleos/workflow/speed.yaml"))
            .ok()
            .and_then(|text| serde_yaml::from_str::<serde_yaml::Value>(&text).ok());
    let tree = doc.as_ref().and_then(|doc| doc.get("tree"));
    let floor = tree
        .and_then(|tree| tree.get("floor_gb"))
        .and_then(serde_yaml::Value::as_f64)
        .unwrap_or(DEFAULT_FLOOR_GB);
    let cap = tree
        .and_then(|tree| tree.get("max_live_branches"))
        .and_then(serde_yaml::Value::as_u64)
        .and_then(|cap| usize::try_from(cap).ok())
        .unwrap_or(DEFAULT_MAX_LIVE_BRANCHES);
    (floor, cap)
}

fn dir_size_bytes(root: &FsPath) -> u64 {
    let mut total = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                total += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// The measured size in GB of each live code branch's target directory. A branch with none, or one
/// that cannot be read, has no entry and counts as 0.
pub(super) async fn live_sizes_gb(manifest: &Manifest) -> BTreeMap<String, f64> {
    let targets: Vec<(String, String)> = live_code_branches(manifest)
        .filter_map(|branch| Some((branch.name.clone(), branch.target_dir.clone()?)))
        .collect();
    if targets.is_empty() {
        return BTreeMap::new();
    }
    tokio::task::spawn_blocking(move || {
        targets
            .into_iter()
            .map(|(name, dir)| {
                (
                    name,
                    dir_size_bytes(FsPath::new(&dir)) as f64 / BYTES_PER_GB,
                )
            })
            .collect::<BTreeMap<String, f64>>()
    })
    .await
    .unwrap_or_default()
}

pub(super) fn admission_book() -> Arc<AdmissionBook> {
    installed().unwrap_or_else(|| {
        Arc::new(AdmissionBook::new(
            default_capacity(),
            Duration::from_secs(900),
            Duration::from_secs(7200),
        ))
    })
}

// ----- POST /trees, GET /trees/{id} -----

#[derive(Debug, Deserialize)]
struct TreeBody {
    id: Option<String>,
    goal: Option<String>,
    feature_branch: Option<String>,
    project_root: Option<String>,
    project_id: Option<String>,
    contract: Option<String>,
    dirty_owner_files: Option<Vec<String>>,
    /// `hold` (default) or `integration`: whether the driver may land the job into master.
    land: Option<String>,
}

/// The values of `land`, the first one being the default (no tree moves master unless asked).
const LAND_MODES: [&str; 2] = ["hold", "integration"];

fn summary(job: &TreeJob, replayed: bool) -> Value {
    json!({
        "id": job.id,
        "root_node_id": job.root_node_id,
        "state": job.state,
        "replayed": replayed,
    })
}

/// The first non-blank line of the goal, cut short, as the root node's title.
fn title_of(goal: &str) -> String {
    let line = goal
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Tree job");
    line.chars().take(80).collect()
}

/// The project registered with `root`; a root no project has is the caller's to fix (400).
async fn project_of_root(state: &AppState, root: &str) -> Result<String, Refusal> {
    match crate::spec_backlog::finish::project_id_for_root(&state.pool, root).await {
        Ok(Some(project_id)) => Ok(project_id),
        Ok(None) => Err(refuse(
            StatusCode::BAD_REQUEST,
            format!(
                "project_id is required: no project is registered with the root {root}; \
                 name a project_id or register the project"
            ),
        )),
        Err(error) => Err(refuse(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not look the project of {root} up: {error}"),
        )),
    }
}

/// `POST /trees`: opens a root node, stores the job and links the two. A repeated id answers with
/// the stored job and opens nothing.
async fn post_tree(
    State(state): State<AppState>,
    body: Result<Json<TreeBody>, JsonRejection>,
) -> Result<Json<Value>, Refusal> {
    let Json(body) = body.map_err(bad_body)?;
    let id = required("id", body.id)?;
    let goal = required("goal", body.goal)?;
    let feature_branch = required("feature_branch", body.feature_branch)?;
    let project_root = required("project_root", body.project_root)?;
    // A job without a project would answer 422 at every node close, so it is refused here, where
    // the owner can still fix it. A body that names only the root gets its project the way
    // `spec_finish` finds one: by the root the project is registered with.
    let project_id = match required("project_id", body.project_id) {
        Ok(project_id) => project_id,
        Err(_) => project_of_root(&state, &project_root).await?,
    };
    let land = match body.land.as_deref().map(str::trim) {
        None | Some("") => LAND_MODES[0].to_owned(),
        Some(mode) if LAND_MODES.contains(&mode) => mode.to_owned(),
        Some(mode) => {
            return Err(refuse(
                StatusCode::BAD_REQUEST,
                format!("land must be one of {LAND_MODES:?}, not {mode:?}"),
            ));
        }
    };

    let lock = job_lock(&id);
    let guard = lock.lock().await;
    if let Some(job) = store::load(&state.pool, &id).await.map_err(store_refusal)? {
        return Ok(Json(summary(&job, true)));
    }

    let opened = work_nodes::open(
        &state.pool,
        Some(project_id.as_str()),
        &NodeLimits::default(),
        &OpenArgs {
            kind: "root".to_owned(),
            title: title_of(&goal),
            goal: goal.clone(),
            ..OpenArgs::default()
        },
    )
    .await
    .map_err(node_refusal)?;
    let dirty = body.dirty_owner_files.unwrap_or_default();
    let created = now();
    let mut manifest = Manifest::new(&id, &feature_branch, &goal, &project_root, &dirty);
    manifest.extra.insert("landing".to_owned(), json!(land));
    let job = TreeJob {
        id: id.clone(),
        root_node_id: opened.id.clone(),
        project_id: Some(project_id),
        feature_branch: Some(feature_branch.clone()),
        manifest,
        goal,
        state: "open".to_owned(),
        contract: body.contract,
        created_at: created.clone(),
        updated_at: created,
    };
    store::save(&state.pool, &job)
        .await
        .map_err(store_refusal)?;
    sqlx::query("UPDATE work_nodes SET tree_job_id = ? WHERE id = ?")
        .bind(&id)
        .bind(&opened.id)
        .execute(&state.pool)
        .await
        .map_err(db_refusal)?;
    // `start` takes the job lock in `open_branch` and `bind_branch`, and the mutex is not reentrant.
    drop(guard);
    // A root that cannot start never fails the request: the job exists and stays open, and the feed
    // says why.
    if let Err(reason) = crate::tree_job::root::start(&state, &job).await {
        tracing::warn!(job = %job.id, %reason, "tree job root run did not start");
        let line = format!(
            "tree job {} is open but its root run did not start: {reason}",
            job.id
        );
        let _ =
            crate::feed::append(&state.pool, None, ROOT_START_FAILED_KIND, &line, None, None).await;
    }
    Ok(Json(summary(&job, false)))
}

/// `GET /trees/{id}`: the job with its manifest.
async fn get_tree(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Refusal> {
    let job = load_job(&state, &id).await?;
    let manifest = serde_json::to_value(&job.manifest)
        .map_err(|error| refuse(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({
        "id": job.id,
        "root_node_id": job.root_node_id,
        "project_id": job.project_id,
        "feature_branch": job.feature_branch,
        "goal": job.goal,
        "state": job.state,
        "contract": job.contract,
        "created_at": job.created_at,
        "updated_at": job.updated_at,
        "manifest": manifest,
    })))
}

// ----- POST /trees/{id}/branches -----

#[derive(Debug, Deserialize)]
struct BranchBody {
    name: Option<String>,
    paths: Option<Vec<String>>,
    layer: Option<String>,
    base: Option<String>,
}

/// `POST /trees/{id}/branches`: adds a `queued` branch. Checked in this order: replay, finished
/// job, base, docs paths, overlap outside the base chain, admission. Nothing is stored on a refusal.
async fn post_branch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<BranchBody>, JsonRejection>,
) -> Result<Json<Value>, Refusal> {
    let Json(body) = body.map_err(bad_body)?;
    let name = required("name", body.name)?;
    let paths = body
        .paths
        .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "paths is required"))?;
    let layer = required("layer", body.layer)?;
    let base = body.base.filter(|base| !base.trim().is_empty());

    let lock = job_lock(&id);
    let _guard = lock.lock().await;
    let mut job = load_job(&state, &id).await?;

    if let Some(existing) = job.manifest.branches.get(&name) {
        if existing.paths == paths && existing.layer == layer && existing.base == base {
            return Ok(Json(json!({
                "name": name, "state": existing.state, "layer": layer, "base": base,
                "replayed": true,
            })));
        }
        return Err(refuse(
            StatusCode::CONFLICT,
            format!("branch {name} already exists with other paths, layer or base"),
        ));
    }
    if job.state == JOB_TERMINAL_STATE {
        return Err(refuse(
            StatusCode::CONFLICT,
            format!("tree job {id} is finished and takes no new branch"),
        ));
    }
    if let Some(base) = base.as_deref()
        && !job.manifest.branches.contains_key(base)
    {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("base branch {base} is not in this tree"),
        ));
    }
    if layer == DOCS_LAYER
        && let Some(path) = paths.iter().find(|path| !is_allowed_docs_path(path))
    {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{path} is not a path a docs branch may write"),
        ));
    }
    if let Some(other) = overlap_outside_base_chain(&job.manifest, &paths, base.as_deref()) {
        return Err(refuse(
            StatusCode::CONFLICT,
            format!("paths overlap branch {other}"),
        ));
    }

    let (floor_gb, max_live) = tree_limits(&job.manifest.project_root);
    let free_disk_gb = if layer == DOCS_LAYER {
        0.0
    } else {
        super::open::free_disk_gb(FsPath::new(&job.manifest.project_root)).map_err(|error| {
            refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not read the free disk space: {error}"),
            )
        })?
    };
    let sizes = live_sizes_gb(&job.manifest).await;
    let admission = branch_admission(
        &admission_book(),
        &job.manifest,
        &layer,
        free_disk_gb,
        floor_gb,
        max_live,
        &sizes,
    );
    if !admission.admit {
        let error = format!("branch not admitted: {}", admission.reasons.join("; "));
        let mut refusal = json!({ "error": error, "reasons": admission.reasons });
        if admission.disk_only {
            // `branch_start` reads this to remember the start for the driver to retry.
            refusal["disk_only"] = json!(true);
        }
        return Err((StatusCode::CONFLICT, Json(refusal)));
    }

    let branch = Branch::new(&name, &paths, &layer, base.as_deref());
    let queued = branch.state.clone();
    job.manifest.branches.insert(name.clone(), branch);
    job.updated_at = now();
    store::update(&state.pool, &job)
        .await
        .map_err(store_refusal)?;
    Ok(Json(json!({
        "name": name, "state": queued, "layer": layer, "base": base,
        "cost_gb": admission.cost_gb, "replayed": false,
    })))
}

/// The admission a new branch of `layer` would get from `manifest` right now: the project's disk,
/// its floor and cap, and the live branches' sizes. The driver asks it before it replays a start
/// the disk refused. `Err` when the free disk cannot be read.
pub(super) async fn current_admission(
    manifest: &Manifest,
    layer: &str,
) -> Result<Admission, String> {
    let (floor_gb, max_live) = tree_limits(&manifest.project_root);
    let free_disk_gb = if layer == DOCS_LAYER {
        0.0
    } else {
        super::open::free_disk_gb(FsPath::new(&manifest.project_root))
            .map_err(|error| format!("could not read the free disk space: {error}"))?
    };
    let sizes = live_sizes_gb(manifest).await;
    Ok(branch_admission(
        &admission_book(),
        manifest,
        layer,
        free_disk_gb,
        floor_gb,
        max_live,
        &sizes,
    ))
}

// ----- POST /trees/{id}/nodes/{n}/transition -----

#[derive(Debug, Deserialize)]
struct TransitionBody {
    transition: Option<String>,
    key: Option<String>,
    prompt: Option<String>,
    project_id: Option<String>,
    cwd: Option<String>,
    mode: Option<String>,
    reason: Option<String>,
}

/// The result `tree_transitions` holds for the key, if the row exists and holds one.
async fn stored_result(
    state: &AppState,
    node: &str,
    transition: &str,
    key: &str,
) -> Result<Option<String>, Refusal> {
    let stored: Option<Option<String>> = sqlx::query_scalar(
        "SELECT result FROM tree_transitions WHERE node_id = ? AND transition = ? AND key = ?",
    )
    .bind(node)
    .bind(transition)
    .bind(key)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_refusal)?;
    Ok(stored.flatten().filter(|result| !result.is_empty()))
}

/// `POST /trees/{id}/nodes/{n}/transition`: either `launch`, which starts the agent run behind the
/// node, or a branch state, which moves the branch that node tops. Both are idempotent per node,
/// transition and key. A blocked branch parks through here, since closing a top branch node is
/// refused until its branch is merged or dropped.
async fn post_transition(
    State(state): State<AppState>,
    Path((id, node)): Path<(String, String)>,
    body: Result<Json<TransitionBody>, JsonRejection>,
) -> Result<Json<Value>, Refusal> {
    let Json(body) = body.map_err(bad_body)?;
    let transition = required("transition", body.transition.clone())?;
    let key = required("key", body.key.clone())?;

    let linked: Option<Option<String>> =
        sqlx::query_scalar("SELECT tree_job_id FROM work_nodes WHERE id = ?")
            .bind(&node)
            .fetch_optional(&state.pool)
            .await
            .map_err(db_refusal)?;
    if linked.flatten().as_deref() != Some(id.as_str()) {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            format!("node {node} is not a node of tree job {id}"),
        ));
    }

    if transition == "launch" {
        return launch(&state, &id, &node, &key, body).await;
    }
    if !BRANCH_STATES.contains(&transition.as_str()) && transition != "done" {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{transition} is not a known transition"),
        ));
    }

    let lock = job_lock(&id);
    let _guard = lock.lock().await;
    let mut job = load_job(&state, &id).await?;
    let Some(name) = job
        .manifest
        .branches
        .values()
        .find(|branch| branch.node_id.as_deref() == Some(node.as_str()))
        .map(|branch| branch.name.clone())
    else {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            format!("node {node} tops no branch of tree job {id}"),
        ));
    };
    if let Some(result) = stored_result(&state, &node, &transition, &key).await? {
        return Ok(Json(json!({ "result": result, "replayed": true })));
    }

    let Some(branch) = job.manifest.branches.get_mut(&name) else {
        return Err(refuse(
            StatusCode::NOT_FOUND,
            format!("branch {name} not found"),
        ));
    };
    let from = branch.state.clone();
    apply_transition(branch, &transition)
        .map_err(|error| refuse(StatusCode::CONFLICT, error.to_string()))?;
    if transition == "parked" {
        let mut park = json!({ "prev_state": from });
        if let Some(reason) = body
            .reason
            .as_deref()
            .filter(|reason| !reason.trim().is_empty())
        {
            park["reason"] = json!(reason);
        }
        branch.park = Some(park);
    } else if from == "parked" && transition != "dropped" {
        branch.park = None;
    }
    // The branch is written before the key is claimed: a crash between the two leaves the key
    // unspent, and a retry is refused as illegal instead of answering a move that never happened.
    job.updated_at = now();
    store::update(&state.pool, &job)
        .await
        .map_err(store_refusal)?;
    let applied = node_run::apply(&state.pool, &node, &transition, &key, &transition)
        .await
        .map_err(run_refusal)?;
    Ok(Json(
        json!({ "result": applied.result, "replayed": applied.replayed }),
    ))
}

/// The `launch` transition. No job lock: it changes no manifest, and the key alone makes it once.
async fn launch(
    state: &AppState,
    id: &str,
    node: &str,
    key: &str,
    body: TransitionBody,
) -> Result<Json<Value>, Refusal> {
    let prompt = body
        .prompt
        .filter(|prompt| !prompt.trim().is_empty())
        .ok_or_else(|| {
            refuse(
                StatusCode::UNPROCESSABLE_ENTITY,
                "prompt is required to launch",
            )
        })?;
    let project_id = match body.project_id {
        Some(project_id) => Some(project_id),
        None => store::load(&state.pool, id)
            .await
            .ok()
            .flatten()
            .and_then(|job| job.project_id),
    };
    let mode = body.mode.unwrap_or_else(|| "shadow".to_owned());
    let replayed = stored_result(state, node, "launch", key).await?.is_some();
    let run_id = launch_recovering(state, node, key, prompt, project_id, body.cwd, &mode)
        .await
        .map_err(run_refusal)?;
    Ok(Json(
        json!({ "result": run_id.to_string(), "replayed": replayed }),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use sqlx::SqlitePool;
    use tokio::task::JoinHandle;
    use tower::ServiceExt;

    use super::{branch_admission, overlap_outside_base_chain, routes};
    use crate::execution::admission::AdmissionBook;
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
    use crate::state::AppState;
    use crate::tree_job::budget::{Admission, check_admission};
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::node_run::NodeRunError;
    use crate::tree_job::store::{self, TreeJob, launch_recovering};

    const CREATED: &str = "2026-10-30T10:00:00+00:00";
    const JOB: &str = "job-1";
    const OTHER_JOB: &str = "job-2";
    const BRANCH: &str = "api";
    const NODE: &str = "node-api";
    const KEY: &str = "key-1";
    /// A file a docs branch may own, so that the routes admit its branch whatever the disk holds.
    const SHARED: &str = ".ai/plans/shared.md";
    const OTHER_DOCS: &str = ".ai/plans/other.md";
    const CODE_PATH: &str = "core/src/shared.rs";
    const OTHER_CODE_PATH: &str = "core/src/other.rs";

    const CORE: &str = "core";
    const DOCS: &str = "docs";
    /// A code layer measured at no cost: the route then needs only the floor free on the real disk.
    const FREE_LAYER: &str = "free";
    /// A code layer no real disk can hold: the route refuses it whatever the disk holds.
    const HUGE_LAYER: &str = "huge";
    const HUGE_GB: f64 = 1e12;

    const FLOOR_GB: f64 = 15.0;
    const CAP: usize = 4;
    const PLENTY_GB: f64 = 10_000.0;
    /// `budget::SEED_COST_GB`, spelled out so that these sums read on their own.
    const SEED_GB: f64 = 8.0;

    /// `OPEN_STATES` in `tree_resources.py`: the states that hold a slot and grow on disk.
    const LIVE_STATES: [&str; 7] = [
        "open",
        "building",
        "gating",
        "green",
        "red",
        "reviewing",
        "fixing",
    ];
    const NOT_LIVE_STATES: [&str; 4] = ["queued", "parked", "merged", "dropped"];

    // ----- fixtures -----

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn branch(name: &str, state: &str, layer: &str, paths: &[&str], base: Option<&str>) -> Branch {
        let mut branch = Branch::new(name, &strings(paths), layer, base);
        branch.state = state.to_owned();
        branch
    }

    fn with_node(mut branch: Branch, node_id: &str) -> Branch {
        branch.node_id = Some(node_id.to_owned());
        branch
    }

    fn manifest_of(id: &str, branches: Vec<Branch>, costs: &[(&str, f64)]) -> Manifest {
        let mut manifest = Manifest::new(id, "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        for branch in branches {
            manifest.branches.insert(branch.name.clone(), branch);
        }
        for (layer, cost) in costs {
            manifest.costs_gb.insert((*layer).to_owned(), json!(cost));
        }
        manifest
    }

    fn book(capacity: u32) -> AdmissionBook {
        AdmissionBook::new(
            capacity,
            Duration::from_secs(900),
            Duration::from_secs(7200),
        )
    }

    fn sizes(entries: &[(&str, f64)]) -> BTreeMap<String, f64> {
        entries
            .iter()
            .map(|(name, size)| ((*name).to_owned(), *size))
            .collect()
    }

    /// A new `core` branch against a free book, the default floor and the default cap.
    fn admit(manifest: &Manifest, free_disk_gb: f64, live_sizes: &[(&str, f64)]) -> Admission {
        branch_admission(
            &book(8),
            manifest,
            CORE,
            free_disk_gb,
            FLOOR_GB,
            CAP,
            &sizes(live_sizes),
        )
    }

    fn assert_refused_naming(admission: &Admission, word: &str) {
        assert!(!admission.admit, "expected a refusal, got {admission:?}");
        assert!(
            admission.reasons.iter().any(|reason| reason.contains(word)),
            "no reason names {word:?}: {admission:?}"
        );
    }

    fn assert_admitted(admission: &Admission) {
        assert!(admission.admit, "expected an admission, got {admission:?}");
        assert!(admission.reasons.is_empty(), "{admission:?}");
    }

    /// Runs `work` on a thread of its own, so that a walk that never ends fails instead of hanging
    /// the whole test binary.
    fn within_five_seconds<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the walk did not terminate")
    }

    /// A state whose fake agent stays alive for the test's length, as in `node_run`'s tests.
    async fn test_state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    /// The same, on a pool of several connections, so that two requests really can interleave.
    async fn shared_state() -> AppState {
        let mut state = test_state().await;
        state.pool = crate::testdb::fresh_shared_pool().await;
        state
    }

    fn app(state: &AppState) -> Router {
        routes().with_state(state.clone())
    }

    async fn send(app: &Router, method: &str, uri: &str, body: Body) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", "Bearer test-token")
            .header("Content-Type", "application/json")
            .body(body)
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
        send(app, "POST", uri, Body::from(body.to_string())).await
    }

    async fn post_raw(app: &Router, uri: &str, body: &'static str) -> (StatusCode, Value) {
        send(app, "POST", uri, Body::from(body)).await
    }

    async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
        send(app, "GET", uri, Body::empty()).await
    }

    fn spawn_post(app: &Router, uri: String, body: Value) -> JoinHandle<(StatusCode, Value)> {
        let app = app.clone();
        tokio::spawn(async move { post(&app, &uri, body).await })
    }

    fn branches_uri(job: &str) -> String {
        format!("/trees/{job}/branches")
    }

    fn transition_uri(job: &str, node: &str) -> String {
        format!("/trees/{job}/nodes/{node}/transition")
    }

    async fn move_branch(
        app: &Router,
        job: &str,
        node: &str,
        transition: &str,
        key: &str,
    ) -> (StatusCode, Value) {
        post(
            app,
            &transition_uri(job, node),
            json!({ "transition": transition, "key": key }),
        )
        .await
    }

    /// Asserts a refusal of exactly `status` with a `{error}` body.
    fn assert_error(response: &(StatusCode, Value), status: StatusCode) {
        assert_eq!(response.0, status, "{:?}", response.1);
        assert!(
            response.1["error"].is_string(),
            "no error text: {:?}",
            response.1
        );
    }

    fn assert_error_naming(response: &(StatusCode, Value), status: StatusCode, name: &str) {
        assert_error(response, status);
        let error = response.1["error"].as_str().unwrap();
        assert!(error.contains(name), "{error:?} does not name {name:?}");
    }

    /// A first call may answer `replayed: false` or leave the field out; it may not claim a replay.
    fn assert_first_call(response: &(StatusCode, Value)) {
        assert_eq!(response.0, StatusCode::OK, "{:?}", response.1);
        assert_ne!(response.1["replayed"], json!(true), "{:?}", response.1);
    }

    fn assert_replayed(response: &(StatusCode, Value)) {
        assert_eq!(response.0, StatusCode::OK, "{:?}", response.1);
        assert_eq!(response.1["replayed"], json!(true), "{:?}", response.1);
    }

    fn tree_body(id: &str) -> Value {
        json!({
            "id": id,
            "goal": "Ship it",
            "feature_branch": "feat/tree",
            "project_root": project_root(),
            "project_id": "project-0",
        })
    }

    /// A directory that exists on the disk the tests run on, with no project config in it, so that
    /// the handler reads real free space and the default floor and cap.
    fn project_root() -> String {
        std::env::temp_dir().to_string_lossy().replace('\\', "/")
    }

    fn docs_branch_body(name: &str, paths: &[&str]) -> Value {
        json!({ "name": name, "paths": paths, "layer": DOCS })
    }

    async fn seed_node(pool: &SqlitePool, id: &str, kind: &str, root_id: &str, job: &str) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, tree_job_id,
                                     created_at, updated_at)
             VALUES (?, ?, 0, ?, 'title', 'goal', 'running', ?, ?, ?)",
        )
        .bind(id)
        .bind(root_id)
        .bind(kind)
        .bind(job)
        .bind(CREATED)
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Stores a job in `state`, its root node, and a node row for each branch that names one.
    async fn seed_job(
        pool: &SqlitePool,
        id: &str,
        state: &str,
        branches: Vec<Branch>,
        costs: &[(&str, f64)],
    ) {
        let mut manifest = manifest_of(id, branches, costs);
        manifest.state = state.to_owned();
        manifest.project_root = project_root();
        let root_node_id = format!("{id}-root");
        seed_node(pool, &root_node_id, "root", &root_node_id, id).await;
        for node_id in manifest
            .branches
            .values()
            .filter_map(|b| b.node_id.as_ref())
        {
            seed_node(pool, node_id, "orchestrator", &root_node_id, id).await;
        }
        let job = TreeJob {
            id: id.to_owned(),
            root_node_id,
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: state.to_owned(),
            manifest,
            contract: None,
            created_at: CREATED.to_owned(),
            updated_at: CREATED.to_owned(),
        };
        store::save(pool, &job).await.unwrap();
    }

    /// One job holding one code branch `api`, queued, behind the node `node-api`.
    async fn seed_one_branch(pool: &SqlitePool, state: &str) {
        let branch = with_node(branch(BRANCH, state, CORE, &[CODE_PATH], None), NODE);
        seed_job(pool, JOB, "open", vec![branch], &[]).await;
    }

    async fn loaded(pool: &SqlitePool, id: &str) -> TreeJob {
        store::load(pool, id).await.unwrap().unwrap()
    }

    async fn branch_state(pool: &SqlitePool, job: &str, name: &str) -> String {
        loaded(pool, job).await.manifest.branches[name]
            .state
            .clone()
    }

    async fn job_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tree_jobs")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn node_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn run_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn transition_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tree_transitions")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn node_run_id(pool: &SqlitePool, id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    // ----- branch_admission: growth -----

    #[test]
    fn admission_refuses_when_the_disk_fits_the_new_branch_alone_but_not_after_growth() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", CORE, &[], None)],
            &[],
        );
        let free = FLOOR_GB + SEED_GB;
        // The trap: the plain check, which knows nothing of live branches, would let it through.
        assert!(check_admission(&book(8), &manifest, CORE, free, FLOOR_GB).admit);

        let admission: Admission = admit(&manifest, free, &[]);

        assert_refused_naming(&admission, "disk");
        assert_eq!(admission.cost_gb, SEED_GB);
    }

    #[test]
    fn admission_counts_an_absent_size_as_zero_so_the_whole_cost_is_growth() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", CORE, &[], None)],
            &[],
        );
        let sizes = [("someone-else", SEED_GB)];

        assert_admitted(&admit(&manifest, FLOOR_GB + SEED_GB * 2.0, &sizes));
        assert_refused_naming(
            &admit(&manifest, FLOOR_GB + SEED_GB * 2.0 - 1.0, &sizes),
            "disk",
        );
    }

    #[test]
    fn admission_lets_a_measured_branch_near_its_cost_grow_only_by_what_is_left() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", CORE, &[], None)],
            &[],
        );
        let sizes = [("a", SEED_GB - 0.5)];

        assert_admitted(&admit(&manifest, FLOOR_GB + SEED_GB + 0.5, &sizes));
        assert_refused_naming(&admit(&manifest, FLOOR_GB + SEED_GB, &sizes), "disk");
    }

    /// `max(0, cost - size)`: a branch larger than its cost is no credit towards the next one.
    #[test]
    fn admission_takes_a_branch_past_its_cost_as_no_growth_and_no_credit() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", CORE, &[], None)],
            &[],
        );
        let sizes = [("a", 100.0)];

        assert_admitted(&admit(&manifest, FLOOR_GB + SEED_GB, &sizes));
        assert_refused_naming(&admit(&manifest, FLOOR_GB + SEED_GB - 1.0, &sizes), "disk");
    }

    #[test]
    fn admission_charges_growth_at_the_live_branch_layer_cost_not_the_new_one() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", "small", &[], None)],
            &[("small", 2.0), ("large", 20.0)],
        );
        let call = |free: f64| {
            branch_admission(
                &book(8),
                &manifest,
                "large",
                free,
                FLOOR_GB,
                CAP,
                &BTreeMap::new(),
            )
        };

        let admitted = call(FLOOR_GB + 20.0 + 2.0);
        assert_admitted(&admitted);
        assert_eq!(admitted.cost_gb, 20.0);
        assert_refused_naming(&call(FLOOR_GB + 20.0 + 1.0), "disk");
    }

    #[test]
    fn admission_sums_the_growth_of_every_live_branch() {
        let manifest = manifest_of(
            "tree-1",
            vec![
                branch("a", "building", CORE, &[], None),
                branch("b", "fixing", CORE, &[], None),
            ],
            &[],
        );
        let sizes = [("a", SEED_GB - 2.0)];
        let enough = FLOOR_GB + SEED_GB + 2.0 + SEED_GB;

        assert_admitted(&admit(&manifest, enough, &sizes));
        assert_refused_naming(&admit(&manifest, enough - 1.0, &sizes), "disk");
    }

    #[test]
    fn admission_counts_growth_for_every_live_state() {
        for state in LIVE_STATES {
            let manifest = manifest_of("tree-1", vec![branch("a", state, CORE, &[], None)], &[]);

            let admission = admit(&manifest, FLOOR_GB + SEED_GB, &[]);

            assert!(!admission.admit, "{state}: {admission:?}");
        }
    }

    /// Per `compute_budget`: `parked` and `queued` are not in `OPEN_STATES`, so like `merged` and
    /// `dropped` they neither grow nor take a slot.
    #[test]
    fn admission_counts_no_growth_for_a_branch_that_is_not_live() {
        for state in NOT_LIVE_STATES {
            let manifest = manifest_of("tree-1", vec![branch("a", state, CORE, &[], None)], &[]);

            assert_admitted(&admit(&manifest, FLOOR_GB + SEED_GB, &[]));
        }
    }

    #[test]
    fn admission_counts_no_growth_for_a_docs_branch_in_any_state() {
        for state in ["queued", "building", "done", "dropped"] {
            let manifest = manifest_of("tree-1", vec![branch("a", state, DOCS, &[], None)], &[]);

            assert_admitted(&admit(&manifest, FLOOR_GB + SEED_GB, &[]));
        }
    }

    #[test]
    fn the_root_branch_is_charged_no_growth() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("root", "building", CORE, &[], None)],
            &[(CORE, SEED_GB)],
        );

        let admission = admit(&manifest, 29.0, &[]);

        assert_admitted(&admission);
    }

    #[test]
    fn a_live_branch_beside_the_root_is_still_charged_growth() {
        let manifest = manifest_of(
            "tree-1",
            vec![
                branch("root", "building", CORE, &[], None),
                branch("api", "building", CORE, &[], None),
            ],
            &[(CORE, SEED_GB)],
        );

        let admission = admit(&manifest, 29.0, &[]);

        assert_refused_naming(&admission, "disk");
    }

    // ----- branch_admission: cap, book, docs -----

    fn live_branches(count: usize, state: &str, layer: &str) -> Vec<Branch> {
        (0..count)
            .map(|index| branch(&format!("b{index}"), state, layer, &[], None))
            .collect()
    }

    #[test]
    fn admission_refuses_at_the_cap_naming_max_live_branches() {
        let manifest = manifest_of("tree-1", live_branches(CAP, "building", CORE), &[]);

        let admission = admit(&manifest, PLENTY_GB, &[]);

        assert_refused_naming(&admission, "max_live_branches");
    }

    #[test]
    fn admission_admits_one_below_the_cap() {
        let manifest = manifest_of("tree-1", live_branches(CAP - 1, "building", CORE), &[]);

        assert_admitted(&admit(&manifest, PLENTY_GB, &[]));
    }

    #[test]
    fn admission_takes_the_cap_it_is_given() {
        let manifest = manifest_of("tree-1", live_branches(1, "building", CORE), &[]);

        let admission = branch_admission(
            &book(8),
            &manifest,
            CORE,
            PLENTY_GB,
            FLOOR_GB,
            1,
            &BTreeMap::new(),
        );

        assert_refused_naming(&admission, "max_live_branches");
    }

    #[test]
    fn the_root_branch_still_counts_against_the_cap() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("root", "building", CORE, &[], None)],
            &[(CORE, SEED_GB)],
        );

        let admission = branch_admission(
            &book(8),
            &manifest,
            CORE,
            PLENTY_GB,
            FLOOR_GB,
            1,
            &BTreeMap::new(),
        );

        assert_refused_naming(&admission, "max_live_branches");
    }

    #[test]
    fn admission_does_not_count_a_branch_that_is_not_live_towards_the_cap() {
        for state in NOT_LIVE_STATES {
            let manifest = manifest_of("tree-1", live_branches(CAP, state, CORE), &[]);

            assert_admitted(&admit(&manifest, PLENTY_GB, &[]));
        }
    }

    #[test]
    fn admission_does_not_count_a_docs_branch_towards_the_cap() {
        let manifest = manifest_of("tree-1", live_branches(CAP, "building", DOCS), &[]);

        assert_admitted(&admit(&manifest, PLENTY_GB, &[]));
    }

    #[test]
    fn admission_refuses_when_the_book_has_no_free_slot() {
        let manifest = manifest_of("tree-1", Vec::new(), &[]);
        let full = book(1);
        full.request(1, "gate", 1);

        let admission = branch_admission(
            &full,
            &manifest,
            CORE,
            PLENTY_GB,
            FLOOR_GB,
            CAP,
            &BTreeMap::new(),
        );

        assert_refused_naming(&admission, "slot");
    }

    #[test]
    fn admission_always_admits_a_docs_layer() {
        let manifest = manifest_of("tree-1", live_branches(CAP, "building", CORE), &[]);
        let full = book(1);
        full.request(1, "gate", 1);

        let admission =
            branch_admission(&full, &manifest, DOCS, 0.0, FLOOR_GB, CAP, &BTreeMap::new());

        assert_admitted(&admission);
        assert_eq!(admission.cost_gb, 0.0);
    }

    // ----- overlap_outside_base_chain -----

    fn holder(name: &str, base: Option<&str>) -> Branch {
        branch(name, "building", CORE, &[CODE_PATH], base)
    }

    fn overlap(manifest: &Manifest, base: Option<&str>) -> Option<String> {
        overlap_outside_base_chain(manifest, &strings(&[CODE_PATH]), base)
    }

    #[test]
    fn overlap_with_the_base_is_exempt() {
        let manifest = manifest_of("tree-1", vec![holder("a", None)], &[]);

        assert_eq!(overlap(&manifest, Some("a")), None);
    }

    #[test]
    fn overlap_with_the_base_of_the_base_is_exempt() {
        let manifest = manifest_of(
            "tree-1",
            vec![holder("grand", None), holder("parent", Some("grand"))],
            &[],
        );

        assert_eq!(overlap(&manifest, Some("parent")), None);
    }

    #[test]
    fn overlap_with_a_stranger_names_it() {
        let manifest = manifest_of("tree-1", vec![holder("z", None)], &[]);

        assert_eq!(overlap(&manifest, None), Some("z".to_owned()));
    }

    /// `overlapping_branch` answers only the first overlap by name, so filtering its answer against
    /// the chain would see `a`, exempt it, and let `z` through.
    #[test]
    fn overlap_finds_the_stranger_behind_a_chain_branch_that_sorts_first() {
        let manifest = manifest_of("tree-1", vec![holder("a", None), holder("z", None)], &[]);
        let paths = strings(&[CODE_PATH]);
        assert_eq!(
            crate::tree_job::engine::overlapping_branch(&manifest, &paths),
            Some("a".to_owned())
        );

        assert_eq!(overlap(&manifest, Some("a")), Some("z".to_owned()));
    }

    #[test]
    fn overlap_does_not_exempt_a_sibling_on_the_same_base() {
        let manifest = manifest_of(
            "tree-1",
            vec![holder("base", None), holder("sibling", Some("base"))],
            &[],
        );

        assert_eq!(overlap(&manifest, Some("base")), Some("sibling".to_owned()));
    }

    #[test]
    fn overlap_walk_terminates_on_a_cycle_and_still_finds_a_stranger() {
        let manifest = manifest_of(
            "tree-1",
            vec![
                holder("a", Some("b")),
                holder("b", Some("a")),
                holder("z", None),
            ],
            &[],
        );

        let found = within_five_seconds(move || overlap(&manifest, Some("a")));

        assert_eq!(found, Some("z".to_owned()));
    }

    #[test]
    fn overlap_walk_terminates_on_a_branch_based_on_itself() {
        let manifest = manifest_of("tree-1", vec![holder("a", Some("a"))], &[]);

        let found = within_five_seconds(move || overlap(&manifest, Some("a")));

        assert_eq!(found, None);
    }

    #[test]
    fn overlap_with_an_unknown_base_exempts_nothing() {
        let manifest = manifest_of("tree-1", vec![holder("a", None)], &[]);

        assert_eq!(overlap(&manifest, Some("ghost")), Some("a".to_owned()));
    }

    #[test]
    fn overlap_keeps_the_chain_up_to_a_base_that_names_a_missing_branch() {
        let manifest = manifest_of("tree-1", vec![holder("a", Some("ghost"))], &[]);

        assert_eq!(overlap(&manifest, Some("a")), None);
    }

    #[test]
    fn overlap_with_no_base_exempts_nothing() {
        let manifest = manifest_of("tree-1", vec![holder("a", None)], &[]);

        assert_eq!(overlap(&manifest, None), Some("a".to_owned()));
    }

    #[test]
    fn overlap_ignores_a_branch_that_released_its_paths() {
        for state in ["merged", "dropped"] {
            let manifest = manifest_of(
                "tree-1",
                vec![branch("a", state, CORE, &[CODE_PATH], None)],
                &[],
            );

            assert_eq!(overlap(&manifest, None), None, "{state}");
        }
    }

    #[test]
    fn overlap_ignores_paths_that_do_not_overlap() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", CORE, &[OTHER_CODE_PATH], None)],
            &[],
        );

        assert_eq!(overlap(&manifest, None), None);
    }

    // ----- POST /trees -----

    #[tokio::test]
    async fn post_trees_opens_a_root_saves_the_job_and_links_the_node() {
        let state = test_state().await;
        let app = app(&state);

        let response = post(&app, "/trees", tree_body(JOB)).await;

        assert_first_call(&response);
        let body = &response.1;
        assert_eq!(body["id"], json!(JOB));
        assert_eq!(body["state"], json!("open"));
        let root_node_id = body["root_node_id"].as_str().expect("a root node id");
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(job.root_node_id, root_node_id);
        assert_eq!(job.goal, "Ship it");
        assert_eq!(job.feature_branch.as_deref(), Some("feat/tree"));
        assert_eq!(job.state, "open");
        assert_eq!(job.manifest.tree_id, JOB);
        assert_eq!(job.manifest.goal, "Ship it");
        assert_eq!(job.manifest.feature_branch, "feat/tree");
        assert_eq!(job.manifest.project_root, project_root());
        let (kind, depth, root_id, tree_job_id): (String, i64, String, Option<String>) =
            sqlx::query_as("SELECT kind, depth, root_id, tree_job_id FROM work_nodes WHERE id = ?")
                .bind(root_node_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(kind, "root");
        assert_eq!(depth, 0);
        assert_eq!(root_id, root_node_id);
        assert_eq!(tree_job_id.as_deref(), Some(JOB));
    }

    #[tokio::test]
    async fn post_trees_keeps_the_optional_fields() {
        let state = test_state().await;
        let mut body = tree_body(JOB);
        body["project_id"] = json!("project-1");
        body["contract"] = json!("the contract");
        body["dirty_owner_files"] = json!(["notes.txt"]);

        let response = post(&app(&state), "/trees", body).await;

        assert_first_call(&response);
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(job.project_id.as_deref(), Some("project-1"));
        assert_eq!(job.contract.as_deref(), Some("the contract"));
        assert_eq!(job.manifest.dirty_owner_files, strings(&["notes.txt"]));
    }

    #[tokio::test]
    async fn post_trees_reads_null_optional_fields_as_absent() {
        let state = test_state().await;
        let mut body = tree_body(JOB);
        body["contract"] = Value::Null;
        body["dirty_owner_files"] = Value::Null;

        let response = post(&app(&state), "/trees", body).await;

        assert_first_call(&response);
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(job.contract, None);
        assert!(job.manifest.dirty_owner_files.is_empty());
    }

    #[tokio::test]
    async fn post_trees_replay_returns_the_same_job_and_opens_no_second_root() {
        let state = test_state().await;
        let app = app(&state);
        let first = post(&app, "/trees", tree_body(JOB)).await;
        let added = post(
            &app,
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;
        assert_first_call(&added);

        let replay = post(&app, "/trees", tree_body(JOB)).await;

        assert_replayed(&replay);
        assert_eq!(replay.1["id"], json!(JOB));
        assert_eq!(replay.1["root_node_id"], first.1["root_node_id"]);
        assert_eq!(replay.1["state"], json!("open"));
        assert_eq!(job_rows(&state.pool).await, 1);
        assert_eq!(node_rows(&state.pool).await, 1);
        // A replay must not rewrite the job from the request body.
        assert!(
            loaded(&state.pool, JOB)
                .await
                .manifest
                .branches
                .contains_key("notes")
        );
    }

    #[tokio::test]
    async fn post_trees_refuses_a_missing_or_null_required_field_with_400() {
        let state = test_state().await;
        let app = app(&state);

        for field in ["id", "goal", "feature_branch", "project_root"] {
            let mut absent = tree_body(JOB);
            absent.as_object_mut().unwrap().remove(field);
            let mut null = tree_body(JOB);
            null[field] = Value::Null;

            for body in [absent, null] {
                let response = post(&app, "/trees", body).await;

                assert_error_naming(&response, StatusCode::BAD_REQUEST, field);
            }
        }
        assert_eq!(job_rows(&state.pool).await, 0);
        assert_eq!(node_rows(&state.pool).await, 0);
    }

    #[tokio::test]
    async fn post_trees_refuses_malformed_json_with_400() {
        let state = test_state().await;

        let response = post_raw(&app(&state), "/trees", "{not json").await;

        assert_error(&response, StatusCode::BAD_REQUEST);
        assert_eq!(job_rows(&state.pool).await, 0);
    }

    // ----- POST /trees: the root run and the landing mode -----

    /// A temp git repository with one commit on `master` and a zero disk floor. The root branch is
    /// still charged its 8 GB seed cost; what admits it whatever the disk holds is the test build's
    /// fixed free disk (`open::free_disk_gb`). The layout `open.rs`'s `Checkout` has.
    struct Repo {
        _container: tempfile::TempDir,
        path: std::path::PathBuf,
    }

    impl Repo {
        fn new() -> Repo {
            let container = space_free_tempdir("tree-post-");
            let path = container.path().join("repo");
            initialize_repo(&path);
            let repo = Repo {
                _container: container,
                path,
            };
            // `initialize_repo` takes the machine's `init.defaultBranch`.
            repo.git(&["branch", "-M", "master"]);
            let dir = repo.path.join(".nucleos/workflow");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("speed.yaml"), "tree:\n  floor_gb: 0\n").unwrap();
            repo
        }

        fn root(&self) -> String {
            self.path.to_string_lossy().replace('\\', "/")
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

        fn has_ref(&self, branch: &str) -> bool {
            !self
                .git(&["for-each-ref", &format!("refs/heads/{branch}")])
                .is_empty()
        }
    }

    fn repo_tree_body(id: &str, repo: &Repo) -> Value {
        let mut body = tree_body(id);
        body["project_root"] = json!(repo.root());
        body
    }

    /// The run id a launch stored under `key` for `node`, which is what makes the launch replay-safe.
    async fn stored_launch_result(pool: &SqlitePool, node: &str, key: &str) -> Option<String> {
        let stored: Option<Option<String>> =
            sqlx::query_scalar("SELECT result FROM tree_transitions WHERE node_id = ? AND key = ?")
                .bind(node)
                .bind(key)
                .fetch_optional(pool)
                .await
                .unwrap();
        stored.flatten()
    }

    async fn feed_lines_naming(pool: &SqlitePool, word: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed WHERE summary LIKE ?")
            .bind(format!("%{word}%"))
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn post_tree_opens_the_root_worktree_binds_the_root_node_and_starts_one_run() {
        let state = test_state().await;
        let repo = Repo::new();

        let response = post(&app(&state), "/trees", repo_tree_body(JOB, &repo)).await;

        assert_first_call(&response);
        let job = loaded(&state.pool, JOB).await;
        let root_branch = job.manifest.branches.get("root").expect("a root branch");
        assert_eq!(
            root_branch.node_id.as_deref(),
            Some(job.root_node_id.as_str())
        );
        assert!(
            repo.path.join(".claude/worktrees/job-1-root").is_dir(),
            "the root worktree is missing"
        );
        assert!(
            repo.has_ref("tree/job-1/root"),
            "the root git branch is missing"
        );
        assert_eq!(run_rows(&state.pool).await, 1);
        let run_id = node_run_id(&state.pool, &job.root_node_id)
            .await
            .expect("the root node links its run");
        assert_eq!(
            stored_launch_result(&state.pool, &job.root_node_id, "root-job-1")
                .await
                .as_deref(),
            Some(run_id.as_str()),
            "the launch is stored under the key root-<tree id>"
        );
    }

    #[tokio::test]
    async fn post_tree_replay_starts_no_second_run() {
        let state = test_state().await;
        let app = app(&state);
        let repo = Repo::new();
        let first = post(&app, "/trees", repo_tree_body(JOB, &repo)).await;
        assert_first_call(&first);
        let run = node_run_id(&state.pool, first.1["root_node_id"].as_str().unwrap()).await;

        let replay = post(&app, "/trees", repo_tree_body(JOB, &repo)).await;

        assert_replayed(&replay);
        assert_eq!(run_rows(&state.pool).await, 1);
        assert_eq!(
            node_run_id(&state.pool, replay.1["root_node_id"].as_str().unwrap()).await,
            run
        );
        assert_eq!(
            repo.git(&["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            2
        );
    }

    /// No git repository at `project_root`: the worktree cannot be made, so no run starts, but the
    /// job exists and says why in the feed.
    #[tokio::test]
    async fn post_tree_start_failure_keeps_the_job_open_and_leaves_a_feed_line() {
        let state = test_state().await;

        let response = post(&app(&state), "/trees", tree_body(JOB)).await;

        assert_first_call(&response);
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(job.state, "open");
        assert_eq!(run_rows(&state.pool).await, 0);
        assert_eq!(node_run_id(&state.pool, &job.root_node_id).await, None);
        assert!(
            !feed_lines_naming(&state.pool, JOB).await.is_empty(),
            "the failed start left no feed line naming the job"
        );
    }

    #[tokio::test]
    async fn post_tree_with_land_hold_stores_the_landing_mode() {
        let state = test_state().await;
        let app = app(&state);
        let mut hold = tree_body(JOB);
        hold["land"] = json!("hold");
        let mut integration = tree_body(OTHER_JOB);
        integration["land"] = json!("integration");

        assert_first_call(&post(&app, "/trees", hold).await);
        assert_first_call(&post(&app, "/trees", integration).await);
        assert_first_call(&post(&app, "/trees", tree_body("job-3")).await);

        let landing = |job: TreeJob| job.manifest.extra.get("landing").cloned();
        assert_eq!(landing(loaded(&state.pool, JOB).await), Some(json!("hold")));
        assert_eq!(
            landing(loaded(&state.pool, OTHER_JOB).await),
            Some(json!("integration"))
        );
        // Decision D1: no tree moves master unless its body says so.
        assert_eq!(
            landing(loaded(&state.pool, "job-3").await),
            Some(json!("hold"))
        );
    }

    #[tokio::test]
    async fn post_tree_refuses_an_unknown_land_value() {
        let state = test_state().await;
        let mut body = tree_body(JOB);
        body["land"] = json!("master");

        let response = post(&app(&state), "/trees", body).await;

        assert_error_naming(&response, StatusCode::BAD_REQUEST, "land");
        assert_eq!(job_rows(&state.pool).await, 0);
        assert_eq!(node_rows(&state.pool).await, 0);
    }

    // ----- POST /trees: the project resolved from its root -----

    /// Registers `project_id` with `root` in the roster `spec_finish` resolves roots against.
    async fn register_project(pool: &SqlitePool, project_id: &str, root: &str) {
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, integration_branch)
             VALUES (?, 'active', ?, 'master')",
        )
        .bind(project_id)
        .bind(root)
        .execute(pool)
        .await
        .expect("seed the project's roster row");
    }

    /// `tree_launch` with only `project_root` is the conductor's everyday call: the daemon finds the
    /// project the same way `spec_finish` does, separators and a trailing slash notwithstanding.
    #[tokio::test]
    async fn post_tree_resolves_the_project_id_from_the_project_root() {
        let state = test_state().await;
        let registered = format!("{}/", project_root().replace('/', "\\"));
        register_project(&state.pool, "project-by-root", &registered).await;
        let app = app(&state);

        for blank in [None, Some(Value::Null), Some(json!("  "))] {
            let id = format!("job-by-root-{}", job_rows(&state.pool).await);
            let mut body = tree_body(&id);
            match blank {
                None => {
                    body.as_object_mut().unwrap().remove("project_id");
                }
                Some(value) => body["project_id"] = value,
            }

            assert_first_call(&post(&app, "/trees", body).await);

            assert_eq!(
                loaded(&state.pool, &id).await.project_id.as_deref(),
                Some("project-by-root")
            );
        }
    }

    /// A project named in the body wins over the roster: the root is only a fallback.
    #[tokio::test]
    async fn post_tree_keeps_a_named_project_id_over_the_root() {
        let state = test_state().await;
        register_project(&state.pool, "project-by-root", &project_root()).await;

        assert_first_call(&post(&app(&state), "/trees", tree_body(JOB)).await);

        assert_eq!(
            loaded(&state.pool, JOB).await.project_id.as_deref(),
            Some("project-0")
        );
    }

    /// No project_id and a root no project has: refused before anything is opened, and the
    /// refusal names the root so the caller sees which one is unknown.
    #[tokio::test]
    async fn post_tree_refuses_an_unknown_root_without_a_project_id_with_400() {
        let state = test_state().await;
        register_project(&state.pool, "elsewhere", "Z:/some/other/root").await;
        let mut body = tree_body(JOB);
        body.as_object_mut().unwrap().remove("project_id");

        let response = post(&app(&state), "/trees", body).await;

        assert_error_naming(&response, StatusCode::BAD_REQUEST, &project_root());
        assert_error_naming(&response, StatusCode::BAD_REQUEST, "project_id");
        assert_eq!(job_rows(&state.pool).await, 0);
        assert_eq!(node_rows(&state.pool).await, 0);
    }

    // ----- GET /trees/{id} -----

    #[tokio::test]
    async fn get_tree_returns_the_job_with_its_manifest() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "queued").await;

        let (status, body) = get(&app(&state), &format!("/trees/{JOB}")).await;

        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["id"], json!(JOB));
        assert_eq!(body["root_node_id"], json!(format!("{JOB}-root")));
        assert_eq!(body["state"], json!("open"));
        assert_eq!(body["goal"], json!("Ship it"));
        assert_eq!(body["manifest"]["tree_id"], json!(JOB));
        assert_eq!(body["manifest"]["branches"][BRANCH]["node_id"], json!(NODE));
    }

    #[tokio::test]
    async fn get_tree_of_an_unknown_id_is_404() {
        let state = test_state().await;

        let response = get(&app(&state), "/trees/no-such-job").await;

        assert_error(&response, StatusCode::NOT_FOUND);
    }

    // ----- POST /trees/{id}/branches -----

    #[tokio::test]
    async fn post_branch_adds_a_queued_branch_that_a_read_shows() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;

        let response = post(
            &app,
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_first_call(&response);
        let (status, read) = get(&app, &format!("/trees/{JOB}")).await;
        assert_eq!(status, StatusCode::OK, "{read:?}");
        let added = &read["manifest"]["branches"]["notes"];
        assert_eq!(added["state"], json!("queued"));
        assert_eq!(added["layer"], json!(DOCS));
        assert_eq!(added["paths"], json!([SHARED]));
        assert_eq!(added["base"], Value::Null);
    }

    /// A code branch is admitted through the handler's own sources: the project's limits, the live
    /// sizes and the free disk. The disk is the test build's fixed figure (`open::free_disk_gb`), not
    /// the volume's: the default `FLOOR_GB` plus the cost is more room than a machine running trees
    /// keeps, and this test failed on exactly that in daemon gates.
    #[tokio::test]
    async fn post_branch_admits_a_code_branch_through_the_handler_sources() {
        let state = test_state().await;
        seed_job(&state.pool, JOB, "open", Vec::new(), &[(FREE_LAYER, 0.0)]).await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "api", "paths": [CODE_PATH], "layer": FREE_LAYER }),
        )
        .await;

        assert_first_call(&response);
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(job.manifest.branches["api"].state, "queued");
        assert_eq!(job.manifest.branches["api"].layer, FREE_LAYER);
    }

    #[tokio::test]
    async fn post_branch_replay_of_an_identical_body_adds_no_duplicate() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![branch("base", "queued", DOCS, &[OTHER_DOCS], None)],
            &[],
        )
        .await;
        let body = json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": "base" });
        assert_first_call(&post(&app, &branches_uri(JOB), body.clone()).await);
        let before = loaded(&state.pool, JOB).await.manifest;

        let replay = post(&app, &branches_uri(JOB), body).await;

        assert_replayed(&replay);
        assert_eq!(loaded(&state.pool, JOB).await.manifest, before);
    }

    #[tokio::test]
    async fn post_branch_replay_reads_a_null_base_as_absent() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;
        let first = post(
            &app,
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;
        assert_first_call(&first);

        let replay = post(
            &app,
            &branches_uri(JOB),
            json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": null }),
        )
        .await;

        assert_replayed(&replay);
    }

    /// The replay answers before admission: a branch already in the manifest does not ask for room
    /// again, even when a new one of its layer would now be refused.
    #[tokio::test]
    async fn post_branch_replay_answers_even_when_admission_would_now_refuse() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![branch("big", "queued", HUGE_LAYER, &[CODE_PATH], None)],
            &[(HUGE_LAYER, HUGE_GB)],
        )
        .await;

        let replay = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "big", "paths": [CODE_PATH], "layer": HUGE_LAYER }),
        )
        .await;

        assert_replayed(&replay);
    }

    #[tokio::test]
    async fn post_branch_with_the_same_name_and_another_body_is_409() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![branch("base", "queued", DOCS, &[OTHER_DOCS], None)],
            &[],
        )
        .await;
        let added = post(
            &app,
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;
        assert_first_call(&added);
        let before = loaded(&state.pool, JOB).await.manifest;

        for body in [
            json!({ "name": "notes", "paths": [".ai/plans/third.md"], "layer": DOCS }),
            json!({ "name": "notes", "paths": [SHARED], "layer": FREE_LAYER }),
            json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": "base" }),
        ] {
            let response = post(&app, &branches_uri(JOB), body.clone()).await;

            assert_error(&response, StatusCode::CONFLICT);
            assert_ne!(response.1["replayed"], json!(true), "{body:?}");
        }
        assert_eq!(loaded(&state.pool, JOB).await.manifest, before);
    }

    #[tokio::test]
    async fn post_branch_overlapping_a_stranger_is_409_naming_it() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![branch("stranger-z", "queued", DOCS, &[SHARED], None)],
            &[],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_error_naming(&response, StatusCode::CONFLICT, "stranger-z");
        assert!(
            !loaded(&state.pool, JOB)
                .await
                .manifest
                .branches
                .contains_key("notes")
        );
    }

    #[tokio::test]
    async fn post_branch_overlapping_its_base_chain_is_200() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![
                branch("grand", "queued", DOCS, &[SHARED], None),
                branch("parent", "queued", DOCS, &[SHARED], Some("grand")),
            ],
            &[],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": "parent" }),
        )
        .await;

        assert_first_call(&response);
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(
            job.manifest.branches["notes"].base.as_deref(),
            Some("parent")
        );
    }

    #[tokio::test]
    async fn post_branch_finds_the_stranger_behind_a_chain_branch_that_sorts_first() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![
                branch("chain-a", "queued", DOCS, &[SHARED], None),
                branch("stranger-z", "queued", DOCS, &[SHARED], None),
            ],
            &[],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": "chain-a" }),
        )
        .await;

        assert_error_naming(&response, StatusCode::CONFLICT, "stranger-z");
    }

    #[tokio::test]
    async fn post_branch_overlapping_a_merged_branch_is_200() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![branch("old", "merged", CORE, &[SHARED], None)],
            &[],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_first_call(&response);
    }

    #[tokio::test]
    async fn post_branch_on_an_unknown_job_is_404() {
        let state = test_state().await;

        let response = post(
            &app(&state),
            &branches_uri("no-such-job"),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_error(&response, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_branch_on_a_finished_job_is_409_and_adds_nothing() {
        let state = test_state().await;
        seed_job(&state.pool, JOB, "finished", Vec::new(), &[]).await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_error(&response, StatusCode::CONFLICT);
        assert!(loaded(&state.pool, JOB).await.manifest.branches.is_empty());
    }

    #[tokio::test]
    async fn post_branch_with_an_unknown_base_is_422() {
        let state = test_state().await;
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": "ghost" }),
        )
        .await;

        assert_error_naming(&response, StatusCode::UNPROCESSABLE_ENTITY, "ghost");
        assert!(loaded(&state.pool, JOB).await.manifest.branches.is_empty());
    }

    /// A single tracked path among allowed ones is enough, or a docs branch could reach the tree.
    #[tokio::test]
    async fn post_branch_of_the_docs_layer_with_a_tracked_path_is_422() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;

        for paths in [vec![CODE_PATH], vec![SHARED, CODE_PATH]] {
            let response = post(&app, &branches_uri(JOB), docs_branch_body("notes", &paths)).await;

            assert_error_naming(&response, StatusCode::UNPROCESSABLE_ENTITY, CODE_PATH);
        }
        assert!(loaded(&state.pool, JOB).await.manifest.branches.is_empty());
    }

    /// A layer measured beyond any real disk, so this refusal holds whatever the machine has free.
    #[tokio::test]
    async fn post_branch_not_admitted_for_disk_is_409_with_reasons() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            Vec::new(),
            &[(HUGE_LAYER, HUGE_GB)],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "big", "paths": [CODE_PATH], "layer": HUGE_LAYER }),
        )
        .await;

        assert_error(&response, StatusCode::CONFLICT);
        let reasons = response.1["reasons"].as_array().expect("a reasons array");
        assert!(
            reasons
                .iter()
                .any(|reason| reason.as_str().is_some_and(|text| text.contains("disk"))),
            "{reasons:?}"
        );
        assert!(loaded(&state.pool, JOB).await.manifest.branches.is_empty());
    }

    fn branch_start_uri(job: &str) -> String {
        format!("/trees/{job}/branch_start")
    }

    fn branch_start_body(name: &str, layer: &str) -> Value {
        json!({
            "name": name,
            "goal": "Do the work",
            "prompt": "Do not run.",
            "owned_paths": [CODE_PATH],
            "layer": layer,
            "node_id": format!("{JOB}-root"),
        })
    }

    async fn table_rows(pool: &SqlitePool, table: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Gap 3. A start the disk alone refuses is remembered and asked about once, without parking
    /// the caller; the agent still gets its 409.
    #[tokio::test]
    async fn branch_start_refused_for_disk_alone_records_a_pending_start_and_asks_once() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            Vec::new(),
            &[(HUGE_LAYER, HUGE_GB)],
        )
        .await;
        let app = app(&state);
        let decisions_before = table_rows(&state.pool, "decisions").await;

        let first = post(
            &app,
            &branch_start_uri(JOB),
            branch_start_body("big", HUGE_LAYER),
        )
        .await;
        assert_error(&first, StatusCode::CONFLICT);

        // The pending start is stored under the manifest, with the body to replay.
        let job = loaded(&state.pool, JOB).await;
        assert!(job.manifest.branches.is_empty(), "no branch is added");
        let record = &job.manifest.extra["pending_starts"]["big"];
        assert_eq!(record["caller"], json!(format!("{JOB}-root")), "{record}");
        assert_eq!(record["layer"], json!(HUGE_LAYER), "{record}");
        assert_eq!(record["goal"], json!("Do the work"), "{record}");
        assert_eq!(record["prompt"], json!("Do not run."), "{record}");
        assert_eq!(record["paths"], json!([CODE_PATH]), "{record}");

        // One open decision on this tree and one feed line; nobody is parked.
        assert_eq!(
            table_rows(&state.pool, "decisions").await,
            decisions_before + 1
        );
        let open: Vec<(String, String)> =
            sqlx::query_as("SELECT tree_id, status FROM decisions WHERE tree_id IS NOT NULL")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(open, vec![(JOB.to_owned(), "open".to_owned())]);
        let lines: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'work_tree_parked'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(lines, 1);
        assert!(job.manifest.branches.values().all(|b| b.state != "parked"));

        // The same refusal again keeps one record and asks no second time.
        let again = post(
            &app,
            &branch_start_uri(JOB),
            branch_start_body("big", HUGE_LAYER),
        )
        .await;
        assert_error(&again, StatusCode::CONFLICT);
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(
            job.manifest.extra["pending_starts"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            table_rows(&state.pool, "decisions").await,
            decisions_before + 1
        );
    }

    /// A refusal that is not the disk's alone stays exactly as it is today: nothing is stored.
    #[tokio::test]
    async fn branch_start_refused_for_the_cap_records_nothing() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            live_branches(CAP, "building", FREE_LAYER),
            &[(FREE_LAYER, 0.0)],
        )
        .await;
        let decisions_before = table_rows(&state.pool, "decisions").await;
        let feed_before = table_rows(&state.pool, "feed").await;

        let response = post(
            &app(&state),
            &branch_start_uri(JOB),
            branch_start_body("api", FREE_LAYER),
        )
        .await;

        assert_error(&response, StatusCode::CONFLICT);
        let job = loaded(&state.pool, JOB).await;
        assert!(!job.manifest.extra.contains_key("pending_starts"));
        assert_eq!(table_rows(&state.pool, "decisions").await, decisions_before);
        assert_eq!(table_rows(&state.pool, "feed").await, feed_before);
    }

    /// Four live branches at no cost: the default cap refuses whatever the disk holds.
    #[tokio::test]
    async fn post_branch_not_admitted_at_the_cap_is_409_with_reasons() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            live_branches(CAP, "building", FREE_LAYER),
            &[(FREE_LAYER, 0.0)],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "api", "paths": [CODE_PATH], "layer": FREE_LAYER }),
        )
        .await;

        assert_error(&response, StatusCode::CONFLICT);
        let reasons = response.1["reasons"].as_array().expect("a reasons array");
        assert!(
            reasons.iter().any(|reason| reason
                .as_str()
                .is_some_and(|text| text.contains("max_live_branches"))),
            "{reasons:?}"
        );
    }

    #[tokio::test]
    async fn post_branch_admits_a_docs_layer_at_the_cap() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            live_branches(CAP, "building", FREE_LAYER),
            &[(FREE_LAYER, 0.0)],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_first_call(&response);
    }

    #[tokio::test]
    async fn post_branch_refuses_a_missing_or_null_required_field_with_400() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;

        for field in ["name", "paths", "layer"] {
            let mut absent = docs_branch_body("notes", &[SHARED]);
            absent.as_object_mut().unwrap().remove(field);
            let mut null = docs_branch_body("notes", &[SHARED]);
            null[field] = Value::Null;

            for body in [absent, null] {
                let response = post(&app, &branches_uri(JOB), body).await;

                assert_error_naming(&response, StatusCode::BAD_REQUEST, field);
            }
        }
        assert!(loaded(&state.pool, JOB).await.manifest.branches.is_empty());
    }

    #[tokio::test]
    async fn post_branch_refuses_malformed_json_with_400() {
        let state = test_state().await;
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;

        let response = post_raw(&app(&state), &branches_uri(JOB), "{not json").await;

        assert_error(&response, StatusCode::BAD_REQUEST);
    }

    // ----- POST /trees/{id}/nodes/{n}/transition: branch states -----

    #[tokio::test]
    async fn transition_moves_the_branch_of_the_node_and_persists_it() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "queued").await;

        let (status, body) = move_branch(&app(&state), JOB, NODE, "open", KEY).await;

        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert!(body["result"].is_string(), "{body:?}");
        assert_eq!(body["replayed"], json!(false));
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "open");
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tree_transitions
             WHERE node_id = ? AND transition = 'open' AND key = ?",
        )
        .bind(NODE)
        .bind(KEY)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn transition_replay_returns_the_stored_result_and_does_not_move_the_branch_back() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "queued").await;
        let (_, first) = move_branch(&app, JOB, NODE, "open", KEY).await;
        let (status, _) = move_branch(&app, JOB, NODE, "building", "key-2").await;
        assert_eq!(status, StatusCode::OK);

        // `building` to `open` is illegal, so a replay that re-applied would answer 409.
        let (status, replay) = move_branch(&app, JOB, NODE, "open", KEY).await;

        assert_eq!(status, StatusCode::OK, "{replay:?}");
        assert_eq!(replay["replayed"], json!(true));
        assert_eq!(replay["result"], first["result"]);
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "building");
        assert_eq!(transition_rows(&state.pool).await, 2);
    }

    /// Spec R4: the key is per node and transition, so one key may serve two transitions.
    #[tokio::test]
    async fn transition_with_the_same_key_and_another_transition_is_no_replay() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "queued").await;
        move_branch(&app, JOB, NODE, "open", KEY).await;

        let (status, body) = move_branch(&app, JOB, NODE, "building", KEY).await;

        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["replayed"], json!(false));
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "building");
    }

    #[tokio::test]
    async fn transition_that_is_illegal_is_409_and_spends_no_key() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "queued").await;

        let refused = move_branch(&app, JOB, NODE, "merged", KEY).await;

        assert_error(&refused, StatusCode::CONFLICT);
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "queued");
        assert_eq!(transition_rows(&state.pool).await, 0);

        // Walked to where `merged` is legal, the very key that was refused now succeeds.
        for (index, step) in ["open", "building", "gating", "green"]
            .into_iter()
            .enumerate()
        {
            let (status, body) = move_branch(&app, JOB, NODE, step, &format!("step-{index}")).await;
            assert_eq!(status, StatusCode::OK, "{step}: {body:?}");
        }
        let (status, body) = move_branch(&app, JOB, NODE, "merged", KEY).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["replayed"], json!(false));
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "merged");
    }

    /// `apply_transition` narrows a parked branch to the state it was parked from; the plain
    /// `can_transition` table would let it resume anywhere parkable.
    #[tokio::test]
    async fn transition_of_a_parked_branch_goes_through_apply_transition() {
        let state = test_state().await;
        let app = app(&state);
        let mut parked = with_node(branch(BRANCH, "parked", CORE, &[CODE_PATH], None), NODE);
        parked.park = Some(json!({ "prev_state": "building" }));
        seed_job(&state.pool, JOB, "open", vec![parked], &[]).await;

        let refused = move_branch(&app, JOB, NODE, "open", KEY).await;
        assert_error(&refused, StatusCode::CONFLICT);

        let (status, body) = move_branch(&app, JOB, NODE, "building", KEY).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "building");
    }

    /// `done` is no member of `BRANCH_STATES`, yet the engine knows it for the docs layer alone.
    #[tokio::test]
    async fn transition_to_done_is_known_and_legal_only_on_a_docs_branch() {
        let state = test_state().await;
        let app = app(&state);
        let docs = with_node(
            branch("notes", "building", DOCS, &[SHARED], None),
            "node-notes",
        );
        let code = with_node(branch(BRANCH, "building", CORE, &[CODE_PATH], None), NODE);
        seed_job(&state.pool, JOB, "open", vec![docs, code], &[]).await;

        let (status, body) = move_branch(&app, JOB, "node-notes", "done", KEY).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(branch_state(&state.pool, JOB, "notes").await, "done");

        let refused = move_branch(&app, JOB, NODE, "done", KEY).await;
        assert_error(&refused, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn transition_with_an_unknown_name_is_422_and_spends_no_key() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "queued").await;

        let response = move_branch(&app(&state), JOB, NODE, "teleport", KEY).await;

        assert_error(&response, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "queued");
        assert_eq!(transition_rows(&state.pool).await, 0);
    }

    #[tokio::test]
    async fn transition_on_a_node_of_another_job_is_404_and_moves_nothing() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "queued").await;
        let other = with_node(
            branch("web", "queued", CORE, &[OTHER_CODE_PATH], None),
            "node-web",
        );
        seed_job(&state.pool, OTHER_JOB, "open", vec![other], &[]).await;

        let response = move_branch(&app(&state), JOB, "node-web", "open", KEY).await;

        assert_error(&response, StatusCode::NOT_FOUND);
        assert_eq!(branch_state(&state.pool, OTHER_JOB, "web").await, "queued");
        assert_eq!(transition_rows(&state.pool).await, 0);
    }

    #[tokio::test]
    async fn transition_on_an_unknown_node_or_job_is_404() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "queued").await;

        for (job, node) in [(JOB, "no-such-node"), ("no-such-job", NODE)] {
            let response = move_branch(&app, job, node, "open", KEY).await;

            assert_error(&response, StatusCode::NOT_FOUND);
        }
        assert_eq!(transition_rows(&state.pool).await, 0);
    }

    #[tokio::test]
    async fn transition_refuses_a_missing_or_null_required_field_with_400() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "queued").await;

        for body in [
            json!({ "key": KEY }),
            json!({ "transition": null, "key": KEY }),
            json!({ "transition": "open" }),
            json!({ "transition": "open", "key": null }),
        ] {
            let response = post(&app, &transition_uri(JOB, NODE), body).await;

            assert_error(&response, StatusCode::BAD_REQUEST);
        }
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "queued");
    }

    #[tokio::test]
    async fn transition_refuses_malformed_json_with_400() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "queued").await;

        let response = post_raw(&app(&state), &transition_uri(JOB, NODE), "{not json").await;

        assert_error(&response, StatusCode::BAD_REQUEST);
    }

    // ----- POST /trees/{id}/nodes/{n}/transition: launch -----

    fn launch_body(key: &str) -> Value {
        json!({ "transition": "launch", "key": key, "prompt": "do it", "mode": "shadow" })
    }

    /// `create.rs:388`: worktree mode with no project and no cwd is `CreateRunError::Invalid`,
    /// raised before any run row exists.
    fn failing_launch_body(key: &str) -> Value {
        json!({ "transition": "launch", "key": key, "prompt": "do it", "mode": "worktree" })
    }

    /// The run id a launch answered, which is the id `launch` stores as the transition's result.
    fn launched_run(response: &(StatusCode, Value)) -> i64 {
        assert_eq!(response.0, StatusCode::OK, "{:?}", response.1);
        response.1["result"]
            .as_str()
            .and_then(|text| text.parse().ok())
            .unwrap_or_else(|| panic!("no run id in {:?}", response.1))
    }

    #[tokio::test]
    async fn launch_starts_one_run_and_links_it_on_the_node() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "building").await;

        let response = post(&app(&state), &transition_uri(JOB, NODE), launch_body(KEY)).await;

        let run_id = launched_run(&response);
        assert_eq!(run_rows(&state.pool).await, 1);
        assert_eq!(
            node_run_id(&state.pool, NODE).await,
            Some(run_id.to_string())
        );
        let (prompt, mode): (String, String) =
            sqlx::query_as("SELECT prompt, mode FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(prompt, "do it");
        assert_eq!(mode, "shadow");
    }

    #[tokio::test]
    async fn launch_replay_returns_the_same_run_and_creates_no_second() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "building").await;
        let first = launched_run(&post(&app, &transition_uri(JOB, NODE), launch_body(KEY)).await);

        let replay = launched_run(&post(&app, &transition_uri(JOB, NODE), launch_body(KEY)).await);

        assert_eq!(replay, first);
        assert_eq!(run_rows(&state.pool).await, 1);
    }

    /// Decision 4: plain `apply` or `launch` would leave an empty reservation and poison the key.
    #[tokio::test]
    async fn launch_whose_create_fails_refuses_and_the_same_key_then_starts_the_run() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "building").await;

        let (status, body) = post(&app, &transition_uri(JOB, NODE), failing_launch_body(KEY)).await;

        assert!(
            status.is_client_error() || status.is_server_error(),
            "{status}: {body:?}"
        );
        assert!(body["error"].is_string(), "{body:?}");
        assert_eq!(run_rows(&state.pool).await, 0);
        assert_eq!(node_run_id(&state.pool, NODE).await, None);

        let retried = post(&app, &transition_uri(JOB, NODE), launch_body(KEY)).await;

        let run_id = launched_run(&retried);
        assert_eq!(run_rows(&state.pool).await, 1);
        assert_eq!(
            node_run_id(&state.pool, NODE).await,
            Some(run_id.to_string())
        );

        let replay = post(&app, &transition_uri(JOB, NODE), launch_body(KEY)).await;

        assert_eq!(launched_run(&replay), run_id);
        assert_eq!(run_rows(&state.pool).await, 1);
    }

    #[tokio::test]
    async fn launch_without_a_prompt_is_422_and_spends_no_key() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "building").await;

        for body in [
            json!({ "transition": "launch", "key": KEY, "mode": "shadow" }),
            json!({ "transition": "launch", "key": KEY, "mode": "shadow", "prompt": null }),
        ] {
            let response = post(&app, &transition_uri(JOB, NODE), body).await;

            assert_error_naming(&response, StatusCode::UNPROCESSABLE_ENTITY, "prompt");
        }
        assert_eq!(run_rows(&state.pool).await, 0);
        assert_eq!(transition_rows(&state.pool).await, 0);

        let response = post(&app, &transition_uri(JOB, NODE), launch_body(KEY)).await;

        launched_run(&response);
    }

    #[tokio::test]
    async fn launch_on_a_node_of_another_job_is_404_and_creates_no_run() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "building").await;
        let other = with_node(
            branch("web", "building", CORE, &[OTHER_CODE_PATH], None),
            "node-web",
        );
        seed_job(&state.pool, OTHER_JOB, "open", vec![other], &[]).await;

        let response = post(
            &app(&state),
            &transition_uri(JOB, "node-web"),
            launch_body(KEY),
        )
        .await;

        assert_error(&response, StatusCode::NOT_FOUND);
        assert_eq!(run_rows(&state.pool).await, 0);
        assert_eq!(node_run_id(&state.pool, "node-web").await, None);
    }

    #[tokio::test]
    async fn launch_does_not_move_the_branch() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "building").await;

        let response = post(&app(&state), &transition_uri(JOB, NODE), launch_body(KEY)).await;

        launched_run(&response);
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "building");
    }

    /// The route and the recovering launch must read one key space, or a route replay could start a
    /// second run. This is also what types the sibling leaf's signature.
    #[tokio::test]
    async fn launch_route_and_launch_recovering_share_the_key() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "building").await;
        let routed =
            launched_run(&post(&app(&state), &transition_uri(JOB, NODE), launch_body(KEY)).await);

        let direct: Result<i64, NodeRunError> =
            launch_recovering(&state, NODE, KEY, "do it".to_owned(), None, None, "shadow").await;

        assert_eq!(direct.unwrap(), routed);
        assert_eq!(run_rows(&state.pool).await, 1);
    }

    // ----- concurrency -----

    const ROUNDS: usize = 20;

    /// A load and a whole-document write with no lock between them would lose one of the two moves.
    /// Several rounds, because one round may happen not to overlap.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_transitions_on_two_branches_both_survive() {
        let state = shared_state().await;
        let app = app(&state);

        for round in 0..ROUNDS {
            let job = format!("job-{round}");
            let (left_node, right_node) = (format!("left-{round}"), format!("right-{round}"));
            let left = with_node(
                branch("left", "queued", CORE, &[CODE_PATH], None),
                &left_node,
            );
            let right = with_node(
                branch("right", "queued", CORE, &[OTHER_CODE_PATH], None),
                &right_node,
            );
            seed_job(&state.pool, &job, "open", vec![left, right], &[]).await;
            let body = json!({ "transition": "open", "key": KEY });

            let (first, second) = tokio::join!(
                spawn_post(&app, transition_uri(&job, &left_node), body.clone()),
                spawn_post(&app, transition_uri(&job, &right_node), body),
            );

            assert_eq!(first.unwrap().0, StatusCode::OK, "round {round}");
            assert_eq!(second.unwrap().0, StatusCode::OK, "round {round}");
            let manifest = loaded(&state.pool, &job).await.manifest;
            assert_eq!(manifest.branches["left"].state, "open", "round {round}");
            assert_eq!(manifest.branches["right"].state, "open", "round {round}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_transitions_with_one_key_move_the_branch_once() {
        let state = shared_state().await;
        let app = app(&state);

        for round in 0..ROUNDS {
            let job = format!("job-{round}");
            let node = format!("node-{round}");
            let only = with_node(branch(BRANCH, "queued", CORE, &[CODE_PATH], None), &node);
            seed_job(&state.pool, &job, "open", vec![only], &[]).await;
            let body = json!({ "transition": "open", "key": KEY });

            let (first, second) = tokio::join!(
                spawn_post(&app, transition_uri(&job, &node), body.clone()),
                spawn_post(&app, transition_uri(&job, &node), body),
            );

            let (first, second) = (first.unwrap(), second.unwrap());
            assert_eq!(first.0, StatusCode::OK, "round {round}: {:?}", first.1);
            assert_eq!(second.0, StatusCode::OK, "round {round}: {:?}", second.1);
            assert_eq!(first.1["result"], second.1["result"], "round {round}");
            assert_ne!(first.1["replayed"], second.1["replayed"], "round {round}");
            assert_eq!(branch_state(&state.pool, &job, BRANCH).await, "open");
        }
    }

    /// Docs branches, so that the outcome turns on the overlap and not on the disk.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_branches_with_overlapping_paths_admit_at_most_one() {
        let state = shared_state().await;
        let app = app(&state);

        for round in 0..ROUNDS {
            let job = format!("job-{round}");
            seed_job(&state.pool, &job, "open", Vec::new(), &[]).await;

            let (first, second) = tokio::join!(
                spawn_post(
                    &app,
                    branches_uri(&job),
                    docs_branch_body("left", &[SHARED])
                ),
                spawn_post(
                    &app,
                    branches_uri(&job),
                    docs_branch_body("right", &[SHARED])
                ),
            );

            let mut statuses = [first.unwrap().0, second.unwrap().0];
            statuses.sort();
            assert_eq!(
                statuses,
                [StatusCode::OK, StatusCode::CONFLICT],
                "round {round}"
            );
            let manifest = loaded(&state.pool, &job).await.manifest;
            assert_eq!(manifest.branches.len(), 1, "round {round}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_branches_with_separate_paths_both_survive() {
        let state = shared_state().await;
        let app = app(&state);

        for round in 0..ROUNDS {
            let job = format!("job-{round}");
            seed_job(&state.pool, &job, "open", Vec::new(), &[]).await;

            let (first, second) = tokio::join!(
                spawn_post(
                    &app,
                    branches_uri(&job),
                    docs_branch_body("left", &[SHARED])
                ),
                spawn_post(
                    &app,
                    branches_uri(&job),
                    docs_branch_body("right", &[OTHER_DOCS])
                ),
            );

            assert_eq!(first.unwrap().0, StatusCode::OK, "round {round}");
            assert_eq!(second.unwrap().0, StatusCode::OK, "round {round}");
            let manifest = loaded(&state.pool, &job).await.manifest;
            assert_eq!(manifest.branches.len(), 2, "round {round}");
        }
    }
}
