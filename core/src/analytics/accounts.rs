//! Token and cost totals grouped by account, for the analytics page's `accounts` block.
//!
//! Two populations, kept apart on purpose (the same rule as `devtime.rs`): the daemon's own runs
//! (`runs.account_id`, written by `accounts::store::record_run_account`) and the owner's manual
//! sessions (`devtime_messages` joined to `devtime_files.account_id`). They are never summed into
//! one figure. A NULL account is its own group (`account_id: null`, "unattributed").
//!
//! Only the opaque account id leaves here: no email, token, credential, plan or tier.

use serde::Serialize;
use sqlx::{Row, SqlitePool};

use super::{Scope, Window};

/// One account's totals over the window. `account_id` is `None` for the unattributed group.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountTotal {
    pub account_id: Option<String>,
    pub input: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub output: i64,
    /// Finished runs, or devtime messages, that make up the totals.
    pub items: i64,
    /// Summed reported `cost_usd` of the runs; `None` for devtime, which carries no cost.
    pub cost_usd: Option<f64>,
}

/// Grouped totals for both populations; each list is ordered by account id, unattributed last.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AccountsBlock {
    pub runs: Vec<AccountTotal>,
    pub devtime: Vec<AccountTotal>,
}

/// `GROUP BY account_id` totals of finished runs completed in `window`, imported rows excluded
/// (as `sessions::finished_runs`). Measured cost only: no time estimate is added here.
pub async fn run_totals(
    pool: &SqlitePool,
    scope: Scope<'_>,
    window: Window,
) -> sqlx::Result<Vec<AccountTotal>> {
    let rows = sqlx::query(
        "SELECT account_id,
                COALESCE(SUM(input_tokens), 0) AS input,
                COALESCE(SUM(cache_read_tokens), 0) AS cache_read,
                COALESCE(SUM(cache_creation_tokens), 0) AS cache_creation,
                COALESCE(SUM(output_tokens), 0) AS output,
                COUNT(*) AS items,
                COALESCE(SUM(cost_usd), 0.0) AS cost
           FROM runs
          WHERE completed_at IS NOT NULL AND completed_at >= ?1 AND completed_at < ?2
            AND (?3 IS NULL OR project_id = ?3)
            AND imported_at IS NULL
          GROUP BY account_id
          ORDER BY account_id IS NULL, account_id",
    )
    .bind(window.from_bound())
    .bind(window.to_bound())
    .bind(scope.project())
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(AccountTotal {
                account_id: row.try_get("account_id")?,
                input: row.try_get("input")?,
                cache_read: row.try_get("cache_read")?,
                cache_creation: row.try_get("cache_creation")?,
                output: row.try_get("output")?,
                items: row.try_get("items")?,
                cost_usd: Some(row.try_get("cost")?),
            })
        })
        .collect()
}

/// `GROUP BY account_id` token totals of devtime messages whose `first_at` falls in `window`.
///
/// The account is the session's file account; a session with several files is collapsed to one
/// row first (`MAX` ignores NULL), so a message is never counted twice.
pub async fn devtime_totals(
    pool: &SqlitePool,
    scope: Scope<'_>,
    window: Window,
) -> sqlx::Result<Vec<AccountTotal>> {
    let rows = sqlx::query(
        "SELECT f.account_id AS account_id,
                COALESCE(SUM(m.input_tokens), 0) AS input,
                COALESCE(SUM(m.cache_read_tokens), 0) AS cache_read,
                COALESCE(SUM(m.cache_creation_tokens), 0) AS cache_creation,
                COALESCE(SUM(m.output_tokens), 0) AS output,
                COUNT(*) AS items
           FROM devtime_messages m
           LEFT JOIN (SELECT session_id, MAX(account_id) AS account_id
                        FROM devtime_files GROUP BY session_id) f
                  ON f.session_id = m.session_id
           LEFT JOIN devtime_sessions s ON s.session_id = m.session_id
          WHERE m.first_at >= ?1 AND m.first_at < ?2
            AND (?3 IS NULL OR s.project_id = ?3)
          GROUP BY f.account_id
          ORDER BY f.account_id IS NULL, f.account_id",
    )
    .bind(super::devtime::stamp(window.start()))
    .bind(super::devtime::stamp(window.end()))
    .bind(scope.project())
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(AccountTotal {
                account_id: row.try_get("account_id")?,
                input: row.try_get("input")?,
                cache_read: row.try_get("cache_read")?,
                cache_creation: row.try_get("cache_creation")?,
                output: row.try_get("output")?,
                items: row.try_get("items")?,
                cost_usd: None,
            })
        })
        .collect()
}

