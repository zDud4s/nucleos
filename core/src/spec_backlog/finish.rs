//! Finishing the backlog: one tree per spec, in dependency order.
//!
//! The plan, in the order it runs: specs that are still `discovered` are extracted and audited
//! first (one may already be done); then each remaining spec gets a tree of its own, a dependency
//! before its dependents, up to `max_parallel` at once and never two together whose owned paths
//! overlap or are unknown; when a tree closes, the spec is audited, and a spec that is still not
//! done after `max_rounds` trees becomes `needs_owner`.
//!
//! In doubt, a spec is unfinished. A spec file is read, never written: this module has no code that
//! creates, changes or removes a file.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use rand::RngExt;
use sqlx::SqlitePool;

use super::audit::{self, AuditReport, SpecLineage, VerifyRunner};
use super::deps::{Cycle, topological_order};
use super::extract::{self, ExtractionRunner};
use super::model::{Criterion, Spec, SpecKind, SpecStatus};
use super::review::NoToolsReviewer;
#[cfg(test)]
use super::review::{
    REVIEW_FILE_CAP, ReviewFile, ReviewMaterial, gather_material, numbered_prefix, paths_named_by,
    review_prompt,
};
use super::scan::ScanRoots;
use super::store::{self, StoreError};
use super::verdict::{Verdict, VerifyState};
use crate::execution::runner::ToolPolicy;
use crate::state::AppState;
use crate::work_nodes::sets_overlap;

/// Trees at once when nothing says otherwise.
const DEFAULT_MAX_PARALLEL: usize = 2;
/// Two trees at most per spec, then the owner decides.
const DEFAULT_MAX_ROUNDS: u32 = 2;
const RUN_POLL: Duration = Duration::from_secs(1);
const RUN_CEILING: Duration = Duration::from_secs(30 * 60);
const TREE_POLL: Duration = Duration::from_secs(5);
const TREE_CEILING: Duration = Duration::from_secs(24 * 60 * 60);

/// Which specs to finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishScope {
    /// Every spec of one registry namespace (`None` is the namespace without a project).
    All { project_id: Option<String> },
    /// One spec and whatever it depends on that is not done yet.
    One { spec_id: String },
}

#[derive(Debug, Clone)]
pub struct FinishConfig {
    /// Trees running at once.
    pub max_parallel: usize,
    /// Trees per spec before it becomes `needs_owner`.
    pub max_rounds: u32,
    /// The branch no tree may be built over.
    pub integration_branch: String,
    pub project_root: String,
    /// Paths each spec writes, by spec id. `Spec` carries no such field, so the caller supplies
    /// them; a spec without an entry has an unknown scope and runs alone.
    pub owned_paths: BTreeMap<String, Vec<String>>,
    /// The checkout an audit resolves evidence against.
    pub worktree: PathBuf,
    pub state_dir: Option<PathBuf>,
}

/// One tree to run for one spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRequest {
    pub id: String,
    pub spec_id: String,
    pub goal: String,
    pub feature_branch: String,
    pub project_root: String,
    pub project_id: Option<String>,
    /// 1 for the first tree of the spec, 2 for the retry.
    pub round: u32,
}

/// What a closed tree leaves for the audit: the runs it made and their lineage roots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeOutcome {
    pub run_ids: Vec<String>,
    pub lineage_roots: Vec<String>,
}

/// Runs a tree to its end.
#[async_trait::async_trait]
pub trait TreeLauncher: Send + Sync {
    async fn run_tree(&self, request: &TreeRequest) -> Result<TreeOutcome, String>;
}

/// Extracts and audits a spec. Both write their result to the store.
#[async_trait::async_trait]
pub trait SpecPreparer: Send + Sync {
    async fn extract(&self, spec_id: &str) -> Result<(), String>;
    async fn audit(&self, spec_id: &str, lineage: &SpecLineage) -> Result<AuditReport, String>;
}

pub struct FinishDeps<'a> {
    pub launcher: &'a dyn TreeLauncher,
    pub preparer: &'a dyn SpecPreparer,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FinishReport {
    /// Ids of the trees started, in the order they started.
    pub trees: Vec<String>,
    pub done: Vec<String>,
    pub needs_owner: Vec<String>,
    /// Already done when finish began; touched by nothing.
    pub skipped: Vec<String>,
    /// Not launched because their tree would have been over the integration branch.
    pub refused: Vec<String>,
    /// Not launched because a dependency ended `needs_owner`, refused or blocked.
    pub blocked: Vec<String>,
    pub cycles: Vec<Cycle>,
}

#[derive(Debug)]
pub enum FinishError {
    Store(StoreError),
    /// Something finish needs to start is missing (a project, its root).
    Config(String),
}

impl fmt::Display for FinishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FinishError::Store(error) => write!(f, "{error}"),
            FinishError::Config(what) => write!(f, "cannot finish the backlog: {what}"),
        }
    }
}

impl std::error::Error for FinishError {}

impl From<StoreError> for FinishError {
    fn from(error: StoreError) -> Self {
        FinishError::Store(error)
    }
}

/// The branch a spec's trees are built on: `spec/<file stem>`, so never an integration branch
/// unless the owner named one `spec/...` on purpose (which `finish_with` then refuses).
pub fn derived_branch(spec_path: &str) -> String {
    let file = spec_path.rsplit(['/', '\\']).next().unwrap_or(spec_path);
    let stem = file.strip_suffix(".md").unwrap_or(file);
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("spec/{clean}")
}

// ----- the storage of extraction runs -----

