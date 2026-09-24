ALTER TABLE autopilot_global ADD COLUMN quota_brake_enabled INTEGER NOT NULL DEFAULT 0 CHECK (quota_brake_enabled IN (0, 1));
ALTER TABLE autopilot_global ADD COLUMN quota_pause_above_percent_5h INTEGER NOT NULL DEFAULT 85 CHECK (quota_pause_above_percent_5h BETWEEN 1 AND 100);
ALTER TABLE autopilot_global ADD COLUMN quota_pause_above_percent_7d INTEGER NOT NULL DEFAULT 90 CHECK (quota_pause_above_percent_7d BETWEEN 1 AND 100);
ALTER TABLE autopilot_global ADD COLUMN quota_blind_announced_at TEXT;
ALTER TABLE autopilot_global ADD COLUMN quota_hold_started_at TEXT;
ALTER TABLE autopilot_global ADD COLUMN quota_hold_ended_at TEXT;
