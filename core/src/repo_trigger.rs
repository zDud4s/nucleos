use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::Path;

use crate::config::RepoTrigger;

/// Repo triggers whose watched-branch SHA has changed since it was last seen.
/// A trigger with no recorded last SHA is being seen for the first time (armed, not fired); a trigger
/// whose current SHA is unknown (git failed this poll) is skipped.
pub fn due_repo_triggers<'a>(
    rules: &'a [RepoTrigger],
    last_shas: &HashMap<String, String>,
    current_shas: &HashMap<String, String>,
) -> Vec<&'a RepoTrigger> {
    rules
        .iter()
        .filter(
            |rule| match (last_shas.get(&rule.name), current_shas.get(&rule.name)) {
                (Some(last), Some(current)) => last != current,
                _ => false,
            },
        )
        .collect()
}

/// The last-seen branch SHA for every repo trigger of a project, keyed by trigger name.
pub async fn last_shas_for_project(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<HashMap<String, String>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT trigger_name, last_sha FROM repo_trigger_state WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Record (upsert) the last-seen branch SHA for one trigger.
pub async fn record_sha(
    pool: &SqlitePool,
    project_id: &str,
    trigger_name: &str,
    sha: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO repo_trigger_state (project_id, trigger_name, last_sha) VALUES (?, ?, ?)
         ON CONFLICT(project_id, trigger_name) DO UPDATE SET last_sha = excluded.last_sha",
    )
    .bind(project_id)
    .bind(trigger_name)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch the repo (best-effort when `fetch` is true) and return the current SHA of `git_ref`
/// (a branch name or any ref). Returns `None` when git is unavailable or the ref does not resolve —
/// the caller treats an unknown current SHA as "skip this trigger this poll".
fn git_bin() -> String {
    std::env::var("NUCLEOS_GIT_BIN").unwrap_or_else(|_| "git".into())
}

pub async fn current_branch_sha(repo: &Path, git_ref: &str, fetch: bool) -> Option<String> {
    if fetch {
        // Best-effort: an offline / remoteless fetch failure must not block reading the local ref.
        let _ = tokio::process::Command::new(git_bin())
            .arg("-C")
            .arg(repo)
            .arg("fetch")
            .arg("--quiet")
            .output()
            .await;
    }
    // A branch name comes from `.ai/autopilot.yaml`, so it is configuration rather than a constant,
    // and `rev-parse` reads a leading `-` as an option: a value like `--git-dir=...` changed what
    // the command did. No shell is involved, so this was argument injection rather than command
    // injection — still not the config file's decision to make. Rejecting the shape is simpler and
    // more predictable than a separator flag, because `rev-parse` echoes `--end-of-options` back as
    // an argument rather than honouring it.
    if git_ref.starts_with('-') {
        tracing::warn!(
            git_ref,
            "repo trigger: refusing a ref that looks like an option"
        );
        return None;
    }
    let output = tokio::process::Command::new(git_bin())
        .arg("-C")
        .arg(repo)
        .arg("rev-parse")
        .arg(git_ref)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

/// Interval between repo-event polls. `git fetch` is network-heavy, so this runs far less often than the
/// 30s scheduler tick.
const REPO_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug)]
struct GateRefusal {
    reason: String,
    stop_tick: bool,
}

#[derive(Debug)]
pub(crate) enum TriggerFireOutcome {
    Fired(i64),
    Deferred { reason: String, stop_tick: bool },
    Busy,
    Failed(String),
}

