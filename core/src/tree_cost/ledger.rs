//! What one tree of work nodes has cost so far: what its runs reported, what the runs that reported
//! nothing are assumed to have cost, and what the judge cost on those runs (spec 5.1).
//!
//! The sums and tokens are the view `tree_run_costs`, and the price of an unmeasured run is
//! `governance::budget::unmeasured_usd`: neither is restated here. Only the judge sum is local,
//! because `judge_spend` has no per-tree scope. The token estimate spec 5.1 adds for top-level
//! sessions (`devtime_messages`) is not part of this figure yet.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::governance::budget::{load_budget_config, parse_run_timestamp, unmeasured_usd};

/// The cost of one tree. `total_usd` is `measured_usd + unmeasured_usd + judges_usd`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TreeCost {
    pub runs: i64,
    pub unmeasured_runs: i64,
    pub measured_usd: f64,
    pub unmeasured_usd: f64,
    pub judges_usd: f64,
    pub total_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

/// The cost of the tree rooted at `root_id`. `now` closes the duration of a run still open. A root
/// with no run has no row in the view, and costs nothing.
pub async fn tree_cost(
    pool: &SqlitePool,
    root_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<TreeCost> {
    let row: Option<(i64, i64, f64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT runs, unmeasured_runs, measured_usd,
                input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens
         FROM tree_run_costs WHERE root_id = ?",
    )
    .bind(root_id)
    .fetch_optional(pool)
    .await?;
    let Some((
        runs,
        unmeasured_runs,
        measured_usd,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
    )) = row
    else {
        return Ok(TreeCost::default());
    };

    let unmeasured = if unmeasured_runs == 0 {
        0.0
    } else {
        price_unmeasured_runs(pool, root_id, now).await?
    };
    let judges_usd = judges_usd(pool, root_id).await?;

    Ok(TreeCost {
        runs,
        unmeasured_runs,
        measured_usd,
        unmeasured_usd: unmeasured,
        judges_usd,
        total_usd: measured_usd + unmeasured + judges_usd,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
    })
}

/// The tree's runs with no reported cost, each priced by the governance rule at the configured rate.
async fn price_unmeasured_runs(
    pool: &SqlitePool,
    root_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<f64> {
    let rate = load_budget_config(pool).await?.time_cost_per_hour_usd;
    let rows: Vec<(String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT r.created_at, r.completed_at, r.model_watch, r.model_requested_at
         FROM work_nodes n
         JOIN runs r ON r.id = CAST(n.run_id AS INTEGER)
         WHERE n.root_id = ? AND r.cost_usd IS NULL",
    )
    .bind(root_id)
    .fetch_all(pool)
    .await?;

    let mut total = 0.0;
    for (created_at, completed, model_watch, model_requested_at) in rows {
        let completed_at = completed.as_deref().map(parse_run_timestamp).transpose()?;
        total += unmeasured_usd(
            parse_run_timestamp(&created_at)?,
            completed_at.unwrap_or(now),
            model_watch.as_deref(),
            model_requested_at.as_deref(),
            completed_at.is_some(),
            rate,
        );
    }
    Ok(total)
}

/// What the judge cost on the tree's runs. Like `judge_spend`, every row counts, observation
/// included, but over the whole life of the tree instead of a window. Each table is summed in its
/// own subselect: joining them would count a resolution once per verdict on the same run.
async fn judges_usd(pool: &SqlitePool, root_id: &str) -> sqlx::Result<f64> {
    sqlx::query_scalar(
        "SELECT
           (SELECT COALESCE(SUM(cost_usd), 0.0) FROM judge_verdicts
            WHERE run_id IN (SELECT r.id FROM work_nodes n
                             JOIN runs r ON r.id = CAST(n.run_id AS INTEGER)
                             WHERE n.root_id = ?1))
         + (SELECT COALESCE(SUM(cost_usd), 0.0) FROM judge_resolutions
            WHERE run_id IN (SELECT r.id FROM work_nodes n
                             JOIN runs r ON r.id = CAST(n.run_id AS INTEGER)
                             WHERE n.root_id = ?1))",
    )
    .bind(root_id)
    .fetch_one(pool)
    .await
}

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
