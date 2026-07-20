use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use croner::Cron;

use crate::autopilot::Mode;
use crate::config::{self, ScheduleRule};
use crate::runs::{CreateRunError, create_run_inner};
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

        let now = Utc::now();
        if current_date != Some(now.date_naive()) {
            fires_today.clear();
            current_date = Some(now.date_naive());
        }

        scheduler_tick(&state, now, &mut fires_today).await;
    }
}

pub(crate) async fn scheduler_tick(
    state: &AppState,
    now: DateTime<Utc>,
    fires_today: &mut HashMap<(String, String), u32>,
) {
    if crate::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return;
    }

    if crate::autopilot::scoped_kill_engaged(&state.pool, "trigger", "scheduled")
        .await
        .unwrap_or(true)
    {
        return;
    }

    if let crate::budget::BudgetDecision::Pause { reason } =
        crate::budget::budget_permits_new_run(&state.pool, now).await
    {
        tracing::info!(reason = %reason, "budget exhausted; scheduler paused this tick");
        return;
    }

    let projects = match crate::autopilot::autopilot_projects(&state.pool).await {
        Ok(projects) => projects,
        Err(error) => {
            tracing::warn!(%error, "failed to load autopilot projects for scheduler tick");
            return;
        }
    };

    for (project_id, project_root, project_mode) in projects {
        if crate::autopilot::scoped_kill_engaged(&state.pool, "project", &project_id)
            .await
            .unwrap_or(true)
        {
            continue;
        }

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

        let persisted_names: HashSet<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
        let mut last_fired = HashMap::new();
        for (rule_name, value) in &rows {
            match DateTime::parse_from_rfc3339(value) {
                Ok(timestamp) => {
                    last_fired.insert(rule_name.clone(), timestamp.with_timezone(&Utc));
                }
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        rule_name = %rule_name,
                        last_fired_at = %value,
                        %error,
                        "re-arming malformed scheduler timestamp"
                    );
                    if let Err(update_error) = sqlx::query(
                        "UPDATE scheduler_state
                         SET last_fired_at = ?
                         WHERE project_id = ? AND rule_name = ?",
                    )
                    .bind(now.to_rfc3339())
                    .bind(&project_id)
                    .bind(rule_name)
                    .execute(&state.pool)
                    .await
                    {
                        tracing::warn!(
                            project_id = %project_id,
                            rule_name = %rule_name,
                            error = %update_error,
                            "failed to re-arm malformed scheduler timestamp"
                        );
                    }
                }
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

            let (cwd, run_mode) = match project_mode {
                Mode::Shadow => (
                    rule.cwd.clone().unwrap_or_else(|| project_root.clone()),
                    "shadow",
                ),
                Mode::Active => (project_root.clone(), "worktree"),
                Mode::Off => continue,
            };
            match create_run_inner(
                state,
                rule.prompt.clone(),
                Some(project_id.clone()),
                Some(cwd),
                run_mode,
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
                            "scheduled run started but scheduler state update failed"
                        );
                    }

                    let count = fires_today.entry(fire_key).or_insert(0);
                    *count += 1;
                    tracing::info!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        run_id,
                        run_mode,
                        "fired scheduled run"
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
                Err(CreateRunError::Busy) => tracing::info!(
                    project_id = %project_id,
                    rule_name = %rule.name,
                    "scheduler deferred a worktree run; project busy"
                ),
                Err(error) => tracing::warn!(
                    project_id = %project_id,
                    rule_name = %rule.name,
                    run_mode,
                    %error,
                    "failed to create scheduled run"
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::runner::FakeCommandRunner;
    use crate::state::{AppState, DEFAULT_RUN_TIMEOUT};
    use std::path::{Path as FsPath, PathBuf};
    use std::process::Command;
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use std::time::Duration;

    const DAILY_CAP: u32 = 24;
    const WORKTREE_ROOT_ENV: &str = "NUCLEOS_WORKTREE_ROOT";
    const ACTIVE_TEST_CHILD_ENV: &str = "NUCLEOS_SCHEDULER_ACTIVE_TEST_CHILD";
    const ACTIVE_TEST_REPO_ENV: &str = "NUCLEOS_SCHEDULER_ACTIVE_TEST_REPO";

    async fn test_state(delay: Option<Duration>) -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        AppState {
            token: Token("test-token".into()),
            pool,
            runner: Arc::new(FakeCommandRunner {
                delay: Mutex::new(delay),
                ..Default::default()
            }),
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_timeout: DEFAULT_RUN_TIMEOUT,
        }
    }

    fn space_free_tempdir(prefix: &str) -> tempfile::TempDir {
        let base = std::env::current_dir().expect("resolve current directory");
        assert!(
            !base.to_string_lossy().contains(' '),
            "test checkout must have a space-free path"
        );
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(base)
            .expect("create space-free tempdir")
    }

    fn initialize_repo(repo: &FsPath) {
        std::fs::create_dir_all(repo).expect("create repository directory");
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@x"],
            vec!["config", "user.name", "test"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(repo)
                    .args(args)
                    .status()
                    .expect("git should start")
                    .success()
            );
        }
        std::fs::write(repo.join("seed.txt"), "seed\n").expect("write seed file");
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["add", "-A"])
                .status()
                .expect("git should start")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["commit", "-m", "seed"])
                .status()
                .expect("git should start")
                .success()
        );
    }

    fn write_schedule(project_root: &FsPath) {
        let ai_dir = project_root.join(".ai");
        std::fs::create_dir_all(&ai_dir).expect("create .ai directory");
        std::fs::write(
            ai_dir.join("autopilot.yaml"),
            "schedules:\n  - name: r1\n    cron: \"* * * * *\"\n    prompt: \"go\"\n",
        )
        .expect("write autopilot schedule");
    }

    async fn seed_project(
        state: &AppState,
        project_root: &FsPath,
        mode: &str,
        last_fired_at: &str,
    ) {
        write_schedule(project_root);
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('proj', ?, ?)",
        )
        .bind(mode)
        .bind(project_root.to_string_lossy().as_ref())
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO scheduler_state (project_id, rule_name, last_fired_at)
             VALUES ('proj', 'r1', ?)",
        )
        .bind(last_fired_at)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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

    #[tokio::test(flavor = "current_thread")]
    async fn tick_fires_a_worktree_run_for_an_active_project() {
        if std::env::var_os(ACTIVE_TEST_CHILD_ENV).is_none() {
            let _lock = env_lock();
            let repo_container = space_free_tempdir("nucleos-scheduler-active-");
            let repo = repo_container.path().join("repo");
            initialize_repo(&repo);
            let worktree_root = space_free_tempdir("nucleos-wt-test-scheduler-");
            let status = Command::new(std::env::current_exe().expect("resolve test executable"))
                .args([
                    "--exact",
                    "scheduler::tests::tick_fires_a_worktree_run_for_an_active_project",
                    "--nocapture",
                ])
                .env(ACTIVE_TEST_CHILD_ENV, "1")
                .env(ACTIVE_TEST_REPO_ENV, &repo)
                .env(WORKTREE_ROOT_ENV, worktree_root.path())
                .status()
                .expect("start isolated scheduler test process");
            assert!(status.success(), "isolated scheduler test process failed");
            return;
        }

        let repo = PathBuf::from(
            std::env::var_os(ACTIVE_TEST_REPO_ENV).expect("active test repository is set"),
        );
        let state = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, &repo, "active", &old).await;

        scheduler_tick(&state, now, &mut HashMap::new()).await;

        let (run_id, mode, project_id): (i64, String, Option<String>) =
            sqlx::query_as("SELECT id, mode, project_id FROM runs")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(mode, "worktree");
        assert_eq!(project_id.as_deref(), Some("proj"));
        let worktree_path: String =
            sqlx::query_scalar("SELECT path FROM worktrees WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let stored: String = sqlx::query_scalar(
            "SELECT last_fired_at FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'r1'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(timestamp(&stored), now);

        let _ = crate::worktree::remove(&repo, &PathBuf::from(worktree_path), &[]).await;
    }

    #[tokio::test]
    async fn tick_fires_a_shadow_run_for_a_shadow_project() {
        let project = tempfile::tempdir().expect("create shadow project");
        let state = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;

        scheduler_tick(&state, now, &mut HashMap::new()).await;

        let (mode, project_id): (String, Option<String>) =
            sqlx::query_as("SELECT mode, project_id FROM runs")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(mode, "shadow");
        assert_eq!(project_id.as_deref(), Some("proj"));
    }

    #[tokio::test]
    async fn tick_defers_when_the_project_is_busy() {
        let project = tempfile::tempdir().expect("create active project");
        let state = test_state(Some(Duration::from_millis(100))).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "active", &old).await;
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('proj', 'already running', 'running', 'worktree', ?)",
        )
        .bind(timestamp("2026-07-18T10:05:00Z").to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();

        scheduler_tick(&state, now, &mut HashMap::new()).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 1);
        let stored: String = sqlx::query_scalar(
            "SELECT last_fired_at FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'r1'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(stored, old);
    }

    #[tokio::test]
    async fn tick_rearms_a_malformed_timestamp() {
        let project = tempfile::tempdir().expect("create shadow project");
        let state = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        seed_project(&state, project.path(), "shadow", "not-a-date").await;

        scheduler_tick(&state, now, &mut HashMap::new()).await;

        let stored: String = sqlx::query_scalar(
            "SELECT last_fired_at FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'r1'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(timestamp(&stored), now);
        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
    }

    #[tokio::test]
    async fn tick_does_nothing_when_kill_switch_engaged() {
        let project = tempfile::tempdir().expect("create active project");
        let state = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "active", &old).await;
        crate::autopilot::set_kill_switch(&state.pool, true)
            .await
            .unwrap();

        scheduler_tick(&state, now, &mut HashMap::new()).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
    }

    #[tokio::test]
    async fn tick_does_nothing_when_over_budget() {
        let project = tempfile::tempdir().expect("create shadow project");
        let state = test_state(None).await;
        let now = timestamp("2026-07-20T10:10:00Z");
        let old = timestamp("2026-07-20T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;

        // A $1 monthly ceiling with $5 of prior autonomous spend this window -> over budget.
        crate::budget::set_budget_config(
            &state.pool,
            &crate::budget::BudgetConfig {
                limit_usd: Some(1.0),
                period: crate::budget::BudgetPeriod::Monthly,
                hourly_limit_usd: None,
                per_run_reserve_usd: 0.5,
                time_cost_per_hour_usd: 3.0,
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, cost_usd, created_at, completed_at)
             VALUES ('proj', 'prior spend', 'completed', 'worktree', 5.0, ?, ?)",
        )
        .bind(timestamp("2026-07-05T09:00:00Z").to_rfc3339())
        .bind(timestamp("2026-07-05T09:10:00Z").to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();

        scheduler_tick(&state, now, &mut HashMap::new()).await;

        // Only the pre-existing spend row remains; the over-budget scheduler fired no new run.
        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 1);
    }

    #[tokio::test]
    async fn tick_skips_a_project_when_its_kill_is_engaged() {
        let project = tempfile::tempdir().expect("create shadow project");
        let state = test_state(None).await;
        let now = timestamp("2026-07-20T10:10:00Z");
        let old = timestamp("2026-07-20T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;
        crate::autopilot::set_scoped_kill(&state.pool, "project", "proj", true)
            .await
            .unwrap();

        scheduler_tick(&state, now, &mut HashMap::new()).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
    }

    #[tokio::test]
    async fn tick_still_fires_when_a_different_project_is_killed() {
        let project = tempfile::tempdir().expect("create shadow project");
        let state = test_state(None).await;
        let now = timestamp("2026-07-20T10:10:00Z");
        let old = timestamp("2026-07-20T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;
        crate::autopilot::set_scoped_kill(&state.pool, "project", "some-other-project", true)
            .await
            .unwrap();

        scheduler_tick(&state, now, &mut HashMap::new()).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 1);
    }

    #[tokio::test]
    async fn tick_skips_all_when_the_scheduled_trigger_kill_is_engaged() {
        let project = tempfile::tempdir().expect("create shadow project");
        let state = test_state(None).await;
        let now = timestamp("2026-07-20T10:10:00Z");
        let old = timestamp("2026-07-20T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;
        crate::autopilot::set_scoped_kill(&state.pool, "trigger", "scheduled", true)
            .await
            .unwrap();

        scheduler_tick(&state, now, &mut HashMap::new()).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
    }
}
