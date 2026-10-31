//! Tree cost: per-tree spending, budgets, cache measurement and the wait policy.

pub mod budget;
pub mod http;
pub mod ledger;
pub mod measure;
pub mod report;
pub mod wait_policy;

#[cfg(test)]
mod tests {
    const NOW: &str = "2026-10-30T10:00:00+00:00";

    async fn names(pool: &sqlx::SqlitePool, kind: &str) -> Vec<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master WHERE type = ? AND name NOT LIKE 'sqlite_%'",
        )
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn insert_node(pool: &sqlx::SqlitePool, id: &str, root_id: &str, run_id: Option<&str>) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, created_at, updated_at) \
             VALUES (?, ?, 0, 'leaf', 't', 'g', 'running', ?, ?, ?)",
        )
        .bind(id)
        .bind(root_id)
        .bind(run_id)
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_run(
        pool: &sqlx::SqlitePool,
        cost: Option<f64>,
        input: i64,
        output: i64,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, cost_usd, created_at, input_tokens, output_tokens, \
             cache_read_tokens, cache_creation_tokens) \
             VALUES ('p', 'completed', ?, ?, ?, ?, 10, 20)",
        )
        .bind(cost)
        .bind(NOW)
        .bind(input)
        .bind(output)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[tokio::test]
    async fn migration_0265_creates_tables_and_view() {
        let pool = crate::testdb::fresh_pool().await;

        let tables = names(&pool, "table").await;
        for table in ["tree_budgets", "cache_waits", "wait_policy"] {
            assert!(tables.iter().any(|t| t == table), "missing table {table}");
        }
        assert!(
            names(&pool, "view")
                .await
                .iter()
                .any(|v| v == "tree_run_costs")
        );

        let keep_warm: String =
            sqlx::query_scalar("SELECT value FROM wait_policy WHERE key = 'keep_warm_enabled'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(keep_warm, "false");
        let rotate: String =
            sqlx::query_scalar("SELECT value FROM wait_policy WHERE key = 'rotate_context_tokens'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(rotate, "60000");

        // A budget with no ceiling keeps the default soft threshold.
        sqlx::query(
            "INSERT INTO tree_budgets (tree_id, created_at, updated_at) VALUES ('root-1', ?, ?)",
        )
        .bind(NOW)
        .bind(NOW)
        .execute(&pool)
        .await
        .unwrap();
        let (limit, soft): (Option<f64>, f64) =
            sqlx::query_as("SELECT limit_usd, soft_pct FROM tree_budgets WHERE tree_id = 'root-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((limit, soft), (None, 80.0));
    }

    #[tokio::test]
    async fn tree_run_costs_joins_text_run_id() {
        let pool = crate::testdb::fresh_pool().await;
        let measured = insert_run(&pool, Some(1.5), 100, 50).await;
        let unmeasured = insert_run(&pool, None, 200, 70).await;
        let other = insert_run(&pool, Some(9.0), 1, 1).await;
        insert_node(&pool, "n1", "root-1", Some(&measured.to_string())).await;
        insert_node(&pool, "n2", "root-1", Some(&unmeasured.to_string())).await;
        insert_node(&pool, "n3", "root-2", Some(&other.to_string())).await;
        insert_node(&pool, "n4", "root-1", None).await;

        let row: (i64, f64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT runs, measured_usd, unmeasured_runs, input_tokens, output_tokens, \
             cache_read_tokens, cache_creation_tokens FROM tree_run_costs WHERE root_id = 'root-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(row, (2, 1.5, 1, 300, 120, 20, 40));
    }
}
