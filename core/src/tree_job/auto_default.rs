//! A default answer for a question to the owner, applied when nobody answers in time
//! (autonomy-trees spec B2, "resolve unless vetoed").
//!
//! - A question that has a safe default carries it in three nullable columns of `decisions`
//!   (migration 0301): the action, its deadline and the evidence the feed line quotes.
//! - Only two actions are admitted, because only they throw no work away and merge no unverified
//!   code: [`DefaultAction::RelaunchFreshWithDiagnosis`] and
//!   [`DefaultAction::SplitRemainingIntoNewNodes`]. Dropping the branch and accepting a test as
//!   flaky are not representable here, so no caller can attach them.
//! - [`sweep`] (called from `wrap::tick`) answers an expired default as the owner would, with the
//!   origin [`ORIGIN`], through `open::answer_and_relaunch`. One default per node and three per job
//!   at most, counted from the answered rows; past the cap the question keeps waiting for the owner
//!   and its default is cleared.
//! - Product, budget and irreversible questions never get one: their sites call plain
//!   `decisions::park_or_ask`.
//!
//! After the defaults, [`sweep`] also drives the decision runs (owner decision 2026-11-13): for
//! every question that is not owner-only the tree launches one read-only run of its own
//! orchestrator and applies its text as the answer at once, through `auto_resolve`. See the
//! "decision runs" section below.
//!
//! `split_remaining_into_new_nodes` has no mechanism to call yet: it can be attached, and the sweep
//! leaves it for the owner.
//!
//! The `autonomy:` section of the project's `speed.yaml` tunes it ([`Config`]); the kill switch is
//! `autonomy.auto_default: false`, read when the default would be applied.

use std::path::Path;

use chrono::{Duration, SecondsFormat, Utc};
use sqlx::SqlitePool;

use super::decisions::{self, Decision, DecisionError};
use super::store;
use crate::state::AppState;

/// The `origin` an applied default leaves on the answered row.
pub const ORIGIN: &str = "auto_default";

const FEED_KIND: &str = "work_tree_auto_default";

/// Two hours without an answer, the spec's default.
const DEFAULT_DEADLINE_SECS: i64 = 2 * 3600;
const DEFAULT_PER_NODE: i64 = 1;
const DEFAULT_PER_JOB: i64 = 3;

/// The only default actions there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultAction {
    /// Start the node again with fresh context and the diagnosis attached.
    RelaunchFreshWithDiagnosis,
    /// Split what is left of the node into new nodes.
    SplitRemainingIntoNewNodes,
}

impl DefaultAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RelaunchFreshWithDiagnosis => "relaunch_fresh_with_diagnosis",
            Self::SplitRemainingIntoNewNodes => "split_remaining_into_new_nodes",
        }
    }

    /// `None` for anything that is not an admitted default, e.g. `drop_branch` or `accept_flaky`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "relaunch_fresh_with_diagnosis" => Some(Self::RelaunchFreshWithDiagnosis),
            "split_remaining_into_new_nodes" => Some(Self::SplitRemainingIntoNewNodes),
            _ => None,
        }
    }

    /// Whether the sweep has a mechanism to apply it.
    fn applicable(self) -> bool {
        matches!(self, Self::RelaunchFreshWithDiagnosis)
    }

    /// What the feed line says was done.
    fn describe(self) -> &'static str {
        match self {
            Self::RelaunchFreshWithDiagnosis => {
                "relaunch the node with fresh context and the diagnosis attached"
            }
            Self::SplitRemainingIntoNewNodes => "split the rest of the node into new nodes",
        }
    }
}

/// The `autonomy:` knobs of B2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub enabled: bool,
    pub deadline_secs: i64,
    pub per_node: i64,
    pub per_job: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            deadline_secs: DEFAULT_DEADLINE_SECS,
            per_node: DEFAULT_PER_NODE,
            per_job: DEFAULT_PER_JOB,
        }
    }
}

impl Config {
    /// The `autonomy:` mapping of a `speed.yaml` over the defaults. A missing or mistyped key keeps
    /// its default; malformed text gives the defaults.
    pub fn from_yaml(text: &str) -> Self {
        let defaults = Self::default();
        let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
            return defaults;
        };
        let Some(section) = doc.get("autonomy") else {
            return defaults;
        };
        let number = |key: &str, default: i64| {
            section
                .get(key)
                .and_then(serde_yaml::Value::as_i64)
                .filter(|value| *value >= 0)
                .unwrap_or(default)
        };
        Self {
            enabled: section
                .get("auto_default")
                .and_then(serde_yaml::Value::as_bool)
                .unwrap_or(defaults.enabled),
            deadline_secs: number("auto_default_deadline_minutes", defaults.deadline_secs / 60)
                * 60,
            per_node: number("auto_default_per_node", defaults.per_node),
            per_job: number("auto_default_per_job", defaults.per_job),
        }
    }

    /// Best effort: `<root>/.nucleos/workflow/speed.yaml`, else the defaults.
    pub fn load(project_root: &str) -> Self {
        std::fs::read_to_string(Path::new(project_root).join(".nucleos/workflow/speed.yaml"))
            .map_or_else(|_| Self::default(), |text| Self::from_yaml(&text))
    }
}

