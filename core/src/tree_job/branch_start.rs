//! `POST /trees/{id}/branch_start`: the whole start of one branch in a single call, for the
//! `tree_branch_start` tool. Five steps, each idempotent, so that a call repeated after a crash (or
//! after a client timeout) finishes the steps the first one did not reach and makes nothing twice:
//! add the branch, open it (worktree), open a work node under the caller, bind the two, launch the
//! agent under the key `branch-<name>`.
//!
//! **Locks.** `open_branch` and `bind_branch` take `http::job_lock` themselves and the lock is not
//! reentrant, so this handler holds it only around the lookup-or-open of the node, which touches no
//! manifest, and never around a step that takes it.
//!
//! **Who reaches it.** Like every `/trees*` route it is in no `auth.rs` table: the control token and
//! an Admin API key, plus one door: `auth::tree_run_admits` admits a `Scope::Run` key on this route
//! for the tree its run's work node belongs to, and for no other tree.

use std::path::Path as FsPath;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{Request, StatusCode};
use axum::routing::post;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::http::{job_lock, refuse};
use super::model::JOB_TERMINAL_STATE;
use super::nested_prompt;
use super::nesting;
use super::node_run::{self, NodeRunError};
use super::open::{self, OpenError};
use super::partition::Kind;
use super::start_log::{self, StartShape};
use super::store::{self, StoreError, launch_recovering};
use crate::auth::Scope;
use crate::conductor::depth::{self, ChildArgs, TreeLimits};
use crate::execution::runs::CreateRunError;
use crate::state::AppState;
use crate::tree_cost::budget::{self, Permit};
use crate::work_nodes::{self, NodeError, NodeLimits, OpenArgs};

/// The layer of a branch started without one (`root.rs` makes the first branch a `code` one).
const DEFAULT_LAYER: &str = "code";
/// A tree branch's agent acts in its own worktree, so it is a worktree run (never `shadow`, which
/// launches in plan mode).
const LAUNCH_MODE: &str = "worktree";

type Refusal = (StatusCode, Json<Value>);

pub fn routes() -> Router<AppState> {
    Router::new().route("/trees/{id}/branch_start", post(post_branch_start))
}

#[derive(Debug, Deserialize)]
struct StartBody {
    name: Option<String>,
    goal: Option<String>,
    owned_paths: Option<Vec<String>>,
    prompt: Option<String>,
    layer: Option<String>,
    base: Option<String>,
    node_id: Option<String>,
    /// `leaf` (the default) or `orchestrator`.
    kind: Option<String>,
    /// What the branch is estimated to take to build, in tokens.
    estimate_tokens: Option<i64>,
}

/// The kind a start asks for: `leaf` when left out, else `leaf` or `orchestrator`.
fn requested_kind(kind: Option<&str>) -> Result<Kind, Refusal> {
    match kind.map(str::trim) {
        None | Some("") | Some("leaf") => Ok(Kind::Leaf),
        Some("orchestrator") => Ok(Kind::Orchestrator),
        Some(other) => Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("kind {other:?} must be \"leaf\" or \"orchestrator\""),
        )),
    }
}

fn required(field: &str, value: Option<String>) -> Result<String, Refusal> {
    value
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, format!("{field} is required")))
}

/// The rule of `open.rs`'s `valid_name`, checked before the first step so that a name the open step
/// would refuse never leaves a queued branch behind.
fn valid_name(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !value.contains("..")
        && !value.ends_with('.')
        && !value.ends_with(".lock")
}

fn db_refusal(error: sqlx::Error) -> Refusal {
    tracing::warn!(%error, "tree branch start storage failed");
    refuse(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("database error: {error}"),
    )
}