/// The one governance path for every repo trigger, whether its branch movement came from polling
/// or an authenticated webhook delivery.
async fn governance_permits_repo_trigger(
    state: &crate::state::AppState,
    project_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), GateRefusal> {
    if crate::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return Err(GateRefusal {
            reason: "global kill switch is engaged or unreadable".to_string(),
            stop_tick: true,
        });
    }
    if crate::autopilot::scoped_kill_engaged(&state.pool, "trigger", "repo")
        .await
        .unwrap_or(true)
    {
        return Err(GateRefusal {
            reason: "repo-trigger kill switch is engaged or unreadable".to_string(),
            stop_tick: true,
        });
    }
    if crate::autopilot::scoped_kill_engaged(&state.pool, "project", project_id)
        .await
        .unwrap_or(true)
    {
        return Err(GateRefusal {
            reason: "project kill switch is engaged or unreadable".to_string(),
            stop_tick: false,
        });
    }
    if let crate::budget::BudgetDecision::Pause { reason, .. } =
        crate::budget::budget_permits_new_run(&state.pool, now).await
    {
        return Err(GateRefusal {
            reason,
            stop_tick: true,
        });
    }
    if let crate::wip::WipDecision::Defer { reason } =
        crate::wip::wip_permits_new_run(&state.pool, project_id).await
    {
        return Err(GateRefusal {
            reason,
            stop_tick: false,
        });
    }
    if let crate::attention::AttentionDecision::Defer { reason, scope } =
        crate::attention::attention_permits_new_run(
            &state.pool,
            &state.run_handles,
            project_id,
            now,
        )
        .await
    {
        return Err(GateRefusal {
            reason,
            stop_tick: matches!(scope, crate::attention::AttentionScope::Global),
        });
    }
    Ok(())
}

/// Fire one already-resolved configured repo trigger through the shared autonomy brakes.
pub(crate) async fn fire_configured_trigger(
    state: &crate::state::AppState,
    now: chrono::DateTime<chrono::Utc>,
    project_id: &str,
    project_root: &str,
    project_mode: crate::autopilot::Mode,
    trigger: &RepoTrigger,
    current_sha: &str,
) -> TriggerFireOutcome {
    if let Err(refusal) = governance_permits_repo_trigger(state, project_id, now).await {
        return TriggerFireOutcome::Deferred {
            reason: refusal.reason,
            stop_tick: refusal.stop_tick,
        };
    }

    let run_mode = match project_mode {
        crate::autopilot::Mode::Shadow => "shadow",
        crate::autopilot::Mode::Active => "worktree",
        crate::autopilot::Mode::Off => {
            return TriggerFireOutcome::Deferred {
                reason: "project autopilot is off".to_string(),
                stop_tick: false,
            };
        }
    };

    // Re-read the emergency stop immediately before committing because the preceding checks take
    // time and a switch thrown during them must stop this queued trigger.
    if crate::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return TriggerFireOutcome::Deferred {
            reason: "global kill switch engaged before run creation".to_string(),
            stop_tick: true,
        };
    }

    match crate::runs::create_run_inner(
        state,
        trigger.prompt.clone(),
        Some(project_id.to_string()),
        Some(project_root.to_string()),
        run_mode,
        false,
    )
    .await
    {
        Ok(run_id) => {
            if let Err(error) =
                record_sha(&state.pool, project_id, &trigger.name, current_sha).await
            {
                tracing::error!(
                    project_id,
                    trigger = %trigger.name,
                    run_id,
                    %error,
                    "repo-trigger run started but its SHA was not recorded — this trigger will fire again"
                );
            }
            TriggerFireOutcome::Fired(run_id)
        }
        Err(crate::runs::CreateRunError::Busy) => TriggerFireOutcome::Busy,
        Err(error) => TriggerFireOutcome::Failed(error.to_string()),
    }
}

/// Background loop: polls every managed project's repo triggers on a slow cadence.
pub async fn run_repo_poller(state: crate::state::AppState) {
    let mut interval = tokio::time::interval(REPO_POLL_INTERVAL);
    loop {
        interval.tick().await;
        poll_tick(&state, chrono::Utc::now()).await;
    }
}

