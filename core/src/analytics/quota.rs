//! The recorded quota history as one series per provider window (spec §6, `GET /analytics/quota`).
//! Rows are written by `quota::record` (migration 0199); this module only reads them.

use serde::Serialize;
use sqlx::SqlitePool;

use super::Window;

/// `GET /analytics/quota` (spec §6). Global only.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaAnalytics {
    pub days: i64,
    pub timezone: &'static str,
    /// The oldest recorded point, window or not: where the history begins. `None` when empty.
    pub since: Option<String>,
    pub series: Vec<QuotaSeries>,
}

/// One provider window, its points by `at` ascending. Steps, not a line: each point holds until
/// the next.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaSeries {
    pub provider: String,
    /// `5h` or `7d`.
    pub window: String,
    /// The latest point's fidelity (`official` or `derived`).
    pub fidelity: String,
    pub points: Vec<QuotaPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaPoint {
    pub at: String,
    /// In [0,1], as stored; the page shows a percentage.
    pub used_fraction: f64,
    pub resets_at: Option<String>,
    pub stale: bool,
}

#[derive(sqlx::FromRow)]
struct HistoryRow {
    provider: String,
    window_name: String,
    used_fraction: f64,
    resets_at: Option<String>,
    fidelity: String,
    stale: i64,
    recorded_at: String,
}

/// The recorded quota history inside `window` (0199, written by `quota::record`).
pub async fn quota_history(pool: &SqlitePool, window: Window) -> sqlx::Result<QuotaAnalytics> {
    let since: Option<String> = sqlx::query_scalar("SELECT MIN(recorded_at) FROM quota_history")
        .fetch_one(pool)
        .await?;
    let rows: Vec<HistoryRow> = sqlx::query_as(
        "SELECT provider, window_name, used_fraction, resets_at, fidelity, stale, recorded_at
           FROM quota_history
          WHERE recorded_at >= ?1 AND recorded_at < ?2
          ORDER BY provider, window_name, recorded_at, id",
    )
    .bind(window.from_bound())
    .bind(window.to_bound())
    .fetch_all(pool)
    .await?;

    let mut series: Vec<QuotaSeries> = Vec::new();
    for row in rows {
        let point = QuotaPoint {
            at: row.recorded_at,
            used_fraction: row.used_fraction,
            resets_at: row.resets_at,
            stale: row.stale != 0,
        };
        match series.last_mut() {
            Some(last) if last.provider == row.provider && last.window == row.window_name => {
                last.fidelity = row.fidelity;
                last.points.push(point);
            }
            _ => series.push(QuotaSeries {
                provider: row.provider,
                window: row.window_name,
                fidelity: row.fidelity,
                points: vec![point],
            }),
        }
    }

    Ok(QuotaAnalytics {
        days: window.days,
        timezone: "UTC",
        since,
        series,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::tests::now;

    async fn quota_point(pool: &SqlitePool, provider: &str, window: &str, at: &str, used: f64) {
        sqlx::query(
            "INSERT INTO quota_history (recorded_at, provider, window_name, used_fraction, fidelity)
             VALUES (?, ?, ?, ?, 'official')",
        )
        .bind(at)
        .bind(provider)
        .bind(window)
        .bind(used)
        .execute(pool)
        .await
        .unwrap();
    }

    /// One series per provider and window, points by `at` ascending; `since` is where the
    /// recorded history begins, even before the window.
    #[tokio::test]
    async fn the_quota_history_is_one_series_per_provider_window_in_time_order() {
        let pool = crate::testdb::fresh_pool().await;
        let window = Window::new(30, now()).unwrap();

        let empty = quota_history(&pool, window).await.unwrap();
        assert_eq!((empty.since.clone(), empty.series.len()), (None, 0));
        assert_eq!((empty.days, empty.timezone), (30, "UTC"));

        quota_point(&pool, "claude", "5h", "2026-10-02T10:00:00+00:00", 0.4).await;
        quota_point(&pool, "claude", "5h", "2026-10-01T10:00:00+00:00", 0.2).await;
        quota_point(&pool, "claude", "7d", "2026-10-01T10:00:00+00:00", 0.1).await;
        quota_point(&pool, "codex", "5h", "2026-10-03T10:00:00+00:00", 0.9).await;
        quota_point(&pool, "claude", "5h", "2026-08-01T10:00:00+00:00", 0.7).await; // before the window

        let history = quota_history(&pool, window).await.unwrap();

        assert_eq!(history.since.as_deref(), Some("2026-08-01T10:00:00+00:00"));
        let shape: Vec<(&str, &str, Vec<f64>)> = history
            .series
            .iter()
            .map(|s| {
                (
                    s.provider.as_str(),
                    s.window.as_str(),
                    s.points.iter().map(|p| p.used_fraction).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("claude", "5h", vec![0.2, 0.4]),
                ("claude", "7d", vec![0.1]),
                ("codex", "5h", vec![0.9]),
            ]
        );
        assert_eq!(history.series[0].fidelity, "official");
        assert!(!history.series[0].points[0].stale);
    }
}