fn store_refusal(error: StoreError) -> Refusal {
    let status = match &error {
        StoreError::NotFound(_) => StatusCode::NOT_FOUND,
        _ => {
            tracing::warn!(%error, "tree branch start storage failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    refuse(status, error.to_string())
}

fn node_refusal(error: NodeError) -> Refusal {
    refuse(error.status_code(), error.to_string())
}

/// The statuses of `open.rs`'s own routes; an admission refusal keeps its reasons.
fn open_refusal(error: OpenError) -> Refusal {
    let status = match &error {
        OpenError::JobNotFound(_) | OpenError::BranchNotFound(_) | OpenError::NodeNotFound(_) => {
            StatusCode::NOT_FOUND
        }
        OpenError::BadName(_) => StatusCode::UNPROCESSABLE_ENTITY,
        OpenError::Git(_) | OpenError::Link(_) | OpenError::Store(_) | OpenError::Db(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        _ => StatusCode::CONFLICT,
    };
    if let OpenError::NotAdmitted(reasons) = &error {
        return (
            status,
            Json(json!({ "error": error.to_string(), "reasons": reasons })),
        );
    }
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::warn!(%error, "tree branch start failed");
    }
    refuse(status, error.to_string())
}

/// The statuses `http.rs` gives a failed launch.
fn run_refusal(error: NodeRunError) -> Refusal {
    let status = match &error {
        NodeRunError::NodeNotFound(_) => StatusCode::NOT_FOUND,
        NodeRunError::Unresolved { .. } => StatusCode::CONFLICT,
        NodeRunError::Create(CreateRunError::Invalid(_)) => StatusCode::UNPROCESSABLE_ENTITY,
        NodeRunError::Create(CreateRunError::Busy | CreateRunError::NoRoomOnDisk(_)) => {
            StatusCode::CONFLICT
        }
        _ => {
            tracing::warn!(%error, "tree branch start launch failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    refuse(status, error.to_string())
}

/// Posts `body` to `uri` on `router`, in this process and with no authentication layer: the
/// handlers it reaches are the ones their own routes run, and this route is already behind the
/// token check. A success answers its JSON; any other status answers as it came, reasons included.
async fn call(
    router: Router<AppState>,
    state: &AppState,
    uri: String,
    body: Value,
) -> Result<Value, Refusal> {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .map_err(|error| refuse(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let Ok(response) = router.with_state(state.clone()).oneshot(request).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|error| refuse(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        Ok(value)
    } else {
        Err((status, Json(value)))
    }
}

/// The node already made for branch `name` under `caller`, if any.
async fn existing_node(
    state: &AppState,
    caller: &str,
    name: &str,
) -> Result<Option<String>, Refusal> {
    sqlx::query_scalar(
        "SELECT id FROM work_nodes WHERE parent_id = ? AND kind IN ('leaf', 'orchestrator')
         AND title = ? ORDER BY created_at, id LIMIT 1",
    )
    .bind(caller)
    .bind(name)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_refusal)
}

/// The kind of the node a branch already has (bound, or made by an earlier call), if any: a replay
/// keeps it, whatever the guards would say now.
async fn known_kind(
    state: &AppState,
    bound: Option<&str>,
    caller: &str,
    name: &str,
) -> Result<Option<Kind>, Refusal> {
    let node = match bound {
        Some(bound) => Some(bound.to_owned()),
        None => existing_node(state, caller, name).await?,
    };
    let Some(node) = node else {
        return Ok(None);
    };
    let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM work_nodes WHERE id = ?")
        .bind(node)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_refusal)?;
    Ok(kind.map(|kind| {
        if kind == "orchestrator" {
            Kind::Orchestrator
        } else {
            Kind::Leaf
        }
    }))
}

/// 409 when the tree's budget does not permit one more node.
async fn check_tree_budget(state: &AppState, root_id: &str) -> Result<(), Refusal> {
    match budget::permits_next_node(&state.pool, root_id).await {
        Permit::Yes => Ok(()),
        Permit::No { reason } => Err(refuse(StatusCode::CONFLICT, reason)),
    }
}

/// Writes the tree's soft-threshold feed line when it is newly crossed; a failure is logged, never
/// the request's.
async fn report_soft(state: &AppState, root_id: &str) {
    if let Err(error) = budget::report_soft_threshold(&state.pool, root_id, Utc::now()).await {
        tracing::warn!(%error, root_id, "could not report the tree's soft budget threshold");
    }
}

/// The node already made for branch `name` under `caller`, else a new one of `kind`. The job lock
/// makes the lookup and the insert one step; nothing in `work_nodes::open` takes that lock. A new
/// node is opened only while the tree's budget permits one; finding one costs nothing.
#[allow(clippy::too_many_arguments)]
async fn find_or_open_node(
    state: &AppState,
    job_id: &str,
    root_id: &str,
    project_root: &str,
    caller: &str,
    name: &str,
    goal: &str,
    paths: &[String],
    kind: Kind,
) -> Result<(String, bool), Refusal> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    if let Some(found) = existing_node(state, caller, name).await? {
        return Ok((found, true));
    }
    check_tree_budget(state, root_id).await?;
    let limits = NodeLimits::load(Some(FsPath::new(project_root)));
    // The tree's own root closes as `done` when its dispatch report arrives, while a start it made
    // may still wait for its base: the driver's replay (and the owner's start) must still reach it.
    let links = work_nodes::OpenLinks {
        parent_may_be_done: caller == root_id,
        ..work_nodes::OpenLinks::default()
    };
    let opened = work_nodes::open_linked(
        &state.pool,
        None,
        &limits,
        &OpenArgs {
            parent_id: Some(caller.to_owned()),
            kind: kind.as_str().to_owned(),
            title: name.to_owned(),
            goal: goal.to_owned(),
            owned_paths: paths.to_vec(),
            ..OpenArgs::default()
        },
        links,
    )
    .await
    .map_err(node_refusal)?;
    report_soft(state, root_id).await;
    Ok((opened.id, false))
}

/// Whether the launch `(node, key)` already holds a run id.
async fn launch_recorded(state: &AppState, node: &str, key: &str) -> Result<bool, Refusal> {
    let stored: Option<Option<String>> = sqlx::query_scalar(
        "SELECT result FROM tree_transitions WHERE node_id = ? AND transition = ? AND key = ?",
    )
    .bind(node)
    .bind(node_run::LAUNCH)
    .bind(key)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_refusal)?;
    Ok(stored.flatten().is_some_and(|result| !result.is_empty()))
}

/// The body of a start the disk refused, as stored under `manifest.extra["pending_starts"]`.
struct PendingStart<'a> {
    caller: &'a str,
    goal: &'a str,
    prompt: &'a str,
    paths: &'a [String],
    layer: &'a str,
    base: Option<&'a str>,
    /// Set when the start waits for this base branch to merge rather than for the disk.
    waits_for_base: Option<&'a str>,
    /// The kind asked for (not the one decided): the replay decides again.
    kind: Kind,
    estimate: Option<i64>,
    parent: Option<&'a str>,
}

/// The base a queued branch waits on: named, in the tree and not merged yet. The same test
/// `open_branch` applies before it refuses with `BaseNotMerged`.
fn unmerged_base<'m>(
    manifest: &'m super::model::Manifest,
    branch: &'m super::model::Branch,
) -> Option<&'m str> {
    let base = branch.base.as_deref()?;
    manifest
        .branches
        .get(base)
        .is_some_and(|base_branch| base_branch.state != "merged")
        .then_some(base)
}

/// Stores a start the disk alone refused under `manifest.extra["pending_starts"][name]` and asks
/// the owner once, without parking the caller. Idempotent by name: a name already pending is left
/// as it is and asks nothing. A failure is logged, never the request's: the agent gets its 409
/// either way.
async fn remember_pending_start(
    state: &AppState,
    job_id: &str,
    name: &str,
    pending: &PendingStart<'_>,
) {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let mut job = match store::load(&state.pool, job_id).await {
        Ok(Some(job)) => job,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, job = job_id, "could not read the job to record a pending start");
            return;
        }
    };
    if job
        .manifest
        .extra
        .get("pending_starts")
        .and_then(|held| held.get(name))
        .is_some()
    {
        return;
    }
    // The record goes first, without a decision id: a crash after it leaves a record that the next
    // driver pass heals (`heal_pending_decision`), never a decision no record knows about. It
    // carries the generation, so a replay after a crash computes the same decision id.
    let generation = current_generation(&job.manifest.extra);
    let record = json!({
        "generation": generation,
        "caller": pending.caller,
        "goal": pending.goal,
        "prompt": pending.prompt,
        "paths": pending.paths,
        "layer": pending.layer,
        "base": pending.base,
        "waits_for_base": pending.waits_for_base,
        "kind": pending.kind.as_str(),
        "estimate_tokens": pending.estimate,
        "parent": pending.parent,
    });
    let slot = job
        .manifest
        .extra
        .entry("pending_starts".to_owned())
        .or_insert_with(|| json!({}));
    if !slot.is_object() {
        *slot = json!({});
    }
    if let Some(map) = slot.as_object_mut() {
        map.insert(name.to_owned(), record);
    }
    job.updated_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    if let Err(error) = store::update(&state.pool, &job).await {
        tracing::warn!(%error, job = job_id, branch = name, "could not record a pending start");
        return;
    }
    // A start that waits for its base asks nobody: the driver replays it when the base merges.
    if pending.waits_for_base.is_some() {
        return;
    }
    // Then the ask, then the id into the record. Both failures are healed by the next pass.
    let asked = match ask_for_pending(
        &state.pool,
        job_id,
        name,
        generation,
        pending.caller,
        pending.layer,
    )
    .await
    {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(%error, job = job_id, branch = name, "could not ask about a pending start");
            return;
        }
    };
    if set_decision_id(&mut job, name, &asked)
        && let Err(error) = store::update(&state.pool, &job).await
    {
        tracing::warn!(%error, job = job_id, branch = name, "could not store a pending start's decision id");
    }
}

/// `manifest.extra` key counting how many pending records of a branch were consumed, plus one: the
/// generation the next record is written under.
pub(super) const GENERATION_KEY: &str = "pending_start_generation";

/// The generation a pending record written now gets: the manifest's counter, 1 when it has none.
pub(super) fn current_generation(extra: &std::collections::BTreeMap<String, Value>) -> u64 {
    extra
        .get(GENERATION_KEY)
        .and_then(Value::as_u64)
        .unwrap_or(1)
}

/// The id of the decision that tells the owner a pending start waits: fixed by job, branch name and
/// generation, so an ask whose id never reached the record is found again, not made twice, while a
/// later refusal of the same branch (a new generation) is a new question.
pub(super) fn pending_decision_id(job_id: &str, name: &str, generation: u64) -> String {
    format!("pending-{job_id}-{name}-{generation}")
}

/// Opens (insert-or-ignore) the decision of a pending start and returns its id.
async fn ask_for_pending(
    pool: &sqlx::SqlitePool,
    job_id: &str,
    name: &str,
    generation: u64,
    caller: &str,
    layer: &str,
) -> Result<String, String> {
    let question = format!(
        "Branch {name} (layer {layer}) waits for disk space and starts by itself once there is room."
    );
    super::decisions::ask_pending_with_id(
        pool,
        job_id,
        caller,
        &question,
        &pending_decision_id(job_id, name, generation),
    )
    .await
    .map(|asked| asked.id)
    .map_err(|e| e.to_string())
}

/// Writes `id` as the `decision_id` of the pending record of `name`. False when there is no record.
fn set_decision_id(job: &mut store::TreeJob, name: &str, id: &str) -> bool {
    let record = job
        .manifest
        .extra
        .get_mut("pending_starts")
        .and_then(|held| held.get_mut(name))
        .and_then(Value::as_object_mut);
    match record {
        Some(record) => {
            record.insert("decision_id".to_owned(), json!(id));
            job.updated_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
            true
        }
        None => false,
    }
}

