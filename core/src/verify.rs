//! `verify` / `verify_status`: the daemon verifies a worktree on a caller's behalf.
//!
//! Spec `2026-10-05-selecao-de-testes-design.md`, F2a-2. A caller names a kind (`check`/`test`)
//! and a scope (`own`/`scope`/`full`); this module resolves which worktree and base that means,
//! asks the pure planner (`verify_plan`) which units to run, answers what it can from the green
//! cache (`verify_store`), queues the rest on the executor (`verify_exec`), and hands back a
//! ticket. The ticket's live state is always derived from `verify_runs`, never stored, so a
//! status read can never disagree with what the executor actually did.

pub mod batch;
pub mod bisect;
pub mod exec;
pub mod failures;
pub mod fingerprint;
pub mod flaky;
pub mod guard;
pub mod inherit;
pub mod ide_box;
pub mod observe;
pub mod plan;
pub mod postgate;
pub mod postgate_worker;
pub mod probe;
pub mod provision;
pub mod runs;
pub mod store;

mod resolve;
mod scope_gate;

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::auth::{ApiTokenLevel, Scope};
use crate::config;
use crate::execution::git_exec::{self, CommandResult, OPERATION_TIMEOUT};
use crate::execution::land;
use crate::execution::vcs;
use crate::inspect;
use crate::state::AppState;
use crate::test_select;
use crate::tests_map::{self, Group, MapState};
use crate::verify::exec::Executor;
use crate::verify::fingerprint as verify_fingerprint;
use crate::verify::plan::{self as verify_plan, Kind, PlanError, ScopeArg, Ticket};
use crate::verify::postgate as verify_postgate;
use crate::verify::postgate_worker as verify_postgate_worker;
use crate::verify::runs::{
    self as verify_runs, ORIGIN_VERIFY, PRIORITY_AUTONOMOUS, PRIORITY_INTERACTIVE,
};
use crate::verify::store::{self as verify_store, CacheKey, NewRequest, PlannedUnit};
use crate::workflow_records;

pub use self::resolve::*;
pub use self::scope_gate::*;

/// How often `settle` re-reads a unit it is waiting on to cache.
const SETTLE_POLL: Duration = Duration::from_millis(500);

/// After this long `settle` gives up on a unit: the executor's own unit timeout ends a run long
/// before, so a row still unfinished here is one nobody will finish, and the task must not live
/// forever waiting on it.
const SETTLE_LIMIT: Duration = Duration::from_secs(24 * 60 * 60);

/// How often `wait_ticket` re-reads a ticket that is not done yet.
const WAIT_POLL: Duration = Duration::from_millis(250);

fn yes() -> bool {
    true
}

/// The body of `POST /verify`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyArgs {
    pub kind: Kind,
    pub scope: ScopeArg,
    /// Required for the owner; a job node may omit it, and may only name its own.
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub files: Option<Vec<String>>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default = "yes")]
    pub wait: bool,
    /// F3-13: the daemon picks the base itself, from the post-merge gate's state for the project's
    /// land target (`cover_base`), so a green run over it can stand as evidence for the tip it
    /// measured. Only a `test`/`scope` request with no `base` and no `files`, over a clean
    /// worktree whose map is the root's, may ask for it; anything else is refused before a row or
    /// a unit exists.
    #[serde(default)]
    pub cover: bool,
    /// The workflow task this verification is for, stored on every unit row the request itself
    /// queues or answers from the cache (`verify_runs.task_id`), so "how many verifications did
    /// this task run" is a GROUP BY. Absent for the daemon's own gates.
    #[serde(default)]
    pub task_id: Option<String>,
    /// The daemon's probe only (`Caller::Probe`): run exactly these map groups, whole, instead of
    /// selecting them from a diff. Anyone else naming it is refused before a row exists.
    #[serde(default)]
    pub groups: Option<Vec<String>>,
}

/// The body of `POST /verify/status`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {
    pub ticket: i64,
    #[serde(default = "yes")]
    pub wait: bool,
}

/// Who asked, as far as verification cares: the owner, one autonomous run, or the daemon gating
/// a job item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Owner,
    Run(i64),
    /// The daemon's own item gate. `from_scope` never produces it, so no key can claim to be one.
    Job(i64),
    /// The merge queue's gate before publish; `from_scope` never produces it.
    Merge(i64),
    /// The daemon's post-merge gate. `from_scope` never produces it.
    Postgate,
    /// The post-merge gate's recheck of a red sha. `from_scope` never produces it.
    FlakeCheck,
    /// One probe of the post-merge gate's bisection. `from_scope` never produces it.
    Bisect,
    /// The daemon's proof of a red group at the master head. `from_scope` never produces it.
    Probe,
}

impl Caller {
    /// Every other key is refused: a sidecar, a team run or a narrower API token has no worktree
    /// of its own to verify, and running arbitrary groups is not part of what those keys grant.
    pub fn from_scope(scope: &Scope) -> Option<Caller> {
        match scope {
            Scope::Control | Scope::ApiToken(ApiTokenLevel::Admin) => Some(Caller::Owner),
            Scope::Run(id) => Some(Caller::Run(*id)),
            _ => None,
        }
    }

    /// A person waiting at the keyboard goes ahead of an autonomous run (spec, "Prioridades").
    pub fn priority(self) -> i64 {
        match self {
            Caller::Merge(_) => PRIORITY_AUTONOMOUS,
            Caller::Owner => PRIORITY_INTERACTIVE,
            Caller::Run(_) | Caller::Job(_) => PRIORITY_AUTONOMOUS,
            Caller::Postgate | Caller::FlakeCheck | Caller::Bisect | Caller::Probe => {
                verify_runs::PRIORITY_POSTGATE
            }
        }
    }

    /// How the request row records its caller, and what `may_read` compares against.
    pub fn label(self) -> String {
        match self {
            Caller::Merge(id) => format!("merge:{id}"),
            Caller::Owner => "owner".to_owned(),
            Caller::Run(id) => format!("run:{id}"),
            Caller::Job(id) => format!("job:{id}"),
            Caller::Postgate | Caller::FlakeCheck | Caller::Bisect => "postgate".to_owned(),
            Caller::Probe => "probe".to_owned(),
        }
    }

    /// What a unit's `requested_by` records. Only the post-merge gate is named by caller; every
    /// other caller keeps the scope text, so existing readers see no new values.
    pub fn requested_by(self, scope: &str) -> String {
        match self {
            Caller::Postgate => verify_runs::REQUESTED_BY_POSTGATE.to_owned(),
            Caller::FlakeCheck => verify_runs::REQUESTED_BY_FLAKE_CHECK.to_owned(),
            Caller::Bisect => verify_runs::REQUESTED_BY_BISECT.to_owned(),
            // Named on purpose: `Executor::gate_env` treats a probe like the post-merge gate.
            Caller::Probe => verify_runs::REQUESTED_BY_PROBE.to_owned(),
            _ => scope.to_owned(),
        }
    }

    /// A proof unit is cut off well before its lease (`PROBE_LEASE_SECONDS`) lapses; every other
    /// caller keeps the executor's configured timeout (0 = "derive it").
    pub fn unit_timeout_ms(self) -> i64 {
        match self {
            Caller::Probe => i64::try_from(probe::UNIT_TIMEOUT.as_millis()).unwrap_or(i64::MAX),
            _ => 0,
        }
    }
}

