//! A tree's spending budget (spec 5.2): whether its next node may open, and the one feed line that
//! says it crossed the soft threshold.
//!
//! Nothing here changes a node or a run: a node in flight is never killed. The limit only stops the
//! next node from opening.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use super::ledger::tree_cost;
use crate::governance::budget::load_budget_config;

/// The default of `tree_budgets.soft_pct`, for a tree with no row in that table.
const DEFAULT_SOFT_PCT: f64 = 80.0;

/// Whether the next node of a tree may open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permit {
    Yes,
    No { reason: String },
}

/// A tree's spend against its budget. With no limit, nothing can be crossed.
#[derive(Debug, Clone, PartialEq)]
pub struct Spend {
    pub total_usd: f64,
    pub limit_usd: Option<f64>,
    pub soft_pct: f64,
    pub soft_crossed: bool,
    pub hard_crossed: bool,
}

/// The tree's `tree_budgets` row as `(limit_usd, soft_pct)`, if it has one.
async fn load_tree_budget(
    pool: &SqlitePool,
    root_id: &str,
) -> sqlx::Result<Option<(Option<f64>, f64)>> {
    sqlx::query_as("SELECT limit_usd, soft_pct FROM tree_budgets WHERE tree_id = ?")
        .bind(root_id)
        .fetch_optional(pool)
        .await
}

