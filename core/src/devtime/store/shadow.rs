//! The automatic actions' shadow log (migrations 0260 and 0261): what each action decided or would
//! have decided, why, and what the human did in the observation window, plus the per-(action, rule)
//! mode. No message text is stored: `params` holds numbers, ids and hashes only.

#![allow(dead_code)] // the actions step is the reader

use super::*;

/// Who may write `devtime_action_state.changed_by`.
pub const ACTION_CHANGED_BY: [&str; 2] = ["owner", "rollback"];

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
pub async fn insert_decision(_pool: &SqlitePool, _row: &ShadowRow) -> sqlx::Result<bool> {
    Ok(false)
}

/// Pending rows whose window ended at or before `now`, oldest window first.
pub async fn pending_due(_pool: &SqlitePool, _now: &str) -> sqlx::Result<Vec<ShadowRow>> {
    Ok(Vec::new())
}

/// Closes a `pending` row. A row that is no longer pending is left alone: returns true when the row
/// was changed.
pub async fn resolve(
    _pool: &SqlitePool,
    _id: i64,
    _outcome: &ShadowOutcome,
) -> sqlx::Result<bool> {
    Ok(false)
}

/// Marks `void` every row of `rule_id` stamped with the stale `rule_version`, resolved ones included
/// (their supporting finding is gone); rows of other versions stay. Returns the rows changed.
pub async fn void_stale(
    _pool: &SqlitePool,
    _rule_id: &str,
    _rule_version: i64,
) -> sqlx::Result<u64> {
    Ok(0)
}

/// The non-void rows of one (action, rule, mode) stamped with any of `action_versions`, oldest first.
pub async fn decisions_for(
    _pool: &SqlitePool,
    _action: &str,
    _rule_id: &str,
    _mode: &str,
    _action_versions: &[i64],
) -> sqlx::Result<Vec<ShadowRow>> {
    Ok(Vec::new())
}

/// The owner's optional `useful` / `noise` mark on a row. Returns true when the row exists.
pub async fn set_human_verdict(_pool: &SqlitePool, _id: i64, _verdict: &str) -> sqlx::Result<bool> {
    Ok(false)
}

pub async fn get_action_state(
    _pool: &SqlitePool,
    _action: &str,
    _rule_id: &str,
) -> sqlx::Result<Option<ActionStateRow>> {
    Ok(None)
}

/// Upserts the state of one pair. `changed_by` must be one of [`ACTION_CHANGED_BY`], else an error
/// and nothing is written. An empty `since` means now.
pub async fn set_action_state(_pool: &SqlitePool, _row: &ActionStateRow) -> sqlx::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(columns, expected, "a new column must be reviewed for privacy");
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