/// A run reads only its own tickets: another run's output tail may carry what that run was
/// working on, and a ticket id is a guessable integer.
pub fn may_read(caller: Caller, row_caller: &str) -> bool {
    match caller {
        Caller::Merge(_) => row_caller == caller.label(),
        Caller::Owner => true,
        Caller::Run(_)
        | Caller::Job(_)
        | Caller::Postgate
        | Caller::FlakeCheck
        | Caller::Bisect
        | Caller::Probe => row_caller == caller.label(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VerifyError {
    BadRequest(String),
    Forbidden(String),
    NotFound(String),
    Unprocessable(String),
    Unavailable(String),
    Internal(String),
}

impl VerifyError {
    pub fn status(&self) -> StatusCode {
        match self {
            VerifyError::BadRequest(_) => StatusCode::BAD_REQUEST,
            VerifyError::Forbidden(_) => StatusCode::FORBIDDEN,
            VerifyError::NotFound(_) => StatusCode::NOT_FOUND,
            VerifyError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            VerifyError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            VerifyError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            VerifyError::BadRequest(message)
            | VerifyError::Forbidden(message)
            | VerifyError::NotFound(message)
            | VerifyError::Unprocessable(message)
            | VerifyError::Unavailable(message)
            | VerifyError::Internal(message) => message,
        }
    }
}

fn internal(error: impl std::fmt::Display) -> VerifyError {
    VerifyError::Internal(error.to_string())
}

/// The running executor. A global rather than an `AppState` field because `AppState` is built
/// literally in dozens of places, and only these two handlers need it.
static EXECUTOR: OnceLock<Arc<Executor>> = OnceLock::new();

/// Called once from `main` when the executor starts. A second call changes nothing: the first
/// executor is the one whose worker loop is draining the queue.
pub fn install(executor: Arc<Executor>) {
    if EXECUTOR.set(executor).is_err() {
        tracing::warn!("verify executor installed twice; keeping the first");
    }
}

/// `None` until `main` installs the executor, which the handlers answer with 503.
pub fn installed() -> Option<Arc<Executor>> {
    EXECUTOR.get().cloned()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A green unit `settle` may write to the cache once it finishes.
struct Candidate {
    run_id: i64,
    group_name: String,
    group: Group,
    argv: Vec<String>,
    fingerprint: String,
}

/// A group unit's fingerprint comes from its map entry; the gate unit's from the command itself,
/// which is never cached but still lets two equal requests join one run.
fn unit_fingerprint(
    unit: &PlannedUnit,
    group: Option<&Group>,
    map_version: Option<u32>,
    gate_command: Option<&str>,
    content: &str,
) -> Result<String, String> {
    match (&unit.group, group, map_version, gate_command) {
        (Some(_), Some(group), Some(version), _) => Ok(verify_fingerprint::group_fingerprint_from(
            version, group, content,
        )),
        (Some(name), _, _, _) => Err(format!("group {name} is not in the map")),
        (None, _, _, Some(command)) => {
            Ok(verify_fingerprint::gate_fingerprint_from(command, content))
        }
        (None, _, _, None) => Err("the gate unit has no gate_command".to_owned()),
    }
}

/// Plans, consults the cache, queues, and records one request. Returns the ticket id.
///
/// The handler runs this inside its own task, so a caller hanging up halfway does not leave a
/// request with half its units queued and no plan recorded.
pub async fn submit(
    executor: &Arc<Executor>,
    caller: Caller,
    args: &VerifyArgs,
) -> Result<i64, VerifyError> {
    let (id, _) = submit_checked(executor, caller, args).await?;
    Ok(id)
}

/// Whether `worktree`'s test map is, right now, the one `project_id`'s root holds
/// (`job::map_is_trusted`). `false` on any doubt: no root recorded, an unreadable record, a join
/// error.
async fn map_trusted_now(pool: &SqlitePool, project_id: &str, worktree: &Path) -> bool {
    let project_root = match inspect::project_root(pool, project_id).await {
        Ok(Some(root)) => PathBuf::from(root),
        _ => return false,
    };
    let worktree = worktree.to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::execution::job::map_is_trusted(&worktree, &project_root)
    })
    .await
    .unwrap_or(false)
}

/// `submit` that also says whether the plan was made from a map the daemon could vouch for at
/// submit time: `true` only for a `test`/`scope` request whose worktree map was the root's (and its
/// scripts untouched) when the map was loaded. `clean_tree` does not see ignored files, so a map
/// (or a script its commands name) that is gitignored can be rewritten before the submit and put
/// back before the ticket completes; the completion-time check alone would then bless a plan the
/// root's map never made. A caller that holds its own tree (`gate_scope_ticketed`) must not
/// confirm it when this is `false`.
async fn submit_checked(
    executor: &Arc<Executor>,
    caller: Caller,
    args: &VerifyArgs,
) -> Result<(i64, bool), VerifyError> {
    let pool = &executor.pool;
    // Naming groups is the daemon's probe alone: refused before any row or git read.
    if args.groups.is_some() && caller != Caller::Probe {
        return Err(VerifyError::BadRequest(
            "groups is for the daemon's probe".into(),
        ));
    }
    // The same bound the workflow ledger holds a task id to, so a task that verifies can be joined
    // to the records it filed.
    if args
        .task_id
        .as_deref()
        .is_some_and(|task_id| task_id.trim().chars().count() > workflow_records::LABEL_LIMIT)
    {
        return Err(VerifyError::BadRequest(format!(
            "task_id is longer than {} characters",
            workflow_records::LABEL_LIMIT
        )));
    }
    let deadline = Instant::now() + OPERATION_TIMEOUT;
    let (root, project_id) =
        resolve_worktree(pool, caller, args.worktree.as_deref(), deadline).await?;

    // F3-13: `cover` only narrows what F3-12's evidence rules accept; it never widens them.
    if args.cover
        && (args.kind != Kind::Test
            || args.scope != ScopeArg::Scope
            || args.base.is_some()
            || args.files.is_some())
    {
        return Err(VerifyError::BadRequest(
            "cover is a test/scope request with no base and no files".to_owned(),
        ));
    }

    // F3-12: read before anything else is, so the tree is the one the plan below is made from. An
    // error is no tree; it never fails the request.
    let submitted_tree = if args.kind == Kind::Test && args.scope == ScopeArg::Scope {
        match clean_tree(&root, deadline).await {
            Ok(tree) => tree,
            Err(error) => {
                tracing::info!(%error, "verify: cannot read the worktree's tree; none will be recorded");
                None
            }
        }
    } else {
        None
    };

    let rules = match config::load_schedule_rules(executor.machine_root.as_deref(), &project_id) {
        Ok(rules) => rules,
        Err(error) => {
            // The rules only supply `gate_command`; without them `full` is refused by the planner
            // with its own message, which says more than this read error would.
            tracing::warn!(%error, %project_id, "verify: cannot read the project's rules");
            config::AutopilotRules::default()
        }
    };

    let map = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || tests_map::load(&root))
            .await
            .map_err(internal)?
    };

    // F3-12: the map the plan is built from must be the root's at this moment, not only when the
    // ticket ends. Untrusted here means no tree is held, so `confirm_measured_tree` never runs.
    let map_trusted = submitted_tree.is_some()
        && matches!(map, MapState::Valid(_))
        && map_trusted_now(pool, &project_id, &root).await;
    // F3-13: a cover request is worth nothing without a tree to record, so it is refused here,
    // before any row exists, rather than run and then not count.
    if args.cover && submitted_tree.is_none() {
        return Err(VerifyError::Unprocessable(
            "cover needs a clean worktree: commit or remove what is uncommitted first".to_owned(),
        ));
    }
    if args.cover && !map_trusted {
        return Err(VerifyError::Unprocessable(
            "cover needs the project root's test map in this worktree, unchanged".to_owned(),
        ));
    }
    let submitted_tree = submitted_tree.filter(|_| map_trusted);
    if !map_trusted && args.kind == Kind::Test && args.scope == ScopeArg::Scope {
        tracing::info!("verify: the test map is not the root's at submit; no tree is recorded");
    }

    let own_files = args.scope == ScopeArg::Own && args.files.is_some();
    let needs_base = caller != Caller::Probe
        && args.scope != ScopeArg::Full
        && matches!(map, MapState::Valid(_))
        && !own_files;
    let base = if needs_base {
        let project_root = inspect::project_root(pool, &project_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| {
                VerifyError::NotFound(format!("project {project_id} has no root recorded"))
            })?;
        let project_root = Path::new(&project_root);
        Some(if args.cover {
            cover_base(pool, &project_id, project_root, &root, deadline).await?
        } else {
            resolve_base(
                pool,
                &project_id,
                project_root,
                &root,
                args.base.as_deref(),
                deadline,
            )
            .await?
        })
    } else {
        None
    };

    // Without a base there is nothing to diff, and the planner does not read `changed` then:
    // `full` ignores it, and a project without a valid map plans from its gate_command alone.
    let diffed = match base.as_deref() {
        Some(base) => changed_paths(&root, base, deadline).await?,
        None => Vec::new(),
    };
    let changed: Vec<String> = match args.scope {
        ScopeArg::Full => Vec::new(),
        ScopeArg::Own => args.files.clone().unwrap_or(diffed),
        ScopeArg::Scope => {
            let mut all: BTreeSet<String> = diffed.into_iter().collect();
            all.extend(args.files.iter().flatten().cloned());
            all.into_iter().collect()
        }
    };
    let fillable = {
        let root = root.clone();
        let changed = changed.clone();
        tokio::task::spawn_blocking(move || test_select::vet(&root, &changed))
            .await
            .map_err(internal)?
    };

    // `true` until a project can declare that it gates after landing (there is no
    // `gate_after_land` yet); the planner already knows how to refuse `full` once there is.
    let planned = match (caller, args.groups.as_deref()) {
        (Caller::Probe, Some(groups)) => verify_plan::plan_groups(&map, args.kind, groups),
        _ => verify_plan::plan(
            &map,
            args.kind,
            args.scope,
            &changed,
            &fillable,
            rules.gate_command.as_deref(),
            true,
            // Filters narrow only a session's own iteration run. A scope run (the daemon's item
            // and merge gates, the evidence a green run leaves for a tip) and any daemon caller
            // measure the whole group.
            args.scope == ScopeArg::Own && matches!(caller, Caller::Owner | Caller::Run(_)),
        ),
    };
    let plan = planned.map_err(|error| match error {
        PlanError::InvalidMap(errors) => VerifyError::Unprocessable(format!(
            "nucleos.tests.yaml is invalid: {}",
            errors.join("; ")
        )),
        PlanError::NoGateCommand => {
            VerifyError::Unprocessable("this project has no gate_command".to_owned())
        }
        PlanError::BadGateCommand(message) => {
            VerifyError::Unprocessable(format!("gate_command is unusable: {message}"))
        }
        PlanError::FullRefused(message) => VerifyError::Forbidden(message),
    })?;

    let worktree = root.to_string_lossy().into_owned();
    let scope = args.scope.as_str();
    let kind = args.kind.as_str();
    let priority = caller.priority();
    let label = caller.label();
    let id = verify_store::insert_request(
        pool,
        &NewRequest {
            project_id: &project_id,
            worktree: &worktree,
            kind,
            scope,
            base: base.as_deref(),
            priority,
            caller: &label,
            note: plan.note.as_deref(),
            unclaimed: &plan.unclaimed,
        },
    )
    .await
    .map_err(internal)?;

    let now = now_ms();
    if let Err(error) = verify_store::cache_prune(pool, now, executor.config.cache_days).await {
        // Pruning is housekeeping: lookups already ignore expired rows, so a failure costs disk,
        // never a wrong answer.
        tracing::warn!(%error, "verify: cannot prune the cache");
    }

    let map_version = match &map {
        MapState::Valid(map) => Some(map.version),
        _ => None,
    };
    let group_of = |name: &str| match &map {
        MapState::Valid(map) => map.tests.groups.get(name),
        _ => None,
    };

    // Groups reading the same paths hash the same content; hashing a large tree once per group
    // would make a many-group request pay for the tree many times over.
    let mut content_memo: HashMap<(Option<Vec<String>>, bool), String> = HashMap::new();
    let mut units: Vec<PlannedUnit> = Vec::with_capacity(plan.units.len());
    let mut candidates = Vec::new();
    // The request row already exists, so a failure part-way must still leave a plan behind:
    // with none the ticket would read as finished with nothing to run, and the units already
    // queued would be orphaned under it. Units never submitted keep no run id and read as
    // `unknown`, which errors the verdict.
    let mut failure: Option<VerifyError> = None;
    let mut pending = plan.units.into_iter();

    for mut unit in pending.by_ref() {
        if unit.skipped.is_some() {
            units.push(unit);
            continue;
        }

        let group = unit.group.as_deref().and_then(group_of);
        let (reads, include_ignored) = match group {
            Some(group) => (group.reads.clone(), group.include_ignored),
            None => (None, false),
        };
        let memo_key = (reads, include_ignored);
        let content = match content_memo.get(&memo_key) {
            Some(content) => Ok(content.clone()),
            None => {
                let hashed =
                    verify_fingerprint::content_hash(&root, memo_key.0.as_deref(), memo_key.1)
                        .await;
                if let Ok(content) = &hashed {
                    content_memo.insert(memo_key, content.clone());
                }
                hashed
            }
        };
        let fingerprint = content.and_then(|content| {
            unit_fingerprint(
                &unit,
                group,
                map_version,
                rules.gate_command.as_deref(),
                &content,
            )
        });
        match fingerprint {
            Ok(fingerprint) => unit.fingerprint = Some(fingerprint),
            Err(error) => {
                // Without a fingerprint the unit still runs; it just cannot be reused or joined.
                tracing::warn!(%error, request = id, "verify: no fingerprint for a unit");
                unit.fingerprint = None;
                unit.cacheable = false;
            }
        }
        if caller == Caller::Probe {
            // A proof neither feeds the cache (`settle` is spawned for any cacheable unit, whatever
            // the priority) nor joins an equal unit of another tree: it runs in its own tree, now.
            unit.fingerprint = None;
            unit.cacheable = false;
        }

        // The post-gate never reads the cache: it is the check that a cache could be wrong.
        if unit.cacheable
            && priority != verify_runs::PRIORITY_POSTGATE
            && let (Some(fingerprint), Some(group_name)) = (&unit.fingerprint, &unit.group)
        {
            let key = CacheKey {
                project_id: &project_id,
                group: group_name,
                kind,
                argv: &unit.argv,
                fingerprint,
            };
            match verify_store::cache_lookup(pool, &key, now, executor.config.cache_days).await {
                Ok(Some(hit)) => {
                    let recorded = verify_store::record_cache_hit(
                        pool,
                        &project_id,
                        &worktree,
                        scope,
                        id,
                        &key,
                    )
                    .await;
                    let recorded = match recorded {
                        Ok(recorded) => recorded,
                        Err(error) => {
                            failure = Some(internal(error));
                            units.push(unit);
                            break;
                        }
                    };
                    unit.run_id = Some(recorded);
                    unit.cached_from = Some(hit.run_id);
                    units.push(unit);
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    // A cache that cannot be read is a miss: the unit runs, which is never wrong.
                    tracing::warn!(%error, request = id, "verify: cannot read the cache");
                }
            }
        }

        let submitted = executor
            .submit(verify_runs::Request {
                project_id: Some(project_id.clone()),
                worktree: worktree.clone(),
                scope: scope.to_owned(),
                origin: ORIGIN_VERIFY.to_owned(),
                origin_id: Some(id),
                requested_by: caller.requested_by(scope),
                group_name: unit.group.clone(),
                kind: Some(kind.to_owned()),
                argv: unit.argv.clone(),
                fingerprint: unit.fingerprint.clone(),
                priority,
                // Zero lets the executor derive both from the argv and its own config.
                weight: 0,
                timeout_ms: caller.unit_timeout_ms(),
            })
            .await;
        let submitted = match submitted {
            Ok(submitted) => submitted,
            Err(error) => {
                failure = Some(internal(error));
                units.push(unit);
                break;
            }
        };
        unit.run_id = Some(submitted.id);

        if unit.cacheable
            && let (Some(fingerprint), Some(group_name), Some(group)) =
                (&unit.fingerprint, &unit.group, group)
        {
            candidates.push(Candidate {
                run_id: submitted.id,
                group_name: group_name.clone(),
                group: group.clone(),
                argv: unit.argv.clone(),
                fingerprint: fingerprint.clone(),
            });
        }
        units.push(unit);
    }

    units.extend(pending);
    verify_store::set_plan(pool, id, &units)
        .await
        .map_err(internal)?;
    if let Some(task_id) = args
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|task_id| !task_id.is_empty())
        && let Err(error) = verify_runs::tag_task(pool, id, task_id).await
    {
        // The task is bookkeeping for the workflow's ledgers; the verdict does not depend on it.
        tracing::warn!(%error, request = id, "verify: cannot record the task on the units");
    }
    if let Some(failure) = failure {
        return Err(failure);
    }

    if let Some(version) = map_version {
        for candidate in candidates {
            tokio::spawn(settle(
                executor.clone(),
                root.clone(),
                project_id.clone(),
                args.kind,
                version,
                candidate,
            ));
        }
    }

    if let (Some(tree), Some(_)) = (submitted_tree, base.as_deref()) {
        tokio::spawn(confirm_measured_tree(
            executor.pool.clone(),
            id,
            root.clone(),
            tree,
        ));
    }

    Ok((id, map_trusted))
}

/// Records, once request `id`'s ticket is done, the tree its `test`/`scope` run measured, and only
/// when the daemon can vouch that it measured that tree (F3-12, decision 2): the worktree was
/// clean when the request was submitted (`submitted`), is clean now with the very same tree, and
/// its test map is the one the project's root holds (`job::map_is_trusted`) — so what the groups
/// ran is what the root's map says they run, not something the worktree wrote for itself.
///
/// Writes only on success; any failure or doubt leaves `measured_tree` NULL, and a NULL never
/// covers anything. Idempotent: `set_measured_tree` writes only while the column is empty, so the
/// detached copy `submit` spawns and the awaited one in `gate_scope_ticketed` may both run. A
/// daemon restart halfway loses the proof, like the cache; the cost is one more gate.
pub async fn confirm_measured_tree(
    pool: SqlitePool,
    id: i64,
    worktree: PathBuf,
    submitted: String,
) {
    let started = Instant::now();
    let ticket = loop {
        match read_ticket(&pool, id).await {
            Ok(Some((ticket, _))) if ticket.done => break ticket,
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(error) => {
                // Transient (a busy database): keep waiting rather than drop the proof.
                tracing::warn!(%error, ticket = id, "verify: cannot read a ticket to record its tree");
            }
        }
        if started.elapsed() >= SETTLE_LIMIT {
            tracing::info!(ticket = id, "verify: gave up waiting to record a tree");
            return;
        }
        tokio::time::sleep(SETTLE_POLL).await;
    };

    let deadline = Instant::now() + OPERATION_TIMEOUT;
    match clean_tree(&worktree, deadline).await {
        Ok(Some(now)) if now == submitted => {}
        Ok(_) => {
            tracing::info!(
                ticket = id,
                "verify: the worktree moved during the run; no tree recorded"
            );
            return;
        }
        Err(error) => {
            tracing::info!(ticket = id, %error, "verify: cannot re-read the worktree; no tree recorded");
            return;
        }
    }

    let project_root = match inspect::project_root(&pool, &ticket.project_id).await {
        Ok(Some(root)) => PathBuf::from(root),
        Ok(None) => {
            tracing::info!(
                ticket = id,
                "verify: the project has no root; no tree recorded"
            );
            return;
        }
        Err(error) => {
            tracing::info!(ticket = id, %error, "verify: cannot read the project's root; no tree recorded");
            return;
        }
    };
    let trusted = {
        let worktree = worktree.clone();
        tokio::task::spawn_blocking(move || {
            crate::execution::job::map_is_trusted(&worktree, &project_root)
        })
        .await
        .unwrap_or(false)
    };
    if !trusted {
        tracing::info!(
            ticket = id,
            "verify: the worktree's test map is not the root's; no tree recorded"
        );
        return;
    }

    if let Err(error) = verify_store::set_measured_tree(&pool, id, &submitted).await {
        tracing::warn!(%error, ticket = id, "verify: cannot record a measured tree");
    }
}

/// Waits for one unit and, if it went green over a worktree that did not move meanwhile, caches it.
///
/// Detached from the request so a caller that stops waiting still leaves its green behind. A
/// daemon restart mid-run loses that green (accepted in v1): the next request simply runs it.
async fn settle(
    executor: Arc<Executor>,
    root: PathBuf,
    project_id: String,
    kind: Kind,
    map_version: u32,
    candidate: Candidate,
) {
    let started = Instant::now();
    let state = loop {
        match executor.get(candidate.run_id).await {
            Ok(Some(state))
                if state.status != verify_runs::STATUS_QUEUED
                    && state.status != verify_runs::STATUS_RUNNING =>
            {
                break state;
            }
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(error) => {
                // Transient (a busy database): keep waiting rather than drop a green.
                tracing::warn!(%error, run = candidate.run_id, "verify: cannot read a unit");
            }
        }
        if started.elapsed() >= SETTLE_LIMIT {
            tracing::warn!(
                run = candidate.run_id,
                "verify: gave up waiting to cache a unit"
            );
            return;
        }
        tokio::time::sleep(SETTLE_POLL).await;
    };
    if state.status != verify_runs::STATUS_PASSED {
        return;
    }

    // The run saw the worktree as it was while it ran; if the content moved under it, the green
    // belongs to neither fingerprint, so it is returned but never reused.
    match verify_fingerprint::group_fingerprint(&root, map_version, &candidate.group).await {
        Ok(now) if now == candidate.fingerprint => {
            let key = CacheKey {
                project_id: &project_id,
                group: &candidate.group_name,
                kind: kind.as_str(),
                argv: &candidate.argv,
                fingerprint: &candidate.fingerprint,
            };
            if let Err(error) = verify_store::cache_store(
                &executor.pool,
                &key,
                candidate.run_id,
                state.duration_ms,
                now_ms(),
            )
            .await
            {
                tracing::warn!(%error, run = candidate.run_id, "verify: cannot cache a green");
            }
        }
        Ok(_) => {
            tracing::info!(
                run = candidate.run_id,
                "fingerprint moved during the run; result returned, not cached"
            );
        }
        Err(error) => {
            tracing::warn!(%error, run = candidate.run_id, "verify: cannot re-fingerprint a green");
        }
    }
}

