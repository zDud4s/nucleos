//! A tree that answers its own decisions (owner decision 2026-11-13): a question determined by the
//! state of the tree and the original mandate is answered by the tree's own orchestrator, and only a
//! closed list of kinds ([`DecisionKind::is_owner_only`]) still goes to the owner.
//!
//! This module is the small API the sibling branches call:
//!
//! - [`Config`] reads the `autonomy:` section of the project's `speed.yaml`
//!   (`tree_decides_itself`, `decision_runs_per_node`, `decision_runs_per_job`).
//! - [`is_owner_only`] and [`verdict`] are PURE: whether a kind may be self-answered at all, and
//!   whether the flag and the caps let one more answer through. A caller about to open a decision
//!   asks [`verdict`] first and, on [`Verdict::Resolve`], answers instead of asking the owner.
//! - [`resolve_decision`] answers an OPEN decision through [`super::relaunch::answer_and_relaunch`]
//!   (so the normal [`decisions::answer`] path runs and the node starts again) and writes exactly
//!   one feed line.

use std::path::Path;

use sqlx::SqlitePool;

use super::decisions::{self, Decision, DecisionError};
use super::store;
use crate::state::AppState;

const FEED_KIND: &str = "work_tree_auto_resolve";
const DEFAULT_PER_NODE: i64 = 3;
const DEFAULT_PER_JOB: i64 = 10;

/// Who answers: the `origin` stored on the decision row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The tree-job driver answered from state alone.
    Driver,
    /// The tree's own orchestrator answered from the original mandate.
    Auto,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Driver => "driver",
            Origin::Auto => "auto",
        }
    }
}

/// What a decision is about. The first five are owner-only; the last is everything the tree may
/// settle itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    /// A governance file: `nucleos.tests.yaml`, `scripts/gates.sh`, and the like.
    GovernanceFile,
    /// Widening the declared paths of a node or branch.
    WidenPaths,
    /// Raising a budget (the `refusal_question` family).
    BudgetRaise,
    /// Who owns a ticket (`task_claims`, `launch.rs`).
    TicketOwnership,
    /// A tree stalled after its decision run failed.
    StalledAfterDecisionRun,
    /// Anything the state and the original mandate determine.
    Ordinary,
}

impl DecisionKind {
    /// Parses the kind a caller names. Unknown text is `None`, and the caller must treat that as
    /// owner-only: an unrecognised question is never answered by the tree.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "governance_file" => Self::GovernanceFile,
            "widen_paths" => Self::WidenPaths,
            "budget_raise" => Self::BudgetRaise,
            "ticket_ownership" => Self::TicketOwnership,
            "stalled_after_decision_run" => Self::StalledAfterDecisionRun,
            "ordinary" => Self::Ordinary,
            _ => return None,
        })
    }

    /// PURE: whether only the owner may answer this kind. See [`is_owner_only`].
    pub fn is_owner_only(self) -> bool {
        !matches!(self, Self::Ordinary)
    }
}

/// PURE: whether `kind` is owner-only. Text that names no known kind is owner-only too (the closed
/// side).
pub fn is_owner_only(kind: &str) -> bool {
    DecisionKind::parse(kind).is_none_or(DecisionKind::is_owner_only)
}

/// The `autonomy:` knobs of this feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// `tree_decides_itself`: on by default.
    pub enabled: bool,
    /// `decision_runs_per_node`: self-answers one node may get.
    pub per_node: i64,
    /// `decision_runs_per_job`: self-answers one tree job may get.
    pub per_job: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
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
        let Some(section) = serde_yaml::from_str::<serde_yaml::Value>(text)
            .ok()
            .and_then(|doc| doc.get("autonomy").cloned())
        else {
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
                .get("tree_decides_itself")
                .and_then(serde_yaml::Value::as_bool)
                .unwrap_or(defaults.enabled),
            per_node: number("decision_runs_per_node", defaults.per_node),
            per_job: number("decision_runs_per_job", defaults.per_job),
        }
    }

    /// Best effort: `<root>/.nucleos/workflow/speed.yaml`, else the defaults.
    pub fn load(project_root: &str) -> Self {
        std::fs::read_to_string(Path::new(project_root).join(".nucleos/workflow/speed.yaml"))
            .map_or_else(|_| Self::default(), |text| Self::from_yaml(&text))
    }
}

