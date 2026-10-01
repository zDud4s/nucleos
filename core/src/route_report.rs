//! `GET /route/report`: a read-only account of what the local llm-router advised, set against what
//! the daemon launched, so the owner can judge whether `shadow` is ready to become `apply`.
//!
//! A pure READ over the columns `route_advice::resolve` writes into `runs` (migration 0151). It
//! writes nothing, holds no state and is windowed on the run's `created_at`. A run counts as
//! passed when it ended `completed` and as failed on any ending that is not one (`failed`,
//! `timed_out`, `interrupted`); a run still running, awaiting approval, cancelled or superseded is
//! in neither column, because none of those says whether the model was good enough.

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::state::AppState;

const DEFAULT_DAYS: i64 = 30;
const MAX_DAYS: i64 = 365;
const MAX_PAIRS: i64 = 50;

#[derive(Debug, Serialize, PartialEq, Eq, sqlx::FromRow)]
pub struct Pair {
    pub runner: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub advised_runner: Option<String>,
    pub advised_model: Option<String>,
    pub advised_effort: Option<String>,
    pub runs: i64,
    pub passed: i64,
    pub failed: i64,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Report {
    pub days: i64,
    pub runs: i64,
    pub shadow: i64,
    pub apply: i64,
    pub advised: i64,
    pub matched: i64,
    pub pairs: Vec<Pair>,
}

#[derive(Deserialize)]
pub struct ReportQuery {
    days: Option<i64>,
}

fn clamp_days(days: Option<i64>) -> i64 {
    days.unwrap_or(DEFAULT_DAYS).clamp(1, MAX_DAYS)
}

pub async fn report(
    pool: &SqlitePool,
    days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<Report> {
    let since = (now - chrono::Duration::days(days)).to_rfc3339();

    let (runs, shadow, apply, advised, matched): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*),
                COALESCE(SUM(route_mode = 'shadow'), 0),
                COALESCE(SUM(route_mode = 'apply'), 0),
                COALESCE(SUM(route_decision_id IS NOT NULL), 0),
                COALESCE(SUM(route_mode = 'shadow'
                             AND advised_runner IS runner
                             AND advised_model IS model
                             AND advised_effort IS effort), 0)
         FROM runs
         WHERE route_mode IS NOT NULL AND created_at >= ?",
    )
    .bind(&since)
    .fetch_one(pool)
    .await?;

    let pairs = sqlx::query_as::<_, Pair>(
        "SELECT runner, model, effort, advised_runner, advised_model, advised_effort,
                COUNT(*) AS runs,
                COALESCE(SUM(status = 'completed'), 0) AS passed,
                COALESCE(SUM(status IN ('failed', 'timed_out', 'interrupted')), 0) AS failed
         FROM runs
         WHERE route_mode IS NOT NULL AND route_decision_id IS NOT NULL AND created_at >= ?
         GROUP BY runner, model, effort, advised_runner, advised_model, advised_effort
         ORDER BY runs DESC, runner, model, effort, advised_runner, advised_model, advised_effort
         LIMIT ?",
    )
    .bind(&since)
    .bind(MAX_PAIRS)
    .fetch_all(pool)
    .await?;

    Ok(Report {
        days,
        runs,
        shadow,
        apply,
        advised,
        matched,
        pairs,
    })
}

