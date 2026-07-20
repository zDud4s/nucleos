use serde::Serialize;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FeedEntry {
    pub id: i64,
    pub project_id: Option<String>,
    pub kind: String,
    pub summary: String,
    pub run_id: Option<i64>,
    pub created_at: String,
}

pub async fn append(
    pool: &sqlx::SqlitePool,
    project_id: Option<&str>,
    kind: &str,
    summary: &str,
    run_id: Option<i64>,
) -> sqlx::Result<i64> {
    let created_at = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO feed (project_id, kind, summary, run_id, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(kind)
    .bind(summary)
    .bind(run_id)
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Returns one feed scope newest-first. `None` selects explicitly aggregated global rows
/// (`project_id IS NULL`), not a merge of every project's feed.
pub async fn list_feed(
    pool: &sqlx::SqlitePool,
    project_id: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<FeedEntry>> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, FeedEntry>(
                "SELECT id, project_id, kind, summary, run_id, created_at
                 FROM feed WHERE project_id = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, FeedEntry>(
                "SELECT id, project_id, kind, summary, run_id, created_at
                 FROM feed WHERE project_id IS NULL ORDER BY id DESC LIMIT ?",
            )
            .bind(limit)
            .fetch_all(pool)
            .await
        }
    }
}

/// Aggregated feed across EVERY scope (global NULL rows + all projects), newest-first, honoring `limit`.
/// Distinct from `list_feed(None)`, which returns only global (`project_id IS NULL`) rows.
pub async fn list_all(pool: &sqlx::SqlitePool, limit: i64) -> sqlx::Result<Vec<FeedEntry>> {
    sqlx::query_as::<_, FeedEntry>(
        "SELECT id, project_id, kind, summary, run_id, created_at
         FROM feed ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::{append, list_all, list_feed};

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn append_and_list_round_trip() {
        let pool = test_pool().await;

        let id = append(
            &pool,
            Some("project-a"),
            "run_interrupted",
            "run interrupted during startup recovery",
            Some(7),
        )
        .await
        .unwrap();

        let entries = list_feed(&pool, Some("project-a"), 50).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].project_id.as_deref(), Some("project-a"));
        assert_eq!(entries[0].kind, "run_interrupted");
        assert_eq!(
            entries[0].summary,
            "run interrupted during startup recovery"
        );
        assert_eq!(entries[0].run_id, Some(7));
        chrono::DateTime::parse_from_rfc3339(&entries[0].created_at).unwrap();
    }

    #[tokio::test]
    async fn global_and_project_feeds_are_isolated() {
        let pool = test_pool().await;
        append(&pool, None, "global", "global summary", None)
            .await
            .unwrap();
        append(&pool, Some("project-a"), "project", "project summary", None)
            .await
            .unwrap();
        append(
            &pool,
            Some("project-b"),
            "project",
            "other project summary",
            None,
        )
        .await
        .unwrap();

        let global = list_feed(&pool, None, 50).await.unwrap();
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].project_id, None);
        assert_eq!(global[0].summary, "global summary");

        let project = list_feed(&pool, Some("project-a"), 50).await.unwrap();
        assert_eq!(project.len(), 1);
        assert_eq!(project[0].project_id.as_deref(), Some("project-a"));
        assert_eq!(project[0].summary, "project summary");
    }

    #[tokio::test]
    async fn list_feed_returns_newest_first_and_honors_limit() {
        let pool = test_pool().await;
        let first = append(&pool, None, "event", "first", None).await.unwrap();
        let second = append(&pool, None, "event", "second", None).await.unwrap();
        let third = append(&pool, None, "event", "third", None).await.unwrap();

        let entries = list_feed(&pool, None, 2).await.unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![third, second]
        );
        assert!(first < second);
    }

    #[tokio::test]
    async fn list_all_merges_every_scope_newest_first() {
        let pool = test_pool().await;
        let global = append(&pool, None, "global", "global summary", None)
            .await
            .unwrap();
        let project_a = append(
            &pool,
            Some("project-a"),
            "project",
            "project a summary",
            None,
        )
        .await
        .unwrap();
        let project_b = append(
            &pool,
            Some("project-b"),
            "project",
            "project b summary",
            None,
        )
        .await
        .unwrap();

        let entries = list_all(&pool, 50).await.unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![project_b, project_a, global]
        );
    }

    #[tokio::test]
    async fn list_all_honors_limit() {
        let pool = test_pool().await;
        let first = append(&pool, None, "event", "first", None).await.unwrap();
        let second = append(&pool, Some("project-a"), "event", "second", None)
            .await
            .unwrap();
        let third = append(&pool, Some("project-b"), "event", "third", None)
            .await
            .unwrap();

        let entries = list_all(&pool, 2).await.unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![third, second]
        );
        assert!(first < second);
    }

    #[tokio::test]
    async fn list_feed_keeps_global_and_project_scopes_isolated() {
        let pool = test_pool().await;
        append(&pool, None, "global", "global summary", None)
            .await
            .unwrap();
        append(
            &pool,
            Some("project-a"),
            "project",
            "project a summary",
            None,
        )
        .await
        .unwrap();
        append(
            &pool,
            Some("project-b"),
            "project",
            "project b summary",
            None,
        )
        .await
        .unwrap();

        let global = list_feed(&pool, None, 50).await.unwrap();
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].project_id, None);
        assert_eq!(global[0].summary, "global summary");

        let project_a = list_feed(&pool, Some("project-a"), 50).await.unwrap();
        assert_eq!(project_a.len(), 1);
        assert_eq!(project_a[0].project_id.as_deref(), Some("project-a"));
        assert_eq!(project_a[0].summary, "project a summary");
    }
}