/// Remembers that `run_id` extracted `spec_id`'s criteria. A repeat changes nothing.
pub async fn record_extraction_run(
    pool: &SqlitePool,
    spec_id: &str,
    run_id: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO spec_extraction_runs (spec_id, run_id, created_at) VALUES (?, ?, ?) \
         ON CONFLICT (spec_id, run_id) DO NOTHING",
    )
    .bind(spec_id)
    .bind(run_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// The runs that extracted the spec, oldest first.
pub async fn extraction_run_ids(
    pool: &SqlitePool,
    spec_id: &str,
) -> Result<Vec<String>, StoreError> {
    Ok(
        sqlx::query_scalar(
            "SELECT run_id FROM spec_extraction_runs WHERE spec_id = ? ORDER BY seq",
        )
        .bind(spec_id)
        .fetch_all(pool)
        .await?,
    )
}

/// Everything that took part in making the spec: its extraction runs and the given trees' runs.
/// An audit must come from none of them.
pub async fn lineage_for(
    pool: &SqlitePool,
    spec_id: &str,
    outcomes: &[TreeOutcome],
) -> Result<SpecLineage, StoreError> {
    let mut lineage = SpecLineage {
        run_ids: extraction_run_ids(pool, spec_id).await?,
        lineage_roots: Vec::new(),
    };
    for outcome in outcomes {
        lineage.run_ids.extend(outcome.run_ids.iter().cloned());
        lineage
            .lineage_roots
            .extend(outcome.lineage_roots.iter().cloned());
    }
    dedup_in_order(&mut lineage.run_ids);
    dedup_in_order(&mut lineage.lineage_roots);
    Ok(lineage)
}

fn dedup_in_order(items: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    items.retain(|item| seen.insert(item.clone()));
}

// ----- the plan -----

type Running<'a> =
    Vec<Pin<Box<dyn Future<Output = (String, Result<SpecRun, FinishError>)> + Send + 'a>>>;

/// How one spec's rounds ended.
struct SpecRun {
    trees: Vec<String>,
    finished: bool,
}

/// A spec waiting for its turn.
struct Waiting {
    spec: Spec,
    owned: Vec<String>,
    waits_on: Vec<String>,
}

/// Where a `discovered` spec stands after extraction and its first audit.
enum Prepared {
    Ready,
    Done,
    NeedsOwner,
    NotASpec,
}

/// Resolves the first of the futures that is ready, removing it. `None` when nothing runs.
async fn next_finished(
    running: &mut Running<'_>,
) -> Option<(String, Result<SpecRun, FinishError>)> {
    if running.is_empty() {
        return None;
    }
    std::future::poll_fn(|cx| {
        for index in 0..running.len() {
            if let Poll::Ready(output) = running[index].as_mut().poll(cx) {
                drop(running.swap_remove(index));
                return Poll::Ready(Some(output));
            }
        }
        Poll::Pending
    })
    .await
}

/// A spec may start when nothing runs, or when its paths are known and disjoint from every
/// running spec's paths.
fn compatible(owned: &[String], running: &[(String, Vec<String>)]) -> bool {
    running.is_empty()
        || (!owned.is_empty()
            && running
                .iter()
                .all(|(_, other)| !other.is_empty() && sets_overlap(owned, other).is_none()))
}

async fn load_scope(pool: &SqlitePool, scope: &FinishScope) -> Result<Vec<Spec>, StoreError> {
    match scope {
        FinishScope::All { project_id } => {
            store::list_specs_by_project(pool, project_id.as_deref()).await
        }
        FinishScope::One { spec_id } => {
            let mut specs: Vec<Spec> = Vec::new();
            let mut pending = vec![spec_id.clone()];
            let mut seen = BTreeSet::new();
            while let Some(id) = pending.pop() {
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Some(spec) = store::get_spec(pool, &id).await? else {
                    if id == *spec_id {
                        return Err(StoreError::NotFound(format!("spec {id}")));
                    }
                    continue;
                };
                if spec.status != SpecStatus::Done {
                    for dep in store::list_deps(pool, &id).await? {
                        pending.push(dep.depends_on);
                    }
                }
                specs.push(spec);
            }
            specs.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(specs)
        }
    }
}

async fn mark_needs_owner(
    pool: &SqlitePool,
    spec_id: &str,
    reason: &str,
) -> Result<(), StoreError> {
    store::set_status(pool, spec_id, SpecStatus::NeedsOwner, Some(reason)).await
}

/// Extracts a `discovered` spec and audits it before any tree is started.
async fn prepare(
    pool: &SqlitePool,
    preparer: &dyn SpecPreparer,
    spec: &Spec,
) -> Result<Prepared, FinishError> {
    if let Err(reason) = preparer.extract(&spec.id).await {
        mark_needs_owner(pool, &spec.id, &format!("extraction failed: {reason}")).await?;
        return Ok(Prepared::NeedsOwner);
    }
    let Some(extracted) = store::get_spec(pool, &spec.id).await? else {
        return Err(StoreError::NotFound(format!("spec {}", spec.id)).into());
    };
    if matches!(extracted.kind, SpecKind::Review | SpecKind::Note) {
        return Ok(Prepared::NotASpec);
    }
    if extracted.status == SpecStatus::NeedsOwner {
        return Ok(Prepared::NeedsOwner);
    }
    let lineage = lineage_for(pool, &spec.id, &[]).await?;
    // An audit that cannot run is not a verdict: the spec still gets its tree, and the audit after
    // the tree decides.
    Ok(match preparer.audit(&spec.id, &lineage).await {
        Ok(report) if report.status == SpecStatus::Done && report.verdict == Verdict::Done => {
            Prepared::Done
        }
        _ => Prepared::Ready,
    })
}

async fn goal_for(
    pool: &SqlitePool,
    spec: &Spec,
    round: u32,
    previous: Option<&str>,
) -> Result<String, StoreError> {
    let criteria = store::list_criteria(pool, &spec.id).await?;
    let mut goal = format!(
        "Finish the spec at {} ({}). Make every one of its acceptance criteria true in the code, \
         with tests, on this branch. The spec file itself is read-only: never edit, move or \
         delete it.\n\nCriteria:\n",
        spec.path, spec.title
    );
    for criterion in &criteria {
        goal.push_str(&format!(
            "- [{}] {}\n",
            criterion.status.as_str(),
            criterion.text
        ));
    }
    if let (true, Some(reason)) = (round > 1, previous) {
        goal.push_str(&format!(
            "\nRound {round}: the previous tree did not finish the spec ({reason}). Address what \
             it left open.\n"
        ));
    }
    Ok(goal)
}

/// One spec, start to end: up to `max_rounds` trees, each followed by an audit.
async fn drive_spec(
    pool: &SqlitePool,
    launcher: &dyn TreeLauncher,
    preparer: &dyn SpecPreparer,
    config: &FinishConfig,
    spec: Spec,
    nonce: String,
) -> Result<SpecRun, FinishError> {
    let branch = derived_branch(&spec.path);
    let short: String = spec.id.chars().take(12).collect();
    let mut outcomes: Vec<TreeOutcome> = Vec::new();
    let mut trees = Vec::new();
    let mut last: Option<String> = None;
    for round in 1..=config.max_rounds {
        let request = TreeRequest {
            // A new id per round, or the second launch would replay the first tree.
            id: format!("spec-{short}-r{round}-{nonce}"),
            spec_id: spec.id.clone(),
            goal: goal_for(pool, &spec, round, last.as_deref()).await?,
            feature_branch: branch.clone(),
            project_root: config.project_root.clone(),
            project_id: spec.project_id.clone(),
            round,
        };
        store::set_status(pool, &spec.id, SpecStatus::InProgress, None).await?;
        store::set_tree_id(pool, &spec.id, &request.id).await?;
        trees.push(request.id.clone());
        match launcher.run_tree(&request).await {
            Ok(outcome) => outcomes.push(outcome),
            Err(reason) => {
                last = Some(format!("round {round}: the tree failed: {reason}"));
                continue;
            }
        }
        let lineage = lineage_for(pool, &spec.id, &outcomes).await?;
        match preparer.audit(&spec.id, &lineage).await {
            Ok(report) if report.status == SpecStatus::Done && report.verdict == Verdict::Done => {
                return Ok(SpecRun {
                    trees,
                    finished: true,
                });
            }
            Ok(report) => {
                last = Some(format!(
                    "round {round}: the audit says {:?}",
                    report.verdict
                ))
            }
            Err(reason) => last = Some(format!("round {round}: the audit failed: {reason}")),
        }
    }
    let why = last.unwrap_or_else(|| "no round was allowed".to_owned());
    let reason = format!("not done after {} tree(s); {why}", trees.len());
    mark_needs_owner(pool, &spec.id, &reason).await?;
    store::add_question(
        pool,
        &spec.id,
        &format!(
            "{} is still not done after {} tree(s) ({why}). What should happen next?",
            spec.path,
            trees.len()
        ),
    )
    .await?;
    Ok(SpecRun {
        trees,
        finished: false,
    })
}

fn fresh_nonce() -> String {
    rand::rng()
        .random::<[u8; 4]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Finishes the backlog through `deps`. See the module doc for the order of things.
pub async fn finish_with<'a>(
    pool: &'a SqlitePool,
    deps: &FinishDeps<'a>,
    config: &'a FinishConfig,
    scope: FinishScope,
) -> Result<FinishReport, FinishError> {
    let mut report = FinishReport::default();
    let mut done_ids: BTreeSet<String> = BTreeSet::new();
    // Specs that will not finish in this run; whatever waits on one is blocked.
    let mut stopped: BTreeSet<String> = BTreeSet::new();

    // 1. Sort what is in scope: done ones are left alone, the rest is prepared.
    let mut runnable: Vec<Spec> = Vec::new();
    for spec in load_scope(pool, &scope).await? {
        if spec.status == SpecStatus::Done {
            done_ids.insert(spec.id.clone());
            report.skipped.push(spec.id);
            continue;
        }
        if spec.status == SpecStatus::Abandoned
            || matches!(spec.kind, SpecKind::Review | SpecKind::Note)
        {
            continue;
        }
        match spec.status {
            SpecStatus::NeedsOwner => {
                stopped.insert(spec.id.clone());
                report.needs_owner.push(spec.id);
            }
            SpecStatus::Discovered | SpecStatus::Extracting => {
                match prepare(pool, deps.preparer, &spec).await? {
                    Prepared::Ready => {
                        let fresh = store::get_spec(pool, &spec.id).await?.unwrap_or(spec);
                        runnable.push(fresh);
                    }
                    Prepared::Done => {
                        done_ids.insert(spec.id.clone());
                        report.done.push(spec.id);
                    }
                    Prepared::NeedsOwner => {
                        stopped.insert(spec.id.clone());
                        report.needs_owner.push(spec.id);
                    }
                    Prepared::NotASpec => {}
                }
            }
            _ => runnable.push(spec),
        }
    }

    // 2. Never a tree over the integration branch.
    let integration = config.integration_branch.trim();
    runnable.retain(|spec| {
        if derived_branch(&spec.path) == integration {
            stopped.insert(spec.id.clone());
            report.refused.push(spec.id.clone());
            false
        } else {
            true
        }
    });

    // 3. Order by dependency; a loop goes to the owner and takes its members out of the plan.
    let mut pairs: Vec<(String, String)> = Vec::new();
    for spec in &runnable {
        for dep in store::list_deps(pool, &spec.id).await? {
            pairs.push((dep.spec_id, dep.depends_on));
        }
    }
    let order = loop {
        let ids: Vec<String> = runnable.iter().map(|spec| spec.id.clone()).collect();
        match topological_order(&ids, &pairs) {
            Ok(order) => break order,
            Err(cycle) => {
                for member in &cycle.specs {
                    mark_needs_owner(pool, member, "part of a dependency loop").await?;
                    stopped.insert(member.clone());
                    report.needs_owner.push(member.clone());
                }
                if let Some(first) = cycle.specs.first() {
                    store::add_question(pool, first, &cycle.owner_question()).await?;
                }
                runnable.retain(|spec| !cycle.specs.contains(&spec.id));
                report.cycles.push(cycle);
            }
        }
    };

    // 4. A dependency outside the plan is satisfied only when it is done.
    let planned: BTreeSet<String> = runnable.iter().map(|spec| spec.id.clone()).collect();
    for (_, target) in &pairs {
        if planned.contains(target) || done_ids.contains(target) || stopped.contains(target) {
            continue;
        }
        match store::get_spec(pool, target).await? {
            Some(spec) if spec.status == SpecStatus::Done => {
                done_ids.insert(target.clone());
            }
            _ => {
                stopped.insert(target.clone());
            }
        }
    }

    // 5. Run them.
    let mut queue: Vec<Waiting> = Vec::new();
    for id in &order {
        let Some(position) = runnable.iter().position(|spec| spec.id == *id) else {
            continue;
        };
        let spec = runnable.swap_remove(position);
        let waits_on = pairs
            .iter()
            .filter(|(from, _)| *from == spec.id)
            .map(|(_, to)| to.clone())
            .collect();
        queue.push(Waiting {
            owned: config
                .owned_paths
                .get(&spec.id)
                .cloned()
                .unwrap_or_default(),
            spec,
            waits_on,
        });
    }
    let budget = config.max_parallel.max(1);
    let mut running: Running<'a> = Vec::new();
    let mut running_paths: Vec<(String, Vec<String>)> = Vec::new();
    loop {
        // What waits on something that will not finish is blocked, and so is what waits on that.
        while let Some(index) = queue
            .iter()
            .position(|waiting| waiting.waits_on.iter().any(|dep| stopped.contains(dep)))
        {
            let blocked = queue.remove(index);
            let dep = blocked
                .waits_on
                .iter()
                .find(|dep| stopped.contains(*dep))
                .cloned()
                .unwrap_or_default();
            store::set_status(
                pool,
                &blocked.spec.id,
                SpecStatus::Blocked,
                Some(&format!("waiting on {dep}, which did not finish")),
            )
            .await?;
            stopped.insert(blocked.spec.id.clone());
            report.blocked.push(blocked.spec.id);
        }
        let mut index = 0;
        while running.len() < budget && index < queue.len() {
            let ready = queue[index]
                .waits_on
                .iter()
                .all(|dep| done_ids.contains(dep));
            if ready && compatible(&queue[index].owned, &running_paths) {
                let Waiting { spec, owned, .. } = queue.remove(index);
                let id = spec.id.clone();
                running_paths.push((id.clone(), owned));
                let (launcher, preparer) = (deps.launcher, deps.preparer);
                let nonce = fresh_nonce();
                running.push(Box::pin(async move {
                    let outcome = drive_spec(pool, launcher, preparer, config, spec, nonce).await;
                    (id, outcome)
                }));
            } else {
                index += 1;
            }
        }
        let Some((id, outcome)) = next_finished(&mut running).await else {
            break;
        };
        running_paths.retain(|(running_id, _)| *running_id != id);
        let outcome = outcome?;
        report.trees.extend(outcome.trees);
        if outcome.finished {
            done_ids.insert(id.clone());
            report.done.push(id);
        } else {
            stopped.insert(id.clone());
            report.needs_owner.push(id);
        }
    }
    // Nothing runs and these were never ready: they wait on something that is neither done nor
    // stopped. Unfinished, and said so.
    for left in queue {
        store::set_status(
            pool,
            &left.spec.id,
            SpecStatus::Blocked,
            Some("a dependency was never resolved"),
        )
        .await?;
        report.blocked.push(left.spec.id);
    }

    for list in [
        &mut report.done,
        &mut report.needs_owner,
        &mut report.skipped,
        &mut report.refused,
        &mut report.blocked,
    ] {
        list.sort();
        list.dedup();
    }
    Ok(report)
}

// ----- the production wiring -----

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

/// What the real reviewer and the extraction agent are started with: the CLI's own refusal of
/// every tool (`ToolPolicy::None`), nobody able to talk to the run, and the final text awaited.
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

/// Starts the run through `create_no_tools_run` (the policy lands in the run itself) and waits for
/// it to end. Returns the run id and its final text.
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

/// The extraction agent, recording each run it makes against the spec.
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

/// The daemon's own `verify scope` gate over the audit worktree (the spec's feature branch), the
/// same call that gates a tree branch. Only a passed ticket is green and only a failed one is red;
/// no executor, nothing measured, a refused submit or a timeout is unknown, which
/// `verdict::done` never reads as done.
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
        let verdict = crate::verify::gate_scope(
            &executor,
            crate::verify::Caller::Job(self.caller_id),
            &self.worktree,
            crate::state::DEFAULT_GATE_TIMEOUT,
        )
        .await;
        match verdict {
            ScopeVerdict::Measured(GateOutcome::Passed) => VerifyState::Green,
            ScopeVerdict::Measured(GateOutcome::Failed { .. }) => VerifyState::Red,
            _ => VerifyState::Unknown,
        }
    }
}

