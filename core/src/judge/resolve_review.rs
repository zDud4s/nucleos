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
/// A label moves readiness exactly as a review does, so the bar is held in the same write.
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
        if done {
            let project: Option<String> =
                sqlx::query_scalar("SELECT project_id FROM runs WHERE id = ?")
                    .bind(run_id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .flatten();
            if let Some(project_id) = project {
                crate::autopilot::hold_judge_resolve_to_the_bar(&mut tx, &project_id).await?;
            }
        }
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

/// D11's bar. Starting values, like every number in spec B.
pub const RESOLVE_MIN_REVIEWED: i64 = 10;
pub const RESOLVE_MIN_AGREE_PERCENT: i64 = 90;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolveReadiness {
    pub reviewed: i64,
    pub agree: i64,
    /// Units where the judge chose a LESS cautious outcome than the person, on D4's order.
    pub less_cautious: i64,
    pub ready: bool,
}

/// PURE: D11's bar. 90% and not spec A's 95%: several outcomes are harder to agree on than a yes
/// or no. Zero tolerance where the risk is.
pub fn ready(reviewed: i64, agree: i64, less_cautious: i64) -> bool {
    reviewed >= RESOLVE_MIN_REVIEWED
        && agree * 100 >= RESOLVE_MIN_AGREE_PERCENT * reviewed
        && less_cautious == 0
}

pub async fn readiness(pool: &SqlitePool, project_id: &str) -> sqlx::Result<ResolveReadiness> {
    readiness_on(pool.acquire().await?.as_mut(), project_id).await
}

/// D11: per unit, "agrees" means every reviewed row of the unit has the judge's outcome equal to
/// the person's; "less cautious" means any row where it was below it. Folded in Rust rather than
/// SQL, because the caution order lives in `Outcome::caution` and must not be restated. Against a
/// caller-supplied connection, so a caller that must decide on it atomically with a write can read
/// it inside its own transaction (as spec A's `readiness_on`).
pub async fn readiness_on(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> sqlx::Result<ResolveReadiness> {
    let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
        "SELECT jr.lineage_root_id, jr.event, jr.tool_input_digest, jr.judge_outcome, jr.human_outcome
         FROM judge_resolutions jr JOIN runs r ON r.id = jr.run_id
         WHERE r.project_id = ? AND jr.judge_outcome IS NOT NULL AND jr.human_outcome IS NOT NULL",
    )
    .bind(project_id)
    .fetch_all(conn)
    .await?;
    let mut units: std::collections::BTreeMap<(i64, String, String), (bool, bool)> =
        Default::default();
    for (lineage, event, digest, judge, human) in rows {
        let (Some(judge), Some(human)) =
            (Outcome::from_db_str(&judge), Outcome::from_db_str(&human))
        else {
            continue;
        };
        let unit = units
            .entry((lineage, event, digest))
            .or_insert((true, false));
        unit.0 &= judge == human;
        unit.1 |= judge.caution() < human.caution();
    }
    let reviewed = units.len() as i64;
    let agree = units.values().filter(|(agrees, _)| *agrees).count() as i64;
    let less_cautious = units.values().filter(|(_, less)| *less).count() as i64;
    Ok(ResolveReadiness {
        reviewed,
        agree,
        less_cautious,
        ready: ready(reviewed, agree, less_cautious),
    })
}

/// The project a resolution's run belongs to — spec A's `project_of_verdict`, for this table.
pub async fn project_of_resolution(
    conn: &mut SqliteConnection,
    id: i64,
) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT runs.project_id FROM judge_resolutions
         JOIN runs ON runs.id = judge_resolutions.run_id WHERE judge_resolutions.id = ?",
    )
    .bind(id)
    .fetch_optional(conn)
    .await
    .map(Option::flatten)
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

    async fn reviewed(pool: &SqlitePool, lineage: i64, opinion: &str, human: &str) {
        let id = resolved(
            pool,
            lineage,
            "park",
            &format!("d{lineage}"),
            Some(opinion),
            "park",
        )
        .await;
        sqlx::query("UPDATE judge_resolutions SET human_outcome = ? WHERE id = ?")
            .bind(human)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// D11: at least 10 units, at least 90% agreeing, and zero where the judge was LESS cautious
    /// than the person (D4's order).
    #[tokio::test]
    async fn the_bar_is_ten_units_ninety_percent_and_nothing_less_cautious() {
        let pool = pool().await;
        for lineage in 0..9 {
            reviewed(&pool, lineage, "park", "park").await;
        }
        assert_eq!(
            readiness(&pool, "p").await.unwrap(),
            ResolveReadiness {
                reviewed: 9,
                agree: 9,
                less_cautious: 0,
                ready: false
            }
        );
        // More cautious than the person: allowed.
        reviewed(&pool, 9, "stop", "park").await;
        assert_eq!(
            readiness(&pool, "p").await.unwrap(),
            ResolveReadiness {
                reviewed: 10,
                agree: 9,
                less_cautious: 0,
                ready: true
            }
        );
        // Less cautious: never allowed.
        reviewed(&pool, 10, "explain", "park").await;
        let r = readiness(&pool, "p").await.unwrap();
        assert_eq!(
            (r.reviewed, r.agree, r.less_cautious, r.ready),
            (11, 9, 1, false)
        );
    }
}