/// One pass of the repo-event poller: for each autopilot project, fetch and read each watched branch,
/// arm first-seen triggers without firing, and fire a run for any trigger whose branch SHA changed —
/// gated by the same global/scoped kill switches and budget as the scheduler.
pub(crate) async fn poll_tick(state: &crate::state::AppState, now: chrono::DateTime<chrono::Utc>) {
    let projects = match crate::autopilot::autopilot_projects(&state.pool).await {
        Ok(projects) => projects,
        Err(error) => {
            tracing::warn!(%error, "failed to load autopilot projects for repo poll tick");
            return;
        }
    };

    for (project_id, project_root, project_mode) in projects {
        if let Err(refusal) = governance_permits_repo_trigger(state, &project_id, now).await {
            tracing::info!(
                project_id = %project_id,
                reason = %refusal.reason,
                "repo-trigger governance brake refused this project"
            );
            if refusal.stop_tick {
                return;
            }
            continue;
        }

        let triggers = match crate::config::load_schedule_rules(Path::new(&project_root)) {
            Ok(rules) => rules.repo_triggers,
            Err(error) => {
                tracing::warn!(
                    project_id = %project_id,
                    project_root = %project_root,
                    %error,
                    "failed to load repo triggers"
                );
                continue;
            }
        };
        if triggers.is_empty() {
            continue;
        }

        let last_shas = last_shas_for_project(&state.pool, &project_id)
            .await
            .unwrap_or_default();

        let mut current_shas = HashMap::new();
        for trigger in &triggers {
            if let Some(sha) =
                current_branch_sha(Path::new(&project_root), &trigger.branch, true).await
            {
                current_shas.insert(trigger.name.clone(), sha);
            }
        }

        // Arm any trigger seen for the first time (known current SHA, no recorded last SHA) — no run.
        for trigger in &triggers {
            if let Some(current) = current_shas.get(&trigger.name)
                && !last_shas.contains_key(&trigger.name)
            {
                let _ = record_sha(&state.pool, &project_id, &trigger.name, current).await;
            }
        }

        for trigger in due_repo_triggers(&triggers, &last_shas, &current_shas) {
            let Some(current) = current_shas.get(&trigger.name).cloned() else {
                continue;
            };

            match fire_configured_trigger(
                state,
                now,
                &project_id,
                &project_root,
                project_mode,
                trigger,
                &current,
            )
            .await
            {
                TriggerFireOutcome::Fired(run_id) => {
                    tracing::info!(
                        project_id = %project_id,
                        trigger = %trigger.name,
                        run_id,
                        "fired repo-trigger run"
                    );
                }
                TriggerFireOutcome::Busy => {
                    tracing::info!(
                        project_id = %project_id,
                        trigger = %trigger.name,
                        "repo trigger deferred; project busy"
                    );
                }
                TriggerFireOutcome::Deferred { reason, stop_tick } => {
                    tracing::info!(
                        project_id = %project_id,
                        trigger = %trigger.name,
                        %reason,
                        "repo-trigger governance brake refused a queued trigger"
                    );
                    if stop_tick {
                        return;
                    }
                }
                TriggerFireOutcome::Failed(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        trigger = %trigger.name,
                        %error,
                        "failed to create repo-trigger run"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(name: &str) -> RepoTrigger {
        RepoTrigger {
            name: name.to_string(),
            branch: "main".to_string(),
            prompt: "go".to_string(),
        }
    }

    #[test]
    fn unchanged_sha_is_not_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        let current = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        assert!(due_repo_triggers(&rules, &last, &current).is_empty());
    }

    #[test]
    fn changed_sha_is_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        let current = HashMap::from([("t1".to_string(), "bbb".to_string())]);
        let due = due_repo_triggers(&rules, &last, &current);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "t1");
    }

    #[test]
    fn first_sight_without_last_sha_is_not_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::new();
        let current = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        assert!(due_repo_triggers(&rules, &last, &current).is_empty());
    }

    #[test]
    fn missing_current_sha_is_not_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        let current = HashMap::new();
        assert!(due_repo_triggers(&rules, &last, &current).is_empty());
    }

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
    async fn last_shas_is_empty_for_a_fresh_project() {
        let pool = test_pool().await;
        assert!(last_shas_for_project(&pool, "p1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn record_sha_then_read_round_trips_and_upserts() {
        let pool = test_pool().await;
        record_sha(&pool, "p1", "t1", "aaa").await.unwrap();
        assert_eq!(
            last_shas_for_project(&pool, "p1").await.unwrap(),
            HashMap::from([("t1".to_string(), "aaa".to_string())])
        );

        record_sha(&pool, "p1", "t1", "bbb").await.unwrap();
        assert_eq!(
            last_shas_for_project(&pool, "p1").await.unwrap(),
            HashMap::from([("t1".to_string(), "bbb".to_string())])
        );
    }

    #[tokio::test]
    async fn shas_are_scoped_per_project() {
        let pool = test_pool().await;
        record_sha(&pool, "p1", "t1", "aaa").await.unwrap();
        record_sha(&pool, "p2", "t1", "zzz").await.unwrap();
        assert_eq!(
            last_shas_for_project(&pool, "p1").await.unwrap(),
            HashMap::from([("t1".to_string(), "aaa".to_string())])
        );
        assert_eq!(
            last_shas_for_project(&pool, "p2").await.unwrap(),
            HashMap::from([("t1".to_string(), "zzz".to_string())])
        );
    }

    fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    fn git_stdout(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn init_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("create repo tempdir");
        assert!(git_ok(repo.path(), &["init"]));
        assert!(git_ok(repo.path(), &["config", "user.email", "test@x"]));
        assert!(git_ok(repo.path(), &["config", "user.name", "test"]));
        std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
        assert!(git_ok(repo.path(), &["add", "-A"]));
        assert!(git_ok(repo.path(), &["commit", "-m", "seed"]));
        repo
    }

    #[tokio::test]
    async fn current_sha_matches_rev_parse_and_tracks_a_new_commit() {
        let repo = init_repo();
        let head = git_stdout(repo.path(), &["rev-parse", "HEAD"]);
        assert_eq!(
            current_branch_sha(repo.path(), "HEAD", false).await,
            Some(head.clone())
        );

        std::fs::write(repo.path().join("b.txt"), "b\n").unwrap();
        assert!(git_ok(repo.path(), &["add", "-A"]));
        assert!(git_ok(repo.path(), &["commit", "-m", "second"]));
        let head2 = git_stdout(repo.path(), &["rev-parse", "HEAD"]);
        assert_ne!(head, head2);
        assert_eq!(
            current_branch_sha(repo.path(), "HEAD", false).await,
            Some(head2)
        );
    }

    #[tokio::test]
    async fn unknown_ref_returns_none() {
        let repo = init_repo();
        assert_eq!(
            current_branch_sha(repo.path(), "no-such-branch", false).await,
            None
        );
    }

    #[tokio::test]
    async fn fetch_true_without_a_remote_is_best_effort() {
        let repo = init_repo();
        // fetch fails (no remote configured) but the local ref still resolves.
        assert!(
            current_branch_sha(repo.path(), "HEAD", true)
                .await
                .is_some()
        );
    }

    fn ts(value: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    async fn test_state() -> crate::state::AppState {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        crate::state::AppState {
            token: crate::auth::Token("test-token".into()),
            pool,
            runner: Arc::new(crate::runner::FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_messages: Arc::new(Mutex::new(HashMap::new())),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    async fn seed_due_repo_project(state: &crate::state::AppState, project_id: &str, repo: &Path) {
        let branch = git_stdout(repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
        std::fs::create_dir_all(repo.join(".ai")).unwrap();
        std::fs::write(
            repo.join(".ai").join("autopilot.yaml"),
            format!("repo_triggers:\n  - name: t1\n    branch: {branch}\n    prompt: \"go\"\n"),
        )
        .unwrap();

        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root)
             VALUES (?, 'shadow', ?)",
        )
        .bind(project_id)
        .bind(repo.to_string_lossy().as_ref())
        .execute(&state.pool)
        .await
        .unwrap();
        let old_head = git_stdout(repo, &["rev-parse", "HEAD"]);
        record_sha(&state.pool, project_id, "t1", &old_head)
            .await
            .unwrap();

        std::fs::write(repo.join(format!("{project_id}.txt")), "changed\n").unwrap();
        assert!(git_ok(repo, &["add", "-A"]));
        assert!(git_ok(repo, &["commit", "-m", "trigger change"]));
    }

    #[tokio::test]
    async fn a_project_heartbeat_only_stops_that_projects_repo_trigger() {
        let repo_a = init_repo();
        let repo_b = init_repo();
        let state = test_state().await;
        seed_due_repo_project(&state, "project-a", repo_a.path()).await;
        seed_due_repo_project(&state, "project-b", repo_b.path()).await;
        let now = ts("2026-07-20T10:00:00Z");
        crate::attention::record_heartbeat(
            &state.pool,
            &crate::attention::AttentionScope::Project("project-a".to_string()),
            now,
        )
        .await
        .unwrap();

        poll_tick(&state, now).await;

        let fired_projects: Vec<String> =
            sqlx::query_scalar("SELECT project_id FROM runs ORDER BY project_id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(fired_projects, ["project-b"]);
    }

    #[tokio::test]
    async fn a_global_heartbeat_stops_all_repo_triggers() {
        let repo_a = init_repo();
        let repo_b = init_repo();
        let state = test_state().await;
        seed_due_repo_project(&state, "project-a", repo_a.path()).await;
        seed_due_repo_project(&state, "project-b", repo_b.path()).await;
        let now = ts("2026-07-20T10:00:00Z");
        crate::attention::record_heartbeat(
            &state.pool,
            &crate::attention::AttentionScope::Global,
            now,
        )
        .await
        .unwrap();

        poll_tick(&state, now).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
    }

    #[tokio::test]
    async fn no_heartbeat_leaves_repo_trigger_firing_unchanged() {
        let repo = init_repo();
        let state = test_state().await;
        seed_due_repo_project(&state, "project", repo.path()).await;

        poll_tick(&state, ts("2026-07-20T10:00:00Z")).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 1);
    }

    #[tokio::test]
    async fn poll_arms_first_then_fires_on_a_new_commit() {
        let repo = init_repo();
        let branch = git_stdout(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
        std::fs::create_dir_all(repo.path().join(".ai")).unwrap();
        std::fs::write(
            repo.path().join(".ai").join("autopilot.yaml"),
            format!("repo_triggers:\n  - name: t1\n    branch: {branch}\n    prompt: \"go\"\n"),
        )
        .unwrap();

        let state = test_state().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('proj', 'shadow', ?)",
        )
        .bind(repo.path().to_string_lossy().as_ref())
        .execute(&state.pool)
        .await
        .unwrap();

        let now = ts("2026-07-20T10:00:00Z");

        // First poll: arms the trigger (records the SHA) without firing.
        poll_tick(&state, now).await;
        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
        let head1 = git_stdout(repo.path(), &["rev-parse", "HEAD"]);
        assert_eq!(
            last_shas_for_project(&state.pool, "proj")
                .await
                .unwrap()
                .get("t1"),
            Some(&head1)
        );

        // A new commit moves the branch tip.
        std::fs::write(repo.path().join("b.txt"), "b\n").unwrap();
        assert!(git_ok(repo.path(), &["add", "-A"]));
        assert!(git_ok(repo.path(), &["commit", "-m", "second"]));
        let head2 = git_stdout(repo.path(), &["rev-parse", "HEAD"]);

        // Second poll: detects the change and fires a shadow run, recording the new SHA.
        poll_tick(&state, now).await;
        let (count, mode, project): (i64, Option<String>, Option<String>) =
            sqlx::query_as("SELECT COUNT(*), MAX(mode), MAX(project_id) FROM runs")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        assert_eq!(mode.as_deref(), Some("shadow"));
        assert_eq!(project.as_deref(), Some("proj"));
        assert_eq!(
            last_shas_for_project(&state.pool, "proj")
                .await
                .unwrap()
                .get("t1"),
            Some(&head2)
        );
    }
}