/// A pending record with no `decision_id` (a crash between the record and the ask, or the ask and
/// the id) asks now, once, and stores the id. Under the job's lock, from a fresh read; the caller
/// must not hold it. A record that is gone or already has an id is left alone.
pub(super) async fn heal_pending_decision(
    state: &AppState,
    job_id: &str,
    name: &str,
) -> Result<(), String> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let Some(mut job) = store::load(&state.pool, job_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let Some(record) = job
        .manifest
        .extra
        .get("pending_starts")
        .and_then(|held| held.get(name))
    else {
        return Ok(());
    };
    if record["decision_id"].as_str().is_some() {
        return Ok(());
    }
    let caller = record["caller"].as_str().unwrap_or_default().to_owned();
    let layer = record["layer"].as_str().unwrap_or(DEFAULT_LAYER).to_owned();
    // The record's own generation; one written before generations existed is generation 1.
    let generation = record["generation"].as_u64().unwrap_or(1);
    let id = ask_for_pending(&state.pool, job_id, name, generation, &caller, &layer).await?;
    if set_decision_id(&mut job, name, &id) {
        store::update(&state.pool, &job)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Replays the stored start of a pending branch through `/branch_start`, with the body it was
/// refused with: every step is idempotent and the launch key is `branch-<name>`, so a replay after
/// a crash makes nothing twice. The caller must not hold the job's lock. `Err` carries the reason.
pub(super) async fn start_pending(
    state: &AppState,
    job_id: &str,
    name: &str,
    record: &Value,
) -> Result<Value, String> {
    let body = json!({
        "name": name,
        "goal": record["goal"],
        "prompt": record["prompt"],
        "owned_paths": record["paths"],
        "layer": record["layer"],
        "base": record["base"],
        "node_id": record["caller"],
        // Records written before nesting carry neither: a leaf with no estimate. The parent is
        // worked out again from the caller.
        "kind": record["kind"],
        "estimate_tokens": record["estimate_tokens"],
    });
    call(
        routes(),
        state,
        format!("/trees/{job_id}/branch_start"),
        body,
    )
    .await
    .map_err(|(status, Json(refusal))| {
        let reason = refusal["error"].as_str().unwrap_or("no reason given");
        format!("{status}: {reason}")
    })
}

/// `POST /trees/{id}/branch_start`. Checked first: the names, then the job, the caller (404 when it
/// is not a node of this tree) and a finished job (409). Then the five steps; a refusal at any of
/// them answers as that step does and leaves the earlier ones in place for the replay. `replayed` is
/// true only when every step found its work already done.
async fn post_branch_start(
    State(state): State<AppState>,
    Path(id): Path<String>,
    scope: Option<Extension<Scope>>,
    body: Result<Json<StartBody>, JsonRejection>,
) -> Result<Json<Value>, Refusal> {
    let body = body
        .map(|Json(body)| body)
        .map_err(|rejection| refuse(StatusCode::BAD_REQUEST, rejection.body_text()));
    let asked = body.as_ref().ok().and_then(|body| body.name.clone());
    let outcome = match body {
        Ok(body) => start_branch(&state, &id, scope.map(|Extension(scope)| scope), body).await,
        Err(refusal) => Err(refusal),
    };
    if let Err((status, Json(refusal))) = &outcome {
        let reason = refusal["error"].as_str().unwrap_or("no reason given");
        start_log::record_refusal(&state, &id, asked.as_deref(), status.as_u16(), reason).await;
    }
    outcome
}

/// 403 when the request carries a run key bound to nodes that do not include `caller`: a node opens
/// only its own children. No key (the control token, an Admin key, the driver's replay) opens any
/// node of the tree. A run bound to a leaf is refused too (owner decision 2026-11-08): a leaf that
/// outgrows its budget finishes the best leaf it can and says so, it does not open branches.
async fn check_own_node(
    state: &AppState,
    scope: Option<&Scope>,
    caller: &work_nodes::Node,
) -> Result<(), Refusal> {
    let Some(Scope::Run(run_id)) = scope else {
        return Ok(());
    };
    let bound = nesting::run_bound_nodes(&state.pool, *run_id)
        .await
        .map_err(db_refusal)?;
    if !bound.contains(&caller.id) {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            format!(
                "a node opens only its own children: node_id {} is not the node this run is bound to",
                caller.id
            ),
        ));
    }
    if caller.kind == work_nodes::KIND_LEAF {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "a leaf does not open branches; report that the work exceeds the leaf budget",
        ));
    }
    Ok(())
}

