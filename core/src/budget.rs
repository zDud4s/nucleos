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
}
