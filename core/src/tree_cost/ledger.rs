//! stub: tree cost ledger

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{DateTime, Utc};
    use sqlx::SqlitePool;

    const STARTED: &str = "2026-10-30T09:00:00+00:00";
    const NOW: &str = "2026-10-30T10:00:00+00:00";
    const EPSILON: f64 = 1e-9;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn assert_close(actual: f64, expected: f64, what: &str) {
        assert!(
            (actual - expected).abs() < EPSILON,
            "{what}: expected {expected}, got {actual}"
        );
    }

    async fn insert_node(pool: &SqlitePool, id: &str, root_id: &str, run_id: Option<i64>) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, created_at, updated_at) \
             VALUES (?, ?, 0, 'leaf', 't', 'g', 'done', ?, ?, ?)",
        )
        .bind(id)
        .bind(root_id)
        .bind(run_id.map(|run| run.to_string()))
        .bind(STARTED)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A finished run that lasted exactly one hour; `model_watch` stays NULL.
    async fn insert_run(pool: &SqlitePool, cost: Option<f64>, input: i64, output: i64) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, cost_usd, created_at, completed_at, input_tokens, \
             output_tokens, cache_read_tokens, cache_creation_tokens) \
             VALUES ('p', 'completed', ?, ?, ?, ?, ?, 10, 20)",
        )
        .bind(cost)
        .bind(STARTED)
        .bind(NOW)
        .bind(input)
        .bind(output)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn insert_verdict(pool: &SqlitePool, run_id: i64, cost: f64) {
        sqlx::query(
            "INSERT INTO judge_verdicts \
             (run_id, tool_name, tool_input_digest, action_class, classifier_decision, judge, \
              model, questions_version, final_decision, cost_usd, created_at) \
             VALUES (?, 'Bash', 'd', 'unrecognized', 'pending_approval', 'observe', 'm', 1, \
                     'pending_approval', ?, ?)",
        )
        .bind(run_id)
        .bind(cost)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_resolution(pool: &SqlitePool, run_id: i64, cost: f64) {
        sqlx::query(
            "INSERT INTO judge_resolutions \
             (run_id, lineage_root_id, event, tool_input_digest, default_outcome, final_outcome, \
              cost_usd, created_at) \
             VALUES (?, ?, 'park', 'd', 'park', 'park', ?, ?)",
        )
        .bind(run_id)
        .bind(run_id)
        .bind(cost)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn set_time_rate(pool: &SqlitePool, rate_per_hour: f64) {
        let updated = sqlx::query("UPDATE autopilot_global SET budget_time_cost_per_hour_usd = ?")
            .bind(rate_per_hour)
            .execute(pool)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(updated, 1);
    }

    #[tokio::test]
    async fn sums_measured_runs_by_root() {
        let pool = crate::testdb::fresh_pool().await;
        let first = insert_run(&pool, Some(0.5), 100, 10).await;
        let second = insert_run(&pool, Some(1.5), 200, 20).await;
        insert_node(&pool, "n1", "root-1", Some(first)).await;
        insert_node(&pool, "n2", "root-1", Some(second)).await;
        // A node that never launched a run adds nothing.
        insert_node(&pool, "n3", "root-1", None).await;

        let cost = tree_cost(&pool, "root-1", at(NOW)).await.unwrap();

        assert_eq!(cost.runs, 2);
        assert_eq!(cost.unmeasured_runs, 0);
        assert_close(cost.measured_usd, 2.0, "measured_usd");
        assert_close(cost.unmeasured_usd, 0.0, "unmeasured_usd");
        assert_close(cost.judges_usd, 0.0, "judges_usd");
        assert_close(cost.total_usd, 2.0, "total_usd");
        assert_eq!(cost.input_tokens, 300);
        assert_eq!(cost.output_tokens, 30);
        assert_eq!(cost.cache_read_tokens, 20);
        assert_eq!(cost.cache_creation_tokens, 40);
    }

    #[tokio::test]
    async fn unmeasured_run_is_never_zero() {
        let pool = crate::testdb::fresh_pool().await;
        set_time_rate(&pool, 6.0).await;
        let measured = insert_run(&pool, Some(0.5), 1, 1).await;
        let unmeasured = insert_run(&pool, None, 1, 1).await;
        insert_node(&pool, "n1", "root-1", Some(measured)).await;
        insert_node(&pool, "n2", "root-1", Some(unmeasured)).await;

        let cost = tree_cost(&pool, "root-1", at(NOW)).await.unwrap();

        // Priced by the governance rule at the configured rate, not by a local copy of it.
        let expected = crate::governance::budget::unmeasured_usd(
            at(STARTED),
            at(NOW),
            None,
            None,
            true,
            6.0,
        );
        assert!(expected > 0.0);
        assert_eq!(cost.runs, 2);
        assert_eq!(cost.unmeasured_runs, 1);
        assert!(cost.unmeasured_usd > 0.0, "an unmeasured run is never free");
        assert_close(cost.unmeasured_usd, expected, "unmeasured_usd");
        assert_close(cost.measured_usd, 0.5, "measured_usd");
        assert_close(cost.total_usd, 0.5 + expected, "total_usd");
    }

    #[tokio::test]
    async fn runs_of_other_trees_are_excluded() {
        let pool = crate::testdb::fresh_pool().await;
        let own = insert_run(&pool, Some(1.5), 100, 10).await;
        let other_measured = insert_run(&pool, Some(9.0), 900, 90).await;
        let other_unmeasured = insert_run(&pool, None, 900, 90).await;
        insert_node(&pool, "n1", "root-1", Some(own)).await;
        insert_node(&pool, "n2", "root-2", Some(other_measured)).await;
        insert_node(&pool, "n3", "root-2", Some(other_unmeasured)).await;

        let cost = tree_cost(&pool, "root-1", at(NOW)).await.unwrap();

        assert_eq!(cost.runs, 1);
        assert_eq!(cost.unmeasured_runs, 0);
        assert_close(cost.measured_usd, 1.5, "measured_usd");
        assert_close(cost.unmeasured_usd, 0.0, "unmeasured_usd");
        assert_close(cost.total_usd, 1.5, "total_usd");
        assert_eq!(cost.input_tokens, 100);
        assert_eq!(cost.output_tokens, 10);
    }

    #[tokio::test]
    async fn judges_of_tree_runs_are_added() {
        let pool = crate::testdb::fresh_pool().await;
        let own = insert_run(&pool, Some(1.0), 1, 1).await;
        let other = insert_run(&pool, Some(9.0), 1, 1).await;
        insert_node(&pool, "n1", "root-1", Some(own)).await;
        insert_node(&pool, "n2", "root-2", Some(other)).await;
        // Two verdicts beside one resolution on the same run: a single join of both tables would
        // count the resolution twice.
        insert_verdict(&pool, own, 0.25).await;
        insert_verdict(&pool, own, 0.5).await;
        insert_resolution(&pool, own, 1.0).await;
        insert_verdict(&pool, other, 16.0).await;
        insert_resolution(&pool, other, 32.0).await;

        let cost = tree_cost(&pool, "root-1", at(NOW)).await.unwrap();

        assert_close(cost.judges_usd, 1.75, "judges_usd");
        assert_close(cost.measured_usd, 1.0, "measured_usd");
        assert_close(cost.total_usd, 2.75, "total_usd");
    }

    #[tokio::test]
    async fn root_without_runs_is_all_zero() {
        let pool = crate::testdb::fresh_pool().await;
        insert_node(&pool, "n1", "root-1", None).await;

        // One root that has nodes but no run, and one nothing has ever heard of.
        for root in ["root-1", "root-unknown"] {
            let cost = tree_cost(&pool, root, at(NOW)).await.unwrap();

            assert_eq!(cost.runs, 0);
            assert_eq!(cost.unmeasured_runs, 0);
            assert_eq!(cost.measured_usd, 0.0);
            assert_eq!(cost.unmeasured_usd, 0.0);
            assert_eq!(cost.judges_usd, 0.0);
            assert_eq!(cost.total_usd, 0.0);
            assert_eq!(cost.input_tokens, 0);
            assert_eq!(cost.output_tokens, 0);
            assert_eq!(cost.cache_read_tokens, 0);
            assert_eq!(cost.cache_creation_tokens, 0);
        }
    }
}