/// The checkout an audit reads: a detached worktree of the spec's feature branch under the
/// project's worktree root, made on first use and moved to the branch tip on later ones. With no
/// feature branch yet (nothing was built), or when the checkout cannot be made, the project root:
/// the spec then shows only what the root already holds, which errs towards unfinished.
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
    /// `<machine root>/projects/<id>`, where the project's own specs live; `None` when the machine
    /// has no config root.
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
        // A spec the agent could not make sense of is stored as `needs_owner` by `extract` itself,
        // which `finish_with` reads back; only a failure to extract at all is an error here.
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
        // The tree's work is on the spec's feature branch, not in the project root.
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

/// Starts a tree through the `POST /trees` handler itself (in process, same body), then waits for
/// the job to reach its terminal state and collects the runs of its nodes.
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
        let body = serde_json::json!({
            "id": request.id,
            "goal": request.goal,
            "feature_branch": request.feature_branch,
            "project_root": request.project_root,
            "project_id": project_id,
        });
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

        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT id, COALESCE(lineage_root_id, id) FROM runs WHERE id IN \
             (SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE root_id = ? AND run_id IS NOT NULL)",
        )
        .bind(&root_node_id)
        .fetch_all(&self.state.pool)
        .await
        .map_err(|error| error.to_string())?;
        let mut outcome = TreeOutcome::default();
        for (run, root) in rows {
            outcome.run_ids.push(run.to_string());
            outcome.lineage_roots.push(root.to_string());
        }
        dedup_in_order(&mut outcome.lineage_roots);
        Ok(outcome)
    }
}