/// The steps of `post_branch_start`, apart from recording its refusals.
async fn start_branch(
    state: &AppState,
    id: &str,
    scope: Option<Scope>,
    body: StartBody,
) -> Result<Json<Value>, Refusal> {
    let state = state.clone();
    let id = id.to_owned();
    let name = required("name", body.name)?;
    let goal = required("goal", body.goal)?;
    let prompt = required("prompt", body.prompt)?;
    let caller = required("node_id", body.node_id)?;
    let paths = body
        .owned_paths
        .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "owned_paths is required"))?;
    let layer = body
        .layer
        .filter(|layer| !layer.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_LAYER.to_owned());
    let base = body.base.filter(|base| !base.trim().is_empty());
    let kind_omitted = body
        .kind
        .as_deref()
        .is_none_or(|kind| kind.trim().is_empty());
    let requested = requested_kind(body.kind.as_deref())?;
    let estimate = body.estimate_tokens;
    if estimate.is_some_and(|estimate| estimate <= 0) {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "estimate_tokens must be a positive number of tokens",
        ));
    }
    for (what, value) in [("tree id", &id), ("branch name", &name)] {
        if !valid_name(value) {
            return Err(refuse(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("{what} {value:?} must be letters, digits, '.', '_' or '-'"),
            ));
        }
    }

    let job = store::load(&state.pool, &id)
        .await
        .map_err(store_refusal)?
        .ok_or_else(|| refuse(StatusCode::NOT_FOUND, format!("tree job {id} not found")))?;
    let not_in_tree = || {
        refuse(
            StatusCode::NOT_FOUND,
            format!("node {caller} is not a node of tree job {id}"),
        )
    };
    let caller_node = match work_nodes::get(&state.pool, &caller).await {
        Ok(node) => node,
        Err(NodeError::NotFound) => return Err(not_in_tree()),
        Err(error) => return Err(node_refusal(error)),
    };
    if caller_node.root_id != job.root_node_id {
        return Err(not_in_tree());
    }
    check_own_node(&state, scope.as_ref(), &caller_node).await?;
    if job.state == JOB_TERMINAL_STATE {
        return Err(refuse(
            StatusCode::CONFLICT,
            format!("tree job {id} is finished and takes no new branch"),
        ));
    }
    // A docs branch has no worktree and its write guard is the classifier hook, so a root without
    // that hook takes no docs branch: nothing else would enforce the guard.
    let docs = layer == "docs"
        || job
            .manifest
            .branches
            .get(&name)
            .is_some_and(|branch| branch.layer == "docs");
    if docs
        && !crate::governance::autopilot::classifier_hook_is_wired(FsPath::new(
            &job.manifest.project_root,
        ))
    {
        return Err(refuse(
            StatusCode::CONFLICT,
            format!(
                "branch {name} is a docs branch: it runs in the main checkout and its guard is the \
                 classifier hook, and the classifier hook is not wired in {}",
                job.manifest.project_root
            ),
        ));
    }

    // The kind and the parent, decided before anything is stored. A branch that already has a node
    // keeps its kind and parent (a replay), whatever the guards would say now.
    let limits = NodeLimits::load(Some(FsPath::new(&job.manifest.project_root)));
    let child_depth = caller_node.depth + 1;
    if let Some(max) = limits.max_depth
        && child_depth > max
    {
        return Err(refuse(
            StatusCode::CONFLICT,
            format!("depth {child_depth} exceeds the limit of {max}"),
        ));
    }
    let nested = caller != job.root_node_id && !docs;
    let existing = job.manifest.branches.get(&name);
    let parent = match existing {
        Some(existing) => existing.parent.clone(),
        None if nested => nesting::parent_branch_of(&job.manifest, &caller),
        None => None,
    };
    let known = known_kind(
        &state,
        existing.and_then(|branch| branch.node_id.as_deref()),
        &caller,
        &name,
    )
    .await?;
    let decision = match known {
        Some(kind) => nesting::Decision {
            kind,
            degraded: None,
        },
        None => {
            let refusal = if nested {
                let chain = nesting::ancestor_chain(&state.pool, caller_node)
                    .await
                    .map_err(node_refusal)?;
                let total = nesting::tree_node_count(&state.pool, &job.root_node_id)
                    .await
                    .map_err(db_refusal)?;
                let args = ChildArgs {
                    owned_paths: paths.clone(),
                    goal: goal.clone(),
                };
                depth::check_child(&chain, &args, total, &TreeLimits::default()).err()
            } else {
                None
            };
            if requested == Kind::Leaf
                && let Some(refusal) = &refusal
            {
                return Err(refuse(StatusCode::CONFLICT, nesting::refusal_text(refusal)));
            }
            if docs && requested == Kind::Orchestrator {
                nesting::Decision {
                    kind: Kind::Leaf,
                    degraded: Some(
                        "a docs branch is never an orchestrator: opened as a leaf".to_owned(),
                    ),
                }
            } else {
                nesting::decide_kind(
                    requested,
                    estimate,
                    limits.leaf_budget,
                    child_depth,
                    limits.max_depth,
                    refusal,
                )
            }
        }
    };
    let kind = decision.kind;
    let shape = StartShape {
        name: &name,
        kind,
        depth: child_depth,
        estimate,
        leaf_budget: limits.leaf_budget,
        degraded: decision.degraded.as_deref(),
        kind_omitted,
    };
    let shape_json = json!({
        "kind": kind.as_str(),
        "depth": child_depth,
        "parent": parent,
        "degraded": decision.degraded,
    });

    // A branch with no node yet will need a new one at step 3. Refuse now when the budget does not
    // permit it, so that a refused call has not added a branch or opened a worktree the driver could
    // pick up. Step 3 checks again under the job lock.
    let already_bound = job
        .manifest
        .branches
        .get(&name)
        .is_some_and(|branch| branch.node_id.is_some());
    if !already_bound && existing_node(&state, &caller, &name).await?.is_none() {
        check_tree_budget(&state, &job.root_node_id).await?;
    }

    // 1. The branch, through the route that adds one: its replay, overlap and admission checks
    // are the ones a direct call gets, and nothing is stored when it refuses.
    let added = match call(
        super::http::routes(),
        &state,
        format!("/trees/{id}/branches"),
        json!({ "name": name, "paths": paths, "layer": layer, "base": base, "parent": parent }),
    )
    .await
    {
        Ok(added) => added,
        Err((status, Json(refusal))) => {
            // The disk alone refused it: the driver retries the start when the disk frees.
            if status == StatusCode::CONFLICT
                && refusal["disk_only"] == json!(true)
                && layer != "docs"
            {
                let pending = PendingStart {
                    caller: &caller,
                    goal: &goal,
                    prompt: &prompt,
                    paths: &paths,
                    layer: &layer,
                    base: base.as_deref(),
                    waits_for_base: None,
                    kind: requested,
                    estimate,
                    parent: parent.as_deref(),
                };
                remember_pending_start(&state, &id, &name, &pending).await;
            }
            return Err((status, Json(refusal)));
        }
    };
    let mut replayed = added["replayed"].as_bool().unwrap_or(false);
    // The shape of the start goes on the branch before its node exists, so that a crash between
    // the two leaves it and the replay does not write it twice.
    start_log::record_start(&state, &id, &shape).await;

    // 2. The worktree. A branch past `queued` was opened by an earlier call.
    let job = store::load(&state.pool, &id)
        .await
        .map_err(store_refusal)?
        .ok_or_else(|| refuse(StatusCode::NOT_FOUND, format!("tree job {id} not found")))?;
    let branch = job
        .manifest
        .branches
        .get(&name)
        .cloned()
        .ok_or_else(|| refuse(StatusCode::NOT_FOUND, format!("branch {name} not found")))?;
    // A base that has not merged keeps the branch from opening, and the root that asked cannot wait
    // for it. The start is remembered whole and the driver replays it once the base merges; an
    // error here would leave a queued branch nobody ever opens, blocking the landing for ever.
    if branch.state == "queued"
        && let Some(base) = unmerged_base(&job.manifest, &branch)
    {
        let pending = PendingStart {
            caller: &caller,
            goal: &goal,
            prompt: &prompt,
            paths: &paths,
            layer: &layer,
            base: Some(base),
            waits_for_base: Some(base),
            kind: requested,
            estimate,
            parent: parent.as_deref(),
        };
        remember_pending_start(&state, &id, &name, &pending).await;
        return Ok(Json(with_shape(
            json!({
                "branch": name,
                "queued": true,
                "waiting_for": base,
                "replayed": false,
            }),
            &shape_json,
        )));
    }
    let worktree = if branch.state == "queued" {
        let opened = call(
            open::routes(),
            &state,
            format!("/trees/{id}/branches/{name}/open"),
            json!({}),
        )
        .await?;
        replayed &= opened["replayed"].as_bool().unwrap_or(false);
        opened["worktree"].as_str().map(str::to_owned)
    } else {
        branch.worktree.clone()
    };
    // A docs branch has no worktree of its own: its agent works in the project root.
    let worktree = worktree.unwrap_or_else(|| job.manifest.project_root.clone());

    // 3. The node: the one the branch is bound to, else the one an earlier call made, else a new one.
    let node_id = match &branch.node_id {
        Some(bound) => bound.clone(),
        None => {
            let (node_id, found) = find_or_open_node(
                &state,
                &id,
                &job.root_node_id,
                &job.manifest.project_root,
                &caller,
                &name,
                &goal,
                &paths,
                kind,
            )
            .await?;
            replayed &= found;
            node_id
        }
    };

    // 4. The binding. It takes the job lock itself.
    let bound = open::bind_branch(&state.pool, &id, &name, &node_id)
        .await
        .map_err(open_refusal)?;
    replayed &= bound.replayed;

    // 5. The agent, once per branch name.
    let key = format!("branch-{name}");
    replayed &= launch_recorded(&state, &node_id, &key).await?;
    // An orchestrator's agent is told what it is on top of its task; the daemon enforces the same
    // rules whatever it does with them.
    let prompt = if kind == Kind::Orchestrator {
        let preamble = nested_prompt::orchestrator_preamble(
            child_depth,
            limits.max_depth,
            limits.leaf_budget,
            nested_prompt::DEFAULT_ORCHESTRATOR_BUDGET,
            &node_id,
            &id,
        );
        format!("{preamble}\n\n{prompt}")
    } else {
        prompt
    };
    let run_id = launch_recovering(
        &state,
        &node_id,
        &key,
        prompt,
        job.project_id.clone(),
        Some(worktree.clone()),
        LAUNCH_MODE,
    )
    .await
    .map_err(run_refusal)?;

    Ok(Json(with_shape(
        json!({
            "branch": name,
            "node_id": node_id,
            "run_id": run_id,
            "worktree": worktree,
            "replayed": replayed,
        }),
        &shape_json,
    )))
}