/// A ticket as it stands now, with the caller label that may read it.
pub async fn read_ticket(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<(Ticket, String)>> {
    read_ticket_with(pool, installed().as_ref(), id, true).await
}

/// `classify`: whether this read may tell an inherited red from the session's own (and so start a
/// master proof). Land evidence and access checks read with `false`: they must have no side effects.
pub async fn read_ticket_with(
    pool: &SqlitePool,
    executor: Option<&Arc<Executor>>,
    id: i64,
    classify: bool,
) -> sqlx::Result<Option<(Ticket, String)>> {
    let Some(request) = verify_store::get_request(pool, id).await? else {
        return Ok(None);
    };
    let mut live = HashMap::new();
    for unit in &request.plan {
        // A skipped or cached unit's report comes from the plan alone.
        if unit.skipped.is_some() || unit.cached_from.is_some() {
            continue;
        }
        if let Some(run_id) = unit.run_id
            && let Some(state) = verify_runs::get(pool, run_id).await?
        {
            live.insert(run_id, state);
        }
    }
    let mut ticket = verify_plan::assemble(&request, &live);
    annotate_red_target(pool, &mut ticket).await;
    if classify {
        inherit::apply(pool, executor, &request, &mut ticket, chrono::Utc::now()).await;
    }
    Ok(Some((ticket, request.caller)))
}

/// The proof that a tree was verified: the `verify` ticket that passed, and the base it diffed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub ticket: i64,
    pub base: String,
}

/// The newest `verify` request that covers `tree` from one of `bases`, if any (F3-12, decisions 4
/// and 9).
///
/// Covers means: the daemon recorded `tree` as that request's `measured_tree` (so it was a `test`
/// kind, `scope` scope, clean-worktree, trusted-map request — see `confirm_measured_tree`), its
/// base is one of `bases`, and its ticket is done with the verdict `passed`. `passed` already
/// means at least one unit passed or was reused from the cache and none failed or errored, so
/// `nothing_ran`, `failed`, `errored` and an unfinished ticket cover nothing. A base outside
/// `bases` covers nothing either: the diff it measured is not the one the caller can trust.
pub async fn covering_request(
    pool: &SqlitePool,
    project_id: &str,
    tree: &str,
    bases: &[&str],
) -> sqlx::Result<Option<Evidence>> {
    for (id, base) in verify_store::requests_measuring(pool, project_id, tree).await? {
        if !bases.contains(&base.as_str()) {
            continue;
        }
        let Some((ticket, _)) = read_ticket_with(pool, None, id, false).await? else {
            continue;
        };
        if ticket.done && ticket.verdict.as_deref() == Some("passed") {
            return Ok(Some(Evidence { ticket: id, base }));
        }
    }
    Ok(None)
}

/// Marks each failed unit whose group (or the gate command) is already red on the project's
/// target branch. Only the unit's note changes: its status and the ticket's verdict stay as
/// they are. A failed read of the post-merge state never fails the ticket.
async fn annotate_red_target(pool: &SqlitePool, ticket: &mut Ticket) {
    if !ticket
        .units
        .iter()
        .any(|unit| unit.status == verify_runs::STATUS_FAILED)
    {
        return;
    }
    let state = match verify_postgate::load(pool, &ticket.project_id).await {
        Ok(Some(state)) => state,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, "verify: cannot read the post-merge state");
            return;
        }
    };
    for unit in &mut ticket.units {
        if unit.status != verify_runs::STATUS_FAILED {
            continue;
        }
        let key = unit
            .group
            .as_deref()
            .unwrap_or(verify_postgate_worker::GATE_UNIT);
        unit.already_failing = state.already_failing(key);
    }
}

/// Re-reads the ticket until it is done or `deadline` has passed, whichever comes first.
pub async fn wait_ticket(
    pool: &SqlitePool,
    id: i64,
    deadline: Duration,
) -> sqlx::Result<Option<(Ticket, String)>> {
    wait_ticket_with(pool, installed().as_ref(), id, deadline, true).await
}

/// `wait_ticket` with the executor and the classification switch spelled out.
pub async fn wait_ticket_with(
    pool: &SqlitePool,
    executor: Option<&Arc<Executor>>,
    id: i64,
    deadline: Duration,
    classify: bool,
) -> sqlx::Result<Option<(Ticket, String)>> {
    let started = Instant::now();
    loop {
        let Some((ticket, caller)) = read_ticket_with(pool, executor, id, classify).await? else {
            return Ok(None);
        };
        if ticket.done || started.elapsed() >= deadline {
            return Ok(Some((ticket, caller)));
        }
        tokio::time::sleep(WAIT_POLL).await;
    }
}

fn refused(error: &VerifyError) -> (StatusCode, String) {
    (error.status(), error.message().to_owned())
}

fn database_error(error: sqlx::Error) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("could not read the ticket: {error}"),
    )
}

/// `POST /verify`: plans and queues, then waits up to `vcs::DEFAULT_WAIT` for the verdict.
pub async fn post_verify(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(args): Json<VerifyArgs>,
) -> Result<Json<Ticket>, (StatusCode, String)> {
    let caller = Caller::from_scope(&scope).ok_or_else(|| {
        refused(&VerifyError::Forbidden(
            "this key cannot ask for verification".to_owned(),
        ))
    })?;
    let executor = installed().ok_or_else(|| {
        refused(&VerifyError::Unavailable(
            "the verify executor is not running".to_owned(),
        ))
    })?;
    let wait = if args.wait {
        vcs::DEFAULT_WAIT
    } else {
        Duration::ZERO
    };
    // Its own task: a dropped connection cancels this handler's future, and a request cut off
    // between queuing units and recording its plan would leave work nobody can see.
    let id = tokio::spawn(async move { submit(&executor, caller, &args).await })
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the verify request did not finish: {error}"),
            )
        })?
        .map_err(|error| refused(&error))?;
    match wait_ticket(&state.pool, id, wait).await {
        Ok(Some((ticket, _))) => Ok(Json(ticket)),
        Ok(None) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("ticket {id} vanished after it was created"),
        )),
        Err(error) => Err(database_error(error)),
    }
}

