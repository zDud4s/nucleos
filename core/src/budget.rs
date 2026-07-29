use chrono::{DateTime, Datelike, Utc};
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

/// Whether autonomy may start a new proactive run under the current budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetDecision {
    Allow,
    Pause { reason: String },
}

/// One run's contribution to the budget, already parsed from the `runs` table.
#[derive(Debug, Clone)]
pub struct SpendRow {
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    // Retained as loaded run telemetry for non-pricing readers. Without a price table these fields
    // must not influence the conservative time approximation.
    #[allow(dead_code)]
    pub input_tokens: Option<i64>,
    #[allow(dead_code)]
    pub output_tokens: Option<i64>,
    #[allow(dead_code)]
    pub cache_read_tokens: Option<i64>,
    #[allow(dead_code)]
    pub num_turns: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// A run with an unknown cost still consumed at least this much wall time for approximation, so a
/// zero/near-zero-duration run is never counted as $0 (spec §8.5: unmeasured cost is never free).
const MIN_APPROX_SECONDS: i64 = 60;

fn time_approx(row: &SpendRow, now: DateTime<Utc>, rate_per_hour: f64) -> f64 {
    let end = row.completed_at.unwrap_or(now);
    let elapsed_seconds = (end - row.created_at).num_seconds();
    let seconds = elapsed_seconds.max(MIN_APPROX_SECONDS);
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
        // ONE of those values rather than their sum; a session with no known cost yet falls back
        // to time.
        //
        // The largest, not the most recent. Cumulativeness is an assumption about the CLI's output,
        // not something this can verify: a restart, a version change, or a crafted result line can
        // report less than the session has already spent, and taking that value verbatim erases
        // real money and reopens the gate. A spend limit has to round the wrong way on purpose,
        // and `max` costs nothing when the assumption does hold.
        let largest_known_cost = group
            .iter()
            .filter_map(|row| row.cost_usd)
            .fold(None::<f64>, |acc, cost| {
                Some(acc.map_or(cost, |a| a.max(cost)))
            });

        match largest_known_cost {
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

/// Calendar-anchored start of the current budget window, in UTC.
fn window_start(period: BudgetPeriod, now: DateTime<Utc>) -> DateTime<Utc> {
    let day = match period {
        BudgetPeriod::Daily => now.date_naive(),
        BudgetPeriod::Weekly => {
            let back = now.weekday().num_days_from_monday() as i64;
            now.date_naive() - chrono::Duration::days(back)
        }
        BudgetPeriod::Monthly => now.date_naive().with_day(1).expect("day 1 is always valid"),
    };
    day.and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_utc()
}

async fn autonomous_rows(pool: &SqlitePool) -> sqlx::Result<Vec<SpendRow>> {
    // (session_id, cost_usd, input_tokens, output_tokens, cache_read_tokens, num_turns,
    // created_at, completed_at)
    type RawRow = (
        Option<String>,
        Option<f64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        String,
        Option<String>,
    );
    let raw: Vec<RawRow> = sqlx::query_as(
        "SELECT session_id, cost_usd, input_tokens, output_tokens, cache_read_tokens, num_turns,
                created_at, completed_at
         FROM runs
         WHERE mode IN ('shadow', 'worktree', 'email_triage')",
    )
    .fetch_all(pool)
    .await?;

    let parse = |value: &str| -> sqlx::Result<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(value)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|error| {
                sqlx::Error::Protocol(format!("invalid run timestamp {value}: {error}"))
            })
    };

    raw.into_iter()
        .map(
            |(
                session_id,
                cost_usd,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                num_turns,
                created_at,
                completed_at,
            )| {
                Ok(SpendRow {
                    session_id,
                    cost_usd,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    num_turns,
                    created_at: parse(&created_at)?,
                    completed_at: completed_at.as_deref().map(parse).transpose()?,
                })
            },
        )
        .collect()
}

/// Total autonomous spend in the current budget window (calendar-anchored in UTC by the configured period).
pub async fn window_spend(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<f64> {
    let cfg = load_budget_config(pool).await?;
    let since = window_start(cfg.period, now);
    let rows: Vec<SpendRow> = autonomous_rows(pool)
        .await?
        .into_iter()
        .filter(|row| row.created_at >= since)
        .collect();
    Ok(compute_spend(&rows, now, &cfg))
}

/// PURE: whether a run was still spending at some point in `[since, now]`.
///
/// The test used to be `created_at >= since`, which asks when a run STARTED rather than whether it
/// was running. A run that began 61 minutes ago and is burning money right now was invisible to
/// the hourly cap — and it is the long expensive run, not the short one, that the cap exists to
/// catch. A run is in the window if it had not finished when the window opened.
fn overlaps_window(row: &SpendRow, since: DateTime<Utc>) -> bool {
    row.completed_at.unwrap_or(DateTime::<Utc>::MAX_UTC) >= since
}

/// Total autonomous spend in the trailing 60 minutes (independent of the window boundary).
///
/// Counted in full rather than apportioned to the part of the run inside the hour. Splitting a
/// reported cost across time would be inventing a spending curve the CLI never reports, and it
/// would contradict this module's own rule that a known cost is authoritative. Counting in full
/// over-counts a run that finished early in the hour, which is the direction a limit should round:
/// a $40 run that ended twenty minutes ago is recent spending, and it drops out an hour later.
pub async fn hourly_spend(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<f64> {
    let cfg = load_budget_config(pool).await?;
    let since = now - chrono::Duration::hours(1);
    let rows: Vec<SpendRow> = autonomous_rows(pool)
        .await?
        .into_iter()
        .filter(|row| overlaps_window(row, since))
        .map(|row| SpendRow {
            // Only for the rows whose cost is approximated from elapsed time: there the model
            // already says cost is linear in duration, so clipping the start to the window is that
            // model applied, not a new one. `cost_usd` ignores this field entirely, which is what
            // keeps a known cost whole and authoritative.
            created_at: row.created_at.max(since),
            ..row
        })
        .collect();
    Ok(compute_spend(&rows, now, &cfg))
}

async fn evaluate_budget(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<BudgetDecision> {
    let cfg = load_budget_config(pool).await?;

    if let Some(limit) = cfg.limit_usd {
        let spent = window_spend(pool, now).await?;
        if spent + cfg.per_run_reserve_usd > limit {
            return Ok(BudgetDecision::Pause {
                reason: format!(
                    "window spend ${spent:.2} + ${:.2} reserve would exceed the ${limit:.2} limit",
                    cfg.per_run_reserve_usd
                ),
            });
        }
    }

    if let Some(hourly_limit) = cfg.hourly_limit_usd {
        let spent = hourly_spend(pool, now).await?;
        if spent + cfg.per_run_reserve_usd > hourly_limit {
            return Ok(BudgetDecision::Pause {
                reason: format!(
                    "hourly spend ${spent:.2} + ${:.2} reserve would exceed the ${hourly_limit:.2} hourly limit",
                    cfg.per_run_reserve_usd
                ),
            });
        }
    }

    Ok(BudgetDecision::Allow)
}

/// Whether the budget currently permits starting a new proactive run. Fails closed (Pause) on any error.
pub async fn budget_permits_new_run(pool: &SqlitePool, now: DateTime<Utc>) -> BudgetDecision {
    match evaluate_budget(pool, now).await {
        Ok(decision) => decision,
        Err(error) => BudgetDecision::Pause {
            reason: format!("budget check failed, pausing to be safe: {error}"),
        },
    }
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
    fn time_approx_keeps_its_floor_even_when_usage_was_reported() {
        let measured = SpendRow {
            session_id: Some("measured".into()),
            cost_usd: None,
            input_tokens: Some(1000),
            output_tokens: Some(500),
            cache_read_tokens: Some(20_000),
            num_turns: Some(12),
            created_at: ts("2026-07-20T10:00:00Z"),
            completed_at: Some(ts("2026-07-20T10:00:30Z")),
        };
        let now = ts("2026-07-20T11:00:00Z");

        // Usage without a reported price cannot make an unknown-cost run free or cheaper.
        approx(time_approx(&measured, now, 3.0), 0.05);
    }

    #[test]
    fn single_completed_run_counts_its_cost() {
        let rows = vec![SpendRow {
            session_id: Some("s1".into()),
            cost_usd: Some(0.5),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            num_turns: None,
            created_at: ts("2026-07-20T10:00:00Z"),
            completed_at: Some(ts("2026-07-20T10:05:00Z")),
        }];
        approx(
            compute_spend(&rows, ts("2026-07-20T11:00:00Z"), &cfg(3.0)),
            0.5,
        );
    }

    #[test]
    fn resumed_session_counts_one_reported_cost_not_their_sum() {
        // Original paused run (no result -> cost None) then a resume run whose cost is the
        // cumulative session total. Must count 0.9 once, NOT 0.9 + time-approx of the first row.
        let rows = vec![
            SpendRow {
                session_id: Some("s1".into()),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
                created_at: ts("2026-07-20T10:00:00Z"),
                completed_at: Some(ts("2026-07-20T10:30:00Z")),
            },
            SpendRow {
                session_id: Some("s1".into()),
                cost_usd: Some(0.9),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
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
    fn a_later_smaller_report_cannot_erase_spend_already_counted() {
        // The cumulative-total assumption is an assumption, not a guarantee: a CLI restart, a
        // version change, or simply a crafted result line can make a resume report LESS than the
        // session already spent. Taking the most recent value verbatim then wipes out real money
        // and reopens the budget gate. A spend limit must round the wrong way on purpose.
        let rows = vec![
            SpendRow {
                session_id: Some("s1".into()),
                cost_usd: Some(40.0),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
                created_at: ts("2026-07-20T10:00:00Z"),
                completed_at: Some(ts("2026-07-20T10:30:00Z")),
            },
            SpendRow {
                session_id: Some("s1".into()),
                cost_usd: Some(0.02),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
                created_at: ts("2026-07-20T10:40:00Z"),
                completed_at: Some(ts("2026-07-20T10:50:00Z")),
            },
        ];
        approx(
            compute_spend(&rows, ts("2026-07-20T11:00:00Z"), &cfg(3.0)),
            40.0,
        );
    }

    #[test]
    fn null_cost_run_is_approximated_by_time_never_zero() {
        // 30 min of runtime at $3/h = $1.50.
        let rows = vec![SpendRow {
            session_id: Some("s1".into()),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            num_turns: None,
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
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
                created_at: ts("2026-07-20T10:00:00Z"),
                completed_at: Some(ts("2026-07-20T10:30:00Z")),
            },
            SpendRow {
                session_id: Some("s1".into()),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
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
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            num_turns: None,
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
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
                created_at: ts("2026-07-20T10:00:00Z"),
                completed_at: Some(ts("2026-07-20T10:05:00Z")),
            },
            SpendRow {
                session_id: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
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
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            num_turns: None,
            created_at: ts("2026-07-20T10:00:00Z"),
            completed_at: None,
        }];
        approx(
            compute_spend(&rows, ts("2026-07-20T10:30:00Z"), &cfg(3.0)),
            1.5,
        );
    }

    async fn insert_run(
        pool: &SqlitePool,
        mode: &str,
        session_id: Option<&str>,
        cost_usd: Option<f64>,
        created_at: &str,
        completed_at: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, cost_usd, created_at, completed_at)
             VALUES ('p', 'completed', ?, ?, ?, ?, ?)",
        )
        .bind(mode)
        .bind(session_id)
        .bind(cost_usd)
        .bind(created_at)
        .bind(completed_at)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_gate_execution_is_not_an_autonomous_row() {
        let pool = test_pool().await;
        let worktree = tempfile::tempdir().expect("create temporary worktree");
        let before = autonomous_rows(&pool).await.unwrap();

        let outcome = crate::gate::run_gate(
            worktree.path(),
            r#"sh -c "exit 0""#,
            std::time::Duration::from_secs(1),
        )
        .await;

        assert!(matches!(outcome, crate::gate::GateOutcome::Passed));
        let after = autonomous_rows(&pool).await.unwrap();
        assert_eq!(after.len(), before.len());
    }

    #[tokio::test]
    async fn window_spend_counts_only_autonomous_runs_in_window() {
        let pool = test_pool().await;
        let now = ts("2026-07-20T12:00:00Z"); // default period = monthly -> window from 2026-07-01
        insert_run(
            &pool,
            "worktree",
            Some("a"),
            Some(1.0),
            "2026-07-10T09:00:00Z",
            Some("2026-07-10T09:10:00Z"),
        )
        .await;
        insert_run(
            &pool,
            "shadow",
            Some("b"),
            Some(0.5),
            "2026-07-15T09:00:00Z",
            Some("2026-07-15T09:10:00Z"),
        )
        .await;
        insert_run(
            &pool,
            "real",
            Some("c"),
            Some(5.0),
            "2026-07-12T09:00:00Z",
            Some("2026-07-12T09:10:00Z"),
        )
        .await; // excluded: manual
        insert_run(
            &pool,
            "worktree",
            Some("d"),
            Some(9.0),
            "2026-06-20T09:00:00Z",
            Some("2026-06-20T09:10:00Z"),
        )
        .await; // excluded: before window

        approx(window_spend(&pool, now).await.unwrap(), 1.5);
    }

    #[tokio::test]
    async fn window_spend_dedups_resumed_session() {
        let pool = test_pool().await;
        let now = ts("2026-07-20T12:00:00Z");
        // paused original (no result cost) then a resume run reporting the cumulative session total.
        insert_run(
            &pool,
            "worktree",
            Some("s1"),
            None,
            "2026-07-10T09:00:00Z",
            Some("2026-07-10T09:30:00Z"),
        )
        .await;
        insert_run(
            &pool,
            "worktree",
            Some("s1"),
            Some(0.9),
            "2026-07-11T09:00:00Z",
            Some("2026-07-11T09:10:00Z"),
        )
        .await;

        approx(window_spend(&pool, now).await.unwrap(), 0.9);
    }

    #[tokio::test]
    async fn hourly_spend_counts_only_the_last_hour() {
        let pool = test_pool().await;
        let now = ts("2026-07-20T12:00:00Z");
        insert_run(
            &pool,
            "worktree",
            Some("a"),
            Some(0.4),
            "2026-07-20T11:30:00Z",
            Some("2026-07-20T11:40:00Z"),
        )
        .await; // within last hour
        insert_run(
            &pool,
            "worktree",
            Some("b"),
            Some(2.0),
            "2026-07-20T09:00:00Z",
            Some("2026-07-20T09:10:00Z"),
        )
        .await; // older today

        approx(hourly_spend(&pool, now).await.unwrap(), 0.4);
    }

    /// The hourly cap is a brake on bursts, and the run it most needs to see is the long expensive
    /// one — which was the single shape it could not see, because the test asked when a run started
    /// rather than whether it was running.
    #[tokio::test]
    async fn hourly_spend_sees_a_run_that_started_before_the_hour_and_is_still_going() {
        let pool = test_pool().await;
        let now = ts("2026-07-20T12:00:00Z");
        // Started 90 minutes ago, still running, $40 reported so far.
        insert_run(
            &pool,
            "worktree",
            Some("long"),
            Some(40.0),
            "2026-07-20T10:30:00Z",
            None,
        )
        .await;

        approx(hourly_spend(&pool, now).await.unwrap(), 40.0);
    }

    /// Counted in full, not apportioned: splitting a reported cost across time would invent a
    /// spending curve the CLI never reports. Over-counting a run that ended early in the hour is
    /// the direction a limit should round, and it drops out an hour after it finishes.
    #[tokio::test]
    async fn a_known_cost_stays_whole_while_it_overlaps_the_hour() {
        let pool = test_pool().await;
        insert_run(
            &pool,
            "worktree",
            Some("spanning"),
            Some(40.0),
            "2026-07-20T09:00:00Z",
            Some("2026-07-20T11:30:00Z"),
        )
        .await;

        // Half an hour after it finished: still inside the trailing hour, counted whole.
        approx(
            hourly_spend(&pool, ts("2026-07-20T12:00:00Z"))
                .await
                .unwrap(),
            40.0,
        );
        // An hour and a minute after: gone.
        approx(
            hourly_spend(&pool, ts("2026-07-20T12:31:00Z"))
                .await
                .unwrap(),
            0.0,
        );
    }

    /// An unknown cost is approximated from elapsed time, so for those rows the linear model is the
    /// whole basis of the number — and clipping the start to the window is that model applied
    /// rather than a second one invented on top.
    #[tokio::test]
    async fn an_approximated_cost_counts_only_its_time_inside_the_hour() {
        let pool = test_pool().await;
        set_budget_config(
            &pool,
            &BudgetConfig {
                limit_usd: None,
                period: BudgetPeriod::Daily,
                hourly_limit_usd: None,
                per_run_reserve_usd: 0.0,
                time_cost_per_hour_usd: 3.0,
            },
        )
        .await
        .unwrap();
        // Three hours in, no reported cost, still running. Only the last hour is inside the window.
        insert_run(
            &pool,
            "worktree",
            Some("slow"),
            None,
            "2026-07-20T09:00:00Z",
            None,
        )
        .await;

        approx(
            hourly_spend(&pool, ts("2026-07-20T12:00:00Z"))
                .await
                .unwrap(),
            3.0,
        );
    }

    #[tokio::test]
    async fn hourly_spend_is_independent_of_window_across_midnight() {
        let pool = test_pool().await;
        // Daily window; at 00:30 UTC the previous hour is in YESTERDAY, before today's window start.
        set_budget_config(
            &pool,
            &BudgetConfig {
                limit_usd: None,
                period: BudgetPeriod::Daily,
                hourly_limit_usd: None,
                per_run_reserve_usd: 0.5,
                time_cost_per_hour_usd: 3.0,
            },
        )
        .await
        .unwrap();
        let now = ts("2026-07-20T00:30:00Z");
        insert_run(
            &pool,
            "worktree",
            Some("a"),
            Some(0.7),
            "2026-07-19T23:45:00Z",
            Some("2026-07-19T23:55:00Z"),
        )
        .await;

        approx(window_spend(&pool, now).await.unwrap(), 0.0); // before today's UTC midnight -> excluded
        approx(hourly_spend(&pool, now).await.unwrap(), 0.7); // within the trailing hour -> included
    }

    #[tokio::test]
    async fn allows_when_no_limit_configured() {
        let pool = test_pool().await;
        let now = ts("2026-07-20T12:00:00Z");
        insert_run(
            &pool,
            "worktree",
            Some("a"),
            Some(100.0),
            "2026-07-10T09:00:00Z",
            Some("2026-07-10T09:10:00Z"),
        )
        .await;

        assert!(matches!(
            budget_permits_new_run(&pool, now).await,
            BudgetDecision::Allow
        ));
    }

    #[tokio::test]
    async fn pauses_when_window_spend_plus_reserve_exceeds_limit() {
        let pool = test_pool().await;
        set_budget_config(
            &pool,
            &BudgetConfig {
                limit_usd: Some(2.0),
                period: BudgetPeriod::Monthly,
                hourly_limit_usd: None,
                per_run_reserve_usd: 0.5,
                time_cost_per_hour_usd: 3.0,
            },
        )
        .await
        .unwrap();
        let now = ts("2026-07-20T12:00:00Z");
        // 0.8 + 0.8 = 1.6; 1.6 + 0.5 reserve = 2.1 > 2.0 -> pause.
        insert_run(
            &pool,
            "worktree",
            Some("a"),
            Some(0.8),
            "2026-07-10T09:00:00Z",
            Some("2026-07-10T09:10:00Z"),
        )
        .await;
        insert_run(
            &pool,
            "worktree",
            Some("b"),
            Some(0.8),
            "2026-07-11T09:00:00Z",
            Some("2026-07-11T09:10:00Z"),
        )
        .await;

        assert!(matches!(
            budget_permits_new_run(&pool, now).await,
            BudgetDecision::Pause { .. }
        ));
    }

    #[tokio::test]
    async fn allows_when_under_limit_with_reserve_headroom() {
        let pool = test_pool().await;
        set_budget_config(
            &pool,
            &BudgetConfig {
                limit_usd: Some(2.0),
                period: BudgetPeriod::Monthly,
                hourly_limit_usd: None,
                per_run_reserve_usd: 0.5,
                time_cost_per_hour_usd: 3.0,
            },
        )
        .await
        .unwrap();
        let now = ts("2026-07-20T12:00:00Z");
        // 1.0 + 0.5 reserve = 1.5 <= 2.0 -> allow.
        insert_run(
            &pool,
            "worktree",
            Some("a"),
            Some(1.0),
            "2026-07-10T09:00:00Z",
            Some("2026-07-10T09:10:00Z"),
        )
        .await;

        assert!(matches!(
            budget_permits_new_run(&pool, now).await,
            BudgetDecision::Allow
        ));
    }

    #[tokio::test]
    async fn pauses_when_hourly_spend_exceeds_hourly_limit() {
        let pool = test_pool().await;
        set_budget_config(
            &pool,
            &BudgetConfig {
                limit_usd: None,
                period: BudgetPeriod::Monthly,
                hourly_limit_usd: Some(1.0),
                per_run_reserve_usd: 0.5,
                time_cost_per_hour_usd: 3.0,
            },
        )
        .await
        .unwrap();
        let now = ts("2026-07-20T12:00:00Z");
        // 0.7 in the last hour + 0.5 reserve = 1.2 > 1.0 hourly limit -> pause (window limit is None).
        insert_run(
            &pool,
            "worktree",
            Some("a"),
            Some(0.7),
            "2026-07-20T11:30:00Z",
            Some("2026-07-20T11:40:00Z"),
        )
        .await;

        assert!(matches!(
            budget_permits_new_run(&pool, now).await,
            BudgetDecision::Pause { .. }
        ));
    }

    #[tokio::test]
    async fn pauses_fail_safe_when_config_row_missing() {
        let pool = test_pool().await;
        sqlx::query("DELETE FROM autopilot_global")
            .execute(&pool)
            .await
            .unwrap();
        let now = ts("2026-07-20T12:00:00Z");

        assert!(matches!(
            budget_permits_new_run(&pool, now).await,
            BudgetDecision::Pause { .. }
        ));
    }
}