/// Whether the tree may answer one more decision itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Resolve,
    /// `autonomy.tree_decides_itself` is off.
    Disabled,
    /// The kind is owner-only, or unknown.
    OwnerOnly,
    /// The node already had its self-answers.
    NodeCapped,
    /// The job already had its self-answers.
    JobCapped,
}

/// PURE: the flag, the owner-only list and the caps, in that order. `node_answered` and
/// `job_answered` count the decisions already answered with origin `auto` or `driver`.
pub fn verdict(config: &Config, kind: &str, node_answered: i64, job_answered: i64) -> Verdict {
    if !config.enabled {
        return Verdict::Disabled;
    }
    if is_owner_only(kind) {
        return Verdict::OwnerOnly;
    }
    if node_answered >= config.per_node {
        return Verdict::NodeCapped;
    }
    if job_answered >= config.per_job {
        return Verdict::JobCapped;
    }
    Verdict::Resolve
}

/// What [`resolve_decision`] did.
#[derive(Debug)]
pub enum Outcome {
    /// Answered; one feed line written.
    Resolved(Box<Decision>),
    /// Not answered: the owner (or nobody yet) keeps the question.
    Declined(Verdict),
    /// The decision belongs to no tree job, or its job is gone: not ours to answer.
    NotATreeDecision,
    /// Somebody answered first; nothing changed and no line was written.
    AlreadyDecided,
}

async fn answered_count(
    pool: &SqlitePool,
    column: &'static str,
    value: &str,
) -> Result<i64, sqlx::Error> {
    // `AssertSqlSafe`: `column` is one of two literal column names, and the value is bound.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM decisions WHERE origin IN (?, ?) AND {column} = ?"
    )))
    .bind(Origin::Auto.as_str())
    .bind(Origin::Driver.as_str())
    .bind(value)
    .fetch_one(pool)
    .await
}

