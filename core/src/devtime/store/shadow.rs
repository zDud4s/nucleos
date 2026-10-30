//! The automatic actions' shadow log (migrations 0260 and 0261): what each action decided or would
//! have decided, why, and what the human did in the observation window, plus the per-(action, rule)
//! mode. No message text is stored: `params` holds numbers, ids and hashes only.

#![allow(dead_code)] // the actions step is the reader

use super::*;

/// Who may write `devtime_action_state.changed_by`.
pub const ACTION_CHANGED_BY: [&str; 2] = ["owner", "rollback"];

/// Alias of [`ACTION_CHANGED_BY`] under the name the actions design spec uses.
pub const CHANGED_BY: [&str; 2] = ACTION_CHANGED_BY;

/// `devtime_shadow.action`.
pub const ACTIONS: [&str; 2] = ["inject", "propose"];
/// `devtime_shadow.mode` (`off` exists only in `devtime_action_state`).
pub const MODES: [&str; 2] = ["shadow", "live"];
/// `devtime_shadow.decision`.
pub const DECISIONS: [&str; 2] = ["fire", "suppress"];
/// `devtime_shadow.suppress_reason`; `holdout` marks the residual control group (spec 6.1).
pub const SUPPRESS_REASONS: [&str; 5] = [
    "cap",
    "cooldown",
    "below_threshold",
    "not_eligible",
    "holdout",
];
/// `devtime_shadow.outcome_state`.
pub const OUTCOME_STATES: [&str; 4] = ["pending", "resolved", "expired", "void"];
/// `devtime_shadow.outcome`.
pub const OUTCOMES: [&str; 5] = ["hit", "miss", "accepted", "rejected", "ignored"];
/// `devtime_shadow.human_verdict`.
pub const HUMAN_VERDICTS: [&str; 2] = ["useful", "noise"];
/// The closed catalogue of `devtime_shadow.template_id`: no free text is ever stored.
pub const TEMPLATE_IDS: [&str; 4] = [
    "inject.d3.v1",
    "inject.d6.v1",
    "propose.f1.v1",
    "propose.f2.v1",
];

/// Hash of the thresholds one `(action, rule)` pair decides under, so a threshold change is visible
/// in the log. Only the thresholds that action reads go in, so tuning `propose` leaves `inject`
/// hashes alone. SHA-256 hex of a fixed text form: stable across runs and versions of the std hasher.
pub fn thresholds_hash(
    config: &crate::config::DevtimeActionsConfig,
    action: &str,
    rule: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let hit_rate_min = match action {
        "inject" => config.shadow_hit_rate_min.inject,
        _ => config.shadow_hit_rate_min.propose,
    };
    let mut text = format!(
        "{action}|{rule}|{}|{}|{}|{}|{}|{hit_rate_min}|{}",
        config.precision_wilson_lower,
        config.effect_min_resolved,
        config.effect_min_sessions,
        config.effect_min_days,
        config.min_mean_hit_cost_ms,
        config.holdout_fraction,
    );
    let specific = match action {
        "inject" => format!(
            "|{}|{}|{}|{}|{}",
            config.inject_window_days,
            config.inject_min_count,
            config.inject_min_cost_ms,
            config.inject_max_per_session,
            config.inject_cooldown_days,
        ),
        _ => format!(
            "|{}|{}|{}|{}",
            config.propose_min_sessions,
            config.propose_min_cost_ms,
            config.propose_window_days,
            config.propose_max_open,
        ),
    };
    text.push_str(&specific);
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// One `devtime_shadow` row, every column.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct ShadowRow {
    /// Assigned by the table; ignored on insert.
    pub id: i64,
    /// `inject` | `propose`.
    pub action: String,
    pub rule_id: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub subject_key: String,
    /// `shadow` | `live`.
    pub mode: String,
    /// `fire` | `suppress`.
    pub decision: String,
    pub suppress_reason: Option<String>,
    pub template_id: String,
    /// JSON object; empty is written as `{}`.
    pub params: String,
    pub basis_precision: Option<f64>,
    pub basis_cases: Option<i64>,
    pub basis_recent_count: Option<i64>,
    pub basis_recent_cost_ms: Option<i64>,
    pub expected_saving_ms: Option<i64>,
    pub rule_version: i64,
    pub action_version: i64,
    pub thresholds_hash: String,
    pub rules_version: String,
    pub decided_at: String,
    pub window_end_at: String,
    /// `pending` | `resolved` | `expired` | `void`.
    pub outcome_state: String,
    /// `hit` | `miss` | `accepted` | `rejected` | `ignored`.
    pub outcome: Option<String>,
    pub outcome_finding_key: Option<String>,
    pub outcome_cost_ms: Option<i64>,
    /// `passed` | `failed` | `not_measured`.
    pub outcome_verified: Option<String>,
    pub proposal_id: Option<i64>,
    /// `useful` | `noise`.
    pub human_verdict: Option<String>,
    pub human_verdict_at: Option<String>,
    pub resolved_at: Option<String>,
}