/// "Finish everything": reads the project's root and integration branch from the daemon's own
/// state and runs the plan against the real tree launcher, extraction agent and reviewer.
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
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT project_root, integration_branch FROM autopilot_state WHERE project_id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(StoreError::from)?;
    let (root, branch) = row.unwrap_or((None, None));
    let project_root = root
        .filter(|root| !root.trim().is_empty())
        .ok_or_else(|| FinishError::Config(format!("project {project_id} has no recorded root")))?;
    let config = FinishConfig {
        max_parallel: DEFAULT_MAX_PARALLEL,
        max_rounds: DEFAULT_MAX_ROUNDS,
        integration_branch: branch
            .filter(|branch| !branch.trim().is_empty())
            .unwrap_or_else(|| "master".to_owned()),
        owned_paths: BTreeMap::new(),
        worktree: PathBuf::from(&project_root),
        state_dir: None,
        project_root,
    };
    let launcher = DaemonLauncher { state };
    // Specs under `<state_dir>/specs` live in the project's state directory, resolved as the
    // HTTP routes do: the machine config root plus the project id.
    let state_dir = state
        .machine_config_root
        .as_deref()
        .and_then(|root| crate::project::state::dir(root, &project_id));
    let preparer = DaemonPreparer {
        state,
        project_root: PathBuf::from(&config.project_root),
        integration_branch: config.integration_branch.clone(),
        state_dir,
    };
    let deps = FinishDeps {
        launcher: &launcher,
        preparer: &preparer,
    };
    finish_with(&state.pool, &deps, &config, scope).await
}

