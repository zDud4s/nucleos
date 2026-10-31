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
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Mutex, PoisonError};
use std::task::Poll;

use rand::RngExt;
use sqlx::SqlitePool;

use super::audit::{AuditReport, SpecLineage};
use super::deps::{Cycle, topological_order};
use super::model::{CriterionStatus, Spec, SpecKind, SpecStatus};
use super::scan::ScanRoots;
use super::store::{self, StoreError};
use super::verdict::{NotDoneReason, Verdict, VerifyState};
use crate::work_nodes::sets_overlap;

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

/// What a tree leaves for the audit, unless it is still live: the runs it made and their lineage
/// roots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeOutcome {
    pub run_ids: Vec<String>,
    pub lineage_roots: Vec<String>,
    /// True when the launcher reached its deadline while the tree was still live.
    pub still_running: bool,
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
    /// The scan that discovers the project's specs failed.
    Scan(String),
}

impl fmt::Display for FinishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FinishError::Store(error) => write!(f, "{error}"),
            FinishError::Config(what) => write!(f, "cannot finish the backlog: {what}"),
            FinishError::Scan(what) => write!(f, "cannot scan the specs: {what}"),
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

pub use super::daemon::{
    ProjectPaths, audit_one, extract_one, finish, no_tools_run_request, project_id_for_root,
    record_extraction_run, resolve_project,
};

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
        store::set_status(
            pool,
            &spec.id,
            SpecStatus::Discovered,
            Some(&format!("extraction failed: {reason}")),
        )
        .await?;
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

/// How many criteria are in doubt when doubt is all that keeps the spec from done.
///
/// Another tree cannot settle a doubt, so a round is only worth paying for when a criterion is
/// unmet (or still unreviewed) or verify is red. `None` means another round is warranted.
fn only_doubts_left(report: &AuditReport) -> Option<usize> {
    if report.verify == VerifyState::Red {
        return None;
    }
    let Verdict::NotDone(reasons) = &report.verdict else {
        return None;
    };
    let mut doubts = 0;
    for reason in reasons {
        match reason {
            NotDoneReason::VerifyNotGreen => {}
            NotDoneReason::Unresolved {
                status: CriterionStatus::Doubt,
                ..
            } => doubts += 1,
            _ => return None,
        }
    }
    (doubts > 0).then_some(doubts)
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
            Ok(outcome) if outcome.still_running => {
                mark_needs_owner(
                    pool,
                    &spec.id,
                    &format!("tree {} is still live", request.id),
                )
                .await?;
                return Ok(SpecRun {
                    trees,
                    finished: false,
                });
            }
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
                if let Some(doubts) = only_doubts_left(&report) {
                    mark_needs_owner(
                        pool,
                        &spec.id,
                        &format!(
                            "{doubts} criteria are in doubt and the suite is not red; \
                             the owner has to settle them"
                        ),
                    )
                    .await?;
                    return Ok(SpecRun {
                        trees,
                        finished: false,
                    });
                }
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
    // A spec written since the last scan is part of "everything".
    let scanned_project = match &scope {
        FinishScope::All { project_id } => Some(project_id.clone()),
        FinishScope::One { spec_id } => store::get_spec(pool, spec_id)
            .await?
            .map(|spec| spec.project_id),
    };
    if let Some(project_id) = scanned_project {
        let roots = ScanRoots {
            project_root: Some(PathBuf::from(&config.project_root)),
            state_dir: config.state_dir.clone(),
        };
        super::scan::scan(pool, project_id.as_deref(), &roots)
            .await
            .map_err(|error| FinishError::Scan(format!("{error:?}")))?;
    }

    let mut report = FinishReport::default();
    let mut done_ids: BTreeSet<String> = BTreeSet::new();
    // Specs that will not finish in this run; whatever waits on one is blocked.
    let mut stopped: BTreeSet<String> = BTreeSet::new();

    // 1. Sort what is in scope: done ones are left alone, the rest is prepared.
    let mut runnable: Vec<Spec> = Vec::new();
    for spec in load_scope(pool, &scope).await? {
        let spec = if let FinishScope::One { spec_id } = &scope
            && spec.id == *spec_id
            && spec.status == SpecStatus::NeedsOwner
        {
            store::set_status(pool, &spec.id, SpecStatus::Discovered, None).await?;
            store::get_spec(pool, &spec.id).await?.unwrap_or(spec)
        } else {
            spec
        };
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
    announce(pool, &report).await;
    Ok(report)
}

/// Says on the feed how each spec ended in this run; one line per spec, none for a spec that was
/// already done. A feed that cannot be written to only warns: the finish itself is not undone.
async fn announce(pool: &SqlitePool, report: &FinishReport) {
    let ended = report.done.iter().map(|id| (id, "spec_done")).chain(
        report
            .needs_owner
            .iter()
            .chain(&report.refused)
            .chain(&report.blocked)
            .map(|id| (id, "spec_blocked")),
    );
    for (id, kind) in ended {
        let spec = match store::get_spec(pool, id).await {
            Ok(spec) => spec,
            Err(error) => {
                tracing::warn!(%error, spec_id = %id, "could not read a spec to announce it");
                None
            }
        };
        let title = spec
            .as_ref()
            .map_or(id.as_str(), |spec| spec.title.as_str());
        let summary = if kind == "spec_done" {
            format!("Spec done: {title}")
        } else {
            let reason = if report.refused.contains(id) {
                "its branch is the integration branch, so no tree was started".to_owned()
            } else {
                spec.as_ref()
                    .and_then(|spec| spec.status_reason.clone())
                    .unwrap_or_else(|| "it did not finish".to_owned())
            };
            format!("Spec not finished: {title} ({reason})")
        };
        post_feed(pool, kind, &summary, id).await;
    }
}

/// One feed line; a failure only warns.
async fn post_feed(pool: &SqlitePool, kind: &str, summary: &str, subject: &str) {
    let result = sqlx::query(
        "INSERT INTO feed (kind, summary, run_id, subject, created_at) VALUES (?, ?, NULL, ?, ?)",
    )
    .bind(kind)
    .bind(summary)
    .bind(subject)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, kind, "could not write a spec feed line");
    }
}