/// What the sweep does with an expired default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Apply,
    /// `autonomy.auto_default` is off.
    Disabled,
    /// An unknown action, or one with no mechanism yet.
    NotApplicable,
    /// The node already had its default.
    NodeCapped,
    /// The job already had its defaults.
    JobCapped,
}

pub fn verdict(config: &Config, action: &str, node_applied: i64, job_applied: i64) -> Verdict {
    if !config.enabled {
        return Verdict::Disabled;
    }
    if !DefaultAction::parse(action).is_some_and(DefaultAction::applicable) {
        return Verdict::NotApplicable;
    }
    if node_applied >= config.per_node {
        return Verdict::NodeCapped;
    }
    if job_applied >= config.per_job {
        return Verdict::JobCapped;
    }
    Verdict::Apply
}

fn stamp(at: chrono::DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// Gives the open decision `decision_id` a default: `action` after the job's deadline. Refused,
/// writing nothing, for a decision that is answered, has one already, or belongs to no tree job.
pub async fn attach(
    pool: &SqlitePool,
    decision_id: &str,
    action: DefaultAction,
    evidence: &str,
) -> Result<bool, DecisionError> {
    let tree: Option<Option<String>> =
        sqlx::query_scalar("SELECT tree_id FROM decisions WHERE id = ?")
            .bind(decision_id)
            .fetch_optional(pool)
            .await?;
    let Some(Some(tree_id)) = tree else {
        return Ok(false);
    };
    let Some(job) = store::load(pool, &tree_id).await? else {
        return Ok(false);
    };
    let config = Config::load(&job.manifest.project_root);
    let due = stamp(Utc::now() + Duration::seconds(config.deadline_secs));
    let updated = sqlx::query(
        "UPDATE decisions SET default_action = ?, default_due = ?, default_evidence = ?
         WHERE id = ? AND status = ? AND default_action IS NULL",
    )
    .bind(action.as_str())
    .bind(due)
    .bind(evidence)
    .bind(decision_id)
    .bind(decisions::STATUS_OPEN)
    .execute(pool)
    .await?;
    Ok(updated.rows_affected() > 0)
}

/// [`decisions::park_or_ask`] for a question whose default is `action`, backed by `evidence`.
pub async fn park_or_ask_with_default(
    pool: &SqlitePool,
    tree_id: &str,
    node_id: &str,
    question: &str,
    action: DefaultAction,
    evidence: &str,
) -> Result<Option<Decision>, DecisionError> {
    let asked = decisions::park_or_ask(pool, tree_id, node_id, question).await?;
    if let Some(decision) = &asked {
        attach(pool, &decision.id, action, evidence).await?;
    }
    Ok(asked)
}

type Expired = (
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
);

/// One pass over the open decisions whose default is due. A failure on one is logged and stops no
/// other. Safe to call again at any time: an applied default is an answered row.
pub async fn sweep(state: &AppState) {
    let pool = &state.pool;
    let due: Result<Vec<Expired>, _> = sqlx::query_as(
        "SELECT id, tree_id, node_id, question, default_action, default_evidence FROM decisions
         WHERE status = ? AND default_action IS NOT NULL AND default_due <= ?
         ORDER BY default_due, id",
    )
    .bind(decisions::STATUS_OPEN)
    .bind(stamp(Utc::now()))
    .fetch_all(pool)
    .await;
    let due = match due {
        Ok(due) => due,
        Err(error) => {
            tracing::warn!(%error, "could not list the due defaults");
            return;
        }
    };
    for row in due {
        let id = row.0.clone();
        if let Err(error) = apply_one(state, row).await {
            tracing::warn!(decision = %id, %error, "could not apply a default answer");
        }
    }
    decision_run_tick(state).await;
}

async fn applied_count(
    pool: &SqlitePool,
    column: &'static str,
    value: &str,
) -> Result<i64, sqlx::Error> {
    // `AssertSqlSafe`: `column` is one of two literal column names, and the value is bound.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM decisions WHERE origin = ? AND {column} = ?"
    )))
    .bind(ORIGIN)
    .bind(value)
    .fetch_one(pool)
    .await
}

