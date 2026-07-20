use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetPeriod {
    Daily,
    Weekly,
    Monthly,
}

impl BudgetPeriod {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BudgetConfig {
    pub limit_usd: Option<f64>,
    pub period: BudgetPeriod,
    pub hourly_limit_usd: Option<f64>,
    pub per_run_reserve_usd: f64,
    pub time_cost_per_hour_usd: f64,
}

/// One run's contribution to the budget, already parsed from the `runs` table.
#[derive(Debug, Clone)]
pub struct SpendRow {
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// A run with an unknown cost still consumed at least this much wall time for approximation, so a
/// zero/near-zero-duration run is never counted as $0 (spec §8.5: unmeasured cost is never free).
const MIN_APPROX_SECONDS: i64 = 60;

fn time_approx(row: &SpendRow, now: DateTime<Utc>, rate_per_hour: f64) -> f64 {
    let end = row.completed_at.unwrap_or(now);
    let seconds = (end - row.created_at).num_seconds().max(MIN_APPROX_SECONDS);
    (seconds as f64 / 3600.0) * rate_per_hour
}

/// Total spend across `rows`, deduping resumed sessions and approximating unknown costs by time.
/// `now` is used for the duration of rows that have not completed yet.
pub fn compute_spend(rows: &[SpendRow], now: DateTime<Utc>, cfg: &BudgetConfig) -> f64 {
    use std::collections::HashMap;

    let rate = cfg.time_cost_per_hour_usd;
    let mut total = 0.0;
    let mut sessions: HashMap<&str, Vec<&SpendRow>> = HashMap::new();

    for row in rows {
        match row.session_id.as_deref() {
            // Sessionless rows cannot be deduped; each counts on its own.
            None => total += row.cost_usd.unwrap_or_else(|| time_approx(row, now, rate)),
            Some(session_id) => sessions.entry(session_id).or_default().push(row),
        }
    }

    for group in sessions.values() {
        // `--resume` reports the cumulative session total, so a session with any known cost counts
        // that single most-recent value once; a session with no known cost yet falls back to time.
        let latest_known_cost = group
            .iter()
            .filter(|row| row.cost_usd.is_some())
            .max_by_key(|row| row.created_at)
            .and_then(|row| row.cost_usd);

        match latest_known_cost {
            Some(cost) => total += cost,
            None => {
                total += group
                    .iter()
                    .map(|row| time_approx(row, now, rate))
                    .sum::<f64>()
            }
        }
    }

    total
}

pub async fn load_budget_config(pool: &SqlitePool) -> sqlx::Result<BudgetConfig> {
    let row: (Option<f64>, String, Option<f64>, f64, f64) = sqlx::query_as(
        "SELECT budget_limit_usd, budget_period, budget_hourly_limit_usd,
                budget_per_run_reserve_usd, budget_time_cost_per_hour_usd
         FROM autopilot_global LIMIT 1",
    )
    .fetch_one(pool)
    .await?;
    let period = BudgetPeriod::from_db_str(&row.1).ok_or_else(|| {
        sqlx::Error::Protocol(format!("invalid budget period in database: {}", row.1))
    })?;
    Ok(BudgetConfig {
        limit_usd: row.0,
        period,
        hourly_limit_usd: row.2,
        per_run_reserve_usd: row.3,
        time_cost_per_hour_usd: row.4,
    })
}

pub async fn set_budget_config(pool: &SqlitePool, cfg: &BudgetConfig) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE autopilot_global
         SET budget_limit_usd = ?, budget_period = ?, budget_hourly_limit_usd = ?,
             budget_per_run_reserve_usd = ?, budget_time_cost_per_hour_usd = ?",
    )
    .bind(cfg.limit_usd)
    .bind(cfg.period.as_db_str())
    .bind(cfg.hourly_limit_usd)
    .bind(cfg.per_run_reserve_usd)
    .bind(cfg.time_cost_per_hour_usd)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn fresh_db_returns_default_budget_config() {
        let pool = test_pool().await;

        assert_eq!(
            load_budget_config(&pool).await.unwrap(),
            BudgetConfig {
                limit_usd: None,
                period: BudgetPeriod::Monthly,
                hourly_limit_usd: None,
                per_run_reserve_usd: 0.5,
                time_cost_per_hour_usd: 3.0,
            }
        );
    }

    #[tokio::test]
    async fn set_then_load_round_trips() {
        let pool = test_pool().await;
        let config = BudgetConfig {
            limit_usd: Some(100.0),
            period: BudgetPeriod::Weekly,
            hourly_limit_usd: Some(10.0),
            per_run_reserve_usd: 1.0,
            time_cost_per_hour_usd: 2.5,
        };

        set_budget_config(&pool, &config).await.unwrap();

        assert_eq!(load_budget_config(&pool).await.unwrap(), config);
    }

    #[tokio::test]
    async fn period_db_str_round_trips() {
        for period in [
            BudgetPeriod::Daily,
            BudgetPeriod::Weekly,
            BudgetPeriod::Monthly,
        ] {
            assert_eq!(BudgetPeriod::from_db_str(period.as_db_str()), Some(period));
        }

        assert_eq!(BudgetPeriod::from_db_str("yearly"), None);
    }

