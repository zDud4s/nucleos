//! Spec backlog: a registry of spec files, their criteria, dependencies and owner questions.

pub mod audit;
pub mod criteria;
mod daemon;
pub mod deps;
pub mod extract;
pub mod finish;
pub mod http;
pub mod model;
pub mod prompts;
mod review;
pub mod scan;
pub mod store;
pub mod verdict;

use sqlx::SqlitePool;

use crate::state::AppState;

pub struct Answered {
    pub question: String,
    pub reopened: Option<String>,
}

/// Records an owner's spec-question answer. Starting any follow-up work belongs to the caller.
pub async fn answer(
    pool: &SqlitePool,
    id: &str,
    text: &str,
    _origin: &str,
) -> Result<Answered, store::StoreError> {
    let (question, reopened) = store::answer_question(pool, id, text).await?;
    Ok(Answered {
        question: question.question,
        reopened: reopened.then_some(question.spec_id),
    })
}

/// Starts the same one-spec retry used by the finish route after an owner answer reopens a spec.
pub fn relaunch_answered(state: AppState, spec_id: String) {
    tokio::spawn(async move {
        let project_id = match store::get_spec(&state.pool, &spec_id).await {
            Ok(Some(spec)) => spec.project_id,
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, %spec_id, "could not read reopened spec");
                None
            }
        };
        let Some(project_id) = project_id else {
            return;
        };
        let Some(claim) = finish::claim_project(&project_id) else {
            return;
        };
        let _claim = claim;
        if let Err(error) = finish::finish(&state, finish::FinishScope::One { spec_id }).await {
            tracing::warn!(%error, "spec retry after owner answer failed");
        }
    });
}

#[cfg(test)]
mod tests {
    async fn names(pool: &sqlx::SqlitePool, kind: &str) -> Vec<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master WHERE type = ? AND name NOT LIKE 'sqlite_%'",
        )
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn migration_0264_creates_tables_and_indexes() {
        let pool = crate::testdb::fresh_pool().await;

        let tables = names(&pool, "table").await;
        for table in ["specs", "spec_criteria", "spec_deps", "spec_questions"] {
            assert!(tables.iter().any(|t| t == table), "missing table {table}");
        }
        let indexes = names(&pool, "index").await;
        for index in [
            "specs_project_path",
            "specs_status",
            "spec_criteria_spec_ordinal",
            "spec_deps_depends_on",
            "spec_questions_spec_status",
        ] {
            assert!(indexes.iter().any(|i| i == index), "missing index {index}");
        }
        assert!(
            names(&pool, "view")
                .await
                .iter()
                .any(|v| v == "owner_decisions")
        );

        // The view lists a spec question with its origin.
        sqlx::query(
            "INSERT INTO spec_questions (id, spec_id, question, created_at) \
             VALUES ('q1', 's1', 'Which port?', '2026-10-30T10:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let row: (String, String, String) =
            sqlx::query_as("SELECT origin, id, status FROM owner_decisions")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row, ("spec_question".into(), "q1".into(), "open".into()));

        // A dependency source outside the closed vocabulary is refused.
        let bad = sqlx::query(
            "INSERT INTO spec_deps (spec_id, depends_on, source) VALUES ('a', 'b', 'guess')",
        )
        .execute(&pool)
        .await;
        assert!(bad.is_err());
    }
}
