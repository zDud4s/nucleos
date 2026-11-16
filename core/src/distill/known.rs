//! What the distiller already knows about a project, so the model is not asked to repeat it
//! (spec 2026-11-16-memoria-agentes-captura, F3 C4): the live titles, and the titles of lessons
//! that were thrown away.
//!
//! RED stage: both bodies are stubs that compile and answer nothing, so the tests fail at run time.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

/// Titles of the live lessons of a project (`active` or `proposed`), without the rows the
/// consolidator wrote.
#[allow(dead_code)]
pub(super) async fn titles(_pool: &SqlitePool, _project_id: &str) -> sqlx::Result<Vec<String>> {
    Ok(Vec::new())
}

/// Titles of the lessons of a project that were reverted, rejected or expired within the last
/// `REFUSED_DAYS` days, newest first, at most `MAX_REFUSED`.
#[allow(dead_code)]
pub(super) async fn refused(
    _pool: &SqlitePool,
    _project_id: &str,
    _now: DateTime<Utc>,
) -> sqlx::Result<Vec<String>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, Utc};
    use sqlx::SqlitePool;

    use super::{refused, titles};
    use crate::distill::MAX_REFUSED;

    async fn seed(
        pool: &SqlitePool,
        project: &str,
        title: &str,
        source: &str,
        status: &str,
        created_at: DateTime<Utc>,
        ended_at: Option<DateTime<Utc>>,
    ) {
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at, ended_at)
             VALUES ('semantic', 'project', ?, ?, 'memory', ?, 'body', ?, ?, ?)",
        )
        .bind(project)
        .bind(source)
        .bind(title)
        .bind(status)
        .bind(created_at.to_rfc3339())
        .bind(ended_at.map(|at| at.to_rfc3339()))
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn titles_exclude_consolidator_rows() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now();
        seed(&pool, "alpha", "Active title", "distiller", "active", now, None).await;
        seed(&pool, "alpha", "Proposed title", "run", "proposed", now, None).await;
        seed(&pool, "alpha", "Consolidated title", "consolidator", "active", now, None).await;
        seed(&pool, "alpha", "Rejected title", "distiller", "rejected", now, Some(now)).await;
        seed(&pool, "beta", "Other project title", "distiller", "active", now, None).await;

        let got = titles(&pool, "alpha").await.unwrap();
        assert!(got.contains(&"Active title".to_owned()), "{got:?}");
        assert!(got.contains(&"Proposed title".to_owned()), "{got:?}");
        assert!(!got.contains(&"Consolidated title".to_owned()), "{got:?}");
        assert!(!got.contains(&"Rejected title".to_owned()), "{got:?}");
        assert!(!got.contains(&"Other project title".to_owned()), "{got:?}");
    }

    #[tokio::test]
    async fn refused_lists_dead_rows_of_the_last_90_days_up_to_20() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now();
        let long_ago = now - Duration::days(400);
        seed(&pool, "alpha", "Reverted old", "distiller", "reverted", long_ago, Some(now - Duration::days(120))).await;
        seed(&pool, "alpha", "Reverted recent", "distiller", "reverted", long_ago, Some(now - Duration::days(10))).await;
        // No `ended_at`: the creation date stands in for it.
        seed(&pool, "alpha", "Rejected recent", "distiller", "rejected", now - Duration::days(5), None).await;
        seed(&pool, "alpha", "Expired recent", "distiller", "expired", long_ago, Some(now - Duration::days(1))).await;
        seed(&pool, "alpha", "Still active", "distiller", "active", now, None).await;
        seed(&pool, "beta", "Other project reverted", "distiller", "reverted", now, Some(now)).await;

        let got = refused(&pool, "alpha", now).await.unwrap();
        for wanted in ["Reverted recent", "Rejected recent", "Expired recent"] {
            assert!(got.contains(&wanted.to_owned()), "{wanted:?} missing from {got:?}");
        }
        for unwanted in ["Reverted old", "Still active", "Other project reverted"] {
            assert!(!got.contains(&unwanted.to_owned()), "{unwanted:?} in {got:?}");
        }

        // More than the cap: the newest rows win.
        for i in 0..25 {
            seed(&pool, "alpha", &format!("Bulk {i}"), "distiller", "reverted", long_ago, Some(now - Duration::days(2))).await;
        }
        let capped = refused(&pool, "alpha", now).await.unwrap();
        assert_eq!(capped.len(), MAX_REFUSED, "{capped:?}");
        assert!(capped.iter().all(|t| t.starts_with("Bulk ")), "{capped:?}");
    }
}