/// Writes the feed line the owner sees when a finish could not even run.
pub async fn announce_failure(pool: &SqlitePool, subject: &str, error: &str) {
    post_feed(pool, "spec_blocked", error, subject).await;
}

/// Projects with a finish under way in this process.
static CLAIMS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Held while a finish runs for a project; released on drop.
#[derive(Debug)]
pub struct FinishClaim {
    project_id: String,
}

impl Drop for FinishClaim {
    fn drop(&mut self) {
        // A poisoned lock still holds a valid set: take it and release the claim.
        CLAIMS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.project_id);
    }
}

/// Claims a project for one finish; `None` when one is already running.
pub fn claim_project(project_id: &str) -> Option<FinishClaim> {
    let mut claims = CLAIMS.lock().unwrap_or_else(PoisonError::into_inner);
    claims.insert(project_id.to_owned()).then(|| FinishClaim {
        project_id: project_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::model::Criterion;
    use super::super::review::{
        REVIEW_FILE_CAP, ReviewFile, ReviewMaterial, gather_material, numbered_prefix,
        paths_named_by, review_prompt,
    };
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
        waited_tree_ids: Mutex<Vec<String>>,
        wait_outcomes: Mutex<BTreeMap<String, Result<TreeOutcome, String>>>,
        free_disk_bytes: AtomicUsize,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        delay: Duration,
    }

    impl FakeLauncher {
        fn new(log: &Log) -> Self {
            Self {
                log: log.clone(),
                requests: Mutex::new(Vec::new()),
                waited_tree_ids: Mutex::new(Vec::new()),
                wait_outcomes: Mutex::new(BTreeMap::new()),
                free_disk_bytes: AtomicUsize::new(usize::MAX),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                delay: Duration::from_millis(40),
            }
        }

        fn requests(&self) -> Vec<TreeRequest> {
            self.requests.lock().unwrap().clone()
        }

        fn wait_result(&self, tree_id: &str, result: Result<TreeOutcome, String>) {
            self.wait_outcomes
                .lock()
                .unwrap()
                .insert(tree_id.to_owned(), result);
        }

        fn waited_tree_ids(&self) -> Vec<String> {
            self.waited_tree_ids.lock().unwrap().clone()
        }

        fn set_free_disk_bytes(&self, bytes: usize) {
            self.free_disk_bytes.store(bytes, Ordering::SeqCst);
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
                ..Default::default()
            })
        }

        async fn wait_tree(&self, tree_id: &str) -> Result<TreeOutcome, String> {
            self.waited_tree_ids.lock().unwrap().push(tree_id.to_owned());
            self.wait_outcomes
                .lock()
                .unwrap()
                .remove(tree_id)
                .unwrap_or_else(|| {
                    Ok(TreeOutcome {
                        run_ids: vec![format!("run-{tree_id}")],
                        lineage_roots: vec![format!("root-{tree_id}")],
                        ..Default::default()
                    })
                })
        }

        fn free_disk_bytes(&self) -> u64 {
            self.free_disk_bytes.load(Ordering::SeqCst) as u64
        }
    }

    /// Extract and audit stand-ins that write to the store like the real ones would.
    struct FakePreparer {
        pool: SqlitePool,
        log: Log,
        /// Specs whose audit ends Done; every other audit ends Audited.
        passes: Vec<String>,
        lineages: Mutex<Vec<(String, SpecLineage)>>,
        fail_first_extractions: AtomicUsize,
        extraction_attempts: AtomicUsize,
    }

    impl FakePreparer {
        fn new(pool: &SqlitePool, log: &Log, passes: &[&String]) -> Self {
            Self {
                pool: pool.clone(),
                log: log.clone(),
                passes: passes.iter().map(|id| (*id).clone()).collect(),
                lineages: Mutex::new(Vec::new()),
                fail_first_extractions: AtomicUsize::new(0),
                extraction_attempts: AtomicUsize::new(0),
            }
        }

        fn fail_first_extraction(&self) {
            self.fail_first_extractions.store(1, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl SpecPreparer for FakePreparer {
        async fn extract(&self, spec_id: &str) -> Result<(), String> {
            self.log.lock().unwrap().push(format!("extract:{spec_id}"));
            let attempt = self.extraction_attempts.fetch_add(1, Ordering::SeqCst);
            if attempt < self.fail_first_extractions.load(Ordering::SeqCst) {
                return Err("temporary extraction error".to_owned());
            }
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
            let sha = store::get_spec(&self.pool, spec_id)
                .await
                .unwrap()
                .unwrap()
                .content_sha;
            store::set_audit_result(&self.pool, spec_id, status, Some("fake audit"), &sha)
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

    struct ScriptedAuditPreparer {
        verify: VerifyState,
        verdict: Verdict,
    }

    impl ScriptedAuditPreparer {
        fn not_done(verify: VerifyState, status: CriterionStatus) -> Self {
            Self {
                verify,
                verdict: Verdict::NotDone(vec![NotDoneReason::Unresolved { ordinal: 2, status }]),
            }
        }
    }

    #[async_trait::async_trait]
    impl SpecPreparer for ScriptedAuditPreparer {
        async fn extract(&self, _spec_id: &str) -> Result<(), String> {
            panic!("these tests start with an extracted spec")
        }

        async fn audit(
            &self,
            _spec_id: &str,
            _lineage: &SpecLineage,
        ) -> Result<AuditReport, String> {
            Ok(AuditReport {
                reviewer_run_id: "reviewer-1".to_owned(),
                verify: self.verify,
                verdict: self.verdict.clone(),
                status: SpecStatus::Audited,
            })
        }
    }

    fn scope_all() -> FinishScope {
        FinishScope::All { project_id: None }
    }

    async fn status_of(pool: &SqlitePool, id: &str) -> SpecStatus {
        store::get_spec(pool, id).await.unwrap().unwrap().status
    }

    async fn seed_criteria(pool: &SqlitePool, id: &str, statuses: &[CriterionStatus]) {
        let items: Vec<NewCriterion> = statuses
            .iter()
            .enumerate()
            .map(|(index, status)| NewCriterion {
                text: format!("criterion {}", index + 1),
                kind: CriterionKind::Behavior,
                verify_hint: None,
                status: *status,
            })
            .collect();
        store::replace_criteria(pool, id, SHA, &items)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_doubt_with_green_verify_does_not_pay_for_another_tree() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "green-doubt", SpecStatus::Extracted).await;
        seed_criteria(&pool, &id, &[CriterionStatus::Met, CriterionStatus::Doubt]).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = ScriptedAuditPreparer::not_done(VerifyState::Green, CriterionStatus::Doubt);
        let mut config = cfg();
        config.max_rounds = 3;
        config.owned_paths = disjoint(&[&id]);

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

        assert_eq!(launcher.requests().len(), 1);
        let spec = store::get_spec(&pool, &id).await.unwrap().unwrap();
        assert_eq!(spec.status, SpecStatus::NeedsOwner);
        assert!(spec.status_reason.unwrap_or_default().contains("doubt"));
    }

    #[tokio::test]
    async fn a_doubt_with_unknown_verify_does_not_pay_for_another_tree() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "unknown-doubt", SpecStatus::Extracted).await;
        seed_criteria(&pool, &id, &[CriterionStatus::Met, CriterionStatus::Doubt]).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = ScriptedAuditPreparer::not_done(VerifyState::Unknown, CriterionStatus::Doubt);
        let mut config = cfg();
        config.max_rounds = 3;
        config.owned_paths = disjoint(&[&id]);

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

        assert_eq!(launcher.requests().len(), 1);
        let spec = store::get_spec(&pool, &id).await.unwrap().unwrap();
        assert_eq!(spec.status, SpecStatus::NeedsOwner);
        assert!(spec.status_reason.unwrap_or_default().contains("doubt"));
    }

    #[tokio::test]
    async fn an_unmet_criterion_still_pays_for_another_tree() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "unmet", SpecStatus::Extracted).await;
        seed_criteria(&pool, &id, &[CriterionStatus::Met, CriterionStatus::Unmet]).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = ScriptedAuditPreparer::not_done(VerifyState::Green, CriterionStatus::Unmet);
        let mut config = cfg();
        config.max_rounds = 2;
        config.owned_paths = disjoint(&[&id]);

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

        assert_eq!(launcher.requests().len(), 2);
    }

    #[tokio::test]
    async fn a_doubt_with_red_verify_still_pays_for_another_tree() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "red-doubt", SpecStatus::Extracted).await;
        seed_criteria(&pool, &id, &[CriterionStatus::Doubt]).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = ScriptedAuditPreparer::not_done(VerifyState::Red, CriterionStatus::Doubt);
        let mut config = cfg();
        config.max_rounds = 2;
        config.owned_paths = disjoint(&[&id]);

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

        assert_eq!(launcher.requests().len(), 2);
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
    async fn review_and_note_dependencies_are_satisfied() {
        let pool = crate::testdb::fresh_pool().await;
        let review = add_spec(&pool, "review-dependency", SpecStatus::Extracted).await;
        let note = add_spec(&pool, "note-dependency", SpecStatus::Extracted).await;
        let dependent = add_spec(&pool, "depends-on-non-specs", SpecStatus::Extracted).await;
        store::set_extraction_result(&pool, &review, SpecKind::Review, SpecStatus::Extracted, None)
            .await
            .unwrap();
        store::set_extraction_result(&pool, &note, SpecKind::Note, SpecStatus::Extracted, None)
            .await
            .unwrap();
        store::add_dep(&pool, &dependent, &review, DepSource::Owner)
            .await
            .unwrap();
        store::add_dep(&pool, &dependent, &note, DepSource::Owner)
            .await
            .unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&dependent]);

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 1);
        assert_eq!(report.needs_owner.len(), 0, "satisfied non-spec dependencies need no owner");
        assert_eq!(report.done, vec![dependent]);
    }

    #[tokio::test]
    async fn abandoned_dependency_marks_dependent_needs_owner_once() {
        let pool = crate::testdb::fresh_pool().await;
        let dependency = add_spec(&pool, "abandoned-dependency", SpecStatus::Abandoned).await;
        let dependent = add_spec(&pool, "abandoned-dependent", SpecStatus::Extracted).await;
        store::add_dep(&pool, &dependent, &dependency, DepSource::Owner)
            .await
            .unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[]);

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 0);
        assert_eq!(report.needs_owner, vec![dependent.clone()], "the owner is told once");
        let reason = store::get_spec(&pool, &dependent)
            .await
            .unwrap()
            .unwrap()
            .status_reason
            .unwrap_or_default();
        assert!(reason.contains(&dependency), "the reason names the dependency");
        assert!(reason.contains("abandoned"), "the reason states that it is abandoned");
    }

    #[tokio::test]
    async fn missing_dependency_marks_dependent_needs_owner_once() {
        let pool = crate::testdb::fresh_pool().await;
        let dependent = add_spec(&pool, "missing-dependent", SpecStatus::Extracted).await;
        let missing = "missing-dependency";
        store::add_dep(&pool, &dependent, missing, DepSource::Owner)
            .await
            .unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[]);

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 0);
        assert_eq!(report.needs_owner, vec![dependent.clone()], "the owner is told once");
        let reason = store::get_spec(&pool, &dependent)
            .await
            .unwrap()
            .unwrap()
            .status_reason
            .unwrap_or_default();
        assert!(reason.contains(missing), "the reason names the dependency");
        assert!(reason.contains("does not exist"), "the reason states that it is missing");
    }

    #[tokio::test]
    async fn invalid_dependency_propagates_to_its_dependents() {
        let pool = crate::testdb::fresh_pool().await;
        let abandoned = add_spec(&pool, "invalid-root", SpecStatus::Abandoned).await;
        let dependent = add_spec(&pool, "invalid-dependent", SpecStatus::Extracted).await;
        let downstream = add_spec(&pool, "invalid-downstream", SpecStatus::Extracted).await;
        store::add_dep(&pool, &dependent, &abandoned, DepSource::Owner)
            .await
            .unwrap();
        store::add_dep(&pool, &downstream, &dependent, DepSource::Owner)
            .await
            .unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[]);

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 0);
        assert_eq!(report.needs_owner, vec![dependent.clone()], "only the invalid dependency is announced");
        assert_eq!(status_of(&pool, &dependent).await, SpecStatus::NeedsOwner);
        assert_eq!(status_of(&pool, &downstream).await, SpecStatus::Blocked);
        assert!(report.blocked.contains(&downstream));
    }

    #[tokio::test]
    async fn done_dependency_behavior_is_unchanged() {
        let pool = crate::testdb::fresh_pool().await;
        let dependency = add_spec(&pool, "done-dependency", SpecStatus::Done).await;
        let dependent = add_spec(&pool, "done-dependent", SpecStatus::Extracted).await;
        store::add_dep(&pool, &dependent, &dependency, DepSource::Owner)
            .await
            .unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&dependent]);

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 1);
        assert_eq!(report.needs_owner.len(), 0);
        assert_eq!(report.done, vec![dependent]);
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
    async fn a_failed_extraction_is_tried_again_by_the_next_finish() {
        let pool = crate::testdb::fresh_pool().await;
        let dependency = add_spec(&pool, "dependency", SpecStatus::Discovered).await;
        let dependent = add_spec(&pool, "dependent", SpecStatus::Extracted).await;
        store::add_dep(&pool, &dependent, &dependency, DepSource::Owner)
            .await
            .unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[]);
        prep.fail_first_extraction();
        let mut config = cfg();
        config.owned_paths = disjoint(&[&dependency, &dependent]);

        let first = finish_with(
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

        assert!(first.trees.is_empty());
        assert!(first.needs_owner.contains(&dependency));
        let failed = store::get_spec(&pool, &dependency).await.unwrap().unwrap();
        assert_eq!(failed.status, SpecStatus::Discovered);
        assert!(
            failed
                .status_reason
                .as_deref()
                .unwrap_or_default()
                .contains("temporary extraction error")
        );
        assert!(
            launcher.requests().is_empty(),
            "a dependent does not start after its dependency extraction fails"
        );

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

        assert_eq!(prep.extraction_attempts.load(Ordering::SeqCst), 2);
        assert!(
            launcher
                .requests()
                .iter()
                .any(|request| request.spec_id == dependency)
        );
    }

    #[tokio::test]
    async fn a_spec_named_by_the_owner_leaves_needs_owner() {
        let pool = crate::testdb::fresh_pool().await;
        let selected = add_spec(&pool, "selected", SpecStatus::NeedsOwner).await;
        let all_scope = add_spec(&pool, "all-scope", SpecStatus::NeedsOwner).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[]);
        let mut config = cfg();
        config.owned_paths = disjoint(&[&selected, &all_scope]);

        finish_with(
            &pool,
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &config,
            FinishScope::One {
                spec_id: selected.clone(),
            },
        )
        .await
        .unwrap();

        assert!(
            log.lock()
                .unwrap()
                .iter()
                .any(|entry| entry == &format!("extract:{selected}"))
        );
        assert!(
            launcher
                .requests()
                .iter()
                .any(|request| request.spec_id == selected)
        );

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

        assert_eq!(status_of(&pool, &all_scope).await, SpecStatus::NeedsOwner);
        assert!(
            !log.lock()
                .unwrap()
                .iter()
                .any(|entry| entry == &format!("extract:{all_scope}"))
        );
    }

    struct StillRunningLauncher {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TreeLauncher for StillRunningLauncher {
        async fn run_tree(&self, _request: &TreeRequest) -> Result<TreeOutcome, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(TreeOutcome {
                still_running: true,
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn a_tree_still_running_is_not_followed_by_another() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "running", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = StillRunningLauncher {
            calls: AtomicUsize::new(0),
        };
        let prep = FakePreparer::new(&pool, &log, &[]);
        let mut config = cfg();
        config.max_rounds = 2;
        config.owned_paths = disjoint(&[&id]);

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

        assert_eq!(launcher.calls.load(Ordering::SeqCst), 1);
        assert!(
            !log.lock()
                .unwrap()
                .iter()
                .any(|entry| entry == &format!("audit:{id}")),
            "a still-running tree is not audited"
        );
        let spec = store::get_spec(&pool, &id).await.unwrap().unwrap();
        assert_eq!(spec.status, SpecStatus::NeedsOwner);
        assert!(
            spec.status_reason
                .as_deref()
                .unwrap_or_default()
                .contains(&report.trees[0])
        );
    }

    #[tokio::test]
    async fn restart_adopts_a_live_tree_without_creating_a_second_tree() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "adopt-live", SpecStatus::Extracted).await;
        let adopted = "tree-adopt-live";
        store::set_status(&pool, &id, SpecStatus::InProgress, None)
            .await
            .unwrap();
        store::set_tree_id(&pool, &id, adopted).await.unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&id]);

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

        assert_eq!(launcher.requests().len(), 0, "no replacement tree is created");
        assert_eq!(launcher.waited_tree_ids(), vec![adopted]);
        assert_eq!(report.needs_owner.len(), 0, "the owner is not told about a live tree");
        assert_eq!(report.done, vec![id]);
    }

    #[tokio::test]
    async fn restart_audits_a_completed_adopted_tree() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "adopt-complete", SpecStatus::Extracted).await;
        let adopted = "tree-adopt-complete";
        store::set_status(&pool, &id, SpecStatus::InProgress, None)
            .await
            .unwrap();
        store::set_tree_id(&pool, &id, adopted).await.unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        launcher.wait_result(
            adopted,
            Ok(TreeOutcome {
                run_ids: vec!["adopted-run".to_owned()],
                lineage_roots: vec!["adopted-root".to_owned()],
                ..Default::default()
            }),
        );
        let prep = FakePreparer::new(&pool, &log, &[&id]);

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

        assert_eq!(launcher.requests().len(), 0, "the completed tree is adopted, not replaced");
        assert_eq!(launcher.waited_tree_ids(), vec![adopted]);
        assert_eq!(report.needs_owner.len(), 0, "the owner is not told twice");
        let lineages = prep.lineages.lock().unwrap();
        assert!(lineages[0].1.run_ids.contains(&"adopted-run".to_owned()));
        assert!(lineages[0].1.lineage_roots.contains(&"adopted-root".to_owned()));
    }

    #[tokio::test]
    async fn missing_adopted_tree_marks_spec_needs_owner() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "adopt-missing", SpecStatus::Extracted).await;
        let adopted = "tree-adopt-missing";
        store::set_status(&pool, &id, SpecStatus::InProgress, None)
            .await
            .unwrap();
        store::set_tree_id(&pool, &id, adopted).await.unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        launcher.wait_result(adopted, Err("tree does not exist".to_owned()));
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

        assert_eq!(launcher.requests().len(), 0, "a missing adopted tree is never replaced");
        assert_eq!(report.needs_owner, vec![id.clone()], "the owner is told once");
        let spec = store::get_spec(&pool, &id).await.unwrap().unwrap();
        let reason = spec.status_reason.unwrap_or_default();
        assert!(reason.contains(adopted), "the reason names the missing tree");
        assert!(reason.contains("does not exist"), "the reason states the missing-tree fact");
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
    async fn unknown_scope_specs_run_in_parallel() {
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
            2,
            "an unknown scope may run beside a declared scope; only declared overlap serialises"
        );
    }

    #[tokio::test]
    async fn declared_overlap_still_runs_one_after_another() {
        let pool = crate::testdb::fresh_pool().await;
        let a = add_spec(&pool, "declared-alpha", SpecStatus::Extracted).await;
        let b = add_spec(&pool, "declared-beta", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&a, &b]);
        let mut config = cfg();
        config.max_parallel = 2;
        config
            .owned_paths
            .insert(a.clone(), vec!["core/src/shared/".to_owned()]);
        config
            .owned_paths
            .insert(b.clone(), vec!["core/src/shared/mod.rs".to_owned()]);

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 2);
        assert_eq!(launcher.max_in_flight.load(Ordering::SeqCst), 1);
        assert_eq!(report.needs_owner.len(), 0);
    }

    #[tokio::test]
    async fn dependent_spec_starts_after_parallel_prerequisites_finish() {
        let pool = crate::testdb::fresh_pool().await;
        let first = add_spec(&pool, "parallel-first", SpecStatus::Extracted).await;
        let second = add_spec(&pool, "parallel-second", SpecStatus::Extracted).await;
        let dependent = add_spec(&pool, "parallel-dependent", SpecStatus::Extracted).await;
        store::add_dep(&pool, &dependent, &first, DepSource::Owner)
            .await
            .unwrap();
        store::add_dep(&pool, &dependent, &second, DepSource::Owner)
            .await
            .unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&first, &second, &dependent]);
        let mut config = cfg();
        config.max_parallel = 2;

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        let requests = launcher.requests();
        let dependent_position = requests
            .iter()
            .position(|request| request.spec_id == dependent)
            .expect("dependent tree");
        assert_eq!(requests.len(), 3);
        assert_eq!(launcher.max_in_flight.load(Ordering::SeqCst), 2);
        assert!(
            requests[..dependent_position]
                .iter()
                .any(|request| request.spec_id == first)
                && requests[..dependent_position]
                    .iter()
                    .any(|request| request.spec_id == second),
            "both prerequisites launch before their dependent"
        );
        assert_eq!(report.needs_owner.len(), 0);
    }

    #[tokio::test]
    async fn disk_below_floor_defers_the_next_spec_tree_without_failing_it() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "disk-deferred", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        launcher.set_free_disk_bytes(99);
        let prep = FakePreparer::new(&pool, &log, &[&id]);
        let mut config = cfg();
        config.min_free_disk_bytes = 100;

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 0, "the next tree waits for disk space");
        assert_eq!(report.needs_owner.len(), 0, "low disk is not an owner question");
        assert_eq!(report.blocked.len(), 0, "low disk does not fail the queued spec");
        assert_eq!(status_of(&pool, &id).await, SpecStatus::Extracted);
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
            &FinishDeps {
                launcher: &launcher,
                preparer: &prep,
            },
            &config,
            FinishScope::All {
                project_id: Some("finish-scan-project".to_owned()),
            },
        )
        .await
        .unwrap();

        assert!(
            log.lock()
                .unwrap()
                .iter()
                .any(|entry| entry == &format!("extract:{expected}"))
        );
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

        for (id, kind, title) in [
            (&done, "spec_done", "done"),
            (&blocked, "spec_blocked", "blocked"),
        ] {
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
        for source in [include_str!("finish.rs"), include_str!("daemon.rs")] {
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
            ..Default::default()
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

    #[test]
    fn the_prompt_says_the_suite_is_run_separately() {
        let criteria = [crit(1, "the test suite passes", None)];
        let prompt = review_prompt(&sample_spec(), &criteria, &ReviewMaterial::default());

        assert!(prompt.contains("runs the project's test suite separately"));
        assert!(prompt.contains("doubt"));
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