async fn apply_one(state: &AppState, row: Expired) -> Result<(), String> {
    let pool = &state.pool;
    let (id, tree_id, node_id, question, action_text, evidence) = row;
    let (Some(tree_id), Some(node_id)) = (tree_id, node_id) else {
        return Ok(());
    };
    let Some(job) = store::load(pool, &tree_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let config = Config::load(&job.manifest.project_root);
    let node_applied = applied_count(pool, "node_id", &node_id)
        .await
        .map_err(|e| e.to_string())?;
    let job_applied = applied_count(pool, "tree_id", &tree_id)
        .await
        .map_err(|e| e.to_string())?;
    match verdict(&config, &action_text, node_applied, job_applied) {
        Verdict::Apply => {}
        Verdict::Disabled | Verdict::NotApplicable => return Ok(()),
        Verdict::NodeCapped | Verdict::JobCapped => {
            // The ceiling is spent: the question waits for the owner and says no default is coming.
            sqlx::query(
                "UPDATE decisions SET default_action = NULL, default_due = NULL,
                        default_evidence = NULL WHERE id = ? AND status = ?",
            )
            .bind(&id)
            .bind(decisions::STATUS_OPEN)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    let action = DefaultAction::parse(&action_text).ok_or("unknown action")?;
    let evidence = evidence.unwrap_or_default();
    let text = format!(
        "No answer came in time, so the default applies: {}. Question: {question} Evidence: \
         {evidence} Diagnose the cause first, then go on.",
        action.describe()
    );
    match super::open::answer_and_relaunch(state, &id, &text, ORIGIN).await {
        Ok(_) => {}
        // The owner answered first.
        Err(DecisionError::AlreadyDecided(_)) => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    let line = format!(
        "{}: default applied — node {node_id}, decision {id} — question: {question} — evidence: \
         {evidence} — action: {}",
        job.goal,
        action.describe()
    );
    crate::feed::append(
        pool,
        job.project_id.as_deref(),
        FEED_KIND,
        &line,
        None,
        None,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ----- decision runs (owner decision 2026-11-13: the tree decides itself) -----
//
// For every question that is not owner-only the tree launches ONE read-only run of its own
// orchestrator, and that run's text is applied as the answer at once through
// `auto_resolve::resolve_decision` (the normal `decisions::answer` path, then
// `relaunch::answer_and_relaunch`, so the text lands in the relaunched node's prompt). There is no
// deadline. The flag and the caps are `auto_resolve`'s (`autonomy.tree_decides_itself`,
// `decision_runs_per_node`, `decision_runs_per_job`); runs in flight count against them, and when
// they are spent the question falls through to the owner. A run is recorded in `tree_transitions`
// as `(node, "decision_run", decision id)` with the run id as result, so a decision gets one run,
// ever; `failed` (or an empty reservation) leaves the question to the owner for good, which is also
// how a stalled-tree question ends up with the owner after its decision run failed.

const DECISION_RUN: &str = "decision_run";
const RUN_FAILED: &str = "failed";
/// An answer is appended to a prompt: keep it a directive, not an essay.
const MAX_ANSWER_CHARS: usize = 4000;
/// The standing mandate every decision run carries.
const MANDATE: &str = "develop this spec until it is done";

/// The kind of a question, read from its wording (`decisions` has no kind column). Anything not
/// recognised as owner-only is `ordinary`.
pub(super) fn classify(question: &str) -> &'static str {
    let lower = question.to_lowercase();
    if lower.contains("nucleos.tests.yaml") || lower.contains("scripts/gates.sh") {
        "governance_file"
    } else if lower.contains("widen") && lower.contains("path") {
        "widen_paths"
    } else if lower.contains("budget") {
        "budget_raise"
    } else if lower.contains("ticket #") || lower.contains("pick an owner") {
        "ticket_ownership"
    } else {
        "ordinary"
    }
}

/// The prompt of a decision run.
pub(super) fn decision_prompt(
    goal: &str,
    node_id: &str,
    branch_state: &str,
    question: &str,
) -> String {
    format!(
        "You are the orchestrator of a NucleOS tree, deciding for the owner. Standing mandate: \
         {MANDATE}.\n\nTree goal: {goal}\nNode: {node_id}\nBranch state: {branch_state}\n\n\
         Question the tree is stuck on:\n{question}\n\n\
         Decide it yourself from the goal and the state. Do not edit anything. Reply with the \
         answer only: a short, concrete instruction for the node, no preamble."
    )
}

/// The answer text of a finished run, bounded; `None` when it said nothing usable.
pub(super) fn answer_of(status: &str, stdout: Option<&str>) -> Option<String> {
    if status != "completed" {
        return None;
    }
    let text = stdout?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(MAX_ANSWER_CHARS).collect())
}

/// One pass over the open tree decisions. A failure on one is logged and stops no other.
pub(super) async fn decision_run_tick(state: &AppState) {
    let rows: Result<Vec<(String, String, String, String)>, _> = sqlx::query_as(
        "SELECT id, tree_id, node_id, question FROM decisions
         WHERE status = ? AND tree_id IS NOT NULL AND node_id IS NOT NULL ORDER BY at, id",
    )
    .bind(decisions::STATUS_OPEN)
    .fetch_all(&state.pool)
    .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "could not list the open decisions for decision runs");
            return;
        }
    };
    for (id, tree_id, node_id, question) in rows {
        if let Err(error) = decision_run_step(state, &id, &tree_id, &node_id, &question).await {
            tracing::warn!(decision = %id, %error, "decision run step failed");
        }
    }
}

async fn run_recorded(
    pool: &SqlitePool,
    node_id: &str,
    decision_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<Option<String>> = sqlx::query_scalar(
        "SELECT result FROM tree_transitions WHERE node_id = ? AND transition = ? AND key = ?",
    )
    .bind(node_id)
    .bind(DECISION_RUN)
    .bind(decision_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Option::unwrap_or_default))
}

