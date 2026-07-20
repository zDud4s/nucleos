ALTER TABLE autopilot_global ADD COLUMN budget_limit_usd REAL;
ALTER TABLE autopilot_global ADD COLUMN budget_period TEXT NOT NULL DEFAULT 'monthly';
ALTER TABLE autopilot_global ADD COLUMN budget_hourly_limit_usd REAL;
ALTER TABLE autopilot_global ADD COLUMN budget_per_run_reserve_usd REAL NOT NULL DEFAULT 0.5;
ALTER TABLE autopilot_global ADD COLUMN budget_time_cost_per_hour_usd REAL NOT NULL DEFAULT 3.0;
