use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use croner::Cron;

use crate::config::{self, ScheduleRule};
use crate::runs::create_run_inner;
use crate::state::AppState;

/// The daemon checks schedules twice per minute so boundary detection remains prompt.
const TICK: Duration = Duration::from_secs(30);
/// A rule cannot create more than one run per minute, even if it uses second-level cron syntax.
const MIN_INTERVAL: chrono::Duration = chrono::Duration::minutes(1);
/// Phase 1 limits each project rule to 24 runs per daemon day.
const DAILY_CAP: u32 = 24;

pub fn due_rules<'a>(
    rules: &'a [ScheduleRule],
    last_fired: &HashMap<String, DateTime<Utc>>,
    fires_today: &HashMap<String, u32>,
    now: DateTime<Utc>,
    min_interval: chrono::Duration,
    daily_cap: u32,
) -> Vec<&'a ScheduleRule> {
    rules
        .iter()
        .filter(|rule| {
            let cron = match rule.cron.parse::<Cron>() {
                Ok(cron) => cron,
                Err(error) => {
                    tracing::warn!(
                        rule_name = %rule.name,
                        cron = %rule.cron,
                        error = %error,
                        "skipping invalid schedule rule"
                    );
                    return false;
                }
            };

            let Some(last_fired_at) = last_fired.get(&rule.name) else {
                return false;
            };

            if now.signed_duration_since(*last_fired_at) < min_interval {
                return false;
            }

            if fires_today.get(&rule.name).copied().unwrap_or(0) >= daily_cap {
                return false;
            }

            match cron.find_next_occurrence(last_fired_at, false) {
                Ok(next) => next <= now,
                Err(error) => {
                    tracing::warn!(
                        rule_name = %rule.name,
                        cron = %rule.cron,
                        error = %error,
                        "could not calculate the next schedule occurrence"
                    );
                    false
                }
            }
        })
        .collect()
}