/// What closes a pending row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShadowOutcome {
    /// The new `outcome_state`: `resolved` or `expired`.
    pub state: String,
    pub outcome: Option<String>,
    pub finding_key: Option<String>,
    pub cost_ms: Option<i64>,
    pub verified: Option<String>,
    pub proposal_id: Option<i64>,
    /// Empty means now.
    pub resolved_at: String,
}

/// One `devtime_action_state` row.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct ActionStateRow {
    pub action: String,
    pub rule_id: String,
    /// `off` | `shadow` | `live`.
    pub mode: String,
    pub since: String,
    /// One of [`ACTION_CHANGED_BY`].
    pub changed_by: String,
    pub reason: Option<String>,
}

/// Records a decision. The unique key `(action, rule_id, subject_key, COALESCE(session_id, ''),
/// action_version, mode)` makes a second insert a no-op: returns true when a row was written.
pub async fn insert_decision(pool: &SqlitePool, row: &ShadowRow) -> sqlx::Result<bool> {
    let params = if row.params.is_empty() {
        "{}"
    } else {
        row.params.as_str()
    };
    let res = sqlx::query(
        "INSERT INTO devtime_shadow (action, rule_id, project_id, session_id, subject_key, mode, \
         decision, suppress_reason, template_id, params, basis_precision, basis_cases, \
         basis_recent_count, basis_recent_cost_ms, expected_saving_ms, rule_version, \
         action_version, thresholds_hash, rules_version, decided_at, window_end_at, outcome_state, \
         outcome, outcome_finding_key, outcome_cost_ms, outcome_verified, proposal_id, \
         human_verdict, human_verdict_at, resolved_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
         ?, ?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(&row.action)
    .bind(&row.rule_id)
    .bind(&row.project_id)
    .bind(&row.session_id)
    .bind(&row.subject_key)
    .bind(&row.mode)
    .bind(&row.decision)
    .bind(&row.suppress_reason)
    .bind(&row.template_id)
    .bind(params)
    .bind(row.basis_precision)
    .bind(row.basis_cases)
    .bind(row.basis_recent_count)
    .bind(row.basis_recent_cost_ms)
    .bind(row.expected_saving_ms)
    .bind(row.rule_version)
    .bind(row.action_version)
    .bind(&row.thresholds_hash)
    .bind(&row.rules_version)
    .bind(&row.decided_at)
    .bind(&row.window_end_at)
    .bind(&row.outcome_state)
    .bind(&row.outcome)
    .bind(&row.outcome_finding_key)
    .bind(row.outcome_cost_ms)
    .bind(&row.outcome_verified)
    .bind(row.proposal_id)
    .bind(&row.human_verdict)
    .bind(&row.human_verdict_at)
    .bind(&row.resolved_at)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 1)
}

/// Pending rows whose window ended at or before `now`, oldest window first.
pub async fn pending_due(pool: &SqlitePool, now: &str) -> sqlx::Result<Vec<ShadowRow>> {
    sqlx::query_as::<_, ShadowRow>(
        "SELECT * FROM devtime_shadow WHERE outcome_state = 'pending' AND window_end_at <= ? \
         ORDER BY window_end_at, id",
    )
    .bind(now)
    .fetch_all(pool)
    .await
}

/// Closes a `pending` row. A row that is no longer pending is left alone: returns true when the row
/// was changed.
pub async fn resolve(pool: &SqlitePool, id: i64, outcome: &ShadowOutcome) -> sqlx::Result<bool> {
    if !["resolved", "expired"].contains(&outcome.state.as_str()) {
        return Err(sqlx::Error::Protocol(format!(
            "devtime_shadow: invalid closing state {:?}",
            outcome.state
        )));
    }
    let resolved_at = if outcome.resolved_at.is_empty() {
        super::now()
    } else {
        outcome.resolved_at.clone()
    };
    let res = sqlx::query(
        "UPDATE devtime_shadow SET outcome_state = ?, outcome = ?, outcome_finding_key = ?, \
         outcome_cost_ms = ?, outcome_verified = ?, proposal_id = ?, resolved_at = ? \
         WHERE id = ? AND outcome_state = 'pending'",
    )
    .bind(&outcome.state)
    .bind(&outcome.outcome)
    .bind(&outcome.finding_key)
    .bind(outcome.cost_ms)
    .bind(&outcome.verified)
    .bind(outcome.proposal_id)
    .bind(resolved_at)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Marks `void` every row of `rule_id` stamped with the stale `rule_version`, resolved ones included
/// (their supporting finding is gone); rows of other versions stay. Returns the rows changed.
pub async fn void_stale(pool: &SqlitePool, rule_id: &str, rule_version: i64) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "UPDATE devtime_shadow SET outcome_state = 'void' \
         WHERE rule_id = ? AND rule_version = ? AND outcome_state <> 'void'",
    )
    .bind(rule_id)
    .bind(rule_version)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// The non-void rows of one (action, rule, mode) stamped with any of `action_versions`, oldest first.