/// Answers the OPEN decision `decision_id` with `answer_text` on the tree's behalf, when
/// [`verdict`] lets it through.
///
/// `kind` is the text of a [`DecisionKind`]; anything else is owner-only. The answer goes through
/// [`super::relaunch::answer_and_relaunch`], so the node starts again as for an owner answer, and
/// exactly one feed line records it. A declined or lost race writes nothing.
pub async fn resolve_decision(
    state: &AppState,
    decision_id: &str,
    kind: &str,
    answer_text: &str,
    origin: Origin,
) -> Result<Outcome, DecisionError> {
    let pool = &state.pool;
    let row: Option<(Option<String>, Option<String>, String, String)> =
        sqlx::query_as("SELECT tree_id, node_id, question, status FROM decisions WHERE id = ?")
            .bind(decision_id)
            .fetch_optional(pool)
            .await?;
    let Some((tree_id, node_id, question, status)) = row else {
        return Err(DecisionError::NotFound(decision_id.to_owned()));
    };
    if status != decisions::STATUS_OPEN {
        return Ok(Outcome::AlreadyDecided);
    }
    let (Some(tree_id), Some(node_id)) = (tree_id, node_id) else {
        return Ok(Outcome::NotATreeDecision);
    };
    let Some(job) = store::load(pool, &tree_id).await? else {
        return Ok(Outcome::NotATreeDecision);
    };
    let config = Config::load(&job.manifest.project_root);
    let node_answered = answered_count(pool, "node_id", &node_id).await?;
    let job_answered = answered_count(pool, "tree_id", &tree_id).await?;
    match verdict(&config, kind, node_answered, job_answered) {
        Verdict::Resolve => {}
        declined => return Ok(Outcome::Declined(declined)),
    }
    match super::relaunch::answer_and_relaunch(state, decision_id, answer_text, origin.as_str())
        .await
    {
        Ok(_) => {}
        Err(DecisionError::AlreadyDecided(_)) => return Ok(Outcome::AlreadyDecided),
        Err(error) => return Err(error),
    }
    let line = format!(
        "{}: the tree decided itself ({}) — node {node_id}, decision {decision_id} — question: \
         {question} — answer: {answer_text}",
        job.goal,
        origin.as_str()
    );
    if let Err(error) = crate::feed::append(
        pool,
        job.project_id.as_deref(),
        FEED_KIND,
        &line,
        None,
        None,
    )
    .await
    {
        tracing::warn!(job = %job.id, %error, "could not write the self-decision feed line");
    }
    let decision = decisions::list(pool)
        .await?
        .into_iter()
        .find(|d| d.id == decision_id)
        .ok_or_else(|| DecisionError::NotFound(decision_id.to_owned()))?;
    Ok(Outcome::Resolved(Box::new(decision)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree_job::model::Manifest;
    use crate::tree_job::store::TreeJob;

    #[test]
    fn exactly_the_listed_kinds_are_owner_only() {
        for kind in [
            "governance_file",
            "widen_paths",
            "budget_raise",
            "ticket_ownership",
            "stalled_after_decision_run",
        ] {
            assert!(is_owner_only(kind), "{kind}");
        }
        assert!(!is_owner_only("ordinary"));
    }

    #[test]
    fn an_unknown_kind_is_owner_only() {
        assert!(is_owner_only("something_new"));
        assert!(is_owner_only(""));
    }

    #[test]
    fn config_defaults_and_overrides() {
        assert_eq!(Config::from_yaml("default: normal\n"), Config::default());
        assert_eq!(Config::from_yaml(": not yaml ["), Config::default());
        assert!(Config::default().enabled);
        let config = Config::from_yaml(
            "autonomy:\n  tree_decides_itself: false\n  decision_runs_per_node: 1\n  \
             decision_runs_per_job: 2\n",
        );
        assert_eq!(
            config,
            Config {
                enabled: false,
                per_node: 1,
                per_job: 2
            }
        );
        // Mistyped or negative values keep the default.
        let config = Config::from_yaml(
            "autonomy:\n  tree_decides_itself: maybe\n  decision_runs_per_node: -1\n  \
             decision_runs_per_job: lots\n",
        );
        assert_eq!(config, Config::default());
    }

    #[test]
    fn the_shipped_speed_yaml_turns_the_flag_on() {
        let text = include_str!("../../assets/nucleos-dev/.nucleos/workflow/speed.yaml");
        let config = Config::from_yaml(text);
        assert!(config.enabled);
        assert_eq!(config.per_node, 3);
        assert_eq!(config.per_job, 10);
    }

    #[test]
    fn the_verdict_checks_flag_then_kind_then_caps() {
        let config = Config::default();
        assert_eq!(verdict(&config, "ordinary", 0, 0), Verdict::Resolve);
        assert_eq!(verdict(&config, "ordinary", 3, 0), Verdict::NodeCapped);
        assert_eq!(verdict(&config, "ordinary", 0, 10), Verdict::JobCapped);
        assert_eq!(verdict(&config, "widen_paths", 0, 0), Verdict::OwnerOnly);
        let off = Config {
            enabled: false,
            ..config
        };
        assert_eq!(verdict(&off, "ordinary", 0, 0), Verdict::Disabled);
        assert_eq!(verdict(&off, "widen_paths", 0, 0), Verdict::Disabled);
    }

    // ----- database -----

    const TREE: &str = "ar-job";
    const NODE: &str = "ar-node";
    const AT: &str = "2020-01-01T00:00:00+00:00";

    async fn seed_job(pool: &SqlitePool) {
        let manifest = Manifest::new(TREE, "feat/ar", "Decide it", "C:/nowhere", &[]);
        let job = TreeJob {
            id: TREE.to_owned(),
            root_node_id: "ar-root".to_owned(),
            project_id: None,
            feature_branch: Some("feat/ar".to_owned()),
            goal: "Decide it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: AT.to_owned(),
            updated_at: AT.to_owned(),
        };
        store::save(pool, &job).await.unwrap();
    }

    async fn seed_open(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status)
             VALUES (?, ?, ?, 'which way?', NULL, 'agent', ?, 'open')",
        )
        .bind(id)
        .bind(TREE)
        .bind(NODE)
        .bind(AT)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn state() -> AppState {
        let (state, _runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(std::time::Duration::from_secs(60)),
            std::time::Duration::from_secs(600),
        )
        .await;
        state
    }

    async fn lines(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed WHERE kind = ? ORDER BY id")
            .bind(FEED_KIND)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_ordinary_decision_is_answered_with_one_feed_line() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(&state.pool, "d-1").await;

        let outcome = resolve_decision(&state, "d-1", "ordinary", "go left", Origin::Auto)
            .await
            .unwrap();

        let Outcome::Resolved(decision) = outcome else {
            panic!("expected Resolved, got {outcome:?}");
        };
        assert_eq!(decision.status, "answered");
        assert_eq!(decision.origin.as_deref(), Some("auto"));
        assert_eq!(decision.answer.as_deref(), Some("go left"));
        let lines = lines(&state.pool).await;
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("go left") && lines[0].contains("d-1"));
    }

    #[tokio::test]
    async fn an_owner_only_kind_is_left_open_and_writes_nothing() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(&state.pool, "d-1").await;

        let outcome = resolve_decision(&state, "d-1", "widen_paths", "yes", Origin::Driver)
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Declined(Verdict::OwnerOnly)));
        let status: String = sqlx::query_scalar("SELECT status FROM decisions WHERE id = 'd-1'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "open");
        assert!(lines(&state.pool).await.is_empty());
    }

    #[tokio::test]
    async fn the_per_node_cap_stops_further_self_answers() {
        let state = state().await;
        seed_job(&state.pool).await;
        for n in 0..4 {
            seed_open(&state.pool, &format!("d-{n}")).await;
        }
        for n in 0..3 {
            let outcome =
                resolve_decision(&state, &format!("d-{n}"), "ordinary", "ok", Origin::Driver)
                    .await
                    .unwrap();
            assert!(matches!(outcome, Outcome::Resolved(_)), "{n}");
        }

        let fourth = resolve_decision(&state, "d-3", "ordinary", "ok", Origin::Driver)
            .await
            .unwrap();

        assert!(matches!(fourth, Outcome::Declined(Verdict::NodeCapped)));
        assert_eq!(lines(&state.pool).await.len(), 3);
    }

    #[tokio::test]
    async fn an_answered_or_ownerless_decision_is_not_touched() {
        let state = state().await;
        seed_job(&state.pool).await;
        seed_open(&state.pool, "d-1").await;
        decisions::answer(&state.pool, "d-1", "owner says", "owner")
            .await
            .unwrap();
        decisions::ask_ownerless(&state.pool, "ticket-1", "park?")
            .await
            .unwrap();

        let answered = resolve_decision(&state, "d-1", "ordinary", "x", Origin::Auto)
            .await
            .unwrap();
        let ownerless = resolve_decision(&state, "ticket-1", "ordinary", "x", Origin::Auto)
            .await
            .unwrap();
        let unknown = resolve_decision(&state, "nope", "ordinary", "x", Origin::Auto).await;

        assert!(matches!(answered, Outcome::AlreadyDecided));
        assert!(matches!(ownerless, Outcome::NotATreeDecision));
        assert!(matches!(unknown, Err(DecisionError::NotFound(_))));
        assert!(lines(&state.pool).await.is_empty());
    }
}