pub async fn run_scheduler(state: AppState) {
    let mut interval = tokio::time::interval(TICK);
    let mut fires_today: HashMap<(String, String), u32> = HashMap::new();
    let mut current_date = None;

    loop {
        interval.tick().await;

        if crate::autopilot::kill_switch_engaged(&state.pool)
            .await
            .unwrap_or(true)
        {
            continue;
        }

        let now = Utc::now();
        if current_date != Some(now.date_naive()) {
            fires_today.clear();
            current_date = Some(now.date_naive());
        }

        let projects = match crate::autopilot::shadow_projects(&state.pool).await {
            Ok(projects) => projects,
            Err(error) => {
                tracing::warn!(%error, "failed to load shadow projects for scheduler tick");
                continue;
            }
        };

        for (project_id, project_root) in projects {
            let rules = match config::load_schedule_rules(Path::new(&project_root)) {
                Ok(rules) => rules.schedules,
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        project_root = %project_root,
                        %error,
                        "failed to load project schedule rules"
                    );
                    continue;
                }
            };

            let rows: Vec<(String, String)> = match sqlx::query_as(
                "SELECT rule_name, last_fired_at
                 FROM scheduler_state
                 WHERE project_id = ?",
            )
            .bind(&project_id)
            .fetch_all(&state.pool)
            .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        %error,
                        "failed to load persisted scheduler state"
                    );
                    continue;
                }
            };

            let persisted_names: HashSet<&str> =
                rows.iter().map(|(name, _)| name.as_str()).collect();
            let mut last_fired = HashMap::new();
            for (rule_name, value) in &rows {
                match DateTime::parse_from_rfc3339(value) {
                    Ok(timestamp) => {
                        last_fired.insert(rule_name.clone(), timestamp.with_timezone(&Utc));
                    }
                    Err(error) => tracing::warn!(
                        project_id = %project_id,
                        rule_name = %rule_name,
                        last_fired_at = %value,
                        %error,
                        "skipping malformed scheduler timestamp"
                    ),
                }
            }

            for rule in &rules {
                if persisted_names.contains(rule.name.as_str()) {
                    continue;
                }

                if let Err(error) = sqlx::query(
                    "INSERT INTO scheduler_state (project_id, rule_name, last_fired_at)
                     VALUES (?, ?, ?)",
                )
                .bind(&project_id)
                .bind(&rule.name)
                .bind(now.to_rfc3339())
                .execute(&state.pool)
                .await
                {
                    tracing::warn!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        %error,
                        "failed to arm new schedule rule"
                    );
                }
            }

            let project_fires: HashMap<String, u32> = fires_today
                .iter()
                .filter_map(|((stored_project_id, rule_name), count)| {
                    (stored_project_id == &project_id).then(|| (rule_name.clone(), *count))
                })
                .collect();
            let due = due_rules(
                &rules,
                &last_fired,
                &project_fires,
                now,
                MIN_INTERVAL,
                DAILY_CAP,
            );

            let mut fired_this_tick = HashSet::new();
            for rule in due {
                let fire_key = (project_id.clone(), rule.name.clone());
                if fires_today.get(&fire_key).copied().unwrap_or(0) >= DAILY_CAP {
                    tracing::warn!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        daily_cap = DAILY_CAP,
                        "scheduler runaway guard suppressed a scheduled run"
                    );
                    continue;
                }
                if !fired_this_tick.insert(rule.name.clone()) {
                    tracing::warn!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        "duplicate schedule rule name suppressed within scheduler tick"
                    );
                    continue;
                }

                let cwd = rule.cwd.clone().unwrap_or_else(|| project_root.clone());
                match create_run_inner(
                    &state,
                    rule.prompt.clone(),
                    Some(project_id.clone()),
                    Some(cwd),
                    "shadow",
                )
                .await
                {
                    Ok(run_id) => {
                        if let Err(error) = sqlx::query(
                            "UPDATE scheduler_state
                             SET last_fired_at = ?
                             WHERE project_id = ? AND rule_name = ?",
                        )
                        .bind(now.to_rfc3339())
                        .bind(&project_id)
                        .bind(&rule.name)
                        .execute(&state.pool)
                        .await
                        {
                            tracing::warn!(
                                project_id = %project_id,
                                rule_name = %rule.name,
                                run_id,
                                %error,
                                "shadow run started but scheduler state update failed"
                            );
                        }

                        let count = fires_today.entry(fire_key).or_insert(0);
                        *count += 1;
                        tracing::info!(
                            project_id = %project_id,
                            rule_name = %rule.name,
                            run_id,
                            "fired scheduled shadow run"
                        );
                        if *count == DAILY_CAP {
                            tracing::warn!(
                                project_id = %project_id,
                                rule_name = %rule.name,
                                daily_cap = DAILY_CAP,
                                "scheduler runaway guard reached; suppressing further runs today"
                            );
                        }
                    }
                    Err(error) => tracing::warn!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        %error,
                        "failed to create scheduled shadow run"
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAILY_CAP: u32 = 24;

    fn timestamp(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn rule(name: &str, cron: &str) -> ScheduleRule {
        ScheduleRule {
            name: name.to_string(),
            cron: cron.to_string(),
            prompt: "test prompt".to_string(),
            cwd: None,
        }
    }

    #[test]
    fn missed_cron_boundary_is_due() {
        let now = timestamp("2026-07-18T10:02:00Z");
        let rules = vec![rule("every-minute", "* * * * *")];
        let last_fired = HashMap::from([(
            "every-minute".to_string(),
            timestamp("2026-07-18T10:00:00Z"),
        )]);

        let due = due_rules(
            &rules,
            &last_fired,
            &HashMap::new(),
            now,
            chrono::Duration::minutes(1),
            DAILY_CAP,
        );

        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "every-minute");
    }

    #[test]
    fn min_interval_suppresses_just_fired_rule() {
        let now = timestamp("2026-07-18T10:02:00Z");
        let rules = vec![rule("every-second", "* * * * * *")];
        let last_fired = HashMap::from([(
            "every-second".to_string(),
            timestamp("2026-07-18T10:01:50Z"),
        )]);

        let due = due_rules(
            &rules,
            &last_fired,
            &HashMap::new(),
            now,
            chrono::Duration::minutes(1),
            DAILY_CAP,
        );

        assert!(due.is_empty());
    }

    #[test]
    fn invalid_cron_is_skipped() {
        let now = timestamp("2026-07-18T10:02:00Z");
        let rules = vec![rule("invalid", "not a cron")];
        let last_fired =
            HashMap::from([("invalid".to_string(), timestamp("2026-07-18T09:00:00Z"))]);

        let due = due_rules(
            &rules,
            &last_fired,
            &HashMap::new(),
            now,
            chrono::Duration::minutes(1),
            DAILY_CAP,
        );

        assert!(due.is_empty());
    }

    #[test]
    fn daily_cap_suppresses_due_rule() {
        let now = timestamp("2026-07-18T10:02:00Z");
        let rules = vec![rule("every-minute", "* * * * *")];
        let last_fired = HashMap::from([(
            "every-minute".to_string(),
            timestamp("2026-07-18T10:00:00Z"),
        )]);
        let fires_today = HashMap::from([("every-minute".to_string(), DAILY_CAP)]);

        let due = due_rules(
            &rules,
            &last_fired,
            &fires_today,
            now,
            chrono::Duration::minutes(1),
            DAILY_CAP,
        );

        assert!(due.is_empty());
    }
}