async fn set_run_result(
    pool: &SqlitePool,
    node_id: &str,
    decision_id: &str,
    result: &str,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE tree_transitions SET result = ? WHERE node_id = ? AND transition = ? AND key = ?",
    )
    .bind(result)
    .bind(node_id)
    .bind(DECISION_RUN)
    .bind(decision_id)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Self-answers so far for a node or a job: answered with origin `auto`/`driver`, plus decision
/// runs still in flight.
async fn runs_spent(
    pool: &SqlitePool,
    column: &'static str,
    value: &str,
) -> Result<i64, sqlx::Error> {
    // `AssertSqlSafe`: `column` is one of two literal column names, and the value is bound.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM decisions d WHERE d.{column} = ? AND (
            d.origin IN ('auto', 'driver')
            OR (d.status = 'open' AND EXISTS (
                SELECT 1 FROM tree_transitions t
                WHERE t.node_id = d.node_id AND t.transition = '{DECISION_RUN}'
                  AND t.key = d.id
                  AND COALESCE(t.result, '') NOT IN ('', '{RUN_FAILED}'))))"
    )))
    .bind(value)
    .fetch_one(pool)
    .await
}

async fn decision_run_step(
    state: &AppState,
    id: &str,
    tree_id: &str,
    node_id: &str,
    question: &str,
) -> Result<(), String> {
    let recorded = run_recorded(&state.pool, node_id, id)
        .await
        .map_err(|e| e.to_string())?;
    match recorded.as_deref() {
        // Failed, or reserved and never stored: the owner keeps it.
        Some("") | Some(RUN_FAILED) => Ok(()),
        Some(run_id) => collect_decision_run(state, id, node_id, run_id).await,
        None => launch_decision_run(state, id, tree_id, node_id, question).await,
    }
}