    fn ts(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn cfg(time_rate: f64) -> BudgetConfig {
        BudgetConfig {
            limit_usd: None,
            period: BudgetPeriod::Monthly,
            hourly_limit_usd: None,
            per_run_reserve_usd: 0.5,
            time_cost_per_hour_usd: time_rate,
        }
    }

    fn approx(got: f64, want: f64) {
        assert!((got - want).abs() < 1e-9, "got {got}, want {want}");
    }

    #[test]
    fn single_completed_run_counts_its_cost() {
        let rows = vec![SpendRow {
            session_id: Some("s1".into()),
            cost_usd: Some(0.5),
            created_at: ts("2026-07-20T10:00:00Z"),
            completed_at: Some(ts("2026-07-20T10:05:00Z")),
        }];
        approx(
            compute_spend(&rows, ts("2026-07-20T11:00:00Z"), &cfg(3.0)),
            0.5,
        );
    }

    #[test]
    fn resumed_session_counts_latest_cost_once_not_summed() {
        // Original paused run (no result -> cost None) then a resume run whose cost is the
        // cumulative session total. Must count 0.9 once, NOT 0.9 + time-approx of the first row.
        let rows = vec![
            SpendRow {
                session_id: Some("s1".into()),
                cost_usd: None,
                created_at: ts("2026-07-20T10:00:00Z"),
                completed_at: Some(ts("2026-07-20T10:30:00Z")),
            },
            SpendRow {
                session_id: Some("s1".into()),
                cost_usd: Some(0.9),
                created_at: ts("2026-07-20T10:40:00Z"),
                completed_at: Some(ts("2026-07-20T10:50:00Z")),
            },
        ];
        approx(
            compute_spend(&rows, ts("2026-07-20T11:00:00Z"), &cfg(3.0)),
            0.9,
        );
    }

    #[test]
    fn null_cost_run_is_approximated_by_time_never_zero() {
        // 30 min of runtime at $3/h = $1.50.
        let rows = vec![SpendRow {
            session_id: Some("s1".into()),
            cost_usd: None,
            created_at: ts("2026-07-20T10:00:00Z"),
            completed_at: Some(ts("2026-07-20T10:30:00Z")),
        }];
        let spent = compute_spend(&rows, ts("2026-07-20T11:00:00Z"), &cfg(3.0));
        assert!(spent > 0.0);
        approx(spent, 1.5);
    }

    #[test]
    fn session_without_real_cost_sums_time_approx() {
        // Two rows, same session, both cost None, 30 min each -> 1.5 + 1.5 = 3.0.
        let rows = vec![
            SpendRow {
                session_id: Some("s1".into()),
                cost_usd: None,
                created_at: ts("2026-07-20T10:00:00Z"),
                completed_at: Some(ts("2026-07-20T10:30:00Z")),
            },
            SpendRow {
                session_id: Some("s1".into()),
                cost_usd: None,
                created_at: ts("2026-07-20T10:40:00Z"),
                completed_at: Some(ts("2026-07-20T11:10:00Z")),
            },
        ];
        approx(
            compute_spend(&rows, ts("2026-07-20T12:00:00Z"), &cfg(3.0)),
            3.0,
        );
    }

    #[test]
    fn zero_duration_null_cost_run_is_floored_not_zero() {
        // completed_at == created_at -> duration floored to 60s -> (60/3600)*3.0 = 0.05.
        let rows = vec![SpendRow {
            session_id: Some("s1".into()),
            cost_usd: None,
            created_at: ts("2026-07-20T10:00:00Z"),
            completed_at: Some(ts("2026-07-20T10:00:00Z")),
        }];
        approx(
            compute_spend(&rows, ts("2026-07-20T11:00:00Z"), &cfg(3.0)),
            0.05,
        );
    }

    #[test]
    fn sessionless_rows_count_independently() {
        // No session_id -> each row is its own unit: 0.2 (real) + 1.5 (30 min approx) = 1.7.
        let rows = vec![
            SpendRow {
                session_id: None,
                cost_usd: Some(0.2),
                created_at: ts("2026-07-20T10:00:00Z"),
                completed_at: Some(ts("2026-07-20T10:05:00Z")),
            },
            SpendRow {
                session_id: None,
                cost_usd: None,
                created_at: ts("2026-07-20T10:00:00Z"),
                completed_at: Some(ts("2026-07-20T10:30:00Z")),
            },
        ];
        approx(
            compute_spend(&rows, ts("2026-07-20T11:00:00Z"), &cfg(3.0)),
            1.7,
        );
    }

    #[test]
    fn running_row_uses_now_for_duration() {
        // No completed_at -> duration is now - created_at = 30 min -> 1.5.
        let rows = vec![SpendRow {
            session_id: Some("s1".into()),
            cost_usd: None,
            created_at: ts("2026-07-20T10:00:00Z"),
            completed_at: None,
        }];
        approx(
            compute_spend(&rows, ts("2026-07-20T10:30:00Z"), &cfg(3.0)),
            1.5,
        );
    }
}
