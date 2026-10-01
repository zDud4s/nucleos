//! §spec autopilot-juiz-resolve-bloqueios
//!
//! Spec B D11: the resolver's own review queue — one entry per unit, (lineage, distinct action)
//! for E1/E3 and (lineage, failed gate) for E4 — the labels a person's decisions give it, and the
//! readiness its `enforce` is gated on.

use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};

use crate::judge::resolve::{Event, Outcome};

/// One entry of the resolver's review queue, with what the reviewer needs to answer it: the action
/// as the classifier saw it (E1/E3, from `shadow_decisions`) or the gate's output (E4).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ResolutionView {
    pub id: i64,
    pub run_id: i64,
    pub lineage_root_id: i64,
    pub event: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<String>,
    pub gate_output: Option<String>,
    pub p_off_task: Option<f64>,
    pub p_needed: Option<f64>,
    pub p_avoidable: Option<f64>,
    pub p_fixable: Option<f64>,
    pub default_outcome: String,
    pub judge_outcome: String,
    pub final_outcome: String,
    pub enforced: bool,
    pub created_at: String,
}

/// D11: one entry per unit — the FIRST answered row of a (lineage, event, digest) — and none once
/// any row of the unit has a human outcome. Unanswered rows carry no opinion to review; `moot`
/// rows are parks that never happened (spec A's judge decided first).
pub async fn list_unreviewed(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<Vec<ResolutionView>> {
    sqlx::query_as(
        "SELECT jr.id, jr.run_id, jr.lineage_root_id, jr.event, sd.tool_name, sd.tool_input,
                CASE WHEN jr.event = 'gate_failed' THEN r.gate_output END AS gate_output,
                jr.p_off_task, jr.p_needed, jr.p_avoidable, jr.p_fixable,
                jr.default_outcome, jr.judge_outcome, jr.final_outcome, jr.enforced, jr.created_at
         FROM judge_resolutions jr
         JOIN runs r ON r.id = jr.run_id
         LEFT JOIN shadow_decisions sd ON sd.id = jr.event_ref
         WHERE r.project_id = ?1
           AND jr.judge_outcome IS NOT NULL
           AND jr.final_outcome <> ?2
           AND jr.id = (SELECT MIN(first.id) FROM judge_resolutions first
                        WHERE (first.lineage_root_id, first.event, first.tool_input_digest)
                              = (jr.lineage_root_id, jr.event, jr.tool_input_digest)
                          AND first.judge_outcome IS NOT NULL AND first.final_outcome <> ?2)
           AND NOT EXISTS (SELECT 1 FROM judge_resolutions seen
                           WHERE (seen.lineage_root_id, seen.event, seen.tool_input_digest)
                                 = (jr.lineage_root_id, jr.event, jr.tool_input_digest)
                             AND seen.human_outcome IS NOT NULL)
         ORDER BY jr.id",
    )
    .bind(project_id)
    .bind(Outcome::Moot.as_db_str())
    .fetch_all(pool)
    .await
}

/// D11: the outcome a person would have chosen, once, and only one D3 allows for that event.
pub async fn set_outcome(
    conn: &mut SqliteConnection,
    id: i64,
    outcome: &str,
) -> sqlx::Result<bool> {
    let event: Option<String> =
        sqlx::query_scalar("SELECT event FROM judge_resolutions WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let allowed = event
        .as_deref()
        .and_then(Event::from_db_str)
        .zip(Outcome::from_db_str(outcome))
        .is_some_and(|(event, outcome)| event.outcomes().contains(&outcome));
    if !allowed {
        return Ok(false);
    }
    let result = sqlx::query(
        "UPDATE judge_resolutions SET human_outcome = ?, reviewed_at = ?
         WHERE id = ? AND human_outcome IS NULL AND judge_outcome IS NOT NULL AND final_outcome <> ?",
    )
    .bind(outcome)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .bind(Outcome::Moot.as_db_str())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// A person's answer to a parked action, through a human door (the HTTP handlers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonsDecision {
    Approve,
    Decline,
    /// No door constructs this one: a reject labels nothing (see [`label_of`]), so its handler
    /// does not call in. The variant keeps that rule written down and tested.
    #[cfg_attr(not(test), allow(dead_code))]
    Reject,
}

/// PURE, spec B D12: approve is `park` (the only E3 outcome under which the action runs), decline
/// is `explain`, and a reject is NO label — under D4's caution order a `stop` label would make
/// every judge `park` "less cautious", which the bar never allows and readiness never forgets; and
/// "reject" often means "not that, carry on".
pub fn label_of(decision: PersonsDecision) -> Option<Outcome> {
    match decision {
        PersonsDecision::Approve => Some(Outcome::Park),
        PersonsDecision::Decline => Some(Outcome::Explain),
        PersonsDecision::Reject => None,
    }
}

/// D12: a person's decision on a proposal is the label of the park that minted it, when the
/// resolver was asked about that park and nobody has labelled it yet. Answers whether a row was
/// labelled. Best-effort: a decision that could not also become a label is still the decision.
///
pub async fn label_from_decision(
    pool: &SqlitePool,
    run_id: i64,
    tool_input: Option<&str>,
    decision: PersonsDecision,
) -> bool {
    let Some(outcome) = label_of(decision) else {
        return false;
    };
    let Some(input) =
        tool_input.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
    else {
        return false;
    };
    let labelled = async {
        let mut tx = pool.begin().await?;
        let done = sqlx::query(
            "UPDATE judge_resolutions SET human_outcome = ?, reviewed_at = ?
             WHERE run_id = ? AND event = 'park' AND final_outcome = 'park' AND tool_input_digest = ?
               AND human_outcome IS NULL AND judge_outcome IS NOT NULL",
        )
        .bind(outcome.as_db_str())
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(run_id)
        .bind(crate::judge::tool_input_digest(&input))
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        tx.commit().await?;
        Ok::<_, sqlx::Error>(done)
    }
    .await;
    match labelled {
        Ok(done) => done,
        Err(error) => {
            tracing::warn!(run_id, %error, "judge: could not label a park from a person's decision");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::test_support::pool;

    /// One resolution of project `p`. Returns its id.
    async fn resolved(
        pool: &SqlitePool,
        lineage: i64,
        event: &str,
        digest: &str,
        opinion: Option<&str>,
        applied: &str,
    ) -> i64 {
        let run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, lineage_root_id, created_at)
             VALUES ('p', 'x', 'completed', 'worktree', ?, '2026-09-27T00:00:00Z')",
        )
        .bind(lineage)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let default = Event::from_db_str(event)
            .unwrap()
            .default_outcome()
            .as_db_str();
        sqlx::query(
            "INSERT INTO judge_resolutions (run_id, lineage_root_id, event, tool_input_digest,
                                            default_outcome, judge_outcome, final_outcome, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, '2026-09-27T00:00:00Z')",
        )
        .bind(run_id)
        .bind(lineage)
        .bind(event)
        .bind(digest)
        .bind(default)
        .bind(opinion)
        .bind(applied)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn set(pool: &SqlitePool, id: i64, outcome: &str) -> bool {
        set_outcome(pool.acquire().await.unwrap().as_mut(), id, outcome)
            .await
            .unwrap()
    }

    async fn queue_ids(pool: &SqlitePool) -> Vec<i64> {
        list_unreviewed(pool, "p")
            .await
            .unwrap()
            .iter()
            .map(|row| row.id)
            .collect()
    }

    /// D11: one entry per unit, never an unanswered or a moot one, gone once any row of the unit
    /// has a human outcome; an outcome outside the event's (D3) is refused, and given once.
    #[tokio::test]
    async fn the_queue_holds_one_entry_per_unit_and_takes_only_the_events_outcomes() {
        let pool = pool().await;
        let first = resolved(&pool, 1, "park", "a", Some("explain"), "park").await;
        resolved(&pool, 1, "park", "a", Some("explain"), "park").await;
        let other_lineage = resolved(&pool, 2, "park", "a", Some("park"), "park").await;
        resolved(&pool, 3, "park", "b", None, "park").await;
        resolved(&pool, 4, "park", "c", Some("explain"), "moot").await;
        let gate = resolved(
            &pool,
            5,
            "gate_failed",
            "gate_failed",
            Some("correction"),
            "owner",
        )
        .await;
        assert_eq!(queue_ids(&pool).await, vec![first, other_lineage, gate]);
        assert!(!set(&pool, gate, "explain").await, "not an E4 outcome");
        assert!(set(&pool, first, "park").await);
        assert!(!set(&pool, first, "stop").await, "once");
        assert_eq!(queue_ids(&pool).await, vec![other_lineage, gate]);
    }

    /// D12: approve labels `park`, decline labels `explain`, a reject labels nothing — and a label,
    /// once given, is never overwritten.
    #[tokio::test]
    async fn a_persons_decision_labels_the_park_it_answered_and_a_reject_does_not() {
        assert_eq!(label_of(PersonsDecision::Approve), Some(Outcome::Park));
        assert_eq!(label_of(PersonsDecision::Decline), Some(Outcome::Explain));
        assert_eq!(label_of(PersonsDecision::Reject), None);
        let pool = pool().await;
        let input = serde_json::json!({ "command": "cargo test | tee t.log" }).to_string();
        let digest = crate::judge::tool_input_digest(&serde_json::from_str(&input).unwrap());
        let id = resolved(&pool, 1, "park", &digest, Some("explain"), "park").await;
        let run_id: i64 = sqlx::query_scalar("SELECT run_id FROM judge_resolutions WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        const LABEL: &str = "SELECT human_outcome FROM judge_resolutions WHERE id = ?";
        let label = |pool: SqlitePool| async move {
            sqlx::query_scalar::<_, Option<String>>(LABEL)
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        assert!(!label_from_decision(&pool, run_id, Some(&input), PersonsDecision::Reject).await);
        assert_eq!(
            label(pool.clone()).await,
            None,
            "a reject leaves the unit for an explicit review"
        );
        assert!(queue_ids(&pool).await.contains(&id));
        assert!(label_from_decision(&pool, run_id, Some(&input), PersonsDecision::Decline).await);
        assert!(!label_from_decision(&pool, run_id, Some(&input), PersonsDecision::Approve).await);
        assert_eq!(
            label(pool.clone()).await.as_deref(),
            Some("explain"),
            "the first answer stands"
        );
    }
}
