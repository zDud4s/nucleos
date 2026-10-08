//! `verify` / `verify_status`: the daemon verifies a worktree on a caller's behalf.
//!
//! Spec `2026-10-05-selecao-de-testes-design.md`, F2a-2. A caller names a kind (`check`/`test`)
//! and a scope (`own`/`scope`/`full`); this module resolves which worktree and base that means,
//! asks the pure planner (`verify_plan`) which units to run, answers what it can from the green
//! cache (`verify_store`), queues the rest on the executor (`verify_exec`), and hands back a
//! ticket. The ticket's live state is always derived from `verify_runs`, never stored, so a
//! status read can never disagree with what the executor actually did.

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
use crate::git_exec::{self, CommandResult, OPERATION_TIMEOUT};
use crate::inspect;
use crate::land;
use crate::state::AppState;
use crate::test_select;
use crate::tests_map::{self, Group, MapState};
use crate::vcs;
use crate::verify_exec::Executor;
use crate::verify_fingerprint;
use crate::verify_plan::{self, Kind, PlanError, ScopeArg, Ticket};
use crate::verify_runs::{self, ORIGIN_VERIFY, PRIORITY_AUTONOMOUS, PRIORITY_INTERACTIVE};
use crate::verify_store::{self, CacheKey, NewRequest, PlannedUnit};

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
pub(crate) enum Caller {
    Owner,
    Run(i64),
    /// The daemon's own item gate. `from_scope` never produces it, so no key can claim to be one.
    Job(i64),
}

impl Caller {
    /// Every other key is refused: a sidecar, a team run or a narrower API token has no worktree
    /// of its own to verify, and running arbitrary groups is not part of what those keys grant.
    pub(crate) fn from_scope(scope: &Scope) -> Option<Caller> {
        match scope {
            Scope::Control | Scope::ApiToken(ApiTokenLevel::Admin) => Some(Caller::Owner),
            Scope::Run(id) => Some(Caller::Run(*id)),
            _ => None,
        }
    }

    /// A person waiting at the keyboard goes ahead of an autonomous run (spec, "Prioridades").
    pub(crate) fn priority(self) -> i64 {
        match self {
            Caller::Owner => PRIORITY_INTERACTIVE,
            Caller::Run(_) | Caller::Job(_) => PRIORITY_AUTONOMOUS,
        }
    }

    /// How the request row records its caller, and what `may_read` compares against.
    pub(crate) fn label(self) -> String {
        match self {
            Caller::Owner => "owner".to_owned(),
            Caller::Run(id) => format!("run:{id}"),
            Caller::Job(id) => format!("job:{id}"),
        }
    }
}