/// Held while a finish runs for a project; released on drop.
pub struct FinishClaim;

/// Claims a project for one finish; `None` when one is already running.
pub fn claim_project(_project_id: &str) -> Option<FinishClaim> {
    Some(FinishClaim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runner::ToolPolicy;
    use crate::spec_backlog::audit::{AuditReport, SpecLineage};
    use crate::spec_backlog::model::{
        CriterionKind, CriterionStatus, DepSource, NewCriterion, SpecKind, SpecStatus, spec_id,
    };
    use crate::spec_backlog::store;
    use crate::spec_backlog::verdict::{NotDoneReason, Verdict, VerifyState};
    use sqlx::SqlitePool;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const SHA: &str = "sha-1";

    fn path_of(name: &str) -> String {
        format!(".ai/specs/2026-01-01-{name}.md")
    }

    /// Registers a spec in `status` and returns its id.
    async fn add_spec(pool: &SqlitePool, name: &str, status: SpecStatus) -> String {
        let path = path_of(name);
        let spec = store::upsert_by_path(pool, None, &path, name, SHA)
            .await
            .unwrap();
        if status == SpecStatus::Done {
            store::set_audit_result(pool, &spec.id, SpecStatus::Done, None, SHA)
                .await
                .unwrap();
        } else if status != SpecStatus::Discovered {
            store::set_extraction_result(pool, &spec.id, SpecKind::Spec, status, None)
                .await
                .unwrap();
        }
        spec.id
    }

    fn cfg() -> FinishConfig {
        FinishConfig {
            max_parallel: 2,
            max_rounds: 2,
            integration_branch: "master".to_owned(),
            project_root: "/repo".to_owned(),
            state_dir: None,
            owned_paths: BTreeMap::new(),
            worktree: std::env::temp_dir(),
        }
    }

    /// Gives every listed spec its own, disjoint owned path.
    fn disjoint(ids: &[&String]) -> BTreeMap<String, Vec<String>> {
        ids.iter()
            .enumerate()
            .map(|(n, id)| ((*id).clone(), vec![format!("core/src/area{n}/")]))
            .collect()
    }

    type Log = Arc<Mutex<Vec<String>>>;

    struct FakeLauncher {
        log: Log,
        requests: Mutex<Vec<TreeRequest>>,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        delay: Duration,
    }

    impl FakeLauncher {
        fn new(log: &Log) -> Self {
            Self {
                log: log.clone(),
                requests: Mutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                delay: Duration::from_millis(40),
            }
        }

        fn requests(&self) -> Vec<TreeRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl TreeLauncher for FakeLauncher {
        async fn run_tree(&self, request: &TreeRequest) -> Result<TreeOutcome, String> {
            self.requests.lock().unwrap().push(request.clone());
            self.log
                .lock()
                .unwrap()
                .push(format!("tree:{}", request.spec_id));
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(TreeOutcome {
                run_ids: vec![format!("run-{}-r{}", request.spec_id, request.round)],
                lineage_roots: vec![format!("root-{}", request.id)],
            })
        }
    }

    /// Extract and audit stand-ins that write to the store like the real ones would.
    struct FakePreparer {
        pool: SqlitePool,
        log: Log,
        /// Specs whose audit ends Done; every other audit ends Audited.
        passes: Vec<String>,
        lineages: Mutex<Vec<(String, SpecLineage)>>,
    }

    impl FakePreparer {
        fn new(pool: &SqlitePool, log: &Log, passes: &[&String]) -> Self {
            Self {
                pool: pool.clone(),
                log: log.clone(),
                passes: passes.iter().map(|id| (*id).clone()).collect(),
                lineages: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl SpecPreparer for FakePreparer {
        async fn extract(&self, spec_id: &str) -> Result<(), String> {
            self.log.lock().unwrap().push(format!("extract:{spec_id}"));
            let item = NewCriterion {
                text: "it works".to_owned(),
                kind: CriterionKind::Behavior,
                verify_hint: None,
                status: CriterionStatus::Open,
            };
            store::replace_criteria(&self.pool, spec_id, SHA, &[item])
                .await
                .unwrap();
            store::set_extraction_result(
                &self.pool,
                spec_id,
                SpecKind::Spec,
                SpecStatus::Extracted,
                None,
            )
            .await
            .unwrap();
            Ok(())
        }

        async fn audit(&self, spec_id: &str, lineage: &SpecLineage) -> Result<AuditReport, String> {
            self.log.lock().unwrap().push(format!("audit:{spec_id}"));
            self.lineages
                .lock()
                .unwrap()
                .push((spec_id.to_owned(), lineage.clone()));
            let done = self.passes.iter().any(|id| id == spec_id);
            let status = if done {
                SpecStatus::Done
            } else {
                SpecStatus::Audited
            };
            store::set_audit_result(&self.pool, spec_id, status, Some("fake audit"), SHA)
                .await
                .unwrap();
            Ok(AuditReport {
                reviewer_run_id: "reviewer-1".to_owned(),
                verify: if done {
                    VerifyState::Green
                } else {
                    VerifyState::Red
                },
                verdict: if done {
                    Verdict::Done
                } else {
                    Verdict::NotDone(vec![NotDoneReason::VerifyNotGreen])
                },
                status,
            })
        }
    }

    fn scope_all() -> FinishScope {
        FinishScope::All { project_id: None }
    }

    async fn status_of(pool: &SqlitePool, id: &str) -> SpecStatus {
        store::get_spec(pool, id).await.unwrap().unwrap().status
    }

    #[tokio::test]
    async fn creates_one_tree_per_spec() {
        let pool = crate::testdb::fresh_pool().await;
        let a = add_spec(&pool, "alpha", SpecStatus::Extracted).await;
        let b = add_spec(&pool, "beta", SpecStatus::Extracted).await;
        let c = add_spec(&pool, "gamma", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&a, &b, &c]);
        let mut config = cfg();
        config.owned_paths = disjoint(&[&a, &b, &c]);

        let report = finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        let requests = launcher.requests();
        assert_eq!(requests.len(), 3, "exactly one tree per spec");
        let mut specs: Vec<_> = requests.iter().map(|r| r.spec_id.clone()).collect();
        specs.sort();
        let mut expected = vec![a.clone(), b.clone(), c.clone()];
        expected.sort();
        assert_eq!(specs, expected);
        let mut ids: Vec<_> = requests.iter().map(|r| r.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3, "every tree has its own id");
        let mut branches: Vec<_> = requests.iter().map(|r| r.feature_branch.clone()).collect();
        branches.sort();
        branches.dedup();
        assert_eq!(branches.len(), 3, "every tree has its own branch");
        for request in &requests {
            assert_ne!(request.feature_branch, "master", "never a tree over master");
            assert_eq!(request.project_root, "/repo");
            assert_eq!(request.round, 1);
            assert!(
                request.goal.contains(".ai/specs/2026-01-01-"),
                "goal names the spec file"
            );
        }
        assert_eq!(report.trees.len(), 3);
        for id in [&a, &b, &c] {
            assert_eq!(status_of(&pool, id).await, SpecStatus::Done);
            let spec = store::get_spec(&pool, id).await.unwrap().unwrap();
            assert!(spec.tree_id.is_some(), "the spec records its tree");
        }
    }

    #[tokio::test]
    async fn respects_dependency_order() {
        let pool = crate::testdb::fresh_pool().await;
        // Inserted in the reverse of the order they must run in.
        let a = add_spec(&pool, "alpha", SpecStatus::Extracted).await;
        let b = add_spec(&pool, "beta", SpecStatus::Extracted).await;
        let c = add_spec(&pool, "gamma", SpecStatus::Extracted).await;
        store::add_dep(&pool, &a, &b, DepSource::Owner)
            .await
            .unwrap();
        store::add_dep(&pool, &b, &c, DepSource::Extracted)
            .await
            .unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&a, &b, &c]);
        let mut config = cfg();
        config.max_parallel = 3;
        config.owned_paths = disjoint(&[&a, &b, &c]);

        finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        let order: Vec<_> = launcher
            .requests()
            .iter()
            .map(|r| r.spec_id.clone())
            .collect();
        assert_eq!(order, vec![c, b, a]);
        assert_eq!(
            launcher.max_in_flight.load(Ordering::SeqCst),
            1,
            "a chain never overlaps"
        );
    }

    #[tokio::test]
    async fn skips_specs_already_done() {
        let pool = crate::testdb::fresh_pool().await;
        let done = add_spec(&pool, "finished", SpecStatus::Done).await;
        let todo = add_spec(&pool, "todo", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&done, &todo]);

        let report = finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();

        let launched: Vec<_> = launcher
            .requests()
            .iter()
            .map(|r| r.spec_id.clone())
            .collect();
        assert_eq!(launched, vec![todo]);
        assert_eq!(report.skipped, vec![done.clone()]);
        assert!(
            !log.lock()
                .unwrap()
                .iter()
                .any(|entry| entry.ends_with(&done)),
            "a done spec is neither extracted, audited nor launched"
        );
    }

    #[tokio::test]
    async fn extracts_and_audits_discovered_specs_before_any_tree() {
        let pool = crate::testdb::fresh_pool().await;
        let ready = add_spec(&pool, "ready", SpecStatus::Extracted).await;
        let fresh_one = add_spec(&pool, "fresh-one", SpecStatus::Discovered).await;
        let fresh_two = add_spec(&pool, "fresh-two", SpecStatus::Discovered).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        // Nothing passes the pre-tree audit, so every spec needs a tree.
        let prep = FakePreparer::new(&pool, &log, &[]);
        let mut config = cfg();
        config.max_rounds = 1;
        config.owned_paths = disjoint(&[&ready, &fresh_one, &fresh_two]);

        finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        let log = log.lock().unwrap().clone();
        let first_tree = log
            .iter()
            .position(|e| e.starts_with("tree:"))
            .expect("trees ran");
        for fresh in [&fresh_one, &fresh_two] {
            let extract = log.iter().position(|e| *e == format!("extract:{fresh}"));
            let audit = log.iter().position(|e| *e == format!("audit:{fresh}"));
            let (extract, audit) = (extract.expect("extracted"), audit.expect("audited"));
            assert!(extract < audit, "extract before audit for {fresh}");
            assert!(audit < first_tree, "audit of {fresh} before any tree");
        }
        assert!(
            !log.iter().any(|e| *e == format!("extract:{ready}")),
            "an already extracted spec is not extracted again"
        );
    }

    #[tokio::test]
    async fn runs_independent_specs_in_parallel_up_to_the_budget() {
        let pool = crate::testdb::fresh_pool().await;
        let ids = [
            add_spec(&pool, "one", SpecStatus::Extracted).await,
            add_spec(&pool, "two", SpecStatus::Extracted).await,
            add_spec(&pool, "three", SpecStatus::Extracted).await,
            add_spec(&pool, "four", SpecStatus::Extracted).await,
        ];
        let refs: Vec<&String> = ids.iter().collect();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &refs);
        let mut config = cfg();
        config.max_parallel = 2;
        config.owned_paths = disjoint(&refs);

        finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 4);
        assert_eq!(
            launcher.max_in_flight.load(Ordering::SeqCst),
            2,
            "independent specs overlap, but never beyond the budget"
        );
    }

    #[tokio::test]
    async fn serialises_specs_with_overlapping_paths() {
        let pool = crate::testdb::fresh_pool().await;
        let a = add_spec(&pool, "alpha", SpecStatus::Extracted).await;
        let b = add_spec(&pool, "beta", SpecStatus::Extracted).await;
        let c = add_spec(&pool, "gamma", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&a, &b, &c]);
        let mut config = cfg();
        config.max_parallel = 4;
        // a and b write under the same directory; c has an unknown scope (no entry).
        config
            .owned_paths
            .insert(a.clone(), vec!["core/src/shared/".to_owned()]);
        config
            .owned_paths
            .insert(b.clone(), vec!["core/src/shared/mod.rs".to_owned()]);

        finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 3);
        assert_eq!(
            launcher.max_in_flight.load(Ordering::SeqCst),
            1,
            "overlapping and unknown scopes never run together"
        );
    }

    #[tokio::test]
    async fn third_failed_round_becomes_needs_owner() {
        let pool = crate::testdb::fresh_pool().await;
        let stubborn = add_spec(&pool, "stubborn", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        // The audit never passes.
        let prep = FakePreparer::new(&pool, &log, &[]);

        let report = finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();

        let rounds: Vec<u32> = launcher.requests().iter().map(|r| r.round).collect();
        assert_eq!(rounds, vec![1, 2], "at most two rounds, never a third tree");
        assert_eq!(status_of(&pool, &stubborn).await, SpecStatus::NeedsOwner);
        let spec = store::get_spec(&pool, &stubborn).await.unwrap().unwrap();
        assert!(
            spec.status_reason.is_some_and(|r| !r.is_empty()),
            "a reason is recorded"
        );
        assert_eq!(report.needs_owner, vec![stubborn.clone()]);
        assert!(report.done.is_empty());
        // The second round's audit sees the lineage of both trees.
        let lineages = prep.lineages.lock().unwrap();
        let (_, last) = lineages.last().unwrap();
        assert!(last.run_ids.iter().any(|r| r.ends_with("-r1")));
        assert!(last.run_ids.iter().any(|r| r.ends_with("-r2")));
    }

    #[tokio::test]
    async fn finish_all_with_nothing_to_do_is_a_noop() {
        let pool = crate::testdb::fresh_pool().await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[]);

        let empty = finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();
        assert_eq!(empty, FinishReport::default());

        let done = add_spec(&pool, "finished", SpecStatus::Done).await;
        let again = finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();
        assert!(again.trees.is_empty());
        assert_eq!(again.skipped, vec![done.clone()]);
        assert!(log.lock().unwrap().is_empty(), "nothing was started");
        assert_eq!(status_of(&pool, &done).await, SpecStatus::Done);
    }

    #[tokio::test]
    async fn finish_discovers_a_spec_written_since_the_last_scan() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let specs = project.path().join(".ai/specs");
        std::fs::create_dir_all(&specs).unwrap();
        std::fs::write(specs.join("new.md"), "# New\n").unwrap();
        let expected = spec_id(Some("finish-scan-project"), ".ai/specs/new.md");
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[]);
        let mut config = cfg();
        config.project_root = project.path().to_string_lossy().to_string();
        config.state_dir = None;

        finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            FinishScope::All { project_id: Some("finish-scan-project".to_owned()) },
        ).await.unwrap();

        assert!(log.lock().unwrap().iter().any(|entry| entry == &format!("extract:{expected}")));
    }

    #[tokio::test]
    async fn finish_says_on_the_feed_how_each_spec_ended() {
        let pool = crate::testdb::fresh_pool().await;
        let done = add_spec(&pool, "done", SpecStatus::Extracted).await;
        let blocked = add_spec(&pool, "blocked", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&done]);
        let mut config = cfg();
        config.owned_paths = disjoint(&[&done, &blocked]);

        finish_with(&pool, &FinishDeps { launcher: &launcher, preparer: &prep }, &config, scope_all()).await.unwrap();

        for (id, kind, title) in [(&done, "spec_done", "done"), (&blocked, "spec_blocked", "blocked")] {
            let row: Option<(String, String, String)> = sqlx::query_as(
                "SELECT kind, summary, subject FROM feed WHERE subject = ? ORDER BY created_at DESC LIMIT 1",
            ).bind(id).fetch_optional(&pool).await.unwrap();
            let (actual_kind, summary, subject) = row.expect("feed row");
            assert_eq!(actual_kind, kind);
            assert_eq!(subject, *id);
            assert!(summary.contains(title));
        }
    }

    #[tokio::test]
    async fn never_a_tree_over_the_integration_branch() {
        let pool = crate::testdb::fresh_pool().await;
        let risky = add_spec(&pool, "risky", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&risky]);
        let mut config = cfg();
        // The branch derived for the spec is exactly the integration branch.
        config.integration_branch = derived_branch(&path_of("risky"));

        let report = finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        assert!(
            launcher.requests().is_empty(),
            "no tree is created over the integration branch"
        );
        assert_eq!(report.refused, vec![risky.clone()]);
        assert_ne!(status_of(&pool, &risky).await, SpecStatus::Done);
        assert!(derived_branch(&path_of("risky")).starts_with("spec/"));
    }

    #[tokio::test]
    async fn spec_files_are_never_altered() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("2026-01-01-sealed.md");
        let bytes = b"# Sealed\n\n- [ ] it works\n".to_vec();
        std::fs::write(&file, &bytes).unwrap();
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "sealed", SpecStatus::Discovered).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&id]);
        let mut config = cfg();
        config.worktree = dir.path().to_path_buf();
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();

        finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read(&file).unwrap(),
            bytes,
            "the spec file is byte-identical"
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            before
        );
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "finish creates nothing beside the spec");
        let source = include_str!("finish.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for needle in [
            "fs::write",
            "OpenOptions",
            "File::create",
            "remove_file",
            "rename(",
        ] {
            assert!(
                !production.contains(needle),
                "production code must not use {needle}"
            );
        }
    }

    #[test]
    fn real_reviewer_adapter_requests_a_no_tools_run() {
        let request = no_tools_run_request("review this", Some("proj"), Some("/repo"));

        assert_eq!(request.prompt, "review this");
        assert_eq!(request.project_id.as_deref(), Some("proj"));
        assert_eq!(request.cwd.as_deref(), Some("/repo"));
        assert_eq!(
            request.tool_policy,
            ToolPolicy::None,
            "the reviewer has no tools at all"
        );
        assert!(
            request.wait_for_final_text,
            "the adapter waits for the final text"
        );
        assert!(
            !request.steerable,
            "nobody talks to a reviewer after it starts"
        );
    }

    #[tokio::test]
    async fn extraction_run_ids_are_persisted() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "alpha", SpecStatus::Extracted).await;
        let other = add_spec(&pool, "beta", SpecStatus::Extracted).await;

        record_extraction_run(&pool, &id, "ex-1").await.unwrap();
        record_extraction_run(&pool, &id, "ex-2").await.unwrap();
        record_extraction_run(&pool, &id, "ex-1").await.unwrap();
        record_extraction_run(&pool, &other, "ex-9").await.unwrap();

        assert_eq!(
            extraction_run_ids(&pool, &id).await.unwrap(),
            vec!["ex-1", "ex-2"]
        );
        assert_eq!(
            extraction_run_ids(&pool, &other).await.unwrap(),
            vec!["ex-9"]
        );
        assert!(
            extraction_run_ids(&pool, "unknown")
                .await
                .unwrap()
                .is_empty()
        );

        // A later audit counts them as part of the spec's own lineage.
        let outcome = TreeOutcome {
            run_ids: vec!["tree-run".to_owned()],
            lineage_roots: vec!["tree-root".to_owned()],
        };
        let lineage = lineage_for(&pool, &id, &[outcome]).await.unwrap();
        for expected in ["ex-1", "ex-2", "tree-run"] {
            assert!(
                lineage.run_ids.iter().any(|r| r == expected),
                "missing {expected}"
            );
        }
        assert!(!lineage.run_ids.iter().any(|r| r == "ex-9"));
        assert!(lineage.lineage_roots.iter().any(|r| r == "tree-root"));
        let _ = spec_id(None, "unused");
    }

    fn crit(ordinal: i64, text: &str, hint: Option<&str>) -> Criterion {
        Criterion {
            id: ordinal,
            spec_id: "s".to_owned(),
            ordinal,
            text: text.to_owned(),
            kind: crate::spec_backlog::model::CriterionKind::Behavior,
            verify_hint: hint.map(str::to_owned),
            status: CriterionStatus::Doubt,
            evidence: None,
            reviewer_run_id: None,
            spec_sha: SHA.to_owned(),
            checked_at: None,
        }
    }

    fn sample_spec() -> Spec {
        Spec {
            id: "s".to_owned(),
            project_id: None,
            path: ".ai/specs/a.md".to_owned(),
            title: "A".to_owned(),
            content_sha: SHA.to_owned(),
            kind: SpecKind::Spec,
            status: SpecStatus::Extracted,
            status_reason: None,
            tree_id: None,
            last_audited_sha: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn criteria_name_files_through_text_and_hint() {
        let criteria = [
            crit(1, "`core/src/a.rs:12` rejects an empty id.", None),
            crit(
                2,
                "Docs updated.",
                Some("see ./docs/b.md and https://x.io/p.html"),
            ),
        ];
        assert_eq!(
            paths_named_by(&criteria),
            vec!["core/src/a.rs", "docs/b.md"]
        );
    }

    #[test]
    fn numbered_prefix_cuts_on_a_line_boundary_and_says_so() {
        assert_eq!(
            numbered_prefix(
                "a
b
", 100
            ),
            (
                "1: a
2: b
"
                .to_owned(),
                false
            )
        );
        assert_eq!(
            numbered_prefix(
                "a
b
", 7
            ),
            (
                "1: a
"
                .to_owned(),
                true
            )
        );
    }

    #[test]
    fn a_cut_prompt_tells_the_reviewer_to_doubt_what_it_cannot_see() {
        let criteria = [crit(1, "x", None)];
        let whole = ReviewMaterial {
            files: vec![ReviewFile {
                path: "a.rs".to_owned(),
                text: "1: fn a() {}
"
                .to_owned(),
            }],
            cut: false,
        };
        let cut = ReviewMaterial {
            cut: true,
            ..whole.clone()
        };
        let shown = review_prompt(&sample_spec(), &criteria, &whole);
        assert!(shown.contains("=== a.rs ===") && shown.contains("1: fn a() {}"));
        assert!(!shown.contains("NOT EVERYTHING IS SHOWN"));
        let hidden = review_prompt(&sample_spec(), &criteria, &cut);
        assert!(hidden.contains("NOT EVERYTHING IS SHOWN") && hidden.contains("doubt"));
    }

    #[tokio::test]
    async fn material_embeds_named_files_within_the_caps_and_flags_a_cut() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/a.rs"),
            "one
two
",
        )
        .unwrap();
        let big = "x".repeat(100);
        std::fs::write(
            dir.path().join("src/big.rs"),
            format!(
                "{big}
"
            )
            .repeat(REVIEW_FILE_CAP),
        )
        .unwrap();
        std::fs::write(dir.path().join("src/bin.dat"), [0u8, 1, 2]).unwrap();
        let criteria = [crit(
            1,
            "src/a.rs, src/big.rs, src/bin.dat and ../escape.rs and src/missing.rs",
            None,
        )];
        // Not a git checkout, so the changed-file list is unreadable and counts as a cut too.
        let material = gather_material(dir.path(), "master", &criteria).await;
        let paths: Vec<&str> = material.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/big.rs"]);
        assert!(material.files[0].text.starts_with(
            "1: one
2: two
"
        ));
        assert!(material.files[1].text.len() <= REVIEW_FILE_CAP);
        assert!(material.cut);
    }
}