/// `POST /verify/status`: one ticket, waited on like `post_verify` unless `wait` is false.
pub async fn post_verify_status(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(args): Json<StatusArgs>,
) -> Result<Json<Ticket>, (StatusCode, String)> {
    let caller = Caller::from_scope(&scope).ok_or_else(|| {
        (
            StatusCode::FORBIDDEN,
            "this key cannot read verification".to_owned(),
        )
    })?;
    // 404 rather than 403 for another run's ticket, so a run cannot learn which ids exist.
    let not_found = || (StatusCode::NOT_FOUND, format!("no ticket {}", args.ticket));
    // Access is checked before waiting, so a refused caller is not held for the whole wait first.
    match read_ticket_with(&state.pool, None, args.ticket, false)
        .await
        .map_err(database_error)?
    {
        Some((_, owner)) if may_read(caller, &owner) => {}
        _ => return Err(not_found()),
    }
    let wait = if args.wait {
        vcs::DEFAULT_WAIT
    } else {
        Duration::ZERO
    };
    match wait_ticket(&state.pool, args.ticket, wait)
        .await
        .map_err(database_error)?
    {
        Some((ticket, _)) => Ok(Json(ticket)),
        None => Err(not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VerifyConfig;
    use crate::verify::exec as verify_exec;
    use crate::verify::plan::NO_MAP_OWN_NOTE;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    /// Long enough for a `git --version` unit on a loaded machine, short enough to bound a test.
    const WAIT: Duration = Duration::from_secs(20);

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// Runs git in `dir`, asserts it succeeded, and returns its trimmed stdout.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "core.autocrlf=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A map of two groups, `core` (claims `core/`) and `py` (claims `py/`), both checked by
    /// `git --version` unless `core_check` says otherwise; `core_extra` is appended to `core`.
    fn map_with(core_check: &str, core_extra: &str) -> String {
        format!(
            "version: 1
tests:
  groups:
    core:
      paths: [core/]
      check: {core_check}
      command: git --version
{core_extra}    py:
      paths: [py/]
      check: git --version
      command: git --version
"
        )
    }

    fn plain_map() -> String {
        map_with("git --version", "")
    }

    struct Fixture {
        pool: SqlitePool,
        ex: Arc<Executor>,
        repo: tempfile::TempDir,
        _machine: tempfile::TempDir,
        /// The first commit: every file, the map included.
        c1: String,
        /// The second commit, on `main`: `core/a.rs` changed.
        c2: String,
    }

    /// A git repository with two commits on `main`, rostered as project `alpha`, and an executor
    /// whose machine root holds `alpha`'s `autopilot.yaml` when `gate` is given. The worker loop
    /// runs only when `run` is set.
    async fn fixture(map: Option<&str>, gate: Option<&str>, run: bool) -> Fixture {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::fs::create_dir_all(root.join("core")).unwrap();
        std::fs::create_dir_all(root.join("py")).unwrap();
        std::fs::write(root.join("core/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(root.join("py/b.py"), "x = 1\n").unwrap();
        if let Some(map) = map {
            std::fs::write(root.join(tests_map::MAP_FILE), map).unwrap();
        }
        git_in(root, &["init", "-q", "-b", "main"]);
        git_in(root, &["add", "-A"]);
        git_in(root, &["commit", "-q", "-m", "one"]);
        let c1 = git_in(root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("core/a.rs"), "fn a() { let _ = 1; }\n").unwrap();
        git_in(root, &["commit", "-q", "-am", "two"]);
        let c2 = git_in(root, &["rev-parse", "HEAD"]);

        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) \
             VALUES ('alpha', 'active', ?)",
        )
        .bind(path_arg(root))
        .execute(&pool)
        .await
        .unwrap();

        let machine = tempfile::tempdir().unwrap();
        if let Some(gate) = gate {
            let dir = machine.path().join("projects").join("alpha");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(crate::project::state::AUTOPILOT_FILE),
                format!("gate_command: \"{gate}\"\n"),
            )
            .unwrap();
        }
        let ex = Executor::new(
            pool.clone(),
            VerifyConfig::default(),
            Some(machine.path().to_path_buf()),
        );
        if run {
            tokio::spawn(verify_exec::run_executor(ex.clone()));
        }
        Fixture {
            pool,
            ex,
            repo,
            _machine: machine,
            c1,
            c2,
        }
    }

    fn path_arg(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn args(
        kind: Kind,
        scope: ScopeArg,
        worktree: &Path,
        files: Option<&[&str]>,
        base: Option<&str>,
    ) -> VerifyArgs {
        VerifyArgs {
            kind,
            scope,
            worktree: Some(path_arg(worktree)),
            files: files.map(|files| files.iter().map(|file| (*file).to_owned()).collect()),
            base: base.map(str::to_owned),
            wait: true,
            cover: false,
            task_id: None,
            groups: None,
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    async fn finished(pool: &SqlitePool, id: i64) -> (Ticket, String) {
        let (ticket, caller) = wait_ticket(pool, id, WAIT).await.unwrap().unwrap();
        assert!(
            ticket.done,
            "ticket {id} did not finish in time: {ticket:?}"
        );
        (ticket, caller)
    }

    async fn count(pool: &SqlitePool, table: &str) -> i64 {
        // Audited: `table` is always a literal table name written in these tests.
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn cache_rows(pool: &SqlitePool) -> i64 {
        count(pool, "verify_cache").await
    }

    /// Polls until the cache holds a row, for at most `WAIT`.
    async fn wait_for_cache(pool: &SqlitePool) {
        let started = Instant::now();
        while cache_rows(pool).await == 0 {
            assert!(started.elapsed() < WAIT, "no green was cached in time");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Long enough for `settle` (polling every 500 ms) to have seen a finished unit and decided.
    async fn let_settle_decide() {
        tokio::time::sleep(SETTLE_POLL * 4).await;
    }

    fn groups(ticket: &Ticket) -> Vec<Option<String>> {
        ticket.units.iter().map(|unit| unit.group.clone()).collect()
    }

    fn own_core(f: &Fixture) -> VerifyArgs {
        args(
            Kind::Check,
            ScopeArg::Own,
            f.repo.path(),
            Some(&["core/a.rs"]),
            None,
        )
    }

    #[test]
    fn caller_priority_is_interactive_for_the_owner_and_autonomous_for_a_run() {
        assert_eq!(Caller::Owner.priority(), PRIORITY_INTERACTIVE);
        assert_eq!(Caller::Run(7).priority(), PRIORITY_AUTONOMOUS);
        assert_eq!(Caller::Owner.label(), "owner");
        assert_eq!(Caller::Run(7).label(), "run:7");

        assert_eq!(Caller::from_scope(&Scope::Control), Some(Caller::Owner));
        assert_eq!(
            Caller::from_scope(&Scope::ApiToken(ApiTokenLevel::Admin)),
            Some(Caller::Owner)
        );
        assert_eq!(Caller::from_scope(&Scope::Run(7)), Some(Caller::Run(7)));
        assert_eq!(
            Caller::from_scope(&Scope::ApiToken(ApiTokenLevel::ReadOnly)),
            None
        );
        assert_eq!(
            Caller::from_scope(&Scope::ApiToken(ApiTokenLevel::RunCreating)),
            None
        );
        assert_eq!(Caller::from_scope(&Scope::TeamRun("t".to_owned())), None);
    }

    #[test]
    fn a_run_may_read_only_its_own_tickets() {
        assert!(may_read(Caller::Owner, "owner"));
        assert!(may_read(Caller::Owner, "run:3"));
        assert!(may_read(Caller::Run(3), "run:3"));
        assert!(!may_read(Caller::Run(3), "run:4"));
        assert!(!may_read(Caller::Run(3), "run:33"));
        assert!(!may_read(Caller::Run(3), "owner"));
    }

    #[test]
    fn a_job_caller_is_autonomous_and_labelled_by_its_job() {
        assert_eq!(Caller::Job(9).priority(), PRIORITY_AUTONOMOUS);
        assert_eq!(Caller::Job(9).label(), "job:9");
        assert!(may_read(Caller::Job(9), "job:9"));
        assert!(!may_read(Caller::Job(9), "job:99"));
        assert!(!may_read(Caller::Job(9), "run:9"));
        // No key maps to a job: only the daemon's own gate can be one.
        for scope in [
            Scope::Control,
            Scope::Run(9),
            Scope::ApiToken(ApiTokenLevel::Admin),
        ] {
            assert_ne!(Caller::from_scope(&scope), Some(Caller::Job(9)));
        }
    }

    #[test]
    fn a_merge_caller_is_autonomous_labelled_by_its_request_and_reads_only_its_own_tickets() {
        assert_eq!(Caller::Merge(7).priority(), PRIORITY_AUTONOMOUS);
        assert_eq!(Caller::Merge(7).label(), "merge:7");
        assert!(may_read(Caller::Merge(7), "merge:7"));
        assert!(!may_read(Caller::Merge(7), "merge:8"));
        assert!(!may_read(Caller::Merge(7), "owner"));
        assert!(!may_read(Caller::Merge(7), "run:7"));
        assert_eq!(Caller::Merge(7).requested_by("x"), "x");
        // No key maps to a merge: only the daemon's own merge gate can be one.
        for scope in [
            Scope::Control,
            Scope::Run(7),
            Scope::ApiToken(ApiTokenLevel::Admin),
        ] {
            assert_ne!(Caller::from_scope(&scope), Some(Caller::Merge(7)));
        }
    }

    #[test]
    fn the_postgate_caller_runs_at_postgate_priority_and_no_key_can_claim_it() {
        assert_eq!(
            Caller::Postgate.priority(),
            crate::verify::runs::PRIORITY_POSTGATE
        );
        assert_eq!(Caller::Postgate.label(), "postgate");
        assert!(may_read(Caller::Postgate, "postgate"));
        assert!(!may_read(Caller::Postgate, "owner"));
        assert!(!may_read(Caller::Postgate, "job:9"));
        assert!(!may_read(Caller::Run(3), "postgate"));
        assert!(!may_read(Caller::Job(9), "postgate"));

        // Only the post-merge gate's units are labelled by caller; every other caller keeps the
        // scope text, so the readers of `requested_by` see no new values.
        assert_eq!(
            Caller::Postgate.requested_by("full"),
            crate::verify::runs::REQUESTED_BY_POSTGATE
        );
        assert_eq!(Caller::Postgate.requested_by("full"), "postgate");
        assert_eq!(Caller::Owner.requested_by("full"), "full");
        assert_eq!(Caller::Job(9).requested_by("diff"), "diff");
        assert_eq!(Caller::Run(7).requested_by("files"), "files");

        // No auth scope maps to it.
        for scope in [
            Scope::Control,
            Scope::Run(9),
            Scope::ApiToken(ApiTokenLevel::Admin),
            Scope::ApiToken(ApiTokenLevel::ReadOnly),
            Scope::ApiToken(ApiTokenLevel::RunCreating),
            Scope::TeamRun("t".to_owned()),
        ] {
            assert_ne!(Caller::from_scope(&scope), Some(Caller::Postgate));
        }
    }

    #[test]
    fn the_flake_check_and_bisect_callers_run_at_postgate_priority_and_label_their_units() {
        for caller in [Caller::FlakeCheck, Caller::Bisect] {
            assert_eq!(caller.priority(), crate::verify::runs::PRIORITY_POSTGATE);
            // The same owner as the gate's own tickets.
            assert_eq!(caller.label(), "postgate");
            assert!(may_read(caller, "postgate"));
            assert!(!may_read(caller, "owner"));
            assert!(!may_read(caller, "job:9"));
        }
        assert!(!may_read(Caller::Run(3), "postgate"));

        assert_eq!(
            Caller::FlakeCheck.requested_by("full"),
            crate::verify::runs::REQUESTED_BY_FLAKE_CHECK
        );
        assert_eq!(Caller::FlakeCheck.requested_by("full"), "flake-check");
        assert_eq!(
            Caller::Bisect.requested_by("full"),
            crate::verify::runs::REQUESTED_BY_BISECT
        );
        assert_eq!(Caller::Bisect.requested_by("full"), "bisect");

        // No auth scope maps to them.
        for scope in [
            Scope::Control,
            Scope::Run(9),
            Scope::ApiToken(ApiTokenLevel::Admin),
            Scope::ApiToken(ApiTokenLevel::ReadOnly),
            Scope::ApiToken(ApiTokenLevel::RunCreating),
            Scope::TeamRun("t".to_owned()),
        ] {
            assert_ne!(Caller::from_scope(&scope), Some(Caller::FlakeCheck));
            assert_ne!(Caller::from_scope(&scope), Some(Caller::Bisect));
        }
    }

    fn unit(
        group: Option<&str>,
        status: &str,
        exit: Option<i64>,
        tail: Option<&str>,
    ) -> verify_plan::UnitReport {
        verify_plan::UnitReport {
            group: group.map(str::to_owned),
            argv: vec!["git".to_owned(), "--version".to_owned()],
            why: "its files changed".to_owned(),
            status: status.to_owned(),
            duration_ms: None,
            exit_code: exit,
            output_tail: tail.map(str::to_owned),
            skipped_reason: None,
            run_id: None,
            cached_from: None,
            already_failing: None,
            failed_tests: None,
            failed_files: Vec::new(),
            inherited: None,
            pending: None,
            not_inherited: None,
        }
    }

    fn ticket(done: bool, verdict: Option<&str>, units: Vec<verify_plan::UnitReport>) -> Ticket {
        Ticket {
            ticket: 1,
            done,
            verdict: verdict.map(str::to_owned),
            project_id: "alpha".to_owned(),
            worktree: "/wt".to_owned(),
            kind: "test".to_owned(),
            scope: "scope".to_owned(),
            base: None,
            note: None,
            unclaimed: Vec::new(),
            progress: verify_plan::Progress {
                total: units.len(),
                finished: units.len(),
                queued: 0,
                running: Vec::new(),
            },
            units,
            inherited_task_ids: Vec::new(),
        }
    }

    #[test]
    fn scope_outcome_maps_each_verdict_to_a_gate_outcome() {
        use crate::gate::GateOutcome;

        let passed = ticket(
            true,
            Some("passed"),
            vec![unit(Some("core"), "passed", Some(0), None)],
        );
        assert!(matches!(
            scope_outcome(&passed),
            ScopeVerdict::Measured(GateOutcome::Passed)
        ));

        let failed = ticket(
            true,
            Some("failed"),
            vec![
                unit(Some("core"), "passed", Some(0), None),
                unit(Some("py"), "failed", Some(3), Some("py boom")),
            ],
        );
        match scope_outcome(&failed) {
            ScopeVerdict::Measured(GateOutcome::Failed { exit_code, output }) => {
                assert_eq!(exit_code, 3);
                assert!(output.contains("py"), "{output}");
                assert!(output.contains("py boom"), "{output}");
                assert!(
                    !output.contains("core"),
                    "a passed unit adds nothing: {output}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        let errored = ticket(
            true,
            Some("errored"),
            vec![unit(None, "errored", None, None)],
        );
        assert!(matches!(
            scope_outcome(&errored),
            ScopeVerdict::Measured(GateOutcome::Errored { .. })
        ));

        // A ticket that is not done has no verdict to translate: that is a measurement that did
        // not finish, not a pass.
        let pending = ticket(false, None, vec![unit(Some("core"), "running", None, None)]);
        assert!(matches!(
            scope_outcome(&pending),
            ScopeVerdict::Measured(GateOutcome::Errored { .. })
        ));

        let nothing = ticket(true, Some("nothing_ran"), Vec::new());
        assert!(matches!(scope_outcome(&nothing), ScopeVerdict::NothingRan));
    }

    #[test]
    fn scoped_final_note_names_scope_and_the_post_merge_gate() {
        use crate::gate::GateOutcome;

        let passed = scoped_final_note("run 7", &ScopeVerdict::Measured(GateOutcome::Passed));
        assert!(passed.contains("run 7"), "{passed}");
        assert!(passed.contains("verify scope"), "{passed}");
        assert!(passed.contains("passed"), "{passed}");
        assert!(passed.contains("gate_after_land"), "{passed}");

        let failed = scoped_final_note(
            "run 7",
            &ScopeVerdict::Measured(GateOutcome::Failed {
                exit_code: 3,
                output: "boom".into(),
            }),
        );
        assert!(failed.contains("verify scope"), "{failed}");
        assert!(failed.contains("failed"), "{failed}");
        assert!(failed.contains('3'), "{failed}");

        let errored = scoped_final_note(
            "run 7",
            &ScopeVerdict::Measured(GateOutcome::Errored {
                reason: "executor gone".into(),
            }),
        );
        assert!(errored.contains("verify scope"), "{errored}");
        assert!(errored.contains("executor gone"), "{errored}");

        let nothing = scoped_final_note("run 7", &ScopeVerdict::NothingRan);
        assert!(nothing.contains("verify scope"), "{nothing}");
        assert!(nothing.contains("measured nothing"), "{nothing}");
        assert!(nothing.contains("gate_after_land"), "{nothing}");

        let unavailable = scoped_final_note("run 7", &ScopeVerdict::Unavailable);
        assert!(unavailable.contains("verify scope"), "{unavailable}");
        assert!(unavailable.contains("full gate"), "{unavailable}");
    }

    #[test]
    fn a_job_gate_scope_still_queued_at_the_deadline_errors_and_says_so() {
        use crate::gate::GateOutcome;

        let mut pending = ticket(false, None, vec![unit(Some("core"), "queued", None, None)]);
        pending.progress.total = 3;
        pending.progress.finished = 1;
        pending.progress.queued = 2;
        match scope_outcome(&pending) {
            ScopeVerdict::Measured(GateOutcome::Errored { reason }) => {
                assert!(
                    reason.contains("2 of 3 units still queued"),
                    "the reason must say how much was still queued: {reason}"
                );
            }
            other => panic!("expected Errored, got {other:?}"),
        }
    }

    async fn insert_job_worktree(f: &Fixture, base: &str) {
        sqlx::query(
            "INSERT INTO worktrees (owner_kind, owner_id, project_id, project_root, path, branch, \
             created_at, base_sha) \
             VALUES ('job', 1, 'alpha', ?, ?, 'main', '2026-10-08T00:00:00Z', ?)",
        )
        .bind(path_arg(f.repo.path()))
        .bind(path_arg(f.repo.path()))
        .bind(base)
        .execute(&f.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_job_gate_scope_runs_only_the_groups_the_job_diff_selects() {
        use crate::gate::GateOutcome;

        let f = fixture(Some(&plain_map()), None, true).await;
        // The job tree stands on c2 and was cut from c1: only core/a.rs changed since.
        insert_job_worktree(&f, &f.c1).await;

        let verdict = gate_job_scope(&f.ex, 1, f.repo.path(), WAIT).await;
        assert!(
            matches!(verdict, ScopeVerdict::Measured(GateOutcome::Passed)),
            "{verdict:?}"
        );
        let groups: Vec<String> =
            sqlx::query_scalar("SELECT group_name FROM verify_runs WHERE origin = 'verify'")
                .fetch_all(&f.pool)
                .await
                .unwrap();
        assert_eq!(groups, vec!["core".to_owned()]);
        let callers: Vec<String> = sqlx::query_scalar("SELECT caller FROM verify_requests")
            .fetch_all(&f.pool)
            .await
            .unwrap();
        assert_eq!(callers, vec!["job:1".to_owned()]);
    }

    #[tokio::test]
    async fn a_job_gate_scope_over_an_unchanged_tree_ran_nothing() {
        let f = fixture(Some(&plain_map()), None, true).await;
        insert_job_worktree(&f, &f.c2).await;

        let verdict = gate_job_scope(&f.ex, 1, f.repo.path(), WAIT).await;
        assert!(matches!(verdict, ScopeVerdict::NothingRan), "{verdict:?}");
        assert_eq!(count(&f.pool, "verify_runs").await, 0);
    }

    /// A scope request that cannot even be submitted measured nothing: the caller must be told so
    /// (and measure the old way), not handed an `Errored` outcome it would record against the item.
    #[tokio::test]
    async fn a_job_gate_scope_that_cannot_be_submitted_is_unavailable() {
        let f = fixture(Some(&plain_map()), None, true).await;
        // A directory that is no git worktree at all: `submit` refuses before queueing anything.
        let bare = tempfile::tempdir().unwrap();

        let verdict = gate_job_scope(&f.ex, 1, bare.path(), WAIT).await;
        assert!(matches!(verdict, ScopeVerdict::Unavailable), "{verdict:?}");
        assert_eq!(count(&f.pool, "verify_requests").await, 0);
    }

    #[test]
    fn verify_error_statuses_map_as_documented() {
        let cases = [
            (VerifyError::BadRequest("a".into()), StatusCode::BAD_REQUEST),
            (VerifyError::Forbidden("b".into()), StatusCode::FORBIDDEN),
            (VerifyError::NotFound("c".into()), StatusCode::NOT_FOUND),
            (
                VerifyError::Unprocessable("d".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                VerifyError::Unavailable("e".into()),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                VerifyError::Internal("f".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, status) in cases {
            assert_eq!(error.status(), status, "{error:?}");
        }
        assert_eq!(VerifyError::Forbidden("why".into()).message(), "why");
        assert_eq!(VerifyError::Internal("boom".into()).message(), "boom");
    }

    #[tokio::test]
    async fn owner_without_a_worktree_is_a_bad_request() {
        let f = fixture(Some(&plain_map()), None, false).await;

        let error = resolve_worktree(&f.pool, Caller::Owner, None, deadline())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            VerifyError::BadRequest("worktree is required".into())
        );

        let mut no_worktree = own_core(&f);
        no_worktree.worktree = None;
        let error = submit(&f.ex, Caller::Owner, &no_worktree)
            .await
            .unwrap_err();
        assert!(matches!(error, VerifyError::BadRequest(_)), "{error:?}");

        // A relative path would resolve against the daemon's directory: refused, not guessed.
        let error = resolve_worktree(&f.pool, Caller::Owner, Some("core"), deadline())
            .await
            .unwrap_err();
        assert!(matches!(error, VerifyError::BadRequest(_)), "{error:?}");

        assert_eq!(count(&f.pool, "verify_requests").await, 0);
    }

    #[tokio::test]
    async fn a_path_outside_any_rostered_project_is_refused() {
        let f = fixture(Some(&plain_map()), None, false).await;

        let stranger = tempfile::tempdir().unwrap();
        git_in(stranger.path(), &["init", "-q", "-b", "main"]);
        git_in(
            stranger.path(),
            &["commit", "-q", "--allow-empty", "-m", "x"],
        );
        let asked = path_arg(stranger.path());
        let error = resolve_worktree(&f.pool, Caller::Owner, Some(&asked), deadline())
            .await
            .unwrap_err();
        assert!(matches!(error, VerifyError::NotFound(_)), "{error:?}");

        // Not a git tree at all: there is nothing to resolve.
        let plain = tempfile::tempdir().unwrap();
        let asked = path_arg(plain.path());
        let error = resolve_worktree(&f.pool, Caller::Owner, Some(&asked), deadline())
            .await
            .unwrap_err();
        assert!(matches!(error, VerifyError::Unprocessable(_)), "{error:?}");

        // The rostered repository itself resolves, to `alpha`.
        let asked = path_arg(f.repo.path());
        let (_, project) = resolve_worktree(&f.pool, Caller::Owner, Some(&asked), deadline())
            .await
            .unwrap();
        assert_eq!(project, "alpha");
    }

    #[tokio::test]
    async fn a_malformed_base_is_a_bad_request() {
        let f = fixture(Some(&plain_map()), None, false).await;

        for base in ["HEAD~1", "--output=x", "abc", "zzzzzzzz"] {
            let request = args(
                Kind::Check,
                ScopeArg::Scope,
                f.repo.path(),
                None,
                Some(base),
            );
            let error = submit(&f.ex, Caller::Owner, &request).await.unwrap_err();
            assert!(
                matches!(error, VerifyError::BadRequest(_)),
                "{base}: {error:?}"
            );
        }

        // Well formed but not a commit here: understood, and cannot be served.
        let error = resolve_base(
            &f.pool,
            "alpha",
            f.repo.path(),
            f.repo.path(),
            Some("deadbeefdeadbeef"),
            deadline(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, VerifyError::Unprocessable(_)), "{error:?}");

        assert_eq!(count(&f.pool, "verify_requests").await, 0);
    }

    #[tokio::test]
    async fn own_on_a_project_without_a_map_runs_nothing() {
        let f = fixture(None, Some("git --version"), true).await;

        let request = args(Kind::Test, ScopeArg::Own, f.repo.path(), None, None);
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, caller) = finished(&f.pool, id).await;

        assert_eq!(caller, "owner");
        assert!(ticket.units.is_empty(), "{ticket:?}");
        assert_eq!(ticket.verdict.as_deref(), Some("nothing_ran"));
        assert_eq!(ticket.note.as_deref(), Some(NO_MAP_OWN_NOTE));
        assert_eq!(ticket.base, None);
        assert_eq!(count(&f.pool, "verify_runs").await, 0);
    }

    #[tokio::test]
    async fn scope_on_a_project_without_a_map_runs_the_gate_command() {
        let f = fixture(None, Some("git --version"), true).await;

        let request = args(Kind::Test, ScopeArg::Scope, f.repo.path(), None, None);
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");
        assert_eq!(ticket.units.len(), 1);
        let unit = &ticket.units[0];
        assert_eq!(unit.group, None);
        assert_eq!(unit.argv, vec!["git".to_owned(), "--version".to_owned()]);
        assert_eq!(unit.why, "gate_command");
        assert_eq!(ticket.project_id, "alpha");
        assert_eq!(ticket.scope, "scope");
        assert_eq!(ticket.base, None);

        let (origin, origin_id, requested_by, priority): (String, Option<i64>, String, i64) =
            sqlx::query_as(
                "SELECT origin, origin_id, requested_by, priority FROM verify_runs WHERE id = ?",
            )
            .bind(unit.run_id.unwrap())
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(origin, ORIGIN_VERIFY);
        assert_eq!(origin_id, Some(id));
        assert_eq!(requested_by, "scope");
        assert_eq!(priority, PRIORITY_INTERACTIVE);
    }

    #[tokio::test]
    async fn own_with_files_runs_only_the_claimed_group() {
        let f = fixture(Some(&plain_map()), None, true).await;

        let id = submit(&f.ex, Caller::Owner, &own_core(&f)).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        assert_eq!(groups(&ticket), vec![Some("core".to_owned())]);
        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");
        assert_eq!(ticket.units[0].status, verify_runs::STATUS_PASSED);
        // Files named by the caller need no base: nothing is diffed.
        assert_eq!(ticket.base, None);
        assert_eq!(ticket.kind, "check");
    }

    /// A request that names its task stores it on every unit it queued, and one that names none
    /// leaves the column empty, so the per-task count never picks up an untagged call.
    #[tokio::test]
    async fn a_named_task_is_stored_on_the_units_the_request_queued() {
        let f = fixture(Some(&plain_map()), None, false).await;
        let tagged = VerifyArgs {
            task_id: Some(" t-7 ".to_owned()),
            ..own_core(&f)
        };
        let id = submit(&f.ex, Caller::Owner, &tagged).await.unwrap();
        let untagged = submit(&f.ex, Caller::Owner, &py_only(&f)).await.unwrap();

        let tasks = |request: i64| {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT task_id FROM verify_runs WHERE origin = ? AND origin_id = ?",
            )
            .bind(ORIGIN_VERIFY)
            .bind(request)
            .fetch_all(&f.pool)
        };
        let tagged_units = tasks(id).await.unwrap();
        assert!(!tagged_units.is_empty(), "the request queued no unit");
        assert!(
            tagged_units
                .iter()
                .all(|task| task.as_deref() == Some("t-7")),
            "{tagged_units:?}"
        );
        let untagged_units = tasks(untagged).await.unwrap();
        assert!(!untagged_units.is_empty(), "the request queued no unit");
        assert!(
            untagged_units.iter().all(Option::is_none),
            "{untagged_units:?}"
        );

        // Over the bound is refused before anything is queued.
        let long = VerifyArgs {
            task_id: Some("t".repeat(workflow_records::LABEL_LIMIT + 1)),
            ..own_core(&f)
        };
        let error = submit(&f.ex, Caller::Owner, &long).await.unwrap_err();
        assert!(matches!(error, VerifyError::BadRequest(_)), "{error:?}");
    }

    /// The `py` group alone, so a second request does not join the first one's `core` unit.
    fn py_only(f: &Fixture) -> VerifyArgs {
        args(
            Kind::Check,
            ScopeArg::Own,
            f.repo.path(),
            Some(&["py/b.py"]),
            None,
        )
    }

    #[tokio::test]
    async fn scope_diffs_against_the_explicit_base() {
        let f = fixture(Some(&plain_map()), None, true).await;

        let short = &f.c1[..10];
        let request = args(
            Kind::Check,
            ScopeArg::Scope,
            f.repo.path(),
            None,
            Some(short),
        );
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        // The short id comes back as the full one.
        assert_eq!(ticket.base.as_deref(), Some(f.c1.as_str()));
        // c1..HEAD changed core/a.rs only, so `py` is not selected.
        assert_eq!(groups(&ticket), vec![Some("core".to_owned())]);
        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");

        // Against HEAD itself nothing changed.
        let request = args(
            Kind::Check,
            ScopeArg::Scope,
            f.repo.path(),
            None,
            Some(&f.c2),
        );
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;
        assert!(ticket.units.is_empty(), "{ticket:?}");
        assert_eq!(ticket.verdict.as_deref(), Some("nothing_ran"));
    }

    #[tokio::test]
    async fn the_base_falls_back_to_the_worktree_row() {
        let f = fixture(Some(&plain_map()), None, true).await;
        // The merge base with `main` would be c2; the recorded c1 has to win.
        sqlx::query(
            "INSERT INTO worktrees (owner_kind, owner_id, project_id, project_root, path, branch, \
             created_at, base_sha) \
             VALUES ('run', 1, 'alpha', ?, ?, 'main', '2026-10-06T00:00:00Z', ?)",
        )
        .bind(path_arg(f.repo.path()))
        .bind(path_arg(f.repo.path()))
        .bind(&f.c1)
        .execute(&f.pool)
        .await
        .unwrap();

        let request = args(Kind::Check, ScopeArg::Scope, f.repo.path(), None, None);
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        assert_eq!(ticket.base.as_deref(), Some(f.c1.as_str()));
        assert_eq!(groups(&ticket), vec![Some("core".to_owned())]);
    }

    #[tokio::test]
    async fn the_base_falls_back_to_the_merge_base_with_the_integration_branch() {
        let f = fixture(Some(&plain_map()), None, true).await;
        let root = f.repo.path();
        git_in(root, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(root.join("py/b.py"), "x = 2\n").unwrap();
        git_in(root, &["commit", "-q", "-am", "three"]);
        sqlx::query(
            "UPDATE autopilot_state SET integration_branch = 'main' WHERE project_id = 'alpha'",
        )
        .execute(&f.pool)
        .await
        .unwrap();

        let request = args(Kind::Check, ScopeArg::Scope, root, None, None);
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        // `feature` branched from `main` at c2 and changed only py/b.py since.
        assert_eq!(ticket.base.as_deref(), Some(f.c2.as_str()));
        assert_eq!(groups(&ticket), vec![Some("py".to_owned())]);
    }

    #[tokio::test]
    async fn a_repeated_green_request_is_served_from_the_cache() {
        let f = fixture(Some(&plain_map()), None, true).await;
        let request = own_core(&f);

        let first = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, first).await;
        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");
        assert_eq!(ticket.units[0].cached_from, None);
        let ran = ticket.units[0].run_id.unwrap();
        wait_for_cache(&f.pool).await;

        let second = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        // Answered at submit time: done on the first read, without waiting.
        let (ticket, _) = read_ticket(&f.pool, second).await.unwrap().unwrap();
        assert!(ticket.done, "{ticket:?}");
        assert_eq!(ticket.verdict.as_deref(), Some("passed"));
        let unit = &ticket.units[0];
        assert_eq!(unit.status, verify_runs::STATUS_SKIPPED_CACHED);
        assert_eq!(unit.cached_from, Some(ran));
        let recorded = unit.run_id.unwrap();
        assert_ne!(recorded, ran);
        let status: String = sqlx::query_scalar("SELECT status FROM verify_runs WHERE id = ?")
            .bind(recorded)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(status, verify_runs::STATUS_SKIPPED_CACHED);
    }

    #[tokio::test]
    async fn a_failed_unit_is_never_cached() {
        let map = map_with("git definitely-not-a-git-subcommand", "");
        let f = fixture(Some(&map), None, true).await;
        let request = own_core(&f);

        let first = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, first).await;
        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        assert!(ticket.units[0].output_tail.is_some());
        let ran = ticket.units[0].run_id.unwrap();
        let_settle_decide().await;
        assert_eq!(cache_rows(&f.pool).await, 0);

        let second = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, second).await;
        assert_eq!(ticket.units[0].cached_from, None);
        assert_ne!(ticket.units[0].run_id, Some(ran));
        assert_eq!(ticket.verdict.as_deref(), Some("failed"));
    }

    /// Records that the post-merge gate found `alpha`'s target `main` red in `groups`, at `s1`.
    async fn target_red_in(pool: &SqlitePool, groups: &[&str]) {
        sqlx::query(
            "INSERT INTO postgate_state (project_id, target, red_groups, red_since_sha, red_sha) \
             VALUES ('alpha', 'main', ?, 's0', 's1')",
        )
        .bind(serde_json::to_string(groups).unwrap())
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_failure_in_a_group_red_on_the_target_is_annotated_and_stays_failed() {
        let map = map_with("git definitely-not-a-git-subcommand", "");
        let f = fixture(Some(&map), None, true).await;
        target_red_in(&f.pool, &["core"]).await;

        let id = submit(&f.ex, Caller::Owner, &own_core(&f)).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        assert_eq!(
            ticket.units[0].already_failing.as_deref(),
            Some("already failing on main@s1")
        );
    }

    #[tokio::test]
    async fn a_failure_in_a_group_the_target_is_not_red_in_is_not_annotated() {
        let map = map_with("git definitely-not-a-git-subcommand", "");
        let f = fixture(Some(&map), None, true).await;
        target_red_in(&f.pool, &["py"]).await;

        let id = submit(&f.ex, Caller::Owner, &own_core(&f)).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        assert_eq!(ticket.units[0].already_failing, None);
    }

    #[tokio::test]
    async fn without_a_postgate_row_a_failed_unit_serializes_with_no_annotation() {
        let map = map_with("git definitely-not-a-git-subcommand", "");
        let f = fixture(Some(&map), None, true).await;

        let id = submit(&f.ex, Caller::Owner, &own_core(&f)).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        let json = serde_json::to_value(&ticket.units[0]).unwrap();
        assert!(
            json.get("already_failing").is_none(),
            "the key must be absent: {json}"
        );
    }

    #[tokio::test]
    async fn a_failing_gate_command_is_annotated_when_the_target_gate_command_is_red() {
        let f = fixture(None, Some("git definitely-not-a-git-subcommand"), true).await;
        target_red_in(&f.pool, &["gate_command"]).await;

        let request = args(Kind::Test, ScopeArg::Scope, f.repo.path(), None, None);
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        assert_eq!(ticket.units[0].group, None);
        assert_eq!(
            ticket.units[0].already_failing.as_deref(),
            Some("already failing on main@s1")
        );
    }

    #[tokio::test]
    async fn a_group_with_cache_false_always_runs() {
        let map = map_with("git --version", "      cache: false\n");
        let f = fixture(Some(&map), None, true).await;
        let request = own_core(&f);

        let first = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, first).await;
        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");
        let ran = ticket.units[0].run_id.unwrap();
        let_settle_decide().await;
        assert_eq!(cache_rows(&f.pool).await, 0);

        let second = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, second).await;
        assert_eq!(ticket.units[0].status, verify_runs::STATUS_PASSED);
        assert_eq!(ticket.units[0].cached_from, None);
        assert_ne!(ticket.units[0].run_id, Some(ran));
    }

    #[tokio::test]
    async fn a_moved_fingerprint_is_returned_but_not_cached() {
        // The unit itself writes an untracked file the group reads, so the tree it ran over is
        // not the tree it was fingerprinted against.
        let map = map_with("git config --file core/moved.cfg a.b c", "");
        let f = fixture(Some(&map), None, true).await;

        let id = submit(&f.ex, Caller::Owner, &own_core(&f)).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;
        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");
        assert!(f.repo.path().join("core/moved.cfg").exists());
        let_settle_decide().await;
        assert_eq!(cache_rows(&f.pool).await, 0);
    }

    #[tokio::test]
    async fn wait_ticket_returns_a_running_ticket_at_the_deadline() {
        // No worker loop: the unit stays queued, so only the deadline can end the wait.
        let f = fixture(Some(&plain_map()), None, false).await;
        let id = submit(&f.ex, Caller::Owner, &own_core(&f)).await.unwrap();

        let limit = Duration::from_millis(600);
        let started = Instant::now();
        let (ticket, caller) = wait_ticket(&f.pool, id, limit).await.unwrap().unwrap();
        let waited = started.elapsed();

        assert!(waited >= limit, "returned after {waited:?}");
        assert!(waited < limit + Duration::from_secs(5), "took {waited:?}");
        assert!(!ticket.done);
        assert_eq!(ticket.verdict, None);
        assert_eq!(ticket.progress.total, 1);
        assert_eq!(ticket.progress.queued, 1);
        assert_eq!(ticket.progress.finished, 0);
        assert_eq!(caller, "owner");

        assert!(
            wait_ticket(&f.pool, id + 1000, limit)
                .await
                .unwrap()
                .is_none()
        );
    }

    async fn insert_run(pool: &SqlitePool, project: &str, cwd: Option<&str>) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, project_id, cwd, created_at) \
             VALUES ('x', 'running', 'worktree', ?, ?, '2026-10-06T00:00:00Z')",
        )
        .bind(project)
        .bind(cwd)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[tokio::test]
    async fn a_run_caller_is_held_to_its_own_worktree() {
        let f = fixture(Some(&plain_map()), None, false).await;
        let own = path_arg(f.repo.path());

        let run = insert_run(&f.pool, "alpha", Some(&own)).await;
        let (_, project) = resolve_worktree(&f.pool, Caller::Run(run), None, deadline())
            .await
            .unwrap();
        assert_eq!(project, "alpha");
        let (_, project) = resolve_worktree(&f.pool, Caller::Run(run), Some(&own), deadline())
            .await
            .unwrap();
        assert_eq!(project, "alpha");

        let other = tempfile::tempdir().unwrap();
        git_in(other.path(), &["init", "-q", "-b", "main"]);
        let error = resolve_worktree(
            &f.pool,
            Caller::Run(run),
            Some(&path_arg(other.path())),
            deadline(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, VerifyError::Forbidden(_)), "{error:?}");

        // A run of another project whose cwd lies in alpha's tree.
        let stray = insert_run(&f.pool, "beta", Some(&own)).await;
        let error = resolve_worktree(&f.pool, Caller::Run(stray), None, deadline())
            .await
            .unwrap_err();
        assert!(matches!(error, VerifyError::Forbidden(_)), "{error:?}");

        let homeless = insert_run(&f.pool, "alpha", None).await;
        let error = resolve_worktree(&f.pool, Caller::Run(homeless), None, deadline())
            .await
            .unwrap_err();
        assert!(matches!(error, VerifyError::Forbidden(_)), "{error:?}");

        let error = resolve_worktree(&f.pool, Caller::Run(999_999), None, deadline())
            .await
            .unwrap_err();
        assert!(matches!(error, VerifyError::Forbidden(_)), "{error:?}");

        // A run's ticket records the run as its caller, at autonomous priority.
        let request = VerifyArgs {
            worktree: None,
            ..own_core(&f)
        };
        let id = submit(&f.ex, Caller::Run(run), &request).await.unwrap();
        let (_, caller) = read_ticket(&f.pool, id).await.unwrap().unwrap();
        assert_eq!(caller, format!("run:{run}"));
        let priority: i64 = sqlx::query_scalar("SELECT priority FROM verify_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(priority, PRIORITY_AUTONOMOUS);
    }

    // F3-12 (decision 2): `verify_requests.measured_tree` is the daemon's record of the exact tree
    // a `test`/`scope` request measured, written only from a clean worktree whose tree and map did
    // not move while the ticket ran.

    /// The tree object of `dir`'s HEAD, as git names it.
    fn head_tree(dir: &Path) -> String {
        git_in(dir, &["rev-parse", "HEAD^{tree}"])
    }

    /// The recorded tree of request `id`, read once.
    async fn measured_once(pool: &SqlitePool, id: i64) -> Option<String> {
        verify_store::measured_tree(pool, id).await.unwrap()
    }

    /// Polls until request `id` records a tree, for at most `WAIT`; `None` when it never does.
    async fn wait_measured(pool: &SqlitePool, id: i64) -> Option<String> {
        let started = Instant::now();
        loop {
            if let Some(tree) = measured_once(pool, id).await {
                return Some(tree);
            }
            if started.elapsed() >= WAIT {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The ticket finished, `confirm_measured_tree` had time to decide, and nothing was recorded.
    async fn assert_nothing_recorded(pool: &SqlitePool, id: i64) {
        finished(pool, id).await;
        let_settle_decide().await;
        assert_eq!(measured_once(pool, id).await, None);
    }

    #[tokio::test]
    async fn a_clean_worktree_records_the_tree_its_scope_measured() {
        let f = fixture(Some(&plain_map()), Some("git --version"), true).await;

        let request = args(
            Kind::Test,
            ScopeArg::Scope,
            f.repo.path(),
            None,
            Some(&f.c1),
        );
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");
        assert_eq!(ticket.base.as_deref(), Some(f.c1.as_str()));
        assert_eq!(
            wait_measured(&f.pool, id).await,
            Some(head_tree(f.repo.path()))
        );
    }

    #[tokio::test]
    async fn a_dirty_worktree_records_no_tree() {
        let f = fixture(Some(&plain_map()), Some("git --version"), true).await;
        // Untracked, and not ignored: a file nobody has added is part of what would be measured.
        std::fs::write(f.repo.path().join("py/new.py"), "y = 2\n").unwrap();

        let request = args(
            Kind::Test,
            ScopeArg::Scope,
            f.repo.path(),
            None,
            Some(&f.c1),
        );
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();

        assert_nothing_recorded(&f.pool, id).await;
    }

    #[tokio::test]
    async fn a_worktree_that_moves_during_the_run_records_no_tree() {
        // No worker yet, so the ticket stays open while the tree changes underneath it.
        let f = fixture(Some(&plain_map()), Some("git --version"), false).await;
        let request = args(
            Kind::Test,
            ScopeArg::Scope,
            f.repo.path(),
            None,
            Some(&f.c1),
        );
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();

        let root = f.repo.path();
        std::fs::write(root.join("core/a.rs"), "fn a() { let _ = 2; }\n").unwrap();
        git_in(root, &["commit", "-q", "-am", "three"]);
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));

        assert_nothing_recorded(&f.pool, id).await;
    }

    #[tokio::test]
    async fn a_worktree_whose_map_differs_from_the_root_records_no_tree() {
        let f = fixture(Some(&plain_map()), Some("git --version"), true).await;
        let holder = tempfile::tempdir().unwrap();
        let side = holder.path().join("side");
        git_in(
            f.repo.path(),
            &["worktree", "add", "-q", "-b", "side", &path_arg(&side)],
        );
        // Still a valid map, but no longer the root's bytes.
        let map_path = side.join(tests_map::MAP_FILE);
        let mut map = std::fs::read_to_string(&map_path).unwrap();
        map.push_str("# changed in the worktree\n");
        std::fs::write(&map_path, map).unwrap();
        git_in(&side, &["commit", "-q", "-am", "map"]);

        let request = args(Kind::Test, ScopeArg::Scope, &side, None, Some(&f.c1));
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();

        assert_nothing_recorded(&f.pool, id).await;
    }

    #[tokio::test]
    async fn an_ignored_map_that_differs_at_submit_records_no_tree_even_if_restored() {
        // No worker yet, so the ticket stays open while the map is put back.
        let f = fixture(Some(&plain_map()), Some("git --version"), false).await;
        let holder = tempfile::tempdir().unwrap();
        let side = holder.path().join("side");
        git_in(
            f.repo.path(),
            &["worktree", "add", "-q", "-b", "side", &path_arg(&side)],
        );
        // The worktree gitignores its map: the file stays on disk, out of `git status`, so a clean
        // tree says nothing about the bytes it holds.
        git_in(&side, &["rm", "-q", "--cached", tests_map::MAP_FILE]);
        let ignore = format!("{}\n", tests_map::MAP_FILE);
        std::fs::write(side.join(".gitignore"), ignore).unwrap();
        git_in(&side, &["add", ".gitignore"]);
        git_in(&side, &["commit", "-q", "-m", "ignore the map"]);

        // Differs from the root's bytes at submit, still a valid map, and the tree stays clean.
        let map_path = side.join(tests_map::MAP_FILE);
        let root_map = std::fs::read(f.repo.path().join(tests_map::MAP_FILE)).unwrap();
        let mut narrowed = String::from_utf8(root_map.clone()).unwrap();
        narrowed.push_str("# changed in the worktree\n");
        std::fs::write(&map_path, narrowed).unwrap();

        let request = args(Kind::Test, ScopeArg::Scope, &side, None, Some(&f.c1));
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();

        // Restored to the root's bytes before the ticket completes.
        std::fs::write(&map_path, root_map).unwrap();
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));

        assert_nothing_recorded(&f.pool, id).await;
    }

    #[tokio::test]
    async fn only_a_test_scope_request_records_a_tree() {
        let f = fixture(Some(&plain_map()), Some("git --version"), true).await;

        let check = args(
            Kind::Check,
            ScopeArg::Scope,
            f.repo.path(),
            None,
            Some(&f.c1),
        );
        let id = submit(&f.ex, Caller::Owner, &check).await.unwrap();
        assert_nothing_recorded(&f.pool, id).await;

        // Own without files diffs from the base too, so only the scope differs from the case above.
        let own = args(Kind::Test, ScopeArg::Own, f.repo.path(), None, Some(&f.c1));
        let id = submit(&f.ex, Caller::Owner, &own).await.unwrap();
        assert_nothing_recorded(&f.pool, id).await;
    }

    // F3-13: `cover` asks the daemon to choose the base itself, from the post-merge gate's state,
    // so a green run over it can be taken as evidence for the tip it was measured against.

    /// Marks `sha` as the post-merge gate's last full green on `main`.
    async fn seed_green(pool: &SqlitePool, sha: &str) {
        verify_postgate::mark_covered(pool, "alpha", "main", sha)
            .await
            .unwrap();
    }

    /// A side worktree branched from `main` (so it descends from both `c1` and `c2`) that carries
    /// one committed `py/` change, so the diff from either is not empty. The tree is clean and its
    /// map is the root's.
    fn side_with_py_change(f: &Fixture, holder: &Path) -> PathBuf {
        let side = holder.join("side");
        git_in(
            f.repo.path(),
            &["worktree", "add", "-q", "-b", "side", &path_arg(&side)],
        );
        std::fs::write(side.join("py/b.py"), "x = 2\n").unwrap();
        git_in(&side, &["commit", "-q", "-am", "py change"]);
        side
    }

    /// The same request, asking the daemon to pick the base.
    fn covering(request: VerifyArgs) -> VerifyArgs {
        VerifyArgs {
            cover: true,
            ..request
        }
    }

    fn unprocessable(error: VerifyError) -> String {
        match error {
            VerifyError::Unprocessable(reason) => reason,
            other => panic!("expected Unprocessable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cover_request_takes_the_last_green_as_its_base() {
        let f = fixture(Some(&plain_map()), Some("git --version"), true).await;
        seed_green(&f.pool, &f.c1).await;
        let holder = tempfile::tempdir().unwrap();
        let side = side_with_py_change(&f, holder.path());

        let request = covering(args(Kind::Test, ScopeArg::Scope, &side, None, None));
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");
        assert_eq!(ticket.base.as_deref(), Some(f.c1.as_str()));
        let tree = wait_measured(&f.pool, id)
            .await
            .expect("a clean, trusted-map cover request records its tree");
        assert_eq!(tree, head_tree(&side));
        let evidence = covering_request(&f.pool, "alpha", &tree, &[f.c1.as_str()])
            .await
            .unwrap();
        assert_eq!(
            evidence,
            Some(Evidence {
                ticket: id,
                base: f.c1.clone()
            })
        );
    }

    #[tokio::test]
    async fn cover_request_prefers_the_covered_tip_over_the_older_green() {
        let f = fixture(Some(&plain_map()), Some("git --version"), true).await;
        seed_green(&f.pool, &f.c1).await;
        sqlx::query("UPDATE postgate_state SET covered_sha = ? WHERE project_id = 'alpha'")
            .bind(&f.c2)
            .execute(&f.pool)
            .await
            .unwrap();
        let holder = tempfile::tempdir().unwrap();
        let side = side_with_py_change(&f, holder.path());

        let request = covering(args(Kind::Test, ScopeArg::Scope, &side, None, None));
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;

        // `c1` is an ancestor of `c2`, so `c2` is the newer of the two proofs.
        assert_eq!(ticket.base.as_deref(), Some(f.c2.as_str()), "{ticket:?}");
        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");
    }

    #[tokio::test]
    async fn cover_request_refuses_a_shape_that_is_not_a_test_scope_without_base_or_files() {
        let f = fixture(Some(&plain_map()), Some("git --version"), false).await;
        seed_green(&f.pool, &f.c1).await;
        let root = f.repo.path();

        let wrong = [
            (
                "check kind",
                args(Kind::Check, ScopeArg::Scope, root, None, None),
            ),
            (
                "own scope",
                args(Kind::Test, ScopeArg::Own, root, None, None),
            ),
            (
                "a named base",
                args(Kind::Test, ScopeArg::Scope, root, None, Some(&f.c1)),
            ),
            (
                "named files",
                args(Kind::Test, ScopeArg::Scope, root, Some(&["py/b.py"]), None),
            ),
        ];
        for (what, request) in wrong {
            let error = submit(&f.ex, Caller::Owner, &covering(request))
                .await
                .unwrap_err();
            assert!(
                matches!(error, VerifyError::BadRequest(_)),
                "{what}: {error:?}"
            );
        }

        assert_eq!(count(&f.pool, "verify_requests").await, 0);
    }

    #[tokio::test]
    async fn cover_request_refuses_a_worktree_that_is_not_clean() {
        let f = fixture(Some(&plain_map()), Some("git --version"), false).await;
        seed_green(&f.pool, &f.c1).await;
        let holder = tempfile::tempdir().unwrap();
        let side = side_with_py_change(&f, holder.path());
        // Untracked and not ignored: part of what would be measured, in no commit.
        std::fs::write(side.join("py/new.py"), "y = 3\n").unwrap();

        let request = covering(args(Kind::Test, ScopeArg::Scope, &side, None, None));
        let error = submit(&f.ex, Caller::Owner, &request).await.unwrap_err();

        let reason = unprocessable(error);
        assert!(reason.contains("clean worktree"), "{reason}");
        assert_eq!(count(&f.pool, "verify_requests").await, 0);
    }

    #[tokio::test]
    async fn cover_request_refuses_when_the_daemon_has_no_base_to_offer() {
        let f = fixture(Some(&plain_map()), Some("git --version"), false).await;
        let holder = tempfile::tempdir().unwrap();
        let side = side_with_py_change(&f, holder.path());
        let request = covering(args(Kind::Test, ScopeArg::Scope, &side, None, None));

        // No post-merge row at all.
        let error = submit(&f.ex, Caller::Owner, &request).await.unwrap_err();
        let reason = unprocessable(error);
        assert!(reason.contains("holds no green"), "{reason}");

        // A green that is a commit made on the root's `main` after `side` branched: it is not in
        // the worktree's history, so it cannot be the base of the worktree's diff.
        git_in(
            f.repo.path(),
            &["commit", "-q", "--allow-empty", "-m", "three"],
        );
        let c3 = git_in(f.repo.path(), &["rev-parse", "HEAD"]);
        seed_green(&f.pool, &c3).await;
        let error = submit(&f.ex, Caller::Owner, &request).await.unwrap_err();
        let reason = unprocessable(error);
        assert!(reason.contains("main"), "names the branch: {reason}");

        // A gate that tracks another branch than the project's integration branch.
        verify_postgate::mark_covered(&f.pool, "alpha", "elsewhere", &f.c1)
            .await
            .unwrap();
        let error = submit(&f.ex, Caller::Owner, &request).await.unwrap_err();
        unprocessable(error);

        assert_eq!(count(&f.pool, "verify_requests").await, 0);
    }

    #[tokio::test]
    async fn cover_request_refuses_a_worktree_whose_map_is_not_the_roots() {
        let f = fixture(Some(&plain_map()), Some("git --version"), false).await;
        seed_green(&f.pool, &f.c1).await;
        let holder = tempfile::tempdir().unwrap();
        let side = side_with_py_change(&f, holder.path());
        // Still a valid map and a clean tree, but no longer the root's bytes.
        let map_path = side.join(tests_map::MAP_FILE);
        let mut map = std::fs::read_to_string(&map_path).unwrap();
        map.push_str("# changed in the worktree\n");
        std::fs::write(&map_path, map).unwrap();
        git_in(&side, &["commit", "-q", "-am", "map"]);

        let request = covering(args(Kind::Test, ScopeArg::Scope, &side, None, None));
        let error = submit(&f.ex, Caller::Owner, &request).await.unwrap_err();

        let reason = unprocessable(error);
        assert!(reason.contains("test map"), "{reason}");
        assert_eq!(count(&f.pool, "verify_requests").await, 0);
    }

    #[test]
    fn verify_args_default_cover_to_false_and_still_deny_unknown_fields() {
        let body = serde_json::json!({ "kind": "test", "scope": "scope" });
        let parsed = serde_json::from_value::<VerifyArgs>(body).unwrap();
        assert!(!parsed.cover);

        let body = serde_json::json!({ "kind": "test", "scope": "scope", "cover": true });
        assert!(serde_json::from_value::<VerifyArgs>(body).unwrap().cover);

        let body = serde_json::json!({ "kind": "test", "scope": "scope", "bogus": 1 });
        assert!(serde_json::from_value::<VerifyArgs>(body).is_err());
    }

    #[tokio::test]
    async fn a_waited_daemon_scope_records_its_tree_before_returning() {
        use crate::gate::GateOutcome;

        let f = fixture(Some(&plain_map()), Some("git --version"), true).await;
        // The daemon's own caller names no base; the worktree row supplies it.
        insert_job_worktree(&f, &f.c1).await;

        let verdict = gate_scope(&f.ex, Caller::Merge(1), f.repo.path(), WAIT).await;
        assert!(
            matches!(verdict, ScopeVerdict::Measured(GateOutcome::Passed)),
            "{verdict:?}"
        );

        // Read once, at once: the awaited confirmation has to have written it already.
        let id: i64 = sqlx::query_scalar("SELECT MAX(id) FROM verify_requests")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(
            measured_once(&f.pool, id).await,
            Some(head_tree(f.repo.path()))
        );
    }

    // F3-12 (decisions 4 and 9): a passed `test`/`scope` request covers the tree it recorded, but
    // only from a base the caller lists, and only when it ran something and nothing failed.

    #[tokio::test]
    async fn a_passed_scope_over_a_listed_base_covers_its_tree() {
        let f = fixture(Some(&plain_map()), Some("git --version"), true).await;
        let request = args(
            Kind::Test,
            ScopeArg::Scope,
            f.repo.path(),
            None,
            Some(&f.c1),
        );
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;
        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");
        let tree = wait_measured(&f.pool, id)
            .await
            .expect("a tree was recorded");

        let evidence = covering_request(&f.pool, "alpha", &tree, &[f.c1.as_str()])
            .await
            .unwrap()
            .expect("the passed scope covers its own tree");

        assert_eq!(evidence.ticket, id);
        assert_eq!(evidence.base, f.c1);
    }

    #[tokio::test]
    async fn a_passed_scope_over_another_base_covers_nothing() {
        let f = fixture(Some(&plain_map()), Some("git --version"), true).await;
        let request = args(
            Kind::Test,
            ScopeArg::Scope,
            f.repo.path(),
            None,
            Some(&f.c1),
        );
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        finished(&f.pool, id).await;
        let tree = wait_measured(&f.pool, id)
            .await
            .expect("a tree was recorded");

        let evidence = covering_request(&f.pool, "alpha", &tree, &[f.c2.as_str()])
            .await
            .unwrap();

        assert!(evidence.is_none(), "{evidence:?}");
    }

    #[tokio::test]
    async fn a_scope_that_ran_nothing_covers_nothing() {
        let f = fixture(Some(&plain_map()), Some("git --version"), true).await;
        // c2 is HEAD: an empty diff, so no group is selected.
        let request = args(
            Kind::Test,
            ScopeArg::Scope,
            f.repo.path(),
            None,
            Some(&f.c2),
        );
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;
        assert_eq!(ticket.verdict.as_deref(), Some("nothing_ran"), "{ticket:?}");
        // The tree is still recorded: what it must not do is cover.
        let tree = wait_measured(&f.pool, id)
            .await
            .expect("a tree was recorded");

        let evidence = covering_request(&f.pool, "alpha", &tree, &[f.c2.as_str()])
            .await
            .unwrap();

        assert!(evidence.is_none(), "{evidence:?}");
    }

    #[tokio::test]
    async fn a_failed_scope_covers_nothing() {
        let failing = "version: 1
tests:
  groups:
    core:
      paths: [core/]
      check: git --version
      command: git no-such-subcommand
    py:
      paths: [py/]
      check: git --version
      command: git --version
";
        let f = fixture(Some(failing), Some("git --version"), true).await;
        let request = args(
            Kind::Test,
            ScopeArg::Scope,
            f.repo.path(),
            None,
            Some(&f.c1),
        );
        let id = submit(&f.ex, Caller::Owner, &request).await.unwrap();
        let (ticket, _) = finished(&f.pool, id).await;
        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        let tree = wait_measured(&f.pool, id)
            .await
            .expect("a tree was recorded");

        let evidence = covering_request(&f.pool, "alpha", &tree, &[f.c1.as_str()])
            .await
            .unwrap();

        assert!(evidence.is_none(), "{evidence:?}");
    }

    // ---- task claims: a red that belongs to the master incident (chunk 4 Task 4) ----------------

    use crate::task_claims::TaskKey;

    /// A group named `group` that runs `script` through `sh -c`; the fixture's other group passes. The
    /// script sits in a YAML block scalar, which takes `: ` and quotes verbatim (a plain scalar would not).
    fn scripted_map(group: &str, script: &str) -> String {
        let (red_paths, ok_name, ok_paths) =
            if group == "core" { ("core/", "py", "py/") } else { ("py/", "core", "core/") };
        format!(
            "version: 1\ntests:\n  groups:\n    {group}:\n      paths: [{red_paths}]\n      check: git --version\n      command: |-\n        sh -c \"{script}\"\n    {ok_name}:\n      paths: [{ok_paths}]\n      check: git --version\n      command: git --version\n"
        )
    }

    /// What libtest prints for `ids` failing, then exit 1.
    fn failing_script(ids: &[&str]) -> String {
        let lines: String = ids.iter().map(|id| format!("test {id} ... FAILED\\n")).collect();
        format!(
            "printf 'running {n} tests\\n{lines}\\ntest result: FAILED. 0 passed; {n} failed; 0 ignored\\n'; exit 1",
            n = ids.len()
        )
    }

    fn red_map(group: &str, ids: &[&str]) -> String {
        scripted_map(group, &failing_script(ids))
    }

    /// `own_core` asks for `Kind::Check`, which runs the group's passing `check`; this runs its `command`.
    fn own_core_test(f: &Fixture) -> VerifyArgs {
        args(Kind::Test, ScopeArg::Own, f.repo.path(), Some(&["core/a.rs"]), None)
    }

    /// `finished`, but through `wait_ticket_with`: `installed()` is a global `OnceLock` the tests do not fill.
    async fn finished_with(f: &Fixture, id: i64) -> (Ticket, String) {
        let (ticket, caller) =
            wait_ticket_with(&f.pool, Some(&f.ex), id, WAIT, true).await.unwrap().unwrap();
        assert!(ticket.done, "ticket {id} did not finish in time: {ticket:?}");
        (ticket, caller)
    }

    /// A live `master-red`, written straight into the table the way the postgate leaves it.
    struct SeedIncident<'a> {
        status: &'a str,
        holder: Option<(&'a str, &'a str)>,
        red_tests: &'a [&'a str],
        red_groups: &'a [&'a str],
        since: Option<&'a str>,
        checked: Option<&'a str>,
    }

    impl Default for SeedIncident<'_> {
        fn default() -> Self {
            Self { status: "open", holder: None, red_tests: &[], red_groups: &[], since: None, checked: None }
        }
    }

    async fn seed_incident(pool: &SqlitePool, seed: SeedIncident<'_>) -> i64 {
        let detail = serde_json::json!({
            "red_tests": seed.red_tests,
            "red_groups": seed.red_groups,
            "red_since_sha": seed.since,
            "red_sha": seed.checked,
        });
        let lease = seed
            .holder
            .map(|_| crate::execution::wave::stamp(chrono::Utc::now() + chrono::Duration::hours(1)));
        sqlx::query(
            "INSERT INTO task_claims (project_id, task_key, kind, title, detail, guard_paths, hint_paths, status, \
             holder_kind, holder_id, holder_worktree, lease_until, checked_sha, created_at) \
             VALUES ('alpha', ?, 'incident', 'master red', ?, '[]', '[]', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(TaskKey::master_red().as_str())
        .bind(detail.to_string())
        .bind(seed.status)
        .bind(seed.holder.map(|h| h.0))
        .bind(seed.holder.map(|h| h.1))
        .bind(seed.holder.map(|h| format!("/wt/{}", h.1)))
        .bind(lease)
        .bind(seed.checked)
        .bind(crate::execution::wave::stamp(chrono::Utc::now()))
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn stored_inherited_ids(pool: &SqlitePool, id: i64) -> String {
        sqlx::query_scalar("SELECT inherited_task_ids FROM verify_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn red_tests_contained_in_the_incident_make_the_ticket_inherited_and_not_green() {
        let f = fixture(Some(&red_map("core", &["a::boom"])), None, true).await;
        let incident = seed_incident(
            &f.pool,
            SeedIncident {
                red_tests: &["a::boom", "a::bang"],
                red_groups: &["gate_command"],
                since: Some(&f.c1),
                checked: Some(&f.c2),
                ..Default::default()
            },
        )
        .await;

        let id = submit(&f.ex, Caller::Owner, &own_core_test(&f)).await.unwrap();
        let (ticket, _) = finished_with(&f, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("inherited"), "{ticket:?}");
        assert_eq!(ticket.units[0].status, "failed", "the unit itself stays failed");
        assert_eq!(ticket.units[0].failed_tests.as_deref(), Some(&["a::boom".to_owned()][..]));
        let note = ticket.units[0].inherited.as_deref().expect("the unit says why");
        assert!(note.contains(&format!("#{incident}")) && note.contains("do not fix"), "{note}");
        assert_eq!(ticket.inherited_task_ids, [incident]);
        assert_eq!(stored_inherited_ids(&f.pool, id).await, format!("[{incident}]"));
    }

    #[tokio::test]
    async fn the_same_group_failing_other_tests_stays_failed_and_unlisted() {
        let f = fixture(Some(&red_map("core", &["a::boom", "a::mine"])), None, true).await;
        seed_incident(
            &f.pool,
            SeedIncident {
                red_tests: &["a::boom"],
                red_groups: &["core"],
                since: Some(&f.c1),
                checked: Some(&f.c2),
                ..Default::default()
            },
        )
        .await;

        let id = submit(&f.ex, Caller::Owner, &own_core_test(&f)).await.unwrap();
        let (ticket, _) = finished_with(&f, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        assert!(ticket.inherited_task_ids.is_empty() && ticket.units[0].inherited.is_none());
        assert_eq!(stored_inherited_ids(&f.pool, id).await, "[]");
    }

    #[tokio::test]
    async fn a_branch_that_does_not_contain_red_since_sha_is_the_tasks_failure() {
        let f = fixture(Some(&red_map("core", &["a::boom"])), None, true).await;
        // A sha git has never heard of cannot be an ancestor of anything.
        let unknown = "0".repeat(40);
        seed_incident(
            &f.pool,
            SeedIncident {
                red_tests: &["a::boom"],
                red_groups: &["core"],
                since: Some(&unknown),
                checked: Some(&f.c2),
                ..Default::default()
            },
        )
        .await;

        let id = submit(&f.ex, Caller::Owner, &own_core_test(&f)).await.unwrap();
        let (ticket, _) = finished_with(&f, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        let note = ticket.units[0].not_inherited.as_deref().expect("the unit says why");
        assert!(note.contains("rebase"), "{note}");
        assert!(ticket.inherited_task_ids.is_empty());
    }

    #[tokio::test]
    async fn a_group_without_test_ids_is_inherited_at_group_level_when_untouched_listed_and_fresh() {
        let f = fixture(Some(&scripted_map("core", "exit 1")), None, true).await;
        let incident = seed_incident(
            &f.pool,
            SeedIncident { red_groups: &["core"], since: Some(&f.c1), checked: Some(&f.c2), ..Default::default() },
        )
        .await;

        // The root is clean against its own base, so the session changed nothing: `core` is suspect.
        let id = submit(&f.ex, Caller::Owner, &own_core_test(&f)).await.unwrap();
        let (ticket, _) = finished_with(&f, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("inherited"), "{ticket:?}");
        assert!(ticket.units[0].failed_tests.is_none());
        assert!(ticket.units[0].inherited.as_deref().unwrap().contains("no test ids"));
        assert_eq!(ticket.inherited_task_ids, [incident]);
    }

    #[tokio::test]
    async fn a_group_without_test_ids_is_the_tasks_failure_when_the_incident_lists_other_groups() {
        let f = fixture(Some(&scripted_map("core", "exit 1")), None, true).await;
        seed_incident(
            &f.pool,
            SeedIncident { red_groups: &["py"], since: Some(&f.c1), checked: Some(&f.c2), ..Default::default() },
        )
        .await;

        let id = submit(&f.ex, Caller::Owner, &own_core_test(&f)).await.unwrap();
        let (ticket, _) = finished_with(&f, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        assert!(ticket.inherited_task_ids.is_empty());
    }

    #[tokio::test]
    async fn a_final_classification_is_read_back_as_stored() {
        let f = fixture(Some(&red_map("core", &["a::boom"])), None, true).await;
        let incident = seed_incident(
            &f.pool,
            SeedIncident { red_tests: &["a::boom"], since: Some(&f.c1), checked: Some(&f.c2), ..Default::default() },
        )
        .await;
        let id = submit(&f.ex, Caller::Owner, &own_core_test(&f)).await.unwrap();
        let (first, _) = finished_with(&f, id).await;
        assert_eq!(first.verdict.as_deref(), Some("inherited"));

        // The incident closing later does not rewrite what this ticket was told; no git, no probe.
        sqlx::query("DELETE FROM task_claims WHERE id = ?").bind(incident).execute(&f.pool).await.unwrap();
        let (again, _) = finished_with(&f, id).await;

        assert_eq!(again.verdict.as_deref(), Some("inherited"));
        assert_eq!(again.inherited_task_ids, [incident]);
    }

    #[tokio::test]
    async fn daemon_callers_are_never_classified() {
        let f = fixture(Some(&red_map("core", &["a::boom"])), None, true).await;
        seed_incident(
            &f.pool,
            SeedIncident { red_tests: &["a::boom"], since: Some(&f.c1), checked: Some(&f.c2), ..Default::default() },
        )
        .await;

        let id = submit(&f.ex, Caller::Job(1), &own_core_test(&f)).await.unwrap();
        let (ticket, caller) = finished_with(&f, id).await;

        assert_eq!(caller, "job:1");
        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        assert!(ticket.inherited_task_ids.is_empty() && ticket.units[0].inherited.is_none());
        let stored: Option<String> = sqlx::query_scalar("SELECT classification FROM verify_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(stored, None, "nothing was written for a daemon caller");
    }

    #[test]
    fn scope_outcome_never_reads_an_inherited_verdict_as_a_pass() {
        use crate::gate::GateOutcome;
        let inherited =
            ticket(true, Some("inherited"), vec![unit(Some("py"), "failed", Some(1), Some("boom"))]);
        match scope_outcome(&inherited) {
            ScopeVerdict::Measured(GateOutcome::Errored { reason }) => {
                assert!(reason.contains("inherited"), "{reason}")
            }
            other => panic!("expected Errored, got {other:?}"),
        }
    }

    // ---- task claims: case 2, suspect groups and the single proof (chunk 4 Task 5) -----------------

    use crate::task_claims::Status as ClaimStatus;
    use crate::task_claims::store as claims;

    #[test]
    fn the_probe_caller_runs_at_postgate_priority_is_labelled_probe_and_no_key_can_claim_it() {
        assert_eq!(Caller::Probe.priority(), verify_runs::PRIORITY_POSTGATE);
        assert_eq!(Caller::Probe.label(), "probe");
        assert_eq!(Caller::Probe.requested_by("own"), verify_runs::REQUESTED_BY_PROBE);
        assert!(may_read(Caller::Probe, "probe") && !may_read(Caller::Probe, "owner"));
        assert!(may_read(Caller::Owner, "probe"), "the owner reads every ticket");
        assert_ne!(Caller::from_scope(&Scope::Control), Some(Caller::Probe));
        assert!(Caller::Probe.unit_timeout_ms() > 0);
        assert_eq!(Caller::Owner.unit_timeout_ms(), 0, "everyone else keeps the configured timeout");
    }

    /// `Caller::Probe` runs the named groups whole, never feeds nor reads the cache, never joins another
    /// run, and cannot be asked for by anyone else.
    #[tokio::test]
    async fn a_probe_request_runs_its_groups_whole_and_is_refused_to_anyone_else() {
        let f = fixture(Some(&plain_map()), None, true).await;
        let mut request = args(Kind::Test, ScopeArg::Own, f.repo.path(), None, None);
        request.groups = Some(vec!["core".to_owned()]);

        // Anyone but the daemon's probe is refused before a row exists.
        let refused = submit(&f.ex, Caller::Owner, &request).await.unwrap_err();
        assert!(matches!(refused, VerifyError::BadRequest(_)), "{refused:?}");
        assert_eq!(count(&f.pool, "verify_requests").await, 0);

        let id = submit(&f.ex, Caller::Probe, &request).await.unwrap();
        let (ticket, caller) = finished(&f.pool, id).await;
        assert_eq!(caller, "probe");
        assert_eq!(groups(&ticket), [Some("core".to_owned())]);
        assert_eq!(ticket.verdict.as_deref(), Some("passed"), "{ticket:?}");
        let_settle_decide().await; // `settle` would have written by now if a candidate had been queued
        assert_eq!(cache_rows(&f.pool).await, 0, "a probe never feeds the green cache");
        let (timeout_ms, requested_by): (i64, String) = sqlx::query_as(
            "SELECT timeout_ms, requested_by FROM verify_runs WHERE origin = 'verify' AND origin_id = ?",
        )
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(timeout_ms, Caller::Probe.unit_timeout_ms());
        assert_eq!(requested_by, verify_runs::REQUESTED_BY_PROBE);
    }

    // ---- case 2 end to end (spec section 5: "Verify") ------------------------------------------------

    /// Probe worktrees go under a space-free temp root, as the postgate worktree tests do; the env override
    /// is process-wide, hence the lock. Field order is drop order: env first, lock last.
    struct ProbeRoot {
        _env: crate::execution::git_exec::tests::WorktreeRootEnv,
        _dir: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    fn probe_root() -> ProbeRoot {
        let lock = crate::execution::worktree::test_env_lock();
        let dir = crate::execution::git_exec::tests::space_free_tempdir("nucleos-probe-");
        let env = crate::execution::git_exec::tests::WorktreeRootEnv::set(dir.path());
        ProbeRoot { _env: env, _dir: dir, _lock: lock }
    }

    /// A probe root that cannot hold a worktree: the override names a regular file.
    fn blocked_probe_root() -> ProbeRoot {
        let lock = crate::execution::worktree::test_env_lock();
        let dir = crate::execution::git_exec::tests::space_free_tempdir("nucleos-probe-");
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, "x").unwrap();
        let env = crate::execution::git_exec::tests::WorktreeRootEnv::set(&file);
        ProbeRoot { _env: env, _dir: dir, _lock: lock }
    }

    /// A linked worktree `name` of `f.repo`, branched from `main` (so it contains c1 and c2), that commits
    /// a change to `core/a.rs` only. The session "touched" `core` and never `py`.
    fn side_touching_core(f: &Fixture, holder: &Path, name: &str, content: &str) -> PathBuf {
        let side = holder.join(name);
        git_in(f.repo.path(), &["worktree", "add", "-q", "-b", name, &path_arg(&side)]);
        std::fs::write(side.join("core/a.rs"), content).unwrap();
        git_in(&side, &["commit", "-q", "-am", "core change"]);
        side
    }

    /// A scope request that also claims `py/b.py`, so `py` runs although the session never touched it.
    /// The diff-aware selection (diff only) does not pick `py`: that is what makes it suspect.
    fn scope_with_py(side: &Path) -> VerifyArgs {
        args(Kind::Test, ScopeArg::Scope, side, Some(&["py/b.py"]), None)
    }

    async fn probe_requests(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM verify_requests WHERE caller = 'probe'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn live_master_red(pool: &SqlitePool) -> Option<crate::task_claims::TaskRow> {
        claims::live_by_key(pool, "alpha", &TaskKey::master_red(), chrono::Utc::now()).await.unwrap()
    }

    /// Master gains `flag.txt`, which the sessions branched before it lack.
    fn master_gains_flag(f: &Fixture) -> String {
        std::fs::write(f.repo.path().join("flag.txt"), "x\n").unwrap();
        git_in(f.repo.path(), &["add", "flag.txt"]);
        git_in(f.repo.path(), &["commit", "-q", "-m", "three"]);
        git_in(f.repo.path(), &["rev-parse", "HEAD"])
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_suspect_group_without_an_incident_is_proven_on_master_and_the_incident_opens() {
        let _root = probe_root();
        let f = fixture(Some(&red_map("py", &["a::boom"])), None, true).await;
        let holder = tempfile::tempdir().unwrap();
        let side = side_touching_core(&f, holder.path(), "side", "fn a() { let _ = 2; }\n");

        let id = submit(&f.ex, Caller::Owner, &scope_with_py(&side)).await.unwrap();
        let (ticket, _) = finished_with(&f, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("inherited"), "{ticket:?}");
        let incident = live_master_red(&f.pool).await.expect("the proof opened the incident");
        assert_eq!(incident.status, ClaimStatus::Open);
        assert_eq!(incident.detail["red_tests"], serde_json::json!(["a::boom"]));
        assert_eq!(incident.detail["red_groups"], serde_json::json!(["py"]));
        assert_eq!(incident.detail["red_sha"], serde_json::json!(f.c2));
        assert_eq!(incident.detail["red_since_sha"], serde_json::json!(f.c2));
        assert_eq!(incident.checked_sha.as_deref(), Some(f.c2.as_str()));
        assert_eq!(ticket.inherited_task_ids, [incident.id]);
        assert_eq!(probe_requests(&f.pool).await, 1);
        let py = ticket.units.iter().find(|u| u.group.as_deref() == Some("py")).unwrap();
        assert!(py.inherited.is_some() && py.pending.is_none(), "{py:?}");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_suspect_group_that_passes_on_master_is_the_tasks_failure_and_one_pass_serves_the_next_ticket() {
        let _root = probe_root();
        let f = fixture(Some(&scripted_map("py", "test -f flag.txt || exit 1")), None, true).await;
        let holder = tempfile::tempdir().unwrap();
        let one = side_touching_core(&f, holder.path(), "one", "fn a() { let _ = 2; }\n");
        let two = side_touching_core(&f, holder.path(), "two", "fn a() { let _ = 3; }\n");
        master_gains_flag(&f); // py passes on master; the sessions branched before the flag

        let id = submit(&f.ex, Caller::Owner, &scope_with_py(&one)).await.unwrap();
        let (first, _) = finished_with(&f, id).await;
        assert_eq!(first.verdict.as_deref(), Some("failed"), "{first:?}");
        let py = first.units.iter().find(|u| u.group.as_deref() == Some("py")).unwrap();
        assert!(py.not_inherited.as_deref().unwrap().contains("passes on master"), "{py:?}");
        assert!(live_master_red(&f.pool).await.is_none(), "the probing row is gone");
        assert_eq!(count(&f.pool, "task_claims").await, 0);
        assert_eq!(probe_requests(&f.pool).await, 1);

        // A second session with the same suspect group, right after: the pass is remembered per master
        // head, so no second proof starts.
        let id = submit(&f.ex, Caller::Owner, &scope_with_py(&two)).await.unwrap();
        let (second, _) = finished_with(&f, id).await;
        assert_eq!(second.verdict.as_deref(), Some("failed"), "{second:?}");
        assert_eq!(probe_requests(&f.pool).await, 1, "one proof per master sha");
        // And a re-read of the first ticket is stable.
        let (again, _) = finished_with(&f, first.ticket).await;
        assert_eq!(again.verdict.as_deref(), Some("failed"));
        assert_eq!(probe_requests(&f.pool).await, 1);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn two_tickets_with_suspect_groups_share_one_probe() {
        let _root = probe_root();
        let f = fixture(Some(&red_map("py", &["a::boom"])), None, true).await;
        let holder = tempfile::tempdir().unwrap();
        let one = side_touching_core(&f, holder.path(), "one", "fn a() { let _ = 2; }\n");
        let two = side_touching_core(&f, holder.path(), "two", "fn a() { let _ = 3; }\n");
        let first = submit(&f.ex, Caller::Owner, &scope_with_py(&one)).await.unwrap();
        let second = submit(&f.ex, Caller::Owner, &scope_with_py(&two)).await.unwrap();

        // Both are read at once: one gets `Started`, the other finds the `probing` row and waits.
        let ((a, _), (b, _)) = tokio::join!(finished_with(&f, first), finished_with(&f, second));

        let incident = live_master_red(&f.pool).await.expect("incident");
        assert_eq!((a.verdict.as_deref(), b.verdict.as_deref()), (Some("inherited"), Some("inherited")));
        assert_eq!((a.inherited_task_ids, b.inherited_task_ids), (vec![incident.id], vec![incident.id]));
        assert_eq!(probe_requests(&f.pool).await, 1);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_probing_row_with_an_expired_lease_is_taken_over_and_proven_again() {
        let _root = probe_root();
        let f = fixture(Some(&red_map("py", &["a::boom"])), None, true).await;
        // A verify that crashed two hours ago left its `probing` row behind.
        let stale = match claims::begin_probe(
            &f.pool,
            "alpha",
            "999",
            "/gone",
            chrono::Utc::now() - chrono::Duration::hours(2),
        )
        .await
        .unwrap()
        {
            crate::task_claims::ProbeStart::Started(row) => row,
            other => panic!("{other:?}"),
        };
        let holder = tempfile::tempdir().unwrap();
        let side = side_touching_core(&f, holder.path(), "side", "fn a() { let _ = 2; }\n");

        let id = submit(&f.ex, Caller::Owner, &scope_with_py(&side)).await.unwrap();
        let (ticket, _) = finished_with(&f, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("inherited"), "{ticket:?}");
        let incident = live_master_red(&f.pool).await.expect("incident");
        assert_eq!(incident.id, stale.id, "the lapsed row was taken over, not duplicated");
        assert_eq!(incident.status, ClaimStatus::Open);
        assert_eq!(probe_requests(&f.pool).await, 1);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_probe_that_cannot_run_is_the_tasks_failure_and_is_not_repeated() {
        let _root = blocked_probe_root(); // the proof tree cannot be created
        let f = fixture(Some(&red_map("py", &["a::boom"])), None, true).await;
        let holder = tempfile::tempdir().unwrap();
        let side = side_touching_core(&f, holder.path(), "side", "fn a() { let _ = 2; }\n");

        let id = submit(&f.ex, Caller::Owner, &scope_with_py(&side)).await.unwrap();
        let (ticket, _) = finished_with(&f, id).await;

        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        let py = ticket.units.iter().find(|u| u.group.as_deref() == Some("py")).unwrap();
        assert!(py.not_inherited.as_deref().unwrap().contains("did not finish"), "{py:?}");
        assert!(live_master_red(&f.pool).await.is_none(), "an aborted proof leaves no probing row");
        assert_eq!(probe_requests(&f.pool).await, 0, "nothing was ever submitted");
        let (again, _) = finished_with(&f, id).await;
        assert_eq!(again.verdict.as_deref(), Some("failed"));
        assert_eq!(count(&f.pool, "task_claims").await, 0, "and a second read starts no second probe");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_land_evidence_read_never_starts_a_probe() {
        let _root = probe_root();
        let f = fixture(Some(&red_map("py", &["a::boom"])), None, true).await;
        let holder = tempfile::tempdir().unwrap();
        let side = side_touching_core(&f, holder.path(), "side", "fn a() { let _ = 2; }\n");
        let id = submit(&f.ex, Caller::Owner, &scope_with_py(&side)).await.unwrap();

        // `covering_request` and the access check read like this: no classification, no side effects.
        let (ticket, _) = wait_ticket_with(&f.pool, Some(&f.ex), id, WAIT, false).await.unwrap().unwrap();

        assert!(ticket.done);
        assert_eq!(ticket.verdict.as_deref(), Some("failed"), "{ticket:?}");
        assert!(ticket.inherited_task_ids.is_empty());
        assert_eq!(count(&f.pool, "task_claims").await, 0);
        assert_eq!(probe_requests(&f.pool).await, 0);
        let stored: Option<String> = sqlx::query_scalar("SELECT classification FROM verify_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(stored, None);
    }
}
