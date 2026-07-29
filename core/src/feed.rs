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

/// The feed scope to search. `Global` deliberately means only `project_id IS NULL`; it does not
/// merge every project's feed, which is what `All` is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedScope {
    All,
    Global,
    Project(String),
}

#[derive(Debug, Clone)]
pub struct SearchFilter {
    pub scope: FeedScope,
    pub q: Option<String>,
    pub kind: Option<String>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    pub limit: i64,
}

fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub async fn append(
    pool: &sqlx::SqlitePool,
    project_id: Option<&str>,
    kind: &str,
    summary: &str,
    run_id: Option<i64>,
) -> sqlx::Result<i64> {
    append_on(
        pool.acquire().await?.as_mut(),
        project_id,
        kind,
        summary,
        run_id,
    )
    .await
}

/// The same append against a caller-supplied connection, so a writer that must be atomic with its
/// feed entry can run both inside one transaction. Feed SQL stays in this module either way.
pub async fn append_on(
    conn: &mut sqlx::SqliteConnection,
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
    .execute(conn)
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

/// Searches feed entries newest-first without changing the meaning of the global scope.
pub async fn search(
    pool: &sqlx::SqlitePool,
    filter: &SearchFilter,
) -> sqlx::Result<Vec<FeedEntry>> {
    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, project_id, kind, summary, run_id, created_at FROM feed WHERE 1 = 1",
    );

    match &filter.scope {
        FeedScope::All => {}
        FeedScope::Global => {
            query.push(" AND project_id IS NULL");
        }
        FeedScope::Project(project_id) => {
            query.push(" AND project_id = ").push_bind(project_id);
        }
    }
    if let Some(kind) = &filter.kind {
        query.push(" AND kind = ").push_bind(kind);
    }
    if let Some(q) = &filter.q {
        query
            .push(" AND summary LIKE ")
            .push_bind(format!("%{}%", escape_like(q)))
            .push(" ESCAPE '\\'");
    }
    if let Some(since) = &filter.since {
        query
            .push(" AND created_at >= ")
            .push_bind(since.to_rfc3339());
    }
    if let Some(until) = &filter.until {
        query
            .push(" AND created_at <= ")
            .push_bind(until.to_rfc3339());
    }
    query
        .push(" ORDER BY created_at DESC, id DESC LIMIT ")
        .push_bind(filter.limit);

    query.build_query_as::<FeedEntry>().fetch_all(pool).await
}

#[cfg(test)]
mod tests {
    use super::{FeedScope, SearchFilter, append, list_all, list_feed, search};

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

    async fn insert_entry(
        pool: &sqlx::SqlitePool,
        project_id: Option<&str>,
        kind: &str,
        summary: &str,
        created_at: &str,
    ) -> i64 {
        sqlx::query("INSERT INTO feed (project_id, kind, summary, created_at) VALUES (?, ?, ?, ?)")
            .bind(project_id)
            .bind(kind)
            .bind(summary)
            .bind(created_at)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
    }

    fn search_filter(scope: FeedScope) -> SearchFilter {
        SearchFilter {
            scope,
            q: None,
            kind: None,
            since: None,
            until: None,
            limit: 50,
        }
    }

    #[tokio::test]
    async fn search_filters_scope_kind_and_summary() {
        let pool = test_pool().await;
        let matching = insert_entry(
            &pool,
            Some("project-a"),
            "worktree_run_completed",
            "Autopilot completed the March worktree run",
            "2026-03-10T12:00:00+00:00",
        )
        .await;
        insert_entry(
            &pool,
            Some("project-a"),
            "shadow_run_completed",
            "Autopilot completed a shadow run",
            "2026-03-11T12:00:00+00:00",
        )
        .await;
        insert_entry(
            &pool,
            Some("project-b"),
            "worktree_run_completed",
            "Autopilot completed the March worktree run",
            "2026-03-12T12:00:00+00:00",
        )
        .await;
        insert_entry(
            &pool,
            None,
            "worktree_run_completed",
            "Autopilot completed the March worktree run",
            "2026-03-13T12:00:00+00:00",
        )
        .await;

        let mut filter = search_filter(FeedScope::Project("project-a".into()));
        filter.kind = Some("worktree_run_completed".into());
        filter.q = Some("March worktree".into());
        let entries = search(&pool, &filter).await.unwrap();

        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            [matching]
        );
    }

    #[tokio::test]
    async fn search_uses_inclusive_normalized_time_bounds_and_orders_newest_first() {
        let pool = test_pool().await;
        insert_entry(
            &pool,
            None,
            "event",
            "before the window",
            "2026-02-28T23:59:59+00:00",
        )
        .await;
        let first = insert_entry(
            &pool,
            None,
            "event",
            "start of window",
            "2026-03-01T00:00:00+00:00",
        )
        .await;
        let second = insert_entry(
            &pool,
            None,
            "event",
            "end of window",
            "2026-03-31T23:59:59+00:00",
        )
        .await;
        insert_entry(
            &pool,
            None,
            "event",
            "after the window",
            "2026-04-01T00:00:00+00:00",
        )
        .await;

        let mut filter = search_filter(FeedScope::Global);
        filter.since = Some(
            chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        filter.until = Some(
            chrono::DateTime::parse_from_rfc3339("2026-03-31T23:59:59Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let entries = search(&pool, &filter).await.unwrap();

        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            [second, first]
        );
    }

    #[tokio::test]
    async fn search_escapes_like_wildcards_and_honors_limit() {
        let pool = test_pool().await;
        let percent = insert_entry(
            &pool,
            None,
            "event",
            "Autopilot spent 50% of its budget",
            "2026-03-03T00:00:00+00:00",
        )
        .await;
        insert_entry(
            &pool,
            None,
            "event",
            "Autopilot spent half of its budget",
            "2026-03-04T00:00:00+00:00",
        )
        .await;

        let mut filter = search_filter(FeedScope::All);
        filter.q = Some("%".into());
        let entries = search(&pool, &filter).await.unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            [percent]
        );

        filter.q = None;
        filter.limit = 1;
        let entries = search(&pool, &filter).await.unwrap();
        assert_eq!(entries.len(), 1);
    }
}
