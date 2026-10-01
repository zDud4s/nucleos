//! The judge's review queue (spec A D11) and, from the tasks that follow, readiness.

use serde::Serialize;
use sqlx::SqlitePool;

/// One entry of the judge's review queue: the verdict, and the action it judged as the classifier
/// saw it (joined from `shadow_decisions`).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct JudgeVerdictView {
    pub id: i64,
    pub run_id: i64,
    pub tool_name: String,
    pub tool_input: Option<String>,
    pub action_class: String,
    pub classifier_decision: String,
    pub judge: String,
    pub model: String,
    pub p_in_scope: Option<f64>,
    pub p_safe: Option<f64>,
    pub p: Option<f64>,
    pub band: String,
    pub capped: bool,
    pub final_decision: String,
    pub enforced: bool,
    pub created_at: String,
}

/// D11: the judge's own review queue, apart from `shadow::list_unreviewed` — which is exactly what
/// the WIP brake counts (`wip.rs:168`, pinned by `autopilot.rs:1306-1343`) and stays untouched.
///
/// Only verdicts in the allow or deny band (the ones that would decide), ONE per distinct action
/// `(tool_name, tool_input_digest)` — the unit the bar counts in, so repeating an action adds no
/// evidence — and an action leaves the queue once any of its verdicts has a human answer. A
/// verdict whose shadow decision was not recorded cannot be shown and is left out.
pub async fn list_unreviewed(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<Vec<JudgeVerdictView>> {
    sqlx::query_as(
        "SELECT jv.id, jv.run_id, jv.tool_name, sd.tool_input, jv.action_class,
                jv.classifier_decision, jv.judge, jv.model, jv.p_in_scope, jv.p_safe, jv.p,
                jv.band, jv.capped, jv.final_decision, jv.enforced, jv.created_at
         FROM judge_verdicts jv
         JOIN runs r ON r.id = jv.run_id
         JOIN shadow_decisions sd ON sd.id = jv.shadow_decision_id
         WHERE r.project_id = ?1
           AND jv.band IN ('allow', 'deny')
           AND jv.human_verdict IS NULL
           AND jv.id = (
               SELECT MIN(first.id) FROM judge_verdicts first
               JOIN runs fr ON fr.id = first.run_id
               WHERE fr.project_id = ?1
                 AND first.tool_name = jv.tool_name
                 AND first.tool_input_digest = jv.tool_input_digest
                 AND first.band IN ('allow', 'deny')
                 AND first.shadow_decision_id IS NOT NULL)
           AND NOT EXISTS (
               SELECT 1 FROM judge_verdicts seen
               JOIN runs sr ON sr.id = seen.run_id
               WHERE sr.project_id = ?1
                 AND seen.tool_name = jv.tool_name
                 AND seen.tool_input_digest = jv.tool_input_digest
                 AND seen.human_verdict IS NOT NULL)
         ORDER BY jv.id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// A human verdict, once, and only one the agreement arithmetic can count — the rule
/// `shadow::set_verdict` keeps (`shadow.rs:185-199`), with its list of verdicts reused.
pub async fn set_verdict(pool: &SqlitePool, id: i64, verdict: &str) -> sqlx::Result<bool> {
    if !crate::shadow::VALID_VERDICTS.contains(&verdict) {
        return Ok(false);
    }
    let result = sqlx::query(
        "UPDATE judge_verdicts SET human_verdict = ?, reviewed_at = ?
         WHERE id = ? AND human_verdict IS NULL AND band IN ('allow', 'deny')",
    )
    .bind(verdict)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::test_support::*;
    use crate::judge::tool_input_digest;
    use serde_json::json;

    /// One judge verdict for review tests: a worktree run of project `p`, a shadow decision it
    /// judged, and the band the judge answered with.
    async fn judged(pool: &sqlx::SqlitePool, command: &str, band: Option<&str>) -> i64 {
        let run_id = running_run(pool).await;
        let input = json!({ "command": command });
        let decision_id = sqlx::query(
            "INSERT INTO shadow_decisions
             (run_id, tool_name, tool_input, decision, reason, action_class, classifier_version, created_at)
             VALUES (?, 'Bash', ?, 'pending_approval', 'r', 'unrecognized', 14, '2026-09-27T00:00:00Z')",
        )
        .bind(run_id)
        .bind(input.to_string())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO judge_verdicts
             (run_id, shadow_decision_id, tool_name, tool_input_digest, action_class,
              classifier_decision, judge, model, questions_version, p, band, final_decision,
              created_at)
             VALUES (?, ?, 'Bash', ?, 'unrecognized', 'pending_approval', 'observe', 'jev-latest',
                     1, 0.9, ?, 'pending_approval', '2026-09-27T00:00:00Z')",
        )
        .bind(run_id)
        .bind(decision_id)
        .bind(tool_input_digest(&input))
        .bind(band)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// D11: only the bands that would have decided, one entry per distinct action, and an action
    /// once reviewed leaves the queue however many times it was judged.
    #[tokio::test]
    async fn the_review_queue_holds_one_entry_per_distinct_decisive_action() {
        let pool = pool().await;
        let first = judged(&pool, "cargo test | tee a.log", Some("allow")).await;
        judged(&pool, "cargo test | tee a.log", Some("allow")).await;
        judged(&pool, "make lint | tee b.log", Some("middle")).await;
        judged(&pool, "make docs | tee c.log", None).await;
        let refused = judged(&pool, "rm -rf /tmp/x", Some("deny")).await;

        let queue = list_unreviewed(&pool, "p").await.unwrap();
        assert_eq!(
            queue.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![first, refused]
        );
        assert_eq!(
            queue[0].tool_input.as_deref(),
            Some("{\"command\":\"cargo test | tee a.log\"}")
        );

        assert!(set_verdict(&pool, first, "approve").await.unwrap());
        assert!(
            !set_verdict(&pool, first, "reject").await.unwrap(),
            "a verdict is recorded once"
        );
        assert!(!set_verdict(&pool, refused, "maybe").await.unwrap());
        let queue = list_unreviewed(&pool, "p").await.unwrap();
        assert_eq!(
            queue.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![refused]
        );
    }
}