/// Both groupings for one scope and window.
pub async fn accounts_block(
    pool: &SqlitePool,
    scope: Scope<'_>,
    window: Window,
) -> sqlx::Result<AccountsBlock> {
    Ok(AccountsBlock {
        runs: run_totals(pool, scope, window).await?,
        devtime: devtime_totals(pool, scope, window).await?,
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{done, insert, now};
    use super::*;

    fn window() -> Window {
        Window::new(7, now()).unwrap()
    }

    async fn run_on(pool: &SqlitePool, project: &'static str, account: Option<&str>, cost: f64) {
        let mut run = done(project, "2026-10-16T10:00:00Z");
        run.cost = Some(cost);
        let id = insert(pool, run).await;
        sqlx::query("UPDATE runs SET account_id = ? WHERE id = ?")
            .bind(account)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn file(pool: &SqlitePool, path: &str, session: &str, account: Option<&str>) {
        sqlx::query(
            "INSERT INTO devtime_files (path, session_id, parser_version, updated_at, account_id)
             VALUES (?, ?, 1, '2026-10-16T10:00:00.000Z', ?)",
        )
        .bind(path)
        .bind(session)
        .bind(account)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn message(pool: &SqlitePool, session: &str, id: &str, first_at: &str, output: i64) {
        sqlx::query(
            "INSERT INTO devtime_messages (session_id, lane, message_id, first_at, last_at,
                                           input_tokens, output_tokens, parser_version)
             VALUES (?, 'main', ?, ?, ?, 5, ?, 1)",
        )
        .bind(session)
        .bind(id)
        .bind(first_at)
        .bind(first_at)
        .bind(output)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn empty_history_gives_empty_groups() {
        let pool = crate::testdb::fresh_pool().await;
        let block = accounts_block(&pool, Scope::Global, window()).await.unwrap();
        assert_eq!(block, AccountsBlock::default());
    }

    #[tokio::test]
    async fn runs_group_by_account_with_null_last() {
        let pool = crate::testdb::fresh_pool().await;
        run_on(&pool, "p", Some("b"), 2.0).await;
        run_on(&pool, "p", None, 4.0).await;
        run_on(&pool, "p", Some("a"), 1.0).await;
        run_on(&pool, "p", Some("a"), 0.5).await;
        run_on(&pool, "q", Some("a"), 8.0).await;

        let totals = run_totals(&pool, Scope::Global, window()).await.unwrap();
        let keys: Vec<_> = totals.iter().map(|t| t.account_id.as_deref()).collect();
        assert_eq!(keys, [Some("a"), Some("b"), None]);
        assert_eq!(totals[0].items, 3);
        assert_eq!(totals[0].cost_usd, Some(9.5));
        assert_eq!((totals[0].input, totals[0].output), (300, 30));
        assert_eq!(totals[2].cost_usd, Some(4.0));

        let scoped = run_totals(&pool, Scope::Project("p"), window()).await.unwrap();
        assert_eq!(scoped[0].items, 2);
        assert_eq!(scoped[0].cost_usd, Some(1.5));
    }

    #[tokio::test]
    async fn runs_outside_the_window_are_left_out() {
        let pool = crate::testdb::fresh_pool().await;
        insert(&pool, done("p", "2026-09-01T10:00:00Z")).await;
        assert!(run_totals(&pool, Scope::Global, window()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn devtime_groups_by_file_account_with_null_unattributed() {
        let pool = crate::testdb::fresh_pool().await;
        file(&pool, "/t/1.jsonl", "s1", Some("a")).await;
        // A second file of the same session must not double its messages.
        file(&pool, "/t/1b.jsonl", "s1", Some("a")).await;
        file(&pool, "/t/2.jsonl", "s2", Some("b")).await;
        file(&pool, "/t/3.jsonl", "s3", None).await;
        let day = "2026-10-16T10:00:00.000Z";
        message(&pool, "s1", "m1", day, 10).await;
        message(&pool, "s1", "m2", day, 20).await;
        message(&pool, "s2", "m3", day, 7).await;
        message(&pool, "s3", "m4", day, 3).await;
        // No file row at all: unattributed too. And one outside the window.
        message(&pool, "s4", "m5", day, 1).await;
        message(&pool, "s1", "m6", "2026-09-01T10:00:00.000Z", 99).await;

        let totals = devtime_totals(&pool, Scope::Global, window()).await.unwrap();
        let keys: Vec<_> = totals.iter().map(|t| t.account_id.as_deref()).collect();
        assert_eq!(keys, [Some("a"), Some("b"), None]);
        assert_eq!((totals[0].items, totals[0].output, totals[0].input), (2, 30, 10));
        assert_eq!(totals[1].output, 7);
        assert_eq!((totals[2].items, totals[2].output), (2, 4));
        assert!(totals.iter().all(|t| t.cost_usd.is_none()));
    }
}