pub async fn decisions_for(
    pool: &SqlitePool,
    action: &str,
    rule_id: &str,
    mode: &str,
    action_versions: &[i64],
) -> sqlx::Result<Vec<ShadowRow>> {
    if action_versions.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb = sqlx::QueryBuilder::<Sqlite>::new("SELECT * FROM devtime_shadow WHERE action = ");
    qb.push_bind(action);
    qb.push(" AND rule_id = ").push_bind(rule_id);
    qb.push(" AND mode = ").push_bind(mode);
    qb.push(" AND outcome_state <> 'void' AND action_version IN (");
    let mut sep = qb.separated(", ");
    for v in action_versions {
        sep.push_bind(*v);
    }
    qb.push(") ORDER BY decided_at, id");
    qb.build_query_as::<ShadowRow>().fetch_all(pool).await
}

/// The owner's optional `useful` / `noise` mark on a row. Returns true when the row exists.
pub async fn set_human_verdict(pool: &SqlitePool, id: i64, verdict: &str) -> sqlx::Result<bool> {
    if !["useful", "noise"].contains(&verdict) {
        return Err(sqlx::Error::Protocol(format!(
            "devtime_shadow: invalid human verdict {verdict:?}"
        )));
    }
    let res = sqlx::query(
        "UPDATE devtime_shadow SET human_verdict = ?, human_verdict_at = ? WHERE id = ?",
    )
    .bind(verdict)
    .bind(super::now())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 1)
}

pub async fn get_action_state(
    pool: &SqlitePool,
    action: &str,
    rule_id: &str,
) -> sqlx::Result<Option<ActionStateRow>> {
    sqlx::query_as::<_, ActionStateRow>(
        "SELECT action, rule_id, mode, since, changed_by, reason FROM devtime_action_state \
         WHERE action = ? AND rule_id = ?",
    )
    .bind(action)
    .bind(rule_id)
    .fetch_optional(pool)
    .await
}