pub async fn get_route_report(
    State(state): State<AppState>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<Report>, StatusCode> {
    report(&state.pool, clamp_days(query.days), chrono::Utc::now())
        .await
        .map(Json)
        .map_err(|err| {
            tracing::error!(error = %err, "route report query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    type Triple = (&'static str, &'static str, &'static str);

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    async fn seed(
        pool: &SqlitePool,
        status: &str,
        created_at: &str,
        mode: Option<&str>,
        decision: Option<&str>,
        launched: (&str, &str, &str),
        advised: Triple,
    ) {
        sqlx::query(
            "INSERT INTO runs (prompt, status, created_at, route_mode, route_decision_id,
                               runner, model, effort, advised_runner, advised_model, advised_effort)
             VALUES ('p', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(status)
        .bind(created_at)
        .bind(mode)
        .bind(decision)
        .bind(launched.0)
        .bind(launched.1)
        .bind(launched.2)
        .bind(advised.0)
        .bind(advised.1)
        .bind(advised.2)
        .execute(pool)
        .await
        .unwrap();
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        "2026-10-01T00:00:00Z".parse().unwrap()
    }

    const SAME: Triple = ("claude", "sonnet", "high");
    const OTHER: Triple = ("codex", "gpt-5", "low");

    #[tokio::test]
    async fn an_empty_database_reports_zeros_and_no_pairs() {
        let pool = test_pool().await;
        let r = report(&pool, 30, now()).await.unwrap();
        assert_eq!(
            r,
            Report {
                days: 30,
                runs: 0,
                shadow: 0,
                apply: 0,
                advised: 0,
                matched: 0,
                pairs: vec![]
            }
        );
    }

    #[tokio::test]
    async fn counts_modes_advice_matches_and_outcomes() {
        let pool = test_pool().await;
        let t = "2026-09-25T00:00:00+00:00";
        // Shadow and advised, advice equal to launch: two passed and one failed.
        seed(
            &pool,
            "completed",
            t,
            Some("shadow"),
            Some("d1"),
            SAME,
            SAME,
        )
        .await;
        seed(
            &pool,
            "completed",
            t,
            Some("shadow"),
            Some("d2"),
            SAME,
            SAME,
        )
        .await;
        seed(
            &pool,
            "timed_out",
            t,
            Some("shadow"),
            Some("d3"),
            SAME,
            SAME,
        )
        .await;
        // Shadow, advised something else, still running: in neither outcome column.
        seed(&pool, "running", t, Some("shadow"), Some("d4"), SAME, OTHER).await;
        // Apply, advised, cancelled: in neither outcome column.
        seed(
            &pool,
            "cancelled",
            t,
            Some("apply"),
            Some("d5"),
            OTHER,
            OTHER,
        )
        .await;
        // Shadow with no usable advice: counted in runs, not advised, not in pairs.
        seed(&pool, "completed", t, Some("shadow"), None, SAME, SAME).await;
        // Never routed, and outside the window: not in the report at all.
        seed(&pool, "completed", t, None, None, SAME, SAME).await;
        let old = "2026-01-01T00:00:00+00:00";
        seed(
            &pool,
            "completed",
            old,
            Some("shadow"),
            Some("d6"),
            SAME,
            SAME,
        )
        .await;

        let r = report(&pool, 30, now()).await.unwrap();
        assert_eq!((r.runs, r.shadow, r.apply, r.advised), (6, 5, 1, 5));
        // d1, d2, d3 and the no-advice row (whose recorded columns also agree) match.
        assert_eq!(r.matched, 3);
        assert_eq!(r.pairs.len(), 3);
        let first = &r.pairs[0];
        assert_eq!(
            (first.runs, first.passed, first.failed),
            (3, 2, 1),
            "{first:?}"
        );
        assert_eq!(first.runner.as_deref(), Some("claude"));
        assert_eq!(first.advised_effort.as_deref(), Some("high"));
        for p in &r.pairs[1..] {
            assert_eq!((p.runs, p.passed, p.failed), (1, 0, 0));
        }
    }

    #[tokio::test]
    async fn a_null_matches_only_a_null_and_stays_null() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO runs (prompt, status, created_at, route_mode, route_decision_id, runner,
                               advised_runner, advised_model)
             VALUES ('p', 'failed', '2026-09-30T00:00:00+00:00', 'shadow', 'd', 'claude',
                     'claude', 'opus')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let r = report(&pool, 30, now()).await.unwrap();
        assert_eq!(r.matched, 0, "model is NULL, advised_model is not");
        let p = &r.pairs[0];
        assert_eq!(p.model, None);
        assert_eq!(p.effort, None);
        assert_eq!(p.advised_model.as_deref(), Some("opus"));
        assert_eq!(p.advised_effort, None);
        assert_eq!(p.failed, 1);

        sqlx::query(
            "UPDATE runs SET advised_model = NULL, model = NULL, advised_runner = 'claude'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let r = report(&pool, 30, now()).await.unwrap();
        assert_eq!(r.matched, 1, "all-NULL fields equal each other");
    }

    #[tokio::test]
    async fn pairs_are_capped_at_fifty_and_sorted_by_runs_then_names() {
        let pool = test_pool().await;
        let t = "2026-09-30T00:00:00+00:00";
        for i in 0..60 {
            let model = format!("m{i:02}");
            seed(
                &pool,
                "completed",
                t,
                Some("shadow"),
                Some("d"),
                ("claude", &model, "low"),
                SAME,
            )
            .await;
        }
        seed(
            &pool,
            "completed",
            t,
            Some("shadow"),
            Some("d"),
            ("claude", "m59", "low"),
            SAME,
        )
        .await;
        let r = report(&pool, 30, now()).await.unwrap();
        assert_eq!(r.pairs.len(), 50);
        assert_eq!(r.pairs[0].model.as_deref(), Some("m59"));
        assert_eq!(r.pairs[0].runs, 2);
        assert_eq!(r.pairs[1].model.as_deref(), Some("m00"));
    }

    #[test]
    fn days_default_to_thirty_and_clamp() {
        assert_eq!(clamp_days(None), 30);
        assert_eq!(clamp_days(Some(0)), 1);
        assert_eq!(clamp_days(Some(-5)), 1);
        assert_eq!(clamp_days(Some(9999)), 365);
        assert_eq!(clamp_days(Some(7)), 7);
    }
}
