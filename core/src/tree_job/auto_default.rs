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