/// Upserts the state of one pair. `changed_by` must be one of [`ACTION_CHANGED_BY`], else an error
/// and nothing is written. An empty `since` means now.
pub async fn set_action_state(pool: &SqlitePool, row: &ActionStateRow) -> sqlx::Result<()> {
    if !ACTION_CHANGED_BY.contains(&row.changed_by.as_str()) {
        return Err(sqlx::Error::Protocol(format!(
            "devtime_action_state: invalid changed_by {:?}",
            row.changed_by
        )));
    }
    if !["off", "shadow", "live"].contains(&row.mode.as_str()) {
        return Err(sqlx::Error::Protocol(format!(
            "devtime_action_state: invalid mode {:?}",
            row.mode
        )));
    }
    let since = if row.since.is_empty() {
        super::now()
    } else {
        row.since.clone()
    };
    sqlx::query(
        "INSERT INTO devtime_action_state (action, rule_id, mode, since, changed_by, reason) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(action, rule_id) DO UPDATE SET mode = excluded.mode, since = excluded.since, \
         changed_by = excluded.changed_by, reason = excluded.reason",
    )
    .bind(&row.action)
    .bind(&row.rule_id)
    .bind(&row.mode)
    .bind(since)
    .bind(&row.changed_by)
    .bind(&row.reason)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabularies_match_the_spec() {
        assert_eq!(ACTIONS, ["inject", "propose"]);
        assert_eq!(MODES, ["shadow", "live"]);
        assert_eq!(DECISIONS, ["fire", "suppress"]);
        assert!(SUPPRESS_REASONS.contains(&"holdout"));
        assert_eq!(SUPPRESS_REASONS.len(), 5);
        assert_eq!(OUTCOME_STATES, ["pending", "resolved", "expired", "void"]);
        assert_eq!(OUTCOMES, ["hit", "miss", "accepted", "rejected", "ignored"]);
        assert_eq!(HUMAN_VERDICTS, ["useful", "noise"]);
        assert_eq!(CHANGED_BY, ["owner", "rollback"]);
        assert!(TEMPLATE_IDS.contains(&"inject.d3.v1"));
    }

    #[test]
    fn thresholds_hash_is_stable_per_pair() {
        let cfg = crate::config::DevtimeActionsConfig::default();
        let a = thresholds_hash(&cfg, "inject", "D3");
        assert_eq!(a, thresholds_hash(&cfg, "inject", "D3"));
        assert!(!a.is_empty());
        assert_ne!(a, thresholds_hash(&cfg, "inject", "D6"));
        assert_ne!(a, thresholds_hash(&cfg, "propose", "D3"));
    }

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    fn sample() -> ShadowRow {
        ShadowRow {
            id: 0,
            action: "inject".into(),
            rule_id: "D3".into(),
            project_id: "alpha".into(),
            session_id: Some("s1".into()),
            subject_key: "D3|alpha".into(),
            mode: "shadow".into(),
            decision: "suppress".into(),
            suppress_reason: Some("cap".into()),
            template_id: "inject.d3.v1".into(),
            params: r#"{"n":4,"secs":120}"#.into(),
            basis_precision: Some(0.75),
            basis_cases: Some(12),
            basis_recent_count: Some(4),
            basis_recent_cost_ms: Some(90_000),
            expected_saving_ms: Some(30_000),
            rule_version: 2,
            action_version: 1,
            thresholds_hash: "th1".into(),
            rules_version: "rv1".into(),
            decided_at: "2026-10-05T10:00:00.000Z".into(),
            window_end_at: "2026-10-05T12:00:00.000Z".into(),
            outcome_state: "pending".into(),
            ..ShadowRow::default()
        }
    }

    async fn all_rows(pool: &SqlitePool) -> Vec<ShadowRow> {
        sqlx::query_as::<_, ShadowRow>("SELECT * FROM devtime_shadow ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn insert_decision_roundtrips_all_fields() {
        let pool = pool().await;
        let row = sample();
        assert!(insert_decision(&pool, &row).await.unwrap());

        let got = decisions_for(&pool, "inject", "D3", "shadow", &[1])
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        let mut expected = row.clone();
        expected.id = got[0].id;
        assert!(expected.id > 0);
        assert_eq!(got[0], expected);
    }

    #[tokio::test]
    async fn insert_decision_is_idempotent_on_key() {
        let pool = pool().await;
        assert!(insert_decision(&pool, &sample()).await.unwrap());
        let mut again = sample();
        again.decision = "fire".into();
        again.suppress_reason = None;
        assert!(!insert_decision(&pool, &again).await.unwrap());

        let rows = all_rows(&pool).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].decision, "suppress", "the first decision stays");

        // A different session, action_version or mode is a different decision.
        let mut other_session = sample();
        other_session.session_id = Some("s2".into());
        let mut other_version = sample();
        other_version.action_version = 2;
        let mut other_mode = sample();
        other_mode.mode = "live".into();
        for row in [&other_session, &other_version, &other_mode] {
            assert!(insert_decision(&pool, row).await.unwrap());
        }
        // NULL session ids collide with each other through COALESCE(session_id, '').
        let mut no_session = sample();
        no_session.session_id = None;
        no_session.action = "propose".into();
        assert!(insert_decision(&pool, &no_session).await.unwrap());
        assert!(!insert_decision(&pool, &no_session).await.unwrap());
        assert_eq!(all_rows(&pool).await.len(), 5);
    }

    #[tokio::test]
    async fn resolved_row_is_not_rewritten_by_recompute() {
        let pool = pool().await;
        assert!(insert_decision(&pool, &sample()).await.unwrap());
        let id = all_rows(&pool).await[0].id;
        let outcome = ShadowOutcome {
            state: "resolved".into(),
            outcome: Some("hit".into()),
            finding_key: Some("fk1".into()),
            cost_ms: Some(45_000),
            verified: Some("passed".into()),
            proposal_id: None,
            resolved_at: "2026-10-05T12:01:00.000Z".into(),
        };
        assert!(resolve(&pool, id, &outcome).await.unwrap());
        let resolved = all_rows(&pool).await.remove(0);
        assert_eq!(resolved.outcome_state, "resolved");
        assert_eq!(resolved.outcome.as_deref(), Some("hit"));
        assert_eq!(resolved.outcome_finding_key.as_deref(), Some("fk1"));
        assert_eq!(resolved.outcome_cost_ms, Some(45_000));
        assert_eq!(resolved.outcome_verified.as_deref(), Some("passed"));
        assert_eq!(
            resolved.resolved_at.as_deref(),
            Some("2026-10-05T12:01:00.000Z")
        );

        // The recompute inserts the same decision again, and a second resolve arrives: neither wins.
        let mut recomputed = sample();
        recomputed.decision = "fire".into();
        assert!(!insert_decision(&pool, &recomputed).await.unwrap());
        let miss = ShadowOutcome {
            state: "resolved".into(),
            outcome: Some("miss".into()),
            ..ShadowOutcome::default()
        };
        assert!(!resolve(&pool, id, &miss).await.unwrap());
        assert_eq!(all_rows(&pool).await.remove(0), resolved);
        assert!(
            pending_due(&pool, "2026-10-06T00:00:00.000Z")
                .await
                .unwrap()
                .is_empty(),
            "a resolved row is no longer pending"
        );
    }

    #[tokio::test]
    async fn void_stale_marks_void_and_leaves_other_versions() {
        let pool = pool().await;
        let stale = sample();
        let mut current = sample();
        current.rule_version = 3;
        current.session_id = Some("s2".into());
        let mut other_rule = sample();
        other_rule.rule_id = "D4".into();
        other_rule.subject_key = "D4|alpha".into();
        for row in [&stale, &current, &other_rule] {
            assert!(insert_decision(&pool, row).await.unwrap());
        }

        assert_eq!(void_stale(&pool, "D3", 2).await.unwrap(), 1);
        let rows = all_rows(&pool).await;
        let state = |rule: &str, version: i64| {
            rows.iter()
                .find(|r| r.rule_id == rule && r.rule_version == version)
                .map(|r| r.outcome_state.clone())
                .unwrap()
        };
        assert_eq!(state("D3", 2), "void");
        assert_eq!(state("D3", 3), "pending");
        assert_eq!(state("D4", 2), "pending");

        // A void row leaves the counts.
        let counted = decisions_for(&pool, "inject", "D3", "shadow", &[1])
            .await
            .unwrap();
        assert_eq!(counted.len(), 1);
        assert_eq!(counted[0].rule_version, 3);
    }

    #[tokio::test]
    async fn schema_has_no_free_text_message_column() {
        let pool = pool().await;
        let mut columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('devtime_shadow')")
                .fetch_all(&pool)
                .await
                .unwrap();
        columns.sort();
        let mut expected: Vec<&str> = vec![
            "id",
            "action",
            "rule_id",
            "project_id",
            "session_id",
            "subject_key",
            "mode",
            "decision",
            "suppress_reason",
            "template_id",
            "params",
            "basis_precision",
            "basis_cases",
            "basis_recent_count",
            "basis_recent_cost_ms",
            "expected_saving_ms",
            "rule_version",
            "action_version",
            "thresholds_hash",
            "rules_version",
            "decided_at",
            "window_end_at",
            "outcome_state",
            "outcome",
            "outcome_finding_key",
            "outcome_cost_ms",
            "outcome_verified",
            "proposal_id",
            "human_verdict",
            "human_verdict_at",
            "resolved_at",
        ];
        expected.sort();
        assert_eq!(
            columns, expected,
            "a new column must be reviewed for privacy"
        );
    }

    #[tokio::test]
    async fn action_state_get_set_validates_changed_by() {
        let pool = pool().await;
        assert_eq!(get_action_state(&pool, "inject", "D3").await.unwrap(), None);

        let owner = ActionStateRow {
            action: "inject".into(),
            rule_id: "D3".into(),
            mode: "shadow".into(),
            since: "2026-10-05T10:00:00.000Z".into(),
            changed_by: "owner".into(),
            reason: Some("start".into()),
        };
        set_action_state(&pool, &owner).await.unwrap();
        assert_eq!(
            get_action_state(&pool, "inject", "D3").await.unwrap(),
            Some(owner.clone())
        );

        let rollback = ActionStateRow {
            mode: "off".into(),
            changed_by: "rollback".into(),
            reason: None,
            since: "2026-10-06T10:00:00.000Z".into(),
            ..owner.clone()
        };
        set_action_state(&pool, &rollback).await.unwrap();
        assert_eq!(
            get_action_state(&pool, "inject", "D3").await.unwrap(),
            Some(rollback.clone())
        );

        let bad = ActionStateRow {
            changed_by: "robot".into(),
            mode: "live".into(),
            ..owner
        };
        assert!(set_action_state(&pool, &bad).await.is_err());
        assert_eq!(
            get_action_state(&pool, "inject", "D3").await.unwrap(),
            Some(rollback),
            "a rejected write changes nothing"
        );
    }
}