/// `response` with the `kind`, `depth`, `parent` and `degraded` of the start added.
fn with_shape(mut response: Value, shape: &Value) -> Value {
    if let (Some(response), Some(shape)) = (response.as_object_mut(), shape.as_object()) {
        for (key, value) in shape {
            response.insert(key.clone(), value.clone());
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};

    const TREE: &str = "branch-start-tree";

    async fn state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    fn app(state: &AppState) -> Router {
        super::super::http::routes().with_state(state.clone())
    }

    async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("Authorization", "Bearer test-token")
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    /// A temp git repository with one commit and a zero disk floor. A code branch is still charged
    /// its seed cost; what admits it whatever the disk holds is the test build's fixed free disk
    /// (`open::free_disk_gb`). The layout `http.rs`'s `Repo` has.
    struct Repo {
        _container: tempfile::TempDir,
        path: std::path::PathBuf,
    }

    impl Repo {
        fn new() -> Repo {
            let container = space_free_tempdir("branch-start-");
            let path = container.path().join("repo");
            initialize_repo(&path);
            let dir = path.join(".nucleos/workflow");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("speed.yaml"), "tree:\n  floor_gb: 0\n").unwrap();
            // A docs branch runs in this root with no worktree, and its write guard is the
            // classifier hook, so a launch needs the root wired (the rule under test below).
            crate::governance::autopilot::wire_classifier_hook(&path).unwrap();
            Repo {
                _container: container,
                path,
            }
        }
    }

    /// A tree job over a real repository, and the id of its root node, the caller of the tool.
    async fn tree() -> (AppState, Router, String, Repo) {
        let state = state().await;
        let app = app(&state);
        let repo = Repo::new();
        let (_, created) = post(
            &app,
            "/trees",
            json!({
                "id": TREE,
                "goal": "Ship it",
                "feature_branch": "feat/branch-start",
                "project_root": repo.path.to_string_lossy().replace('\\', "/"),
                "project_id": "project-0",
            }),
        )
        .await;
        let caller = created["root_node_id"]
            .as_str()
            .expect("the tree route creates its root node")
            .to_owned();
        (state, app, caller, repo)
    }

    async fn count(state: &AppState, table: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    fn body(caller: &str) -> Value {
        json!({
            "name": "notes",
            "goal": "Write the branch notes",
            "owned_paths": [".ai/notes/**"],
            "prompt": "Write the notes.",
            "layer": "docs",
            "node_id": caller,
        })
    }

    fn uri() -> String {
        format!("/trees/{TREE}/branch_start")
    }

    #[tokio::test]
    async fn branch_start_adds_opens_binds_and_launches_in_one_call() {
        let (state, app, caller, _repo) = tree().await;
        let nodes = count(&state, "work_nodes").await;
        let runs = count(&state, "runs").await;

        let (status, response) = post(&app, &uri(), body(&caller)).await;

        assert_eq!(status, StatusCode::OK, "{response:?}");
        assert_eq!(response["branch"], "notes");
        assert!(response["node_id"].is_string(), "{response:?}");
        assert!(response["run_id"].is_i64(), "{response:?}");
        assert!(response["worktree"].is_string(), "{response:?}");
        assert_eq!(response["replayed"], false);
        assert_eq!(count(&state, "work_nodes").await, nodes + 1);
        assert_eq!(count(&state, "runs").await, runs + 1);
        let job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap();
        let branch = &job.manifest.branches["notes"];
        assert_eq!(branch.node_id.as_deref(), response["node_id"].as_str());
    }

    /// A docs branch works in the main checkout with no worktree, and its write guard lives in the
    /// PreToolUse classifier hook. Nothing else guarantees that hook is there, so a root without it
    /// takes no docs branch: the guard might never fire.
    #[tokio::test]
    async fn a_docs_branch_is_refused_when_the_classifier_hook_is_not_wired() {
        let (state, app, caller, repo) = tree().await;
        // Un-wire the root: a settings file that registers nothing.
        std::fs::write(repo.path.join(".claude/settings.json"), "{}").unwrap();
        assert!(!crate::governance::autopilot::classifier_hook_is_wired(
            &repo.path
        ));
        let nodes = count(&state, "work_nodes").await;
        let runs = count(&state, "runs").await;

        let (status, response) = post(&app, &uri(), body(&caller)).await;

        assert!(!status.is_success(), "{status:?} {response:?}");
        assert!(
            response.to_string().contains("classifier hook"),
            "the refusal names the missing hook: {response:?}"
        );
        assert_eq!(count(&state, "runs").await, runs, "no run is launched");
        assert_eq!(
            count(&state, "work_nodes").await,
            nodes,
            "no node is opened"
        );
        let job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap();
        assert!(
            job.manifest
                .branches
                .get("notes")
                .is_none_or(|branch| branch.state != "building"),
            "the branch is not running"
        );
    }

    fn dependent_body(caller: &str) -> Value {
        json!({
            "name": "later",
            "goal": "Write the later notes",
            "owned_paths": [".ai/plans/**"],
            "prompt": "Write the later notes.",
            "layer": "docs",
            "base": "notes",
            "node_id": caller,
        })
    }

    /// A branch whose base has not merged cannot open yet, and the root that asked for it reports
    /// and stops (it cannot wait). The start is remembered so that the driver replays it once the
    /// base merges; without that the branch stays `queued` for ever and blocks the landing.
    #[tokio::test]
    async fn a_start_whose_base_is_not_merged_is_remembered_not_lost() {
        let (state, app, caller, _repo) = tree().await;
        let (status, first) = post(&app, &uri(), body(&caller)).await;
        assert_eq!(status, StatusCode::OK, "{first:?}");
        let nodes = count(&state, "work_nodes").await;
        let runs = count(&state, "runs").await;

        let (status, waiting) = post(&app, &uri(), dependent_body(&caller)).await;

        assert_eq!(status, StatusCode::OK, "{waiting:?}");
        assert_eq!(waiting["waiting_for"], "notes", "{waiting:?}");
        assert_eq!(count(&state, "work_nodes").await, nodes, "no node yet");
        assert_eq!(count(&state, "runs").await, runs, "no run yet");
        let job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.manifest.branches["later"].state, "queued");
        let record = &job.manifest.extra["pending_starts"]["later"];
        assert_eq!(record["waits_for_base"], "notes", "{record:?}");
        assert_eq!(record["prompt"], "Write the later notes.");
        assert!(
            record["decision_id"].is_null(),
            "no owner question: {record:?}"
        );
    }

    /// The replay the driver makes once the base merged starts the branch from the stored record.
    #[tokio::test]
    async fn a_remembered_start_runs_once_its_base_has_merged() {
        let (state, app, caller, _repo) = tree().await;
        post(&app, &uri(), body(&caller)).await;
        post(&app, &uri(), dependent_body(&caller)).await;
        let mut job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap();
        job.manifest.branches.get_mut("notes").unwrap().state = "merged".to_owned();
        crate::tree_job::store::update(&state.pool, &job)
            .await
            .unwrap();
        let record = job.manifest.extra["pending_starts"]["later"].clone();
        let runs = count(&state, "runs").await;

        start_pending(&state, TREE, "later", &record).await.unwrap();

        assert_eq!(count(&state, "runs").await, runs + 1);
        let job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap();
        assert!(job.manifest.branches["later"].node_id.is_some());
    }

    /// The root closes as `done` once its dispatch report arrives, before the base merges. The replay
    /// (and a fresh owner start) must still open the child under it; a branch left half-started by an
    /// earlier failed replay (no node) is finished, not refused.
    #[tokio::test]
    async fn a_remembered_start_runs_under_a_root_that_is_already_done() {
        let (state, app, caller, _repo) = tree().await;
        post(&app, &uri(), body(&caller)).await;
        post(&app, &uri(), dependent_body(&caller)).await;
        let mut job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap();
        job.manifest.branches.get_mut("notes").unwrap().state = "merged".to_owned();
        crate::tree_job::store::update(&state.pool, &job)
            .await
            .unwrap();
        let record = job.manifest.extra["pending_starts"]["later"].clone();
        sqlx::query("UPDATE work_nodes SET status = 'done' WHERE id = ?")
            .bind(&caller)
            .execute(&state.pool)
            .await
            .unwrap();
        let runs = count(&state, "runs").await;

        start_pending(&state, TREE, "later", &record).await.unwrap();
        // A second replay is idempotent.
        start_pending(&state, TREE, "later", &record).await.unwrap();

        assert_eq!(count(&state, "runs").await, runs + 1);
        let job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap();
        assert!(job.manifest.branches["later"].node_id.is_some());
        assert_eq!(job.manifest.branches["later"].state, "building");
    }

    /// Only `done` is waived: a root that failed still takes no child.
    #[tokio::test]
    async fn a_failed_root_still_takes_no_new_branch() {
        let (state, app, caller, _repo) = tree().await;
        sqlx::query("UPDATE work_nodes SET status = 'failed' WHERE id = ?")
            .bind(&caller)
            .execute(&state.pool)
            .await
            .unwrap();

        let (status, response) = post(&app, &uri(), body(&caller)).await;

        assert_eq!(status, StatusCode::CONFLICT, "{response:?}");
    }

    #[tokio::test]
    async fn branch_start_called_twice_makes_one_branch_one_node_one_run() {
        let (state, app, caller, _repo) = tree().await;
        let nodes = count(&state, "work_nodes").await;
        let runs = count(&state, "runs").await;
        let first = post(&app, &uri(), body(&caller)).await;
        let second = post(&app, &uri(), body(&caller)).await;
        assert_eq!(count(&state, "work_nodes").await, nodes + 1);
        assert_eq!(count(&state, "runs").await, runs + 1);

        assert_eq!(first.0, StatusCode::OK, "{:?}", first.1);
        assert_eq!(second.0, StatusCode::OK, "{:?}", second.1);
        assert_eq!(second.1["branch"], first.1["branch"]);
        assert_eq!(second.1["node_id"], first.1["node_id"]);
        assert_eq!(second.1["run_id"], first.1["run_id"]);
        assert_eq!(second.1["replayed"], true);
    }

    #[tokio::test]
    async fn branch_start_after_a_crash_between_steps_completes_the_missing_ones() {
        let (state, app, caller, _repo) = tree().await;
        // The first step alone: a branch with no worktree, no node and no run, as a crash leaves it.
        let nodes = count(&state, "work_nodes").await;
        let runs = count(&state, "runs").await;
        let added = post(
            &app,
            &format!("/trees/{TREE}/branches"),
            json!({ "name": "notes", "paths": [".ai/notes/**"], "layer": "docs" }),
        )
        .await;
        assert_eq!(added.0, StatusCode::OK, "{:?}", added.1);

        let resumed = post(&app, &uri(), body(&caller)).await;
        assert_eq!(resumed.0, StatusCode::OK, "{:?}", resumed.1);
        assert_eq!(resumed.1["branch"], "notes");
        assert!(resumed.1["node_id"].is_string(), "{:?}", resumed.1);
        assert!(resumed.1["run_id"].is_i64(), "{:?}", resumed.1);
        assert_eq!(resumed.1["replayed"], false);
        assert_eq!(count(&state, "work_nodes").await, nodes + 1);
        assert_eq!(count(&state, "runs").await, runs + 1);
    }

    #[tokio::test]
    async fn branch_start_refuses_a_finished_job_and_an_unknown_caller_node() {
        let (state, app, caller, _repo) = tree().await;
        let unknown = post(&app, &uri(), body("unknown-caller")).await;
        assert_eq!(unknown.0, StatusCode::NOT_FOUND, "{:?}", unknown.1);

        sqlx::query("UPDATE tree_jobs SET state = 'finished' WHERE id = ?")
            .bind(TREE)
            .execute(&state.pool)
            .await
            .unwrap();

        let finished = post(&app, &uri(), body(&caller)).await;
        assert_eq!(finished.0, StatusCode::CONFLICT, "{:?}", finished.1);
    }

    #[tokio::test]
    async fn branch_start_surfaces_an_admission_refusal_without_leaving_a_node() {
        let (state, app, caller, _repo) = tree().await;
        let mut job = crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap();
        job.manifest.costs_gb.insert("core".to_owned(), json!(1e20));
        crate::tree_job::store::update(&state.pool, &job)
            .await
            .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        let refused = post(
            &app,
            &uri(),
            json!({
                "name": "too-large",
                "goal": "Refused before a node is opened",
                "owned_paths": ["core/**"],
                "prompt": "Do not run.",
                "layer": "core",
                "node_id": caller,
            }),
        )
        .await;

        assert_eq!(refused.0, StatusCode::CONFLICT, "{:?}", refused.1);
        let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(after, before);
    }

    /// Sets the tree's budget row, keyed on its root node id, as `PUT /trees/{id}/budget` does.
    async fn set_tree_budget(state: &AppState, root_id: &str, limit: f64, soft_pct: f64) {
        sqlx::query(
            "INSERT INTO tree_budgets (tree_id, limit_usd, soft_pct, created_at, updated_at) \
             VALUES (?, ?, ?, '2026-10-30T10:00:00+00:00', '2026-10-30T10:00:00+00:00') \
             ON CONFLICT(tree_id) DO UPDATE SET limit_usd = excluded.limit_usd, \
             soft_pct = excluded.soft_pct",
        )
        .bind(root_id)
        .bind(limit)
        .bind(soft_pct)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    /// Records `cost` USD of measured spending against the tree, as the ledger counts it: a
    /// finished node of the tree with a completed run.
    async fn record_tree_spend(state: &AppState, root_id: &str, node_id: &str, cost: f64) {
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, cost_usd, created_at, completed_at) \
             VALUES ('p', 'completed', ?, '2026-10-30T10:00:00+00:00', '2026-10-30T10:00:00+00:00')",
        )
        .bind(cost)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, created_at, updated_at) \
             VALUES (?, ?, 0, 'leaf', 't', 'g', 'done', ?, '2026-10-30T10:00:00+00:00', '2026-10-30T10:00:00+00:00')",
        )
        .bind(node_id)
        .bind(root_id)
        .bind(run_id.to_string())
        .execute(&state.pool)
        .await
        .unwrap();
    }

    /// A reserve of zero, so that only the spend against the limit decides the permit.
    async fn zero_reserve(state: &AppState) {
        sqlx::query("UPDATE autopilot_global SET budget_per_run_reserve_usd = 0")
            .execute(&state.pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn branch_start_is_refused_when_the_tree_budget_is_spent() {
        let (state, app, caller, _repo) = tree().await;
        zero_reserve(&state).await;
        // A limit already reached: nothing spent yet is still `spent >= limit`.
        set_tree_budget(&state, &caller, 0.0, 80.0).await;
        let nodes = count(&state, "work_nodes").await;

        let refused = post(&app, &uri(), body(&caller)).await;

        assert_eq!(refused.0, StatusCode::CONFLICT, "{:?}", refused.1);
        assert_eq!(count(&state, "work_nodes").await, nodes);
    }

    #[tokio::test]
    async fn branch_start_replay_of_an_existing_node_survives_a_spent_budget() {
        let (state, app, caller, _repo) = tree().await;
        zero_reserve(&state).await;
        let first = post(&app, &uri(), body(&caller)).await;
        assert_eq!(first.0, StatusCode::OK, "{:?}", first.1);
        let nodes = count(&state, "work_nodes").await;

        set_tree_budget(&state, &caller, 0.0, 80.0).await;
        let second = post(&app, &uri(), body(&caller)).await;

        assert_eq!(second.0, StatusCode::OK, "{:?}", second.1);
        assert_eq!(second.1["node_id"], first.1["node_id"]);
        assert_eq!(second.1["replayed"], true);
        assert_eq!(count(&state, "work_nodes").await, nodes);
    }

    #[tokio::test]
    async fn branch_start_writes_the_soft_budget_line_once() {
        let (state, app, caller, _repo) = tree().await;
        zero_reserve(&state).await;
        // The soft threshold is 1 USD of a 1000 USD limit; 5 USD is over it and far under the limit.
        set_tree_budget(&state, &caller, 1000.0, 0.1).await;
        record_tree_spend(&state, &caller, "spent-node", 5.0).await;
        let lines = |state: AppState| async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM feed WHERE kind = 'tree_budget_soft' AND subject = ?",
            )
            .bind(format!("tree:{}", caller_of(&state).await))
            .fetch_one(&state.pool)
            .await
            .unwrap()
        };
        assert_eq!(lines(state.clone()).await, 0);

        let first = post(&app, &uri(), body(&caller)).await;
        assert_eq!(first.0, StatusCode::OK, "{:?}", first.1);
        assert_eq!(lines(state.clone()).await, 1);

        // A replay opens no node, and writes no second line.
        let second = post(&app, &uri(), body(&caller)).await;
        assert_eq!(second.0, StatusCode::OK, "{:?}", second.1);
        assert_eq!(lines(state.clone()).await, 1);
    }

    /// The root node id of the test tree.
    async fn caller_of(state: &AppState) -> String {
        sqlx::query_scalar("SELECT root_node_id FROM tree_jobs WHERE id = ?")
            .bind(TREE)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    /// The permission and directory of the launch the runner saw last, once there is one.
    async fn seen_launch(
        runner: &crate::execution::runner::FakeCommandRunner,
    ) -> Option<(crate::execution::runner::Permission, std::path::PathBuf)> {
        // Hang guard only: wait for the launch, not for a short budget.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            let permission = *runner.last_permission.lock().unwrap();
            let cwd = runner.last_cwd.lock().unwrap().clone();
            if let (Some(permission), Some(cwd)) = (permission, cwd) {
                return Some((permission, cwd));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }

    /// A branch agent has to write code, run its gate and call `node_close`: launched in plan mode
    /// (`--permission-mode plan`) it plans, ends, and the wrap parks the branch. It also has to work
    /// in the worktree `open` made for its branch, not in one of its own.
    #[tokio::test]
    async fn a_tree_branch_run_is_not_launched_in_plan_mode() {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        let app = app(&state);
        let repo = Repo::new();
        let (_, created) = post(
            &app,
            "/trees",
            json!({
                "id": TREE,
                "goal": "Ship it",
                "feature_branch": "feat/branch-start",
                "project_root": repo.path.to_string_lossy().replace('\\', "/"),
                "project_id": "project-0",
            }),
        )
        .await;
        let caller = created["root_node_id"]
            .as_str()
            .expect("the tree route creates its root node")
            .to_owned();
        // The root run launched first; forget it, so that what is read next is the branch's.
        seen_launch(&runner)
            .await
            .expect("the root run reached the runner");
        *runner.last_permission.lock().unwrap() = None;
        *runner.last_cwd.lock().unwrap() = None;

        let (status, response) = post(
            &app,
            &uri(),
            json!({
                "name": "api",
                "goal": "Build the api",
                "owned_paths": ["core/**"],
                "prompt": "Build it.",
                "layer": "core",
                "node_id": caller,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{response:?}");
        let (permission, cwd) = seen_launch(&runner)
            .await
            .expect("the branch run reached the runner");
        assert_ne!(
            permission,
            crate::execution::runner::Permission::Plan,
            "a tree agent launched in plan mode cannot call its tools or write"
        );
        assert_eq!(
            Some(cwd),
            response["worktree"].as_str().map(std::path::PathBuf::from),
            "the branch run starts in the worktree its branch was opened with"
        );
        let mode: String = sqlx::query_scalar("SELECT mode FROM runs WHERE id = ?")
            .bind(
                response["run_id"]
                    .as_i64()
                    .expect("the answer names the run"),
            )
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(mode, "worktree");
    }

    // ----- kind, estimate and nesting -----

    fn code_body(caller: &str, name: &str, goal: &str, paths: &[&str]) -> Value {
        json!({
            "name": name,
            "goal": goal,
            "owned_paths": paths,
            "prompt": format!("Build {name}."),
            "layer": "core",
            "node_id": caller,
        })
    }

    fn with(mut body: Value, key: &str, value: Value) -> Value {
        body[key] = value;
        body
    }

    async fn post_as(app: &Router, body: Value, scope: Scope) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(uri())
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        request.extensions_mut().insert(scope);
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn job_of(state: &AppState) -> crate::tree_job::store::TreeJob {
        crate::tree_job::store::load(&state.pool, TREE)
            .await
            .unwrap()
            .unwrap()
    }

    async fn kind_of(state: &AppState, node: &str) -> String {
        sqlx::query_scalar("SELECT kind FROM work_nodes WHERE id = ?")
            .bind(node)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn descents(state: &AppState) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'work_tree_descent'")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    /// A stem orchestrator `api` owning `core/**`; returns the id of its node.
    async fn stem_orchestrator(app: &Router, caller: &str) -> String {
        let body = with(
            code_body(caller, "api", "Build the api", &["core/**"]),
            "kind",
            json!("orchestrator"),
        );
        let (status, response) =
            post(app, &uri(), with(body, "estimate_tokens", json!(400_000))).await;
        assert_eq!(status, StatusCode::OK, "{response:?}");
        response["node_id"].as_str().unwrap().to_owned()
    }

    fn nested_body(api_node: &str) -> Value {
        with(
            with(
                code_body(api_node, "api-x", "Build the x half", &["core/x/**"]),
                "kind",
                json!("orchestrator"),
            ),
            "estimate_tokens",
            json!(180_000),
        )
    }

    #[tokio::test]
    async fn a_start_without_kind_opens_a_leaf_exactly_as_before() {
        let (state, app, caller, _repo) = tree().await;
        let body = code_body(&caller, "api", "Build the api", &["core/**"]);

        let (status, response) = post(&app, &uri(), body).await;

        assert_eq!(status, StatusCode::OK, "{response:?}");
        assert_eq!(response["kind"], "leaf");
        assert_eq!(response["parent"], Value::Null);
        assert_eq!(response["degraded"], Value::Null);
        assert_eq!(response["depth"], 1);
        assert_eq!(
            kind_of(&state, response["node_id"].as_str().unwrap()).await,
            "leaf"
        );
        let job = job_of(&state).await;
        let branch = &job.manifest.branches["api"];
        assert_eq!(branch.parent, None);
        assert!(!branch.extra.contains_key("descent"));
        assert!(!branch.extra.contains_key("degraded"));
        assert_eq!(
            branch.extra["kind_reason"],
            "kind omitted: opened as a leaf"
        );
        assert_eq!(descents(&state).await, 0);
    }

    #[tokio::test]
    async fn an_orchestrator_start_opens_an_orchestrator_node_with_a_parentless_branch_at_the_stem()
    {
        let (state, app, caller, _repo) = tree().await;

        let api_node = stem_orchestrator(&app, &caller).await;

        assert_eq!(kind_of(&state, &api_node).await, "orchestrator");
        let job = job_of(&state).await;
        let branch = &job.manifest.branches["api"];
        assert_eq!(branch.parent, None, "the stem is not nested");
        assert_eq!(branch.extra["estimate_tokens"], 400_000);
        assert_eq!(branch.extra["descent"]["kind"], "orchestrator");
        assert_eq!(descents(&state).await, 1);
    }

    #[tokio::test]
    async fn a_nested_start_records_parent_depth_and_the_descent_reason() {
        let (state, app, caller, _repo) = tree().await;
        let api_node = stem_orchestrator(&app, &caller).await;

        let (status, response) = post(&app, &uri(), nested_body(&api_node)).await;

        assert_eq!(status, StatusCode::OK, "{response:?}");
        assert_eq!(response["kind"], "orchestrator");
        assert_eq!(response["parent"], "api");
        assert_eq!(response["depth"], 2);
        let job = job_of(&state).await;
        let child = &job.manifest.branches["api-x"];
        assert_eq!(child.parent.as_deref(), Some("api"));
        assert_eq!(
            child.extra["descent"]["reason"],
            "piece api-x estimated 180000 tokens > leaf_budget 120000"
        );
        assert_eq!(child.extra["descent"]["depth"], 2);
        assert_eq!(
            kind_of(&state, response["node_id"].as_str().unwrap()).await,
            "orchestrator"
        );
    }

    #[tokio::test]
    async fn a_nested_start_replayed_adds_one_node_and_one_descent() {
        let (state, app, caller, _repo) = tree().await;
        let api_node = stem_orchestrator(&app, &caller).await;
        let first = post(&app, &uri(), nested_body(&api_node)).await;
        assert_eq!(first.0, StatusCode::OK, "{:?}", first.1);
        let nodes = count(&state, "work_nodes").await;
        let runs = count(&state, "runs").await;
        let lines = descents(&state).await;

        let second = post(&app, &uri(), nested_body(&api_node)).await;

        assert_eq!(second.0, StatusCode::OK, "{:?}", second.1);
        assert_eq!(second.1["node_id"], first.1["node_id"]);
        assert_eq!(second.1["replayed"], true);
        assert_eq!(second.1["kind"], "orchestrator");
        assert_eq!(count(&state, "work_nodes").await, nodes);
        assert_eq!(count(&state, "runs").await, runs);
        assert_eq!(descents(&state).await, lines, "no second descent line");
        let job = job_of(&state).await;
        assert_eq!(
            crate::tree_job::nesting::children(&job.manifest, "api").len(),
            1
        );
    }

    #[tokio::test]
    async fn a_disk_queued_nested_start_keeps_kind_estimate_and_parent_when_the_driver_retries_it()
    {
        let (state, app, caller, _repo) = tree().await;
        let api_node = stem_orchestrator(&app, &caller).await;
        let pending = PendingStart {
            caller: &api_node,
            goal: "Build the x half",
            prompt: "Build api-x.",
            paths: &["core/x/**".to_owned()],
            layer: "core",
            base: None,
            waits_for_base: Some("api"),
            kind: Kind::Orchestrator,
            estimate: Some(180_000),
            parent: Some("api"),
        };
        remember_pending_start(&state, TREE, "api-x", &pending).await;
        let record = job_of(&state).await.manifest.extra["pending_starts"]["api-x"].clone();
        assert_eq!(record["kind"], "orchestrator");
        assert_eq!(record["estimate_tokens"], 180_000);
        assert_eq!(record["parent"], "api");

        let started = start_pending(&state, TREE, "api-x", &record).await.unwrap();

        assert_eq!(started["kind"], "orchestrator");
        assert_eq!(started["parent"], "api");
        let job = job_of(&state).await;
        let child = &job.manifest.branches["api-x"];
        assert_eq!(child.parent.as_deref(), Some("api"));
        assert_eq!(child.extra["estimate_tokens"], 180_000);
        assert_eq!(
            kind_of(&state, started["node_id"].as_str().unwrap()).await,
            "orchestrator"
        );
    }

    #[tokio::test]
    async fn a_nested_leaf_with_a_repeated_ancestor_goal_is_409_but_an_orchestrator_degrades_to_a_leaf()
     {
        let (state, app, caller, _repo) = tree().await;
        let api_node = stem_orchestrator(&app, &caller).await;
        let repeated = |name: &str, kind: &str| {
            with(
                with(
                    code_body(&api_node, name, "Build the api", &["core/y/**"]),
                    "kind",
                    json!(kind),
                ),
                "estimate_tokens",
                json!(500_000),
            )
        };

        let (status, refused) = post(&app, &uri(), repeated("api-leaf", "leaf")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused:?}");
        assert!(refused.to_string().contains("repeated goal"), "{refused:?}");

        let (status, degraded) = post(&app, &uri(), repeated("api-orch", "orchestrator")).await;
        assert_eq!(status, StatusCode::OK, "{degraded:?}");
        assert_eq!(degraded["kind"], "leaf");
        assert!(
            degraded["degraded"]
                .as_str()
                .unwrap()
                .contains("repeated goal")
        );
        assert_eq!(
            kind_of(&state, degraded["node_id"].as_str().unwrap()).await,
            "leaf"
        );
        let job = job_of(&state).await;
        let branch = &job.manifest.branches["api-orch"];
        assert!(
            branch.extra["degraded"]
                .as_str()
                .unwrap()
                .contains("repeated goal")
        );
        assert_eq!(branch.extra["descent"]["kind"], "leaf");
    }

    /// Inserts a running node of the tree, to stand where a deep tree would put one.
    async fn deep_node(state: &AppState, root: &str, id: &str, depth: i64) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, parent_id, depth, kind, title, goal, status, \
             owned_paths, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'orchestrator', ?, ?, 'running', '[\"core/**\"]', \
             '2026-11-08T10:00:00+00:00', '2026-11-08T10:00:00+00:00')",
        )
        .bind(id)
        .bind(root)
        .bind(root)
        .bind(depth)
        .bind(id)
        .bind(format!("goal of {id}"))
        .execute(&state.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_child_at_depth_seven_is_409() {
        let (state, app, caller, repo) = tree().await;
        std::fs::write(
            repo.path.join(".nucleos/workflow/speed.yaml"),
            "tree:\n  floor_gb: 0\nnodes:\n  max_depth: 6\n",
        )
        .unwrap();
        deep_node(&state, &caller, "deep-6", 6).await;
        deep_node(&state, &caller, "deep-5", 5).await;
        let nodes = count(&state, "work_nodes").await;

        let (status, refused) = post(
            &app,
            &uri(),
            code_body("deep-6", "too-deep", "Too deep", &["core/z/**"]),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused:?}");
        assert!(refused.to_string().contains("depth 7"), "{refused:?}");
        assert_eq!(count(&state, "work_nodes").await, nodes);

        // At depth 6 an orchestrator is asked for and a leaf is made.
        let body = with(
            with(
                code_body("deep-5", "last-level", "Last level", &["core/y/**"]),
                "kind",
                json!("orchestrator"),
            ),
            "estimate_tokens",
            json!(900_000),
        );
        let (status, last) = post(&app, &uri(), body).await;
        assert_eq!(status, StatusCode::OK, "{last:?}");
        assert_eq!(last["kind"], "leaf");
        assert_eq!(last["depth"], 6);
        assert!(last["degraded"].as_str().unwrap().contains("depth 6"));
    }

    #[tokio::test]
    async fn estimate_zero_negative_and_unknown_kind_are_422() {
        let (state, app, caller, _repo) = tree().await;
        let nodes = count(&state, "work_nodes").await;
        let base = code_body(&caller, "api", "Build the api", &["core/**"]);
        for (key, value) in [
            ("estimate_tokens", json!(0)),
            ("estimate_tokens", json!(-5)),
            ("kind", json!("branch")),
        ] {
            let (status, refused) = post(&app, &uri(), with(base.clone(), key, value)).await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{key}: {refused:?}"
            );
        }
        assert_eq!(count(&state, "work_nodes").await, nodes);
    }

    #[tokio::test]
    async fn a_run_bound_caller_cannot_start_branches_for_another_node() {
        let (state, app, caller, repo) = tree().await;
        // The two successful starts at the end would be the fifth live branch under the default
        // limit of four; this test is about who may start, not about that limit.
        std::fs::write(
            repo.path.join(".nucleos/workflow/speed.yaml"),
            "tree:\n  floor_gb: 0\n  max_live_branches: 12\n",
        )
        .unwrap();
        let api_node = stem_orchestrator(&app, &caller).await;
        let (status, web) = post(
            &app,
            &uri(),
            code_body(&caller, "web", "Build the web", &["web/**"]),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{web:?}");
        let web_node = web["node_id"].as_str().unwrap().to_owned();
        sqlx::query("UPDATE work_nodes SET run_id = '4242' WHERE id = ?")
            .bind(&api_node)
            .execute(&state.pool)
            .await
            .unwrap();
        let nodes = count(&state, "work_nodes").await;

        // The root and a sibling are not the node the run is bound to.
        for other in [caller.as_str(), web_node.as_str()] {
            let body = code_body(other, "stolen", "Not mine", &["core/s/**"]);
            let (status, refused) = post_as(&app, body, Scope::Run(4242)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{other}: {refused:?}");
            assert!(
                refused.to_string().contains("only its own children"),
                "{refused:?}"
            );
        }
        assert_eq!(count(&state, "work_nodes").await, nodes);

        // Its own node is fine, and so is the control token for any node of the tree.
        let own = code_body(&api_node, "api-own", "Build the own half", &["core/o/**"]);
        let (status, ok) = post_as(&app, own, Scope::Run(4242)).await;
        assert_eq!(status, StatusCode::OK, "{ok:?}");
        let (status, ok) = post_as(
            &app,
            code_body(&web_node, "web-child", "Build the web child", &["web/c/**"]),
            Scope::Control,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ok:?}");
    }

    #[tokio::test]
    async fn a_leaf_run_cannot_start_branches_and_the_refusal_is_recorded() {
        let (state, app, caller, _repo) = tree().await;
        let (status, web) = post(
            &app,
            &uri(),
            code_body(&caller, "web", "Build the web", &["web/**"]),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{web:?}");
        let leaf = web["node_id"].as_str().unwrap().to_owned();
        assert_eq!(kind_of(&state, &leaf).await, work_nodes::KIND_LEAF);
        sqlx::query("UPDATE work_nodes SET run_id = '4343' WHERE id = ?")
            .bind(&leaf)
            .execute(&state.pool)
            .await
            .unwrap();
        let nodes = count(&state, "work_nodes").await;

        let body = code_body(&leaf, "web-child", "Too big for a leaf", &["web/c/**"]);
        let (status, refused) = post_as(&app, body, Scope::Run(4343)).await;

        assert_eq!(status, StatusCode::FORBIDDEN, "{refused:?}");
        assert!(
            refused
                .to_string()
                .contains("a leaf does not open branches"),
            "{refused:?}"
        );
        assert_eq!(count(&state, "work_nodes").await, nodes);
        let job = job_of(&state).await;
        let last = job.manifest.extra[start_log::REFUSALS_KEY]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(last["branch"], "web-child");
        assert_eq!(last["status"], 403);
        assert!(
            last["reason"].as_str().unwrap().contains("leaf budget"),
            "{last:?}"
        );

        // The control token still opens under any node of the tree, a leaf included.
        let body = code_body(&leaf, "web-ok", "Opened by the owner", &["web/o/**"]);
        let (status, ok) = post_as(&app, body, Scope::Control).await;
        assert_eq!(status, StatusCode::OK, "{ok:?}");
    }

    #[tokio::test]
    async fn a_refused_start_leaves_a_recorded_refusal_naming_the_reason() {
        let (state, app, caller, _repo) = tree().await;
        let (status, _) = post(
            &app,
            &uri(),
            code_body(&caller, "a", "Build a", &["core/**"]),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, refused) = post(
            &app,
            &uri(),
            code_body(&caller, "b", "Build b", &["core/**"]),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused:?}");

        let job = job_of(&state).await;
        let list = job.manifest.extra[start_log::REFUSALS_KEY]
            .as_array()
            .unwrap();
        let last = list.last().unwrap();
        assert_eq!(last["job"], TREE);
        assert_eq!(last["branch"], "b");
        assert_eq!(last["status"], 409);
        assert!(
            last["reason"].as_str().unwrap().contains("overlap"),
            "{last:?}"
        );
    }

    #[tokio::test]
    async fn the_estimate_survives_a_reload_of_the_manifest() {
        let (state, app, caller, _repo) = tree().await;
        let body = with(
            code_body(&caller, "api", "Build the api", &["core/**"]),
            "estimate_tokens",
            json!(50_000),
        );
        let (status, response) = post(&app, &uri(), body).await;
        assert_eq!(status, StatusCode::OK, "{response:?}");

        let job = job_of(&state).await;
        let text = serde_json::to_string(&job.manifest).unwrap();
        let reloaded: crate::tree_job::model::Manifest = serde_json::from_str(&text).unwrap();

        assert_eq!(reloaded.branches["api"].extra["estimate_tokens"], 50_000);
        assert_eq!(
            job.manifest.branches["api"].extra["estimate_tokens"],
            50_000
        );
    }
}
