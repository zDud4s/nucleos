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
