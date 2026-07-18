use serde::Serialize;
use serde_json::Value;
use sqlx::{FromRow, SqlitePool};

use crate::classifier::{self, Classification};

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ShadowDecision {
    pub id: i64,
    pub run_id: i64,
    pub tool_name: String,
    pub tool_input: Option<String>,
    pub decision: String,
    pub reason: Option<String>,
    pub action_class: String,
    pub classifier_version: i64,
    pub human_verdict: Option<String>,
    pub reviewed_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, FromRow)]
pub struct ClassTally {
    pub action_class: String,
    pub total: i64,
    pub would_allow: i64,
    pub would_pend: i64,
    pub would_deny: i64,
    pub reviewed: i64,
    pub agree: i64,
    pub disagree: i64,
}

pub async fn record_decision(
    pool: &SqlitePool,
    run_id: i64,
    tool_name: &str,
    tool_input: &Value,
    classification: &Classification,
) -> sqlx::Result<i64> {
    let created_at = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO shadow_decisions
         (run_id, tool_name, tool_input, decision, reason, action_class,
          classifier_version, human_verdict, reviewed_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
    )
    .bind(run_id)
    .bind(tool_name)
    .bind(tool_input.to_string())
    .bind(&classification.decision.decision)
    .bind(&classification.reason)
    .bind(classification.action_class)
    .bind(classifier::CLASSIFIER_VERSION as i64)
    .bind(created_at)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

pub async fn list_unreviewed(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<Vec<ShadowDecision>> {
    sqlx::query_as(
        "SELECT shadow_decisions.*
         FROM shadow_decisions
         JOIN runs ON runs.id = shadow_decisions.run_id
         WHERE runs.project_id = ? AND shadow_decisions.human_verdict IS NULL
         ORDER BY shadow_decisions.id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

pub async fn set_verdict(pool: &SqlitePool, id: i64, verdict: &str) -> sqlx::Result<bool> {
    let reviewed_at = chrono::Utc::now().to_rfc3339();
    let result =
        sqlx::query("UPDATE shadow_decisions SET human_verdict = ?, reviewed_at = ? WHERE id = ?")
            .bind(verdict)
            .bind(reviewed_at)
            .bind(id)
            .execute(pool)
            .await?;

    Ok(result.rows_affected() == 1)
}

pub async fn scoreboard(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<ClassTally>> {
    sqlx::query_as(
        "SELECT
             shadow_decisions.action_class,
             COUNT(*) AS total,
             SUM(CASE WHEN shadow_decisions.decision = 'allow' THEN 1 ELSE 0 END) AS would_allow,
             SUM(CASE WHEN shadow_decisions.decision = 'pending_approval' THEN 1 ELSE 0 END) AS would_pend,
             SUM(CASE WHEN shadow_decisions.decision = 'deny' THEN 1 ELSE 0 END) AS would_deny,
             SUM(CASE WHEN shadow_decisions.human_verdict IS NOT NULL THEN 1 ELSE 0 END) AS reviewed,
             SUM(CASE
                 WHEN shadow_decisions.human_verdict = 'approve'
                      AND shadow_decisions.decision = 'allow' THEN 1
                 WHEN shadow_decisions.human_verdict = 'reject'
                      AND shadow_decisions.decision IN ('deny', 'pending_approval') THEN 1
                 ELSE 0
             END) AS agree,
             SUM(CASE
                 WHEN shadow_decisions.human_verdict IS NOT NULL
                      AND NOT (
                          (shadow_decisions.human_verdict = 'approve'
                           AND shadow_decisions.decision = 'allow')
                          OR
                          (shadow_decisions.human_verdict = 'reject'
                           AND shadow_decisions.decision IN ('deny', 'pending_approval'))
                      ) THEN 1
                 ELSE 0
             END) AS disagree
         FROM shadow_decisions
         JOIN runs ON runs.id = shadow_decisions.run_id
         WHERE runs.project_id = ?
         GROUP BY shadow_decisions.action_class
         ORDER BY shadow_decisions.action_class",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::{CLASSIFIER_VERSION, Classification};
    use crate::hooks::Decision;
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn insert_run(pool: &sqlx::SqlitePool, project_id: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, created_at)
             VALUES (?, 'test', 'running', '2026-07-18T00:00:00Z')",
        )
        .bind(project_id)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn insert_shadow(
        pool: &sqlx::SqlitePool,
        run_id: i64,
        action_class: &str,
        decision: &str,
        verdict: Option<&str>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO shadow_decisions
             (run_id, tool_name, tool_input, decision, reason, action_class,
              classifier_version, human_verdict, reviewed_at, created_at)
             VALUES (?, 'Bash', '{}', ?, 'test reason', ?, ?, ?,
                     CASE WHEN ? IS NULL THEN NULL ELSE '2026-07-18T01:00:00Z' END,
                     '2026-07-18T00:00:00Z')",
        )
        .bind(run_id)
        .bind(decision)
        .bind(action_class)
        .bind(CLASSIFIER_VERSION as i64)
        .bind(verdict)
        .bind(verdict)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    fn classification(decision: &str, action_class: &'static str) -> Classification {
        Classification {
            decision: Decision {
                decision: decision.to_owned(),
                reason: "classified reason".to_owned(),
            },
            action_class,
            reason: "classified reason".to_owned(),
        }
    }

    #[tokio::test]
    async fn record_decision_persists_reviewable_classifier_snapshot() {
        let pool = test_pool().await;
        let run_id = insert_run(&pool, "project-a").await;
        let tool_input = json!({"command": "cargo test", "nested": {"full": true}});

        let id = record_decision(
            &pool,
            run_id,
            "Bash",
            &tool_input,
            &classification("allow", "read-local"),
        )
        .await
        .unwrap();

        let row: ShadowDecision = sqlx::query_as("SELECT * FROM shadow_decisions WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.run_id, run_id);
        assert_eq!(row.tool_name, "Bash");
        assert_eq!(
            row.tool_input.as_deref(),
            Some(tool_input.to_string().as_str())
        );
        assert_eq!(row.decision, "allow");
        assert_eq!(row.reason.as_deref(), Some("classified reason"));
        assert_eq!(row.action_class, "read-local");
        assert_eq!(row.classifier_version, CLASSIFIER_VERSION as i64);
        assert_eq!(row.human_verdict, None);
        assert_eq!(row.reviewed_at, None);
        chrono::DateTime::parse_from_rfc3339(&row.created_at).unwrap();
    }

    #[tokio::test]
    async fn scoreboard_is_project_scoped_and_counts_would_decisions_by_class() {
        let pool = test_pool().await;
        let run_a = insert_run(&pool, "project-a").await;
        let run_b = insert_run(&pool, "project-b").await;
        insert_shadow(&pool, run_a, "read-local", "allow", None).await;
        insert_shadow(&pool, run_a, "read-local", "pending_approval", None).await;
        insert_shadow(&pool, run_a, "destructive", "deny", None).await;
        insert_shadow(&pool, run_b, "read-local", "deny", Some("reject")).await;

        let tallies = scoreboard(&pool, "project-a").await.unwrap();

        assert_eq!(
            tallies,
            vec![
                ClassTally {
                    action_class: "destructive".to_owned(),
                    total: 1,
                    would_allow: 0,
                    would_pend: 0,
                    would_deny: 1,
                    reviewed: 0,
                    agree: 0,
                    disagree: 0,
                },
                ClassTally {
                    action_class: "read-local".to_owned(),
                    total: 2,
                    would_allow: 1,
                    would_pend: 1,
                    would_deny: 0,
                    reviewed: 0,
                    agree: 0,
                    disagree: 0,
                },
            ]
        );
    }

    #[tokio::test]
    async fn scoreboard_counts_human_agreement_and_disagreement() {
        let pool = test_pool().await;
        let run_id = insert_run(&pool, "project-a").await;
        insert_shadow(&pool, run_id, "governance", "allow", Some("approve")).await;
        insert_shadow(
            &pool,
            run_id,
            "governance",
            "pending_approval",
            Some("reject"),
        )
        .await;
        insert_shadow(&pool, run_id, "governance", "deny", Some("approve")).await;

        let tallies = scoreboard(&pool, "project-a").await.unwrap();

        assert_eq!(tallies[0].reviewed, 3);
        assert_eq!(tallies[0].agree, 2);
        assert_eq!(tallies[0].disagree, 1);
    }

    #[tokio::test]
    async fn list_unreviewed_returns_only_null_verdicts_for_project() {
        let pool = test_pool().await;
        let run_a = insert_run(&pool, "project-a").await;
        let run_b = insert_run(&pool, "project-b").await;
        let expected = insert_shadow(&pool, run_a, "read-local", "allow", None).await;
        insert_shadow(&pool, run_a, "destructive", "deny", Some("reject")).await;
        insert_shadow(&pool, run_b, "read-local", "allow", None).await;

        let decisions = list_unreviewed(&pool, "project-a").await.unwrap();

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].id, expected);
        assert_eq!(decisions[0].human_verdict, None);
    }

    #[tokio::test]
    async fn set_verdict_removes_unreviewed_and_updates_scoreboard() {
        let pool = test_pool().await;
        let run_id = insert_run(&pool, "project-a").await;
        let id = insert_shadow(&pool, run_id, "read-local", "allow", None).await;

        set_verdict(&pool, id, "approve").await.unwrap();

        assert!(
            list_unreviewed(&pool, "project-a")
                .await
                .unwrap()
                .is_empty()
        );
        let row: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT human_verdict, reviewed_at FROM shadow_decisions WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0.as_deref(), Some("approve"));
        chrono::DateTime::parse_from_rfc3339(row.1.as_deref().unwrap()).unwrap();
        let tally = &scoreboard(&pool, "project-a").await.unwrap()[0];
        assert_eq!((tally.reviewed, tally.agree, tally.disagree), (1, 1, 0));
    }
}
