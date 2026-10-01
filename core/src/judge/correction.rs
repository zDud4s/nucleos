//! §spec autopilot-juiz-resolve-bloqueios
//!
//! Spec E4: a completed worktree run whose gate failed, and the correction, which is one more
//! turn in the same conversation and tree, with a clock of its own (D6). This file owns the
//! questions about a whole LINEAGE that decide whether a correction may happen.

use sqlx::{SqliteConnection, SqlitePool};

/// Spec D6, condition 2 (S2): any trace in the WHOLE lineage of an approved risky action or of
/// git, as the reason a correction goes to the owner, or `None`.
///
/// - ANY `action_grants` row, whatever its class or state: grants exist only for risky actions
///   the owner approved, and telling a "harmless" one apart is a judgement that cannot be made
///   safely. The owner knows what they approved and why.
/// - ANY `vcs_requests` row not `rejected` or `cancelled`: only those two say nothing happened.
///   A list of "bad" states would leave out `running`, `escalated`, `blocked` and whatever state
///   is added next. "Of the lineage" is three ways: by a lineage run's `run_id`; queued as
///   `Origin::Human` by an approval in the lineage (the grant's `queued_request_id`, already
///   refused by the first rule and kept so the two stay independent); and naming the tree's
///   branch anywhere in its arguments, which catches a human request with no run at all.
///
/// On a connection, because it is asked twice: before the transaction (fast refusal) and inside
/// it, after the correction row took the write lock, where it can no longer change until commit.
#[cfg_attr(not(test), allow(dead_code))] // consumed by Task 6.4 and Task 7.1
pub(crate) async fn lineage_trace_on(
    conn: &mut SqliteConnection,
    root: i64,
    branch: Option<&str>,
) -> sqlx::Result<Option<String>> {
    let granted: Option<i64> = sqlx::query_scalar(
        "SELECT g.proposal_id FROM action_grants g JOIN runs r ON r.id = g.run_id
         WHERE r.id = ?1 OR r.lineage_root_id = ?1
         LIMIT 1",
    )
    .bind(root)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(proposal_id) = granted {
        return Ok(Some(format!(
            "an action was approved in this lineage (proposal #{proposal_id})"
        )));
    }
    let request: Option<(i64, String)> = sqlx::query_as(
        "SELECT v.id, v.status FROM vcs_requests v
         WHERE v.status NOT IN ('rejected', 'cancelled')
           AND (v.run_id IN (SELECT id FROM runs WHERE id = ?1 OR lineage_root_id = ?1)
                OR v.id IN (SELECT g.queued_request_id FROM action_grants g
                            JOIN runs r ON r.id = g.run_id
                            WHERE (r.id = ?1 OR r.lineage_root_id = ?1)
                              AND g.queued_request_id IS NOT NULL)
                OR (?2 IS NOT NULL
                    AND EXISTS (SELECT 1 FROM json_each(v.args) WHERE json_each.value = ?2)))
         ORDER BY v.id
         LIMIT 1",
    )
    .bind(root)
    .bind(branch)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(request.map(|(id, status)| {
        format!("git request {id} of this lineage is {status}, so its work may already have left")
    }))
}

/// Spec D6, condition 8 (S8): whether any run of the lineage read a stranger's words. No
/// worktree run consults the mark today (spec §1.2), so the judge consults it itself: an
/// automatic continuation, with nobody watching, of a session that may have read a stranger is
/// exactly the case to hand to the owner.
#[cfg_attr(not(test), allow(dead_code))] // consumed by Task 6.4 and Task 7.1
pub(crate) async fn lineage_read_untrusted(pool: &SqlitePool, root: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM runs WHERE (id = ?1 OR lineage_root_id = ?1) AND read_untrusted = 1)",
    )
    .bind(root)
    .fetch_one(pool)
    .await
}

