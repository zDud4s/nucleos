//! The work-in-progress brake (spec §8.4/§8.7).
//!
//! The budget bounds what autonomy costs; this bounds what it costs *you*. An unbounded approval
//! queue turns autonomy into a second job — the system generates faster than the human reviews, and
//! the backlog becomes the very bottleneck the Autopilot existed to remove. So a project stops
//! starting new autonomous work once it already has enough unreviewed proposals waiting.
//!
//! Unlike the budget, this brake is self-clearing: it releases the moment the human reviews
//! something, because it throttles on the thing that is actually saturated.

use sqlx::SqlitePool;

/// Whether a project may start another autonomous run, judged by how much already waits for review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WipDecision {
    Allow,
    Defer { reason: String },
}

/// The effective open-proposal ceiling for one project: its own override when set, otherwise the
/// global default. `None` means the brake is switched off (only expressible globally, by design).
pub async fn wip_limit(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<i64>> {
    let project_override: Option<Option<i64>> =
        sqlx::query_scalar("SELECT wip_limit FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;
    if let Some(Some(limit)) = project_override {
        return Ok(Some(limit));
    }
    global_wip_limit(pool).await
}

/// The default ceiling every project inherits when it has no override of its own.
pub async fn global_wip_limit(pool: &SqlitePool) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT wip_limit FROM autopilot_global LIMIT 1")
        .fetch_optional(pool)
        .await
        .map(Option::flatten)
}

/// How much a project has waiting on the human right now.
///
/// Both kinds of waiting, not just proposals. A shadow run never mints a proposal — it records
/// `shadow_decisions` for review instead — so counting proposals alone meant the brake was inert in
/// the one mode whose entire purpose is to accumulate reviewable evidence. A busy watched branch
/// could pile up an unbounded backlog while this kept answering Allow, which is precisely the
/// failure §8.4 describes: the system generating faster than the human reviews.
///
/// Kept as one query so the roster's `queue_full` flag and this gate stay the same arithmetic.
pub const OPEN_REVIEW_ITEMS_SQL: &str = "SELECT
    (SELECT COUNT(*) FROM proposals
     WHERE project_id = ?1 AND status = 'pending')
    +
    (SELECT COUNT(*) FROM shadow_decisions
     JOIN runs ON shadow_decisions.run_id = runs.id
     WHERE runs.project_id = ?1 AND shadow_decisions.human_verdict IS NULL)";

pub async fn open_proposals(pool: &SqlitePool, project_id: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar(OPEN_REVIEW_ITEMS_SQL)
        .bind(project_id)
        .fetch_one(pool)
        .await
}

/// PURE: whether a queue at `open` is full against `limit`. Split out so the daemon's decision and
/// the roster flag the shell renders are the same comparison.
pub fn queue_full(open: i64, limit: Option<i64>) -> bool {
    limit.is_some_and(|limit| open >= limit)
}

/// Fails CLOSED — an unreadable queue defers rather than piling more onto a human who may already
/// be behind, the same posture the budget gate takes.
pub async fn wip_permits_new_run(pool: &SqlitePool, project_id: &str) -> WipDecision {
    let limit = match wip_limit(pool, project_id).await {
        Ok(None) => return WipDecision::Allow,
        Ok(Some(limit)) => limit,
        Err(error) => {
            return WipDecision::Defer {
                reason: format!("could not read the WIP limit: {error}"),
            };
        }
    };

    let open = match open_proposals(pool, project_id).await {
        Ok(open) => open,
        Err(error) => {
            return WipDecision::Defer {
                reason: format!("could not count open proposals: {error}"),
            };
        }
    };

    if queue_full(open, Some(limit)) {
        return WipDecision::Defer {
            reason: format!("{open} proposals already waiting for review (limit {limit})"),
        };
    }
    WipDecision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> SqlitePool {
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

    async fn add_project(pool: &SqlitePool, project_id: &str) {
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, 'shadow')")
            .bind(project_id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Shadow work waiting for review: a run owned by the project, plus decisions recorded under it
    /// with no human verdict yet. This is what a shadow run actually produces — no proposal is ever
    /// minted — which is why the brake has to see it.
    async fn add_unreviewed_shadow_decisions(pool: &SqlitePool, project_id: &str, count: usize) {
        let run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES (?, 'shadow work', 'completed', 'shadow', '2026-07-27T00:00:00Z')",
        )
        .bind(project_id)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();

        for index in 0..count {
            sqlx::query(
                "INSERT INTO shadow_decisions
                 (run_id, tool_name, decision, action_class, classifier_version, created_at)
                 VALUES (?, 'Bash', 'allow', 'read-local', 2, ?)",
            )
            .bind(run_id)
            .bind(format!("2026-07-27T00:0{index}:00Z"))
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn add_pending_proposals(pool: &SqlitePool, project_id: &str, count: usize) {
        for index in 0..count {
            sqlx::query(
                "INSERT INTO proposals (kind, status, run_id, project_id, reasoning, created_at)
                 VALUES ('action-approval', 'pending', ?, ?, 'test', '2026-07-27T00:00:00Z')",
            )
            .bind(index as i64 + 1)
            .bind(project_id)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    #[test]
    fn queue_full_only_when_a_limit_is_set_and_reached() {
        assert!(!queue_full(9, None));
        assert!(!queue_full(2, Some(3)));
        assert!(queue_full(3, Some(3)));
        // Over the line (a limit lowered under an existing queue) still counts as full.
        assert!(queue_full(5, Some(3)));
    }

    /// The brake counted `proposals`, and a shadow run never mints one — it records
    /// `shadow_decisions` for the human to review instead. So the one mode whose entire purpose is
    /// to accumulate reviewable evidence was the one mode the review-backlog brake ignored, and a
    /// busy watched branch could pile up an unbounded queue while `wip_permits_new_run` kept
    /// answering Allow.
    #[tokio::test]
    async fn unreviewed_shadow_decisions_count_towards_the_queue() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_unreviewed_shadow_decisions(&pool, "project-a", 3).await;

        assert_eq!(open_proposals(&pool, "project-a").await.unwrap(), 3);
        assert!(matches!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Defer { .. }
        ));
    }

    #[tokio::test]
    async fn a_reviewed_shadow_decision_releases_the_brake() {
        // Self-clearing is the property that makes this brake tolerable: it has to throttle on what
        // is actually saturated, and release the moment the human clears it.
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_unreviewed_shadow_decisions(&pool, "project-a", 3).await;
        sqlx::query("UPDATE shadow_decisions SET human_verdict = 'approve' WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(open_proposals(&pool, "project-a").await.unwrap(), 2);
        assert!(matches!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow
        ));
    }

    #[tokio::test]
    async fn proposals_and_shadow_decisions_add_up() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_pending_proposals(&pool, "project-a", 2).await;
        add_unreviewed_shadow_decisions(&pool, "project-a", 1).await;

        assert_eq!(open_proposals(&pool, "project-a").await.unwrap(), 3);
    }

    #[tokio::test]
    async fn the_global_default_is_on_at_three() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;

        // Migration 0017 ships the brake engaged, so a fresh install is protected by default.
        assert_eq!(global_wip_limit(&pool).await.unwrap(), Some(3));
        assert_eq!(wip_limit(&pool, "project-a").await.unwrap(), Some(3));
    }

    #[tokio::test]
    async fn a_project_override_wins_over_the_global_default() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_project(&pool, "project-b").await;
        sqlx::query("UPDATE autopilot_state SET wip_limit = 1 WHERE project_id = 'project-a'")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(wip_limit(&pool, "project-a").await.unwrap(), Some(1));
        // project-b left its override NULL, so it still inherits.
        assert_eq!(wip_limit(&pool, "project-b").await.unwrap(), Some(3));
    }

    #[tokio::test]
    async fn a_null_global_limit_switches_the_brake_off() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        sqlx::query("UPDATE autopilot_global SET wip_limit = NULL")
            .execute(&pool)
            .await
            .unwrap();
        add_pending_proposals(&pool, "project-a", 50).await;

        assert_eq!(wip_limit(&pool, "project-a").await.unwrap(), None);
        assert_eq!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow
        );
    }

    #[tokio::test]
    async fn a_full_queue_defers_and_an_empty_one_allows() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;

        assert_eq!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow
        );

        add_pending_proposals(&pool, "project-a", 3).await;

        let WipDecision::Defer { reason } = wip_permits_new_run(&pool, "project-a").await else {
            panic!("a full queue must defer");
        };
        assert!(reason.contains('3'), "got: {reason}");
    }

    #[tokio::test]
    async fn decided_proposals_release_the_brake() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_pending_proposals(&pool, "project-a", 3).await;
        sqlx::query("UPDATE proposals SET status = 'approved' WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();

        // Self-clearing: reviewing ONE item is enough to let work start again.
        assert_eq!(open_proposals(&pool, "project-a").await.unwrap(), 2);
        assert_eq!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow
        );
    }

    #[tokio::test]
    async fn the_queue_is_counted_per_project() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_project(&pool, "project-b").await;
        add_pending_proposals(&pool, "project-a", 3).await;

        assert_eq!(open_proposals(&pool, "project-b").await.unwrap(), 0);
        assert_eq!(
            wip_permits_new_run(&pool, "project-b").await,
            WipDecision::Allow
        );
    }
}