/// A run reads only its own tickets: another run's output tail may carry what that run was
/// working on, and a ticket id is a guessable integer.
pub(crate) fn may_read(caller: Caller, row_caller: &str) -> bool {
    match caller {
        Caller::Owner => true,
        Caller::Run(_) | Caller::Job(_) => row_caller == caller.label(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum VerifyError {
    BadRequest(String),
    Forbidden(String),
    NotFound(String),
    Unprocessable(String),
    Unavailable(String),
    Internal(String),
}

impl VerifyError {
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            VerifyError::BadRequest(_) => StatusCode::BAD_REQUEST,
            VerifyError::Forbidden(_) => StatusCode::FORBIDDEN,
            VerifyError::NotFound(_) => StatusCode::NOT_FOUND,
            VerifyError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            VerifyError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            VerifyError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub(crate) fn message(&self) -> &str {
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
pub(crate) fn installed() -> Option<Arc<Executor>> {
    EXECUTOR.get().cloned()
}

/// One git read inside a request's budget.
///
/// Calls `run_git` directly, past `git_exec`'s list of sanctioned entries, for the reason
/// `map_stamp::digest` gives: this is not a queue operation and writes nothing. It keeps the
/// property that list protects anyway — what is left of `deadline` is computed here, and an
/// already-spent budget is refused before a child is spawned.
async fn git(repo: &Path, args: &[&str], deadline: Instant) -> Result<CommandResult, String> {
    let budget = deadline.saturating_duration_since(Instant::now());
    if budget.is_zero() {
        return Err("the request ran out of time before git could answer".to_owned());
    }
    let args: Vec<&OsStr> = args.iter().map(|arg| OsStr::new(*arg)).collect();
    git_exec::run_git(repo, &args, budget).await
}

/// Which worktree is verified, and the project it belongs to.
///
/// A job node verifies the worktree its run works in and nothing else: verification runs the
/// project's own commands, so naming another worktree would run them over code this run does not
/// own. The owner may name any worktree, but it has to be a registered worktree of a known
/// project — a path that merely holds a `.git` is not something the daemon has agreed to run.
pub(crate) async fn resolve_worktree(
    pool: &SqlitePool,
    caller: Caller,
    asked: Option<&str>,
    deadline: Instant,
) -> Result<(PathBuf, String), VerifyError> {
    let (root, run_project) = match caller {
        Caller::Run(id) => {
            let row: Option<(Option<String>, Option<String>)> =
                sqlx::query_as("SELECT project_id, cwd FROM runs WHERE id = ?")
                    .bind(id)
                    .fetch_optional(pool)
                    .await
                    .map_err(internal)?;
            let Some((project_id, Some(cwd))) = row else {
                return Err(VerifyError::Forbidden(format!(
                    "run {id} has no working directory to verify"
                )));
            };
            let own = git_exec::toplevel(Path::new(&cwd), deadline)
                .await
                .map_err(|error| {
                    VerifyError::Unprocessable(format!("{cwd} is not a git worktree: {error}"))
                })?;
            if let Some(asked) = asked {
                let own_key = git_exec::canonical(&own)
                    .await
                    .map_err(VerifyError::Unprocessable)?;
                // A path that cannot be canonicalised cannot be the run's own, which can.
                let asked_key = git_exec::canonical(Path::new(asked)).await.ok();
                if asked_key.as_deref() != Some(own_key.as_str()) {
                    return Err(VerifyError::Forbidden(format!(
                        "a job node verifies its own worktree, {}",
                        own.display()
                    )));
                }
            }
            (own, project_id)
        }
        // The daemon names the job's worktree itself, and it is held to the same registered-
        // worktree-of-a-known-project check as the owner's.
        Caller::Owner | Caller::Job(_) => {
            let Some(asked) = asked else {
                return Err(VerifyError::BadRequest("worktree is required".to_owned()));
            };
            // A relative path would be resolved against the daemon's own directory, which is
            // never what a caller in another directory meant.
            if !Path::new(asked).is_absolute() {
                return Err(VerifyError::BadRequest(format!(
                    "worktree must be an absolute path, not {asked}"
                )));
            }
            let root = git_exec::toplevel(Path::new(asked), deadline)
                .await
                .map_err(|error| {
                    VerifyError::Unprocessable(format!("{asked} is not a git worktree: {error}"))
                })?;
            (root, None)
        }
    };

    let project_id = vcs::project_for_worktree(pool, &root, deadline)
        .await
        .map_err(VerifyError::NotFound)?;
    if let Some(run_project) = run_project.as_deref()
        && run_project != project_id
    {
        return Err(VerifyError::Forbidden(format!(
            "this run belongs to {run_project}, but its worktree is in {project_id}"
        )));
    }
    let project_root = inspect::project_root(pool, &project_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            VerifyError::NotFound(format!("project {project_id} has no root recorded"))
        })?;

    let listed = git(
        Path::new(&project_root),
        &["worktree", "list", "--porcelain"],
        deadline,
    )
    .await
    .map_err(VerifyError::Unprocessable)?;
    if !listed.succeeded() {
        return Err(VerifyError::Unprocessable(format!(
            "could not list {project_id}'s worktrees: {}",
            listed.output_tail
        )));
    }
    let root_key = git_exec::canonical(&root)
        .await
        .map_err(VerifyError::Unprocessable)?;
    let mut registered = false;
    for path in listed
        .stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
    {
        // A listed worktree whose directory is gone cannot canonicalise; it is simply not ours.
        if git_exec::canonical(Path::new(path)).await.ok().as_deref() == Some(root_key.as_str()) {
            registered = true;
            break;
        }
    }
    if !registered {
        return Err(VerifyError::NotFound(format!(
            "{} is not a worktree of {project_id}",
            root.display()
        )));
    }
    Ok((root, project_id))
}

/// The full object id `rev` names in `repo`, or why git would not resolve it to a commit.
async fn resolve_commit(repo: &Path, rev: &str, deadline: Instant) -> Result<String, String> {
    let spec = format!("{rev}^{{commit}}");
    let resolved = git(repo, &["rev-parse", "--verify", "--quiet", &spec], deadline).await?;
    let sha = resolved.stdout.trim();
    if !resolved.succeeded() || sha.is_empty() {
        return Err(format!("{rev} is not a commit here"));
    }
    Ok(sha.to_owned())
}

/// The commit `own` and `scope` diff against.
///
/// In order: what the caller named; the base the daemon recorded when it created this worktree
/// (exact, survives the integration branch moving on); else the merge base with the project's
/// integration branch, which is what a hand-made worktree branched from.
pub(crate) async fn resolve_base(
    pool: &SqlitePool,
    project_id: &str,
    project_root: &Path,
    worktree_root: &Path,
    explicit: Option<&str>,
    deadline: Instant,
) -> Result<String, VerifyError> {
    if let Some(explicit) = explicit {
        // Hex only: whatever is passed here reaches git's argv, and a ref name could be an option.
        let well_formed =
            (7..=40).contains(&explicit.len()) && explicit.chars().all(|c| c.is_ascii_hexdigit());
        if !well_formed {
            return Err(VerifyError::BadRequest(format!(
                "base must be a commit id (7 to 40 hex digits), not {explicit}"
            )));
        }
        return resolve_commit(worktree_root, explicit, deadline)
            .await
            .map_err(VerifyError::Unprocessable);
    }

    let mut reasons = Vec::new();

    let recorded: Vec<(String, String)> = sqlx::query_as(
        "SELECT path, base_sha FROM worktrees WHERE project_id = ? AND removed_at IS NULL \
         AND base_sha IS NOT NULL ORDER BY created_at DESC",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(internal)?;
    let root_key = git_exec::canonical(worktree_root).await.ok();
    if let Some(root_key) = root_key.as_deref() {
        for (path, base_sha) in recorded {
            if git_exec::canonical(Path::new(&path)).await.ok().as_deref() == Some(root_key) {
                return Ok(base_sha);
            }
        }
    }
    reasons.push("the daemon recorded no base for this worktree".to_owned());

    match land::integration_branch(pool, project_id, project_root, deadline).await {
        Ok(branch) => {
            let target = format!("refs/heads/{}", branch.as_str());
            match git(worktree_root, &["merge-base", "HEAD", &target], deadline).await {
                Ok(result) if result.succeeded() && !result.stdout.trim().is_empty() => {
                    return Ok(result.stdout.trim().to_owned());
                }
                Ok(result) => reasons.push(format!(
                    "no merge base with {}: {}",
                    branch.as_str(),
                    result.output_tail.trim()
                )),
                Err(error) => reasons.push(error),
            }
        }
        Err(error) => reasons.push(error),
    }

    Err(VerifyError::Unprocessable(format!(
        "cannot resolve a base: {}",
        reasons.join("; ")
    )))
}

/// Splits git's `-z` output into its non-empty entries.
fn nul_separated(stdout: &str) -> impl Iterator<Item = &str> {
    stdout.split('\0').filter(|entry| !entry.is_empty())
}

/// Every path that differs from `base`: committed or not, and untracked files too, because a new
/// file nobody has added yet is still code the caller is about to hand in.
pub(crate) async fn changed_paths(
    worktree_root: &Path,
    base: &str,
    deadline: Instant,
) -> Result<Vec<String>, VerifyError> {
    let diff = git(
        worktree_root,
        &["diff", "--name-only", "-z", "--no-renames", base, "--"],
        deadline,
    )
    .await
    .map_err(VerifyError::Unprocessable)?;
    if !diff.succeeded() {
        return Err(VerifyError::Unprocessable(format!(
            "could not diff against {base}: {}",
            diff.output_tail
        )));
    }
    let untracked = git(
        worktree_root,
        &["ls-files", "-z", "--others", "--exclude-standard"],
        deadline,
    )
    .await
    .map_err(VerifyError::Unprocessable)?;
    if !untracked.succeeded() {
        return Err(VerifyError::Unprocessable(format!(
            "could not list untracked files: {}",
            untracked.output_tail
        )));
    }
    let paths: BTreeSet<String> = nul_separated(&diff.stdout)
        .chain(nul_separated(&untracked.stdout))
        .map(str::to_owned)
        .collect();
    Ok(paths.into_iter().collect())
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
pub(crate) async fn submit(
    executor: &Arc<Executor>,
    caller: Caller,
    args: &VerifyArgs,
) -> Result<i64, VerifyError> {
    let pool = &executor.pool;
    let deadline = Instant::now() + OPERATION_TIMEOUT;
    let (root, project_id) =
        resolve_worktree(pool, caller, args.worktree.as_deref(), deadline).await?;

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

    let own_files = args.scope == ScopeArg::Own && args.files.is_some();
    let needs_base =
        args.scope != ScopeArg::Full && matches!(map, MapState::Valid(_)) && !own_files;
    let base = if needs_base {
        let project_root = inspect::project_root(pool, &project_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| {
                VerifyError::NotFound(format!("project {project_id} has no root recorded"))
            })?;
        Some(
            resolve_base(
                pool,
                &project_id,
                Path::new(&project_root),
                &root,
                args.base.as_deref(),
                deadline,
            )
            .await?,
        )
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
    let plan = verify_plan::plan(
        &map,
        args.kind,
        args.scope,
        &changed,
        &fillable,
        rules.gate_command.as_deref(),
        true,
    )
    .map_err(|error| match error {
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
                requested_by: scope.to_owned(),
                group_name: unit.group.clone(),
                kind: Some(kind.to_owned()),
                argv: unit.argv.clone(),
                fingerprint: unit.fingerprint.clone(),
                priority,
                // Zero lets the executor derive both from the argv and its own config.
                weight: 0,
                timeout_ms: 0,
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

    Ok(id)
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
pub(crate) async fn read_ticket(
    pool: &SqlitePool,
    id: i64,
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
    let ticket = verify_plan::assemble(&request, &live);
    Ok(Some((ticket, request.caller)))
}

/// Re-reads the ticket until it is done or `deadline` has passed, whichever comes first.
pub(crate) async fn wait_ticket(
    pool: &SqlitePool,
    id: i64,
    deadline: Duration,
) -> sqlx::Result<Option<(Ticket, String)>> {
    let started = Instant::now();
    loop {
        let Some((ticket, caller)) = read_ticket(pool, id).await? else {
            return Ok(None);
        };
        if ticket.done || started.elapsed() >= deadline {
            return Ok(Some((ticket, caller)));
        }
        tokio::time::sleep(WAIT_POLL).await;
    }
}

/// What an item gate makes of a `scope` verification.
#[derive(Debug)]
pub(crate) enum ScopeVerdict {
    /// The ticket reached a verdict the gate can record.
    Measured(crate::gate::GateOutcome),
    /// No unit agreed with the work: nothing was measured, so there is nothing to bless.
    NothingRan,
}

/// Translates a ticket into the outcome an item gate records. A failed ticket carries the tail of
/// each failed unit, so the retry prompt still receives the end of the output as it does today.
pub(crate) fn scope_outcome(ticket: &Ticket) -> ScopeVerdict {
    use crate::gate::GateOutcome;

    if !ticket.done {
        return ScopeVerdict::Measured(GateOutcome::Errored {
            reason: "scope verification did not finish in time".to_owned(),
        });
    }
    match ticket.verdict.as_deref() {
        Some("passed") => ScopeVerdict::Measured(GateOutcome::Passed),
        Some("nothing_ran") => ScopeVerdict::NothingRan,
        Some("failed") => {
            let failed: Vec<&verify_plan::UnitReport> = ticket
                .units
                .iter()
                .filter(|unit| unit.status == "failed")
                .collect();
            let exit_code = failed
                .first()
                .and_then(|unit| unit.exit_code)
                .and_then(|code| i32::try_from(code).ok())
                .unwrap_or(1);
            let output = failed
                .iter()
                .map(|unit| {
                    let name = unit.group.clone().unwrap_or_else(|| unit.argv.join(" "));
                    format!(
                        "== {name} ({}) ==
{}",
                        unit.why,
                        unit.output_tail.as_deref().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            ScopeVerdict::Measured(GateOutcome::Failed { exit_code, output })
        }
        other => ScopeVerdict::Measured(GateOutcome::Errored {
            reason: format!(
                "scope verification ended {}",
                other.unwrap_or("without a verdict")
            ),
        }),
    }
}

/// Gates a job item over its worktree's diff from its recorded base: `scope` of kind `test`,
/// cached, at autonomous priority. A refusal or a ticket that does not finish within `wait` is an
/// `Errored` outcome, never a pass; the units it queued keep running and may still fill the cache.
pub(crate) async fn gate_job_scope(
    executor: &Arc<Executor>,
    job_id: i64,
    worktree: &Path,
    wait: Duration,
) -> ScopeVerdict {
    use crate::gate::GateOutcome;

    let args = VerifyArgs {
        kind: Kind::Test,
        scope: ScopeArg::Scope,
        worktree: Some(worktree.to_string_lossy().into_owned()),
        files: None,
        base: None,
        wait: false,
    };
    let id = match submit(executor, Caller::Job(job_id), &args).await {
        Ok(id) => id,
        Err(error) => {
            return ScopeVerdict::Measured(GateOutcome::Errored {
                reason: format!("scope verification was refused: {}", error.message()),
            });
        }
    };
    match wait_ticket(&executor.pool, id, wait).await {
        Ok(Some((ticket, _))) => scope_outcome(&ticket),
        Ok(None) => ScopeVerdict::Measured(GateOutcome::Errored {
            reason: format!("scope verification ticket {id} vanished"),
        }),
        Err(error) => ScopeVerdict::Measured(GateOutcome::Errored {
            reason: format!("could not read scope verification ticket {id}: {error}"),
        }),
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
    match read_ticket(&state.pool, args.ticket)
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
    use crate::verify_exec;
    use crate::verify_plan::NO_MAP_OWN_NOTE;
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
                dir.join(crate::project_state::AUTOPILOT_FILE),
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
}