/// Spec D6, condition 7: the project's corrections in the last 24 hours. A rolling day rather
/// than a calendar one: no timezone to choose, and no midnight at which six can happen in an
/// hour.
#[cfg_attr(not(test), allow(dead_code))] // consumed by Task 6.4 and Task 7.1
pub(crate) async fn corrections_in_last_day_on(
    conn: &mut SqliteConnection,
    project_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM judge_corrections WHERE project_id = ? AND created_at >= ?",
    )
    .bind(project_id)
    .bind((now - chrono::Duration::hours(24)).to_rfc3339())
    .fetch_one(&mut *conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// A root, its handoff successor, and a stranger; returns (root, successor).
    async fn lineage(pool: &SqlitePool) -> (i64, i64) {
        let insert = |root: Option<i64>| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO runs (project_id, prompt, status, mode, lineage_root_id, created_at)
                     VALUES ('p', 'x', 'completed', 'worktree', ?, '2026-09-27T00:00:00Z')",
                )
                .bind(root)
                .execute(&pool)
                .await
                .unwrap()
                .last_insert_rowid()
            }
        };
        let root = insert(None).await;
        let successor = insert(Some(root)).await;
        insert(None).await;
        (root, successor)
    }

    async fn request(pool: &SqlitePool, run_id: Option<i64>, status: &str, args: &str) {
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, run_id, status, created_at)
             VALUES ('push', ?, 'p', 'C:/x', ?, ?, ?, '2026-09-27T00:00:00Z')",
        )
        .bind(args)
        .bind(if run_id.is_some() { "run" } else { "human" })
        .bind(run_id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn trace(pool: &SqlitePool, root: i64) -> Option<String> {
        lineage_trace_on(
            &mut *pool.acquire().await.unwrap(),
            root,
            Some("nucleos/run-1"),
        )
        .await
        .unwrap()
    }

    /// Condition 2 (S2): ANY grant in the lineage, whatever its class or state, sends the case to
    /// the owner, including one granted to a successor and not to the run that failed.
    #[tokio::test]
    async fn any_grant_in_the_lineage_goes_to_the_owner() {
        let pool = pool().await;
        let (root, successor) = lineage(&pool).await;
        assert_eq!(trace(&pool, root).await, None);
        sqlx::query(
            "INSERT INTO action_grants (run_id, tool_name, action_class, proposal_id, created_at, consumed_at)
             VALUES (?, 'Bash', 'read-local', 42, '2026-09-27T00:00:00Z', '2026-09-27T00:00:01Z')",
        )
        .bind(successor)
        .execute(&pool)
        .await
        .unwrap();
        assert!(trace(&pool, root).await.unwrap().contains("42"));
    }

    /// Condition 2 (S2): every git request of the lineage that was not rejected or cancelled,
    /// by the lineage's runs or naming the tree's branch (a human request carries no run_id),
    /// sends the case to the owner; `rejected` and `cancelled` say nothing happened.
    #[tokio::test]
    async fn any_git_request_that_may_have_acted_goes_to_the_owner() {
        for status in [
            "awaiting_approval",
            "queued",
            "running",
            "succeeded",
            "failed",
            "blocked",
            "escalated",
            "interrupted",
        ] {
            let pool = pool().await;
            let (root, successor) = lineage(&pool).await;
            request(
                &pool,
                Some(successor),
                status,
                "{\"op\":\"push\",\"remote\":\"origin\",\"branch\":\"main\"}",
            )
            .await;
            assert!(
                trace(&pool, root).await.unwrap().contains(status),
                "{status}"
            );
        }
        for status in ["rejected", "cancelled"] {
            let pool = pool().await;
            let (root, successor) = lineage(&pool).await;
            request(
                &pool,
                Some(successor),
                status,
                "{\"op\":\"push\",\"remote\":\"origin\",\"branch\":\"main\"}",
            )
            .await;
            assert_eq!(trace(&pool, root).await, None, "{status}");
        }
        let pool = pool().await;
        let (root, _) = lineage(&pool).await;
        request(
            &pool,
            None,
            "succeeded",
            "{\"op\":\"push\",\"remote\":\"origin\",\"branch\":\"nucleos/run-1\"}",
        )
        .await;
        assert!(
            trace(&pool, root).await.is_some(),
            "a human request naming the tree's branch"
        );
    }

    /// Condition 8 (S8): any run of the lineage that read a stranger's words.
    #[tokio::test]
    async fn a_lineage_that_read_untrusted_text_is_seen() {
        let pool = pool().await;
        let (root, successor) = lineage(&pool).await;
        assert!(!lineage_read_untrusted(&pool, root).await.unwrap());
        sqlx::query("UPDATE runs SET read_untrusted = 1 WHERE id = ?")
            .bind(successor)
            .execute(&pool)
            .await
            .unwrap();
        assert!(lineage_read_untrusted(&pool, root).await.unwrap());
    }

    /// Condition 7: corrections of the project in the last day.
    #[tokio::test]
    async fn the_daily_ceiling_counts_one_project_one_day() {
        let pool = pool().await;
        let now: chrono::DateTime<chrono::Utc> = "2026-09-27T12:00:00Z".parse().unwrap();
        for (root, project, at) in [
            (1, "p", "2026-09-27T01:00:00+00:00"),
            (2, "p", "2026-09-26T13:00:00+00:00"),
            (3, "p", "2026-09-26T11:00:00+00:00"),
            (4, "q", "2026-09-27T01:00:00+00:00"),
        ] {
            sqlx::query(
                "INSERT INTO judge_corrections (root_run_id, origin_run_id, project_id, created_at) VALUES (?, ?, ?, ?)",
            )
            .bind(root)
            .bind(root)
            .bind(project)
            .bind(at)
            .execute(&pool)
            .await
            .unwrap();
        }
        let count = corrections_in_last_day_on(&mut *pool.acquire().await.unwrap(), "p", now)
            .await
            .unwrap();
        assert_eq!(count, 2);
    }
}