async fn launch_decision_run(
    state: &AppState,
    id: &str,
    tree_id: &str,
    node_id: &str,
    question: &str,
) -> Result<(), String> {
    use super::auto_resolve as resolve;
    let pool = &state.pool;
    let Some(job) = store::load(pool, tree_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let config = resolve::Config::load(&job.manifest.project_root);
    let node_spent = runs_spent(pool, "node_id", node_id)
        .await
        .map_err(|e| e.to_string())?;
    let job_spent = runs_spent(pool, "tree_id", tree_id)
        .await
        .map_err(|e| e.to_string())?;
    if resolve::verdict(&config, classify(question), node_spent, job_spent)
        != resolve::Verdict::Resolve
    {
        return Ok(());
    }
    let claimed = sqlx::query(
        "INSERT INTO tree_transitions (node_id, transition, key, result, created_at)
         VALUES (?, ?, ?, '', ?) ON CONFLICT (node_id, transition, key) DO NOTHING",
    )
    .bind(node_id)
    .bind(DECISION_RUN)
    .bind(id)
    .bind(stamp(Utc::now()))
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?
    .rows_affected();
    if claimed == 0 {
        return Ok(());
    }
    let branch_state = job
        .manifest
        .branches
        .values()
        .find(|branch| branch.node_id.as_deref() == Some(node_id))
        .map_or_else(
            || "no branch".to_owned(),
            |branch| format!("{} is {}", branch.name, branch.state),
        );
    let text = decision_prompt(&job.goal, node_id, &branch_state, question);
    // "shadow" is plan-only: no worktree, no edits.
    let started = crate::execution::runs::create_run_inner(
        state,
        text,
        job.project_id.clone(),
        None,
        "shadow",
        false,
    )
    .await;
    match started {
        Ok(run_id) => set_run_result(pool, node_id, id, &run_id.to_string()).await,
        Err(error) => {
            set_run_result(pool, node_id, id, RUN_FAILED).await?;
            Err(format!("could not start the decision run: {error:?}"))
        }
    }
}

async fn collect_decision_run(
    state: &AppState,
    id: &str,
    node_id: &str,
    run_id: &str,
) -> Result<(), String> {
    use super::auto_resolve as resolve;
    let pool = &state.pool;
    if super::relaunch::run_is_live(pool, run_id)
        .await
        .map_err(|e| e.to_string())?
    {
        return Ok(());
    }
    let Ok(number) = run_id.parse::<i64>() else {
        return set_run_result(pool, node_id, id, RUN_FAILED).await;
    };
    let run: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT status, stdout FROM runs WHERE id = ?")
            .bind(number)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
    let Some(answer) = run.and_then(|(status, stdout)| answer_of(&status, stdout.as_deref()))
    else {
        return set_run_result(pool, node_id, id, RUN_FAILED).await;
    };
    // `resolve_decision` re-checks the flag and the caps against answered rows.
    match resolve::resolve_decision(state, id, "ordinary", &answer, resolve::Origin::Auto).await {
        Ok(resolve::Outcome::Resolved(_) | resolve::Outcome::AlreadyDecided) => Ok(()),
        Ok(resolve::Outcome::Declined(_) | resolve::Outcome::NotATreeDecision) => {
            set_run_result(pool, node_id, id, RUN_FAILED).await
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod decision_run_tests {
    //! Tests of the decision runs (owner decision 2026-11-13).

    use super::*;
    use crate::tree_job::auto_resolve;
    use crate::tree_job::model::Manifest;
    use crate::tree_job::store::TreeJob;

    #[test]
    fn owner_only_questions_are_recognised_from_their_wording() {
        let budget = "Node n1 was not started again: the tree budget does not permit it (x). \
                      Raise the budget, or say how the node should go on.";
        let ticket =
            "Ticket #5 k was launched twice and nobody fixed it. Pick an owner or close it.";
        assert_eq!(classify(budget), "budget_raise");
        assert_eq!(classify(ticket), "ticket_ownership");
        assert_eq!(classify("May I edit scripts/gates.sh?"), "governance_file");
        assert_eq!(classify("Edit nucleos.tests.yaml?"), "governance_file");
        assert_eq!(
            classify("Widen the declared paths to core/?"),
            "widen_paths"
        );
        for kind in [
            "governance_file",
            "widen_paths",
            "budget_raise",
            "ticket_ownership",
        ] {
            assert!(auto_resolve::is_owner_only(kind), "{kind}");
        }
    }

    #[test]
    fn everything_else_is_ordinary_including_a_stalled_tree() {
        assert_eq!(classify("which way?"), "ordinary");
        assert_eq!(
            classify("The tree has not moved for 30 minutes: nothing runs. What should it do?"),
            "ordinary"
        );
        assert!(!auto_resolve::is_owner_only("ordinary"));
    }

    #[test]
    fn the_prompt_carries_mandate_question_and_state() {
        let text = decision_prompt("Ship B", "n7", "b1 is parked", "which way?");
        for part in [MANDATE, "Ship B", "n7", "b1 is parked", "which way?"] {
            assert!(text.contains(part), "{part}");
        }
    }

    #[test]
    fn only_a_completed_run_with_text_answers() {
        assert_eq!(
            answer_of("completed", Some("  go left \n")).as_deref(),
            Some("go left")
        );
        assert_eq!(answer_of("completed", Some("  ")), None);
        assert_eq!(answer_of("completed", None), None);
        assert_eq!(answer_of("failed", Some("go left")), None);
        let long = "x".repeat(MAX_ANSWER_CHARS + 50);
        assert_eq!(
            answer_of("completed", Some(&long)).unwrap().chars().count(),
            MAX_ANSWER_CHARS
        );
    }

    // ----- database -----

    const TREE: &str = "dr-job";
    const NODE: &str = "dr-node";
    const AT: &str = "2020-01-01T00:00:00+00:00";

    async fn state() -> AppState {
        let (state, _runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(std::time::Duration::from_secs(60)),
            std::time::Duration::from_secs(600),
        )
        .await;
        state
    }

    async fn seed_job(pool: &SqlitePool) {
        let manifest = Manifest::new(TREE, "feat/dr", "Decide it", "C:/nowhere", &[]);
        let job = TreeJob {
            id: TREE.to_owned(),
            root_node_id: "dr-root".to_owned(),
            project_id: None,
            feature_branch: Some("feat/dr".to_owned()),
            goal: "Decide it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: AT.to_owned(),
            updated_at: AT.to_owned(),
        };
        store::save(pool, &job).await.unwrap();
    }

    async fn seed_open(pool: &SqlitePool, id: &str, question: &str) {
        sqlx::query(
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status)
             VALUES (?, ?, ?, ?, NULL, 'agent', ?, 'open')",
        )
        .bind(id)
        .bind(TREE)
        .bind(NODE)
        .bind(question)
        .bind(AT)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_transition(pool: &SqlitePool, id: &str, result: &str) {
        sqlx::query(
            "INSERT INTO tree_transitions (node_id, transition, key, result, created_at)
             VALUES (?, 'decision_run', ?, ?, ?)",
        )
        .bind(NODE)
        .bind(id)
        .bind(result)
        .bind(AT)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_run(pool: &SqlitePool, status: &str, stdout: Option<&str>) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, stdout, mode, created_at)
             VALUES ('p', ?, ?, 'shadow', ?)",
        )
        .bind(status)
        .bind(stdout)
        .bind(AT)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn row(pool: &SqlitePool, id: &str) -> (String, Option<String>, Option<String>) {
        sqlx::query_as("SELECT status, answer, origin FROM decisions WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_finished_decision_run_is_applied_as_the_answer_at_once() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(&state.pool, "d-1", "which way?").await;
        let run = seed_run(&state.pool, "completed", Some("take the left branch")).await;
        seed_transition(&state.pool, "d-1", &run.to_string()).await;

        decision_run_tick(&state).await;

        let (status, answer, origin) = row(&state.pool, "d-1").await;
        assert_eq!(status, "answered");
        assert_eq!(answer.as_deref(), Some("take the left branch"));
        assert_eq!(origin.as_deref(), Some("auto"));
    }

    #[tokio::test]
    async fn a_failed_decision_run_leaves_the_question_with_the_owner_for_good() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(&state.pool, "d-1", "which way?").await;
        let run = seed_run(&state.pool, "failed", Some("boom")).await;
        seed_transition(&state.pool, "d-1", &run.to_string()).await;

        decision_run_tick(&state).await;
        decision_run_tick(&state).await;

        assert_eq!(row(&state.pool, "d-1").await.0, "open");
        let result = run_recorded(&state.pool, NODE, "d-1").await.unwrap();
        assert_eq!(result.as_deref(), Some(RUN_FAILED));
    }

    #[tokio::test]
    async fn an_owner_only_question_gets_no_run() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(
            &state.pool,
            "d-1",
            "Raise the budget, or say how the node should go on.",
        )
        .await;

        decision_run_tick(&state).await;

        assert_eq!(run_recorded(&state.pool, NODE, "d-1").await.unwrap(), None);
        assert_eq!(row(&state.pool, "d-1").await.0, "open");
    }

    #[tokio::test]
    async fn spent_caps_start_no_run_and_runs_in_flight_count() {
        let state = state().await;
        seed_job(&state.pool).await;
        // Three runs in flight on the node: the per-node default cap is spent.
        for n in 0..3 {
            let id = format!("d-{n}");
            seed_open(&state.pool, &id, "which way?").await;
            let run = seed_run(&state.pool, "running", None).await;
            seed_transition(&state.pool, &id, &run.to_string()).await;
        }
        assert_eq!(runs_spent(&state.pool, "node_id", NODE).await.unwrap(), 3);
        seed_open(&state.pool, "d-9", "which way?").await;

        decision_run_tick(&state).await;

        assert_eq!(run_recorded(&state.pool, NODE, "d-9").await.unwrap(), None);
        assert_eq!(row(&state.pool, "d-9").await.0, "open");
    }

    #[tokio::test]
    async fn a_run_still_live_is_left_alone() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(&state.pool, "d-1", "which way?").await;
        let run = seed_run(&state.pool, "running", None).await;
        seed_transition(&state.pool, "d-1", &run.to_string()).await;

        decision_run_tick(&state).await;

        assert_eq!(row(&state.pool, "d-1").await.0, "open");
        let result = run_recorded(&state.pool, NODE, "d-1").await.unwrap();
        assert_eq!(result, Some(run.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_admitted_actions_parse() {
        assert_eq!(
            DefaultAction::parse("relaunch_fresh_with_diagnosis"),
            Some(DefaultAction::RelaunchFreshWithDiagnosis)
        );
        assert_eq!(
            DefaultAction::parse("split_remaining_into_new_nodes"),
            Some(DefaultAction::SplitRemainingIntoNewNodes)
        );
        for never in ["drop_branch", "accept_flaky", "accept_test_as_flaky", ""] {
            assert_eq!(DefaultAction::parse(never), None, "{never}");
        }
    }

    #[test]
    fn the_defaults_are_two_hours_one_per_node_three_per_job_on() {
        let config = Config::default();
        assert!(config.enabled);
        assert_eq!(config.deadline_secs, 7200);
        assert_eq!((config.per_node, config.per_job), (1, 3));
    }

    #[test]
    fn the_autonomy_section_overrides_and_bad_text_keeps_the_defaults() {
        let config = Config::from_yaml(
            "autonomy:\n  auto_default: false\n  auto_default_deadline_minutes: 30\n  \
             auto_default_per_node: 2\n  auto_default_per_job: 5\n",
        );
        assert_eq!(
            config,
            Config {
                enabled: false,
                deadline_secs: 1800,
                per_node: 2,
                per_job: 5
            }
        );
        assert_eq!(
            Config::from_yaml("nodes:\n  max_depth: 3\n"),
            Config::default()
        );
        assert_eq!(Config::from_yaml(": : :"), Config::default());
        let mistyped = Config::from_yaml("autonomy:\n  auto_default: maybe\n");
        assert!(mistyped.enabled);
    }

    #[test]
    fn the_verdict_applies_a_relaunch_under_the_caps() {
        let config = Config::default();
        assert_eq!(
            verdict(&config, "relaunch_fresh_with_diagnosis", 0, 2),
            Verdict::Apply
        );
    }

    #[test]
    fn the_verdict_stops_at_the_node_cap_and_at_the_job_cap() {
        let config = Config::default();
        let relaunch = "relaunch_fresh_with_diagnosis";
        assert_eq!(verdict(&config, relaunch, 1, 1), Verdict::NodeCapped);
        assert_eq!(verdict(&config, relaunch, 0, 3), Verdict::JobCapped);
    }

    #[test]
    fn the_kill_switch_wins_over_everything() {
        let config = Config {
            enabled: false,
            ..Config::default()
        };
        assert_eq!(
            verdict(&config, "relaunch_fresh_with_diagnosis", 0, 0),
            Verdict::Disabled
        );
    }

    #[test]
    fn a_split_or_an_unknown_action_is_not_applied() {
        let config = Config::default();
        for action in [
            "split_remaining_into_new_nodes",
            "drop_branch",
            "accept_flaky",
        ] {
            assert_eq!(
                verdict(&config, action, 0, 0),
                Verdict::NotApplicable,
                "{action}"
            );
        }
    }

    // ----- database -----

    use crate::tree_job::model::Manifest;
    use crate::tree_job::store::TreeJob;

    const TREE: &str = "ad-job";
    const NODE: &str = "ad-node";
    const OLD: &str = "2020-01-01T00:00:00+00:00";
    const RELAUNCH: &str = "relaunch_fresh_with_diagnosis";

    async fn seed_job(pool: &SqlitePool) {
        let manifest = Manifest::new(TREE, "feat/ad", "Default it", "C:/nowhere", &[]);
        let job = TreeJob {
            id: TREE.to_owned(),
            root_node_id: "ad-root".to_owned(),
            project_id: None,
            feature_branch: Some("feat/ad".to_owned()),
            goal: "Default it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: OLD.to_owned(),
            updated_at: OLD.to_owned(),
        };
        store::save(pool, &job).await.unwrap();
    }

    /// An open decision on `node`; `due` and `action` set its default when given.
    async fn seed_open(pool: &SqlitePool, id: &str, node: &str, due: Option<&str>, action: &str) {
        sqlx::query(
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status,
                                    default_action, default_due, default_evidence)
             VALUES (?, ?, ?, 'which way?', NULL, 'agent', ?, 'open', ?, ?, 'two crashes')",
        )
        .bind(id)
        .bind(TREE)
        .bind(node)
        .bind(OLD)
        .bind(due.map(|_| action))
        .bind(due)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_applied(pool: &SqlitePool, id: &str, node: &str) {
        seed_open(pool, id, node, None, RELAUNCH).await;
        sqlx::query(
            "UPDATE decisions SET status = 'answered', origin = ?, answer = 'x' WHERE id = ?",
        )
        .bind(ORIGIN)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn status_of(pool: &SqlitePool, id: &str) -> (String, Option<String>, Option<String>) {
        sqlx::query_as("SELECT status, origin, default_action FROM decisions WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn state() -> AppState {
        let (state, _runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(std::time::Duration::from_secs(60)),
            std::time::Duration::from_secs(600),
        )
        .await;
        state
    }

    async fn auto_default_lines(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed WHERE kind = ? ORDER BY id")
            .bind(FEED_KIND)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn attach_sets_the_action_the_deadline_and_the_evidence_once() {
        let pool = crate::testdb::fresh_pool().await;
        seed_job(&pool).await;
        seed_open(&pool, "d-1", NODE, None, RELAUNCH).await;

        let first = attach(
            &pool,
            "d-1",
            DefaultAction::RelaunchFreshWithDiagnosis,
            "evidence",
        )
        .await
        .unwrap();
        let again = attach(
            &pool,
            "d-1",
            DefaultAction::SplitRemainingIntoNewNodes,
            "other",
        )
        .await
        .unwrap();

        assert!(first);
        assert!(!again, "a decision keeps its first default");
        let (action, due, evidence): (String, String, String) = sqlx::query_as(
            "SELECT default_action, default_due, default_evidence FROM decisions WHERE id = 'd-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(action, RELAUNCH);
        assert_eq!(evidence, "evidence");
        let due = chrono::DateTime::parse_from_rfc3339(&due).unwrap();
        let wait = (due.with_timezone(&Utc) - Utc::now()).num_seconds();
        assert!((7100..=7200).contains(&wait), "two hours ahead, got {wait}");
    }

    #[tokio::test]
    async fn attach_refuses_an_ownerless_or_answered_decision() {
        let pool = crate::testdb::fresh_pool().await;
        seed_job(&pool).await;
        seed_applied(&pool, "d-done", NODE).await;
        decisions::ask_ownerless(&pool, "ticket-1", "park?")
            .await
            .unwrap();

        let answered = attach(
            &pool,
            "d-done",
            DefaultAction::RelaunchFreshWithDiagnosis,
            "e",
        )
        .await;
        let ownerless = attach(
            &pool,
            "ticket-1",
            DefaultAction::RelaunchFreshWithDiagnosis,
            "e",
        )
        .await;
        let unknown = attach(
            &pool,
            "nope",
            DefaultAction::RelaunchFreshWithDiagnosis,
            "e",
        )
        .await;

        assert!(!answered.unwrap());
        assert!(!ownerless.unwrap());
        assert!(!unknown.unwrap());
    }

    #[tokio::test]
    async fn the_sweep_applies_an_expired_default_as_an_answer_and_writes_the_feed_line() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(&state.pool, "d-1", NODE, Some(OLD), RELAUNCH).await;

        sweep(&state).await;

        let (status, origin, _) = status_of(&state.pool, "d-1").await;
        assert_eq!(
            (status.as_str(), origin.as_deref()),
            ("answered", Some(ORIGIN))
        );
        let lines = auto_default_lines(&state.pool).await;
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("which way?"), "{}", lines[0]);
        assert!(lines[0].contains("two crashes"), "{}", lines[0]);
        assert!(lines[0].contains("fresh context"), "{}", lines[0]);
    }

    #[tokio::test]
    async fn the_sweep_leaves_a_default_that_is_not_due_yet() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(
            &state.pool,
            "d-1",
            NODE,
            Some("2999-01-01T00:00:00+00:00"),
            RELAUNCH,
        )
        .await;

        sweep(&state).await;

        assert_eq!(status_of(&state.pool, "d-1").await.0, "open");
        assert!(auto_default_lines(&state.pool).await.is_empty());
    }

    #[tokio::test]
    async fn the_sweep_never_answers_a_question_without_a_default() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(&state.pool, "d-1", NODE, None, RELAUNCH).await;

        sweep(&state).await;

        assert_eq!(status_of(&state.pool, "d-1").await.0, "open");
    }

    #[tokio::test]
    async fn the_node_cap_clears_the_second_default_and_the_question_waits_for_the_owner() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_applied(&state.pool, "d-old", NODE).await;
        seed_open(&state.pool, "d-2", NODE, Some(OLD), RELAUNCH).await;

        sweep(&state).await;

        let (status, _, action) = status_of(&state.pool, "d-2").await;
        assert_eq!(status, "open");
        assert_eq!(action, None, "no default is left to wait for");
        assert!(auto_default_lines(&state.pool).await.is_empty());
    }

    #[tokio::test]
    async fn the_job_cap_stops_a_fourth_default_on_another_node() {
        let state = state().await;
        seed_job(&state.pool).await;
        for (id, node) in [("d-a", "n-a"), ("d-b", "n-b"), ("d-c", "n-c")] {
            seed_applied(&state.pool, id, node).await;
        }
        seed_open(&state.pool, "d-4", "n-d", Some(OLD), RELAUNCH).await;

        sweep(&state).await;

        let (status, _, action) = status_of(&state.pool, "d-4").await;
        assert_eq!((status.as_str(), action), ("open", None));
    }

    #[tokio::test]
    async fn the_sweep_keeps_a_split_default_for_the_owner() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(
            &state.pool,
            "d-1",
            NODE,
            Some(OLD),
            "split_remaining_into_new_nodes",
        )
        .await;

        sweep(&state).await;

        assert_eq!(status_of(&state.pool, "d-1").await.0, "open");
        assert!(auto_default_lines(&state.pool).await.is_empty());
    }

    #[tokio::test]
    async fn the_kill_switch_in_speed_yaml_stops_the_sweep() {
        let state = state().await;
        let root = tempfile::tempdir().unwrap();
        let workflow = root.path().join(".nucleos/workflow");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::write(
            workflow.join("speed.yaml"),
            "autonomy:\n  auto_default: false\n",
        )
        .unwrap();
        let mut manifest = Manifest::new(
            TREE,
            "feat/ad",
            "Default it",
            &root.path().to_string_lossy(),
            &[],
        );
        manifest.state = "open".to_owned();
        let job = TreeJob {
            id: TREE.to_owned(),
            root_node_id: "ad-root".to_owned(),
            project_id: None,
            feature_branch: None,
            goal: "Default it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: OLD.to_owned(),
            updated_at: OLD.to_owned(),
        };
        store::save(&state.pool, &job).await.unwrap();
        seed_open(&state.pool, "d-1", NODE, Some(OLD), RELAUNCH).await;

        sweep(&state).await;

        assert_eq!(status_of(&state.pool, "d-1").await.0, "open");
        assert!(auto_default_lines(&state.pool).await.is_empty());
    }

    #[tokio::test]
    async fn an_owner_answer_before_the_sweep_wins() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(&state.pool, "d-1", NODE, Some(OLD), RELAUNCH).await;
        decisions::answer(&state.pool, "d-1", "do it my way", "owner")
            .await
            .unwrap();

        sweep(&state).await;

        let (_, origin, _) = status_of(&state.pool, "d-1").await;
        assert_eq!(origin.as_deref(), Some("owner"));
        assert!(auto_default_lines(&state.pool).await.is_empty());
    }
}