/// What the tree has spent, against its limit and soft threshold.
pub async fn tree_spend(
    pool: &SqlitePool,
    root_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Spend> {
    let total_usd = tree_cost(pool, root_id, now).await?.total_usd;
    let (limit_usd, soft_pct) = load_tree_budget(pool, root_id)
        .await?
        .unwrap_or((None, DEFAULT_SOFT_PCT));
    let (soft_crossed, hard_crossed) = match limit_usd {
        Some(limit) => (total_usd >= limit * soft_pct / 100.0, total_usd >= limit),
        None => (false, false),
    };
    Ok(Spend {
        total_usd,
        limit_usd,
        soft_pct,
        soft_crossed,
        hard_crossed,
    })
}

/// Reads the limit first, so a tree with no ceiling is permitted without pricing anything: only a
/// tree that has a limit can be refused over what it spent.
async fn evaluate_next_node(
    pool: &SqlitePool,
    root_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Permit> {
    let Some((Some(limit), _)) = load_tree_budget(pool, root_id).await? else {
        return Ok(Permit::Yes);
    };

    let spent = tree_cost(pool, root_id, now).await?.total_usd;
    if spent >= limit {
        return Ok(Permit::No {
            reason: format!("tree budget reached: spent ${spent:.2} of the ${limit:.2} limit"),
        });
    }
    // The same strict rule as the global ceiling in `governance::budget::evaluate_budget`.
    let reserve = load_budget_config(pool).await?.per_run_reserve_usd;
    if spent + reserve > limit {
        return Ok(Permit::No {
            reason: format!(
                "tree budget: spent ${spent:.2} + ${reserve:.2} reserve would exceed the ${limit:.2} limit"
            ),
        });
    }
    Ok(Permit::Yes)
}

/// Whether the tree's budget permits opening its next node. Fails closed on any database error, as
/// `governance::budget::budget_permits_new_run` does.
pub async fn permits_next_node(pool: &SqlitePool, root_id: &str) -> Permit {
    match evaluate_next_node(pool, root_id, Utc::now()).await {
        Ok(permit) => permit,
        Err(error) => Permit::No {
            reason: format!("tree budget check failed, refusing to be safe: {error}"),
        },
    }
}

/// Writes the tree's `tree_budget_soft` feed line the first time its soft threshold is found
/// crossed, and says whether this call wrote it. One statement, so two concurrent callers cannot
/// both write it. The guard compares the whole subject, so `tree-1` and `tree-10` stay apart.
pub async fn report_soft_threshold(
    pool: &SqlitePool,
    root_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let spend = tree_spend(pool, root_id, now).await?;
    let (true, Some(limit)) = (spend.soft_crossed, spend.limit_usd) else {
        return Ok(false);
    };

    let summary = format!(
        "tree {root_id} crossed its soft budget threshold ({}%): spent ${:.2} of the ${limit:.2} limit",
        spend.soft_pct, spend.total_usd
    );
    let subject = format!("tree:{root_id}");
    let written = sqlx::query(
        "INSERT INTO feed (kind, summary, subject, created_at)
         SELECT 'tree_budget_soft', ?, ?, ?
         WHERE NOT EXISTS (SELECT 1 FROM feed WHERE kind = 'tree_budget_soft' AND subject = ?)",
    )
    .bind(summary)
    .bind(&subject)
    .bind(now.to_rfc3339())
    .bind(&subject)
    .execute(pool)
    .await?
    .rows_affected()
        == 1;
    if written {
        // `nucleos_base::feed::append` always does this; a raw insert has to do it itself.
        nucleos_base::wake::bump(nucleos_base::wake::Topic::Feed);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{DateTime, Utc};
    use sqlx::SqlitePool;

    const NOW: &str = "2026-10-30T10:00:00+00:00";
    const EPSILON: f64 = 1e-9;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(NOW)
            .unwrap()
            .with_timezone(&Utc)
    }

    async fn insert_node(pool: &SqlitePool, id: &str, root_id: &str, status: &str, run_id: i64) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, created_at, updated_at) \
             VALUES (?, ?, 0, 'leaf', 't', 'g', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(root_id)
        .bind(status)
        .bind(run_id.to_string())
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Adds a finished node with a measured run, so the tree's spend does not depend on the clock.
    async fn spend(pool: &SqlitePool, node_id: &str, root_id: &str, cost: f64) {
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, cost_usd, created_at, completed_at) \
             VALUES ('p', 'completed', ?, ?, ?)",
        )
        .bind(cost)
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        insert_node(pool, node_id, root_id, "done", run_id).await;
    }

    async fn set_budget(pool: &SqlitePool, root_id: &str, limit: Option<f64>, soft_pct: f64) {
        sqlx::query(
            "INSERT INTO tree_budgets (tree_id, limit_usd, soft_pct, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(root_id)
        .bind(limit)
        .bind(soft_pct)
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn set_reserve(pool: &SqlitePool, reserve: f64) {
        let updated = sqlx::query("UPDATE autopilot_global SET budget_per_run_reserve_usd = ?")
            .bind(reserve)
            .execute(pool)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(updated, 1);
    }

    async fn soft_feed_summaries(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'tree_budget_soft' ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn report(pool: &SqlitePool, root_id: &str) -> bool {
        report_soft_threshold(pool, root_id, now()).await.unwrap()
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    fn assert_refused_over_budget(permit: Permit) {
        match permit {
            Permit::No { reason } => assert!(
                reason.to_lowercase().contains("budget"),
                "the reason must mention the budget: {reason}"
            ),
            Permit::Yes => panic!("expected a refusal"),
        }
    }

    #[tokio::test]
    async fn no_budget_means_permit() {
        let pool = crate::testdb::fresh_pool().await;
        set_reserve(&pool, 0.5).await;
        spend(&pool, "n1", "no-row", 1024.0).await;
        spend(&pool, "n2", "null-limit", 1024.0).await;
        set_budget(&pool, "null-limit", None, 80.0).await;

        assert_eq!(permits_next_node(&pool, "no-row").await, Permit::Yes);
        assert_eq!(permits_next_node(&pool, "null-limit").await, Permit::Yes);
    }

    #[tokio::test]
    async fn permits_below_limit_minus_reserve() {
        let pool = crate::testdb::fresh_pool().await;
        set_reserve(&pool, 0.5).await;
        set_budget(&pool, "root-1", Some(2.0), 80.0).await;

        spend(&pool, "n1", "root-1", 1.0).await;
        assert_eq!(permits_next_node(&pool, "root-1").await, Permit::Yes);

        // spent + reserve == limit: the rule is a strict "greater than", so this still permits.
        spend(&pool, "n2", "root-1", 0.5).await;
        assert_eq!(permits_next_node(&pool, "root-1").await, Permit::Yes);

        // Spent is still below the limit, but the reserve no longer fits.
        spend(&pool, "n3", "root-1", 0.25).await;
        assert_refused_over_budget(permits_next_node(&pool, "root-1").await);
    }

    #[tokio::test]
    async fn refuses_next_node_at_limit() {
        let pool = crate::testdb::fresh_pool().await;
        set_reserve(&pool, 0.5).await;
        set_budget(&pool, "root-1", Some(2.0), 80.0).await;
        spend(&pool, "n1", "root-1", 2.0).await;

        assert_refused_over_budget(permits_next_node(&pool, "root-1").await);
    }

    #[tokio::test]
    async fn soft_threshold_reports_once() {
        let pool = crate::testdb::fresh_pool().await;
        // `tree-1` is a prefix of `tree-10`, on purpose: the guard must tell them apart.
        set_budget(&pool, "tree-10", Some(10.0), 50.0).await;
        set_budget(&pool, "tree-1", Some(10.0), 50.0).await;

        // Below the threshold.
        spend(&pool, "n1", "tree-10", 4.0).await;
        assert!(!report(&pool, "tree-10").await);
        assert!(soft_feed_summaries(&pool).await.is_empty());

        // Crossed, exactly at the threshold.
        spend(&pool, "n2", "tree-10", 1.0).await;
        assert!(report(&pool, "tree-10").await);
        let summaries = soft_feed_summaries(&pool).await;
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].contains("tree-10"), "{}", summaries[0]);

        // Asked again, still crossed: nothing new.
        assert!(!report(&pool, "tree-10").await);
        assert_eq!(soft_feed_summaries(&pool).await.len(), 1);

        // Another root reports its own, even with a first line whose summary contains its id.
        spend(&pool, "n3", "tree-1", 8.0).await;
        assert!(report(&pool, "tree-1").await);
        let summaries = soft_feed_summaries(&pool).await;
        assert_eq!(summaries.len(), 2);
        assert!(summaries[1].contains("tree-1"), "{}", summaries[1]);
        assert!(!summaries[1].contains("tree-10"), "{}", summaries[1]);

        // No budget, or no ceiling: there is no threshold to cross.
        spend(&pool, "n4", "no-row", 1024.0).await;
        spend(&pool, "n5", "null-limit", 1024.0).await;
        set_budget(&pool, "null-limit", None, 50.0).await;
        assert!(!report(&pool, "no-row").await);
        assert!(!report(&pool, "null-limit").await);
        assert_eq!(soft_feed_summaries(&pool).await.len(), 2);
    }

    #[tokio::test]
    async fn in_flight_node_is_never_killed() {
        let pool = crate::testdb::fresh_pool().await;
        set_reserve(&pool, 0.5).await;
        set_budget(&pool, "root-1", Some(1.0), 50.0).await;
        spend(&pool, "n-done", "root-1", 3.0).await;
        let live_run =
            sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('p', 'running', ?)")
                .bind(NOW)
                .execute(&pool)
                .await
                .unwrap()
                .last_insert_rowid();
        insert_node(&pool, "n-live", "root-1", "running", live_run).await;

        // Every entry point of this module, over the limit: none of them may touch a status.
        assert_refused_over_budget(permits_next_node(&pool, "root-1").await);
        let spent = tree_spend(&pool, "root-1", now()).await.unwrap();
        assert!(spent.hard_crossed);
        assert!(report(&pool, "root-1").await);

        let node_status: String =
            sqlx::query_scalar("SELECT status FROM work_nodes WHERE id = 'n-live'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(node_status, "running");
        let (run_status, completed_at): (String, Option<String>) =
            sqlx::query_as("SELECT status, completed_at FROM runs WHERE id = ?")
                .bind(live_run)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(run_status, "running");
        assert_eq!(completed_at, None);
    }

    #[tokio::test]
    async fn database_error_refuses_next_node() {
        let pool = crate::testdb::fresh_pool().await;
        set_budget(&pool, "root-1", Some(1024.0), 80.0).await;
        pool.close().await;

        // Even a tree far below its limit: an unreadable budget fails closed.
        assert_refused_over_budget(permits_next_node(&pool, "root-1").await);
    }

    #[tokio::test]
    async fn tree_spend_reports_total_limit_and_thresholds() {
        let pool = crate::testdb::fresh_pool().await;
        set_budget(&pool, "root-1", Some(10.0), 50.0).await;

        spend(&pool, "n1", "root-1", 4.0).await;
        let below = tree_spend(&pool, "root-1", now()).await.unwrap();
        assert_close(below.total_usd, 4.0);
        assert_eq!(below.limit_usd, Some(10.0));
        assert_eq!(below.soft_pct, 50.0);
        assert!(!below.soft_crossed);
        assert!(!below.hard_crossed);

        // Both thresholds are ">=": exactly on one counts as crossed.
        spend(&pool, "n2", "root-1", 1.0).await;
        let at_soft = tree_spend(&pool, "root-1", now()).await.unwrap();
        assert!(at_soft.soft_crossed);
        assert!(!at_soft.hard_crossed);

        spend(&pool, "n3", "root-1", 5.0).await;
        let at_hard = tree_spend(&pool, "root-1", now()).await.unwrap();
        assert_close(at_hard.total_usd, 10.0);
        assert!(at_hard.soft_crossed);
        assert!(at_hard.hard_crossed);

        // With no ceiling the spend is still reported, and nothing can be crossed.
        spend(&pool, "n4", "no-row", 2.0).await;
        let unlimited = tree_spend(&pool, "no-row", now()).await.unwrap();
        assert_close(unlimited.total_usd, 2.0);
        assert_eq!(unlimited.limit_usd, None);
        assert!(!unlimited.soft_crossed);
        assert!(!unlimited.hard_crossed);

        set_budget(&pool, "null-limit", None, 80.0).await;
        let null_limit = tree_spend(&pool, "null-limit", now()).await.unwrap();
        assert_eq!(null_limit.limit_usd, None);
        assert_eq!(null_limit.soft_pct, 80.0);
        assert!(!null_limit.soft_crossed);
        assert!(!null_limit.hard_crossed);
    }
}
