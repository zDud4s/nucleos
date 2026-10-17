//! Authenticated branch-movement deliveries for configured repo triggers.
//!
//! This module turns a delivery into "this configured branch moved"; `repo_trigger` remains the
//! owner of every autonomy brake and run creation. The daemon still binds this API to localhost.
//! Whether an owner makes the endpoint reachable through a tunnel or reverse proxy is their
//! deployment decision, not a reachability claim made by this code.

use serde::{Deserialize, Serialize};

use crate::repo_trigger::TriggerFireOutcome;
use crate::state::AppState;

/// A push notification is tiny: identifiers plus one SHA. This route-specific ceiling prevents the
/// daemon's only inbound trigger from inheriting axum's much larger default body allowance.
pub const WEBHOOK_BODY_LIMIT: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
pub struct Delivery {
    pub project_id: String,
    pub branch: String,
    pub sha: String,
    #[serde(default)]
    pub delivery_id: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeliveryOutcome {
    Fired { run_ids: Vec<i64> },
    Duplicate,
    Deferred { reason: String },
}

#[derive(Debug)]
pub enum DeliveryError {
    Invalid,
    Unconfigured,
    Storage(sqlx::Error),
    Config(std::io::Error),
}

fn valid_field(value: &str) -> bool {
    !value.trim().is_empty()
}

fn dedupe_key(delivery: &Delivery) -> Result<String, DeliveryError> {
    match delivery.delivery_id.as_deref() {
        Some(id) if valid_field(id) && id.len() <= 256 => Ok(format!("delivery:{id}")),
        Some(_) => Err(DeliveryError::Invalid),
        None => serde_json::to_string(&(
            "push",
            &delivery.project_id,
            &delivery.branch,
            &delivery.sha,
        ))
        .map_err(|_| DeliveryError::Invalid),
    }
}

async fn claim_delivery(
    state: &AppState,
    delivery: &Delivery,
    key: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<bool, DeliveryError> {
    // A push keyed by (project, branch, sha) only needs to dedupe against the LAST sha seen on its
    // branch. Keeping every older one made push A -> B -> A drop the second A forever.
    if key.starts_with("[\"push\"") {
        sqlx::query(
            "DELETE FROM webhook_deliveries
              WHERE project_id = ? AND branch = ? AND sha <> ? AND delivery_key LIKE '[\"push\"%'",
        )
        .bind(&delivery.project_id)
        .bind(&delivery.branch)
        .bind(&delivery.sha)
        .execute(&state.pool)
        .await
        .map_err(DeliveryError::Storage)?;
    }
    let result = sqlx::query(
        "INSERT INTO webhook_deliveries
             (delivery_key, project_id, branch, sha, received_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(delivery_key) DO NOTHING",
    )
    .bind(key)
    .bind(&delivery.project_id)
    .bind(&delivery.branch)
    .bind(&delivery.sha)
    .bind(now.to_rfc3339())
    .execute(&state.pool)
    .await
    .map_err(DeliveryError::Storage)?;
    Ok(result.rows_affected() == 1)
}

async fn release_claim(state: &AppState, key: &str) -> Result<(), DeliveryError> {
    sqlx::query("DELETE FROM webhook_deliveries WHERE delivery_key = ?")
        .bind(key)
        .execute(&state.pool)
        .await
        .map_err(DeliveryError::Storage)?;
    Ok(())
}

/// Resolve an untrusted delivery against the managed-project roster and that project's configured
/// repo triggers, then ask `repo_trigger` to start work through its shared governance path.
pub async fn deliver(
    state: &AppState,
    delivery: Delivery,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<DeliveryOutcome, DeliveryError> {
    if !valid_field(&delivery.project_id)
        || !valid_field(&delivery.branch)
        || !valid_field(&delivery.sha)
    {
        return Err(DeliveryError::Invalid);
    }

    let projects = crate::governance::autopilot::autopilot_projects(&state.pool)
        .await
        .map_err(DeliveryError::Storage)?;
    let Some((project_id, project_root, project_mode)) = projects
        .into_iter()
        .find(|(project_id, _, _)| project_id == &delivery.project_id)
    else {
        return Err(DeliveryError::Unconfigured);
    };

    let rules =
        crate::config::load_schedule_rules(state.machine_config_root.as_deref(), &project_id)
            .map_err(DeliveryError::Config)?;
    let triggers = rules
        .repo_triggers
        .into_iter()
        .filter(|trigger| trigger.branch == delivery.branch)
        .collect::<Vec<_>>();
    if triggers.is_empty() {
        return Err(DeliveryError::Unconfigured);
    }

    let key = dedupe_key(&delivery)?;
    if !claim_delivery(state, &delivery, &key, now).await? {
        return Ok(DeliveryOutcome::Duplicate);
    }

    // A trigger that already fired for this very SHA (a partial delivery retried, or the poller got
    // there first) must not start a second run.
    let last_shas = crate::repo_trigger::last_shas_for_project(&state.pool, &project_id)
        .await
        .map_err(DeliveryError::Storage)?;
    let mut run_ids = Vec::new();
    let mut deferred_reason = None;
    let mut already_fired = 0usize;
    for trigger in &triggers {
        if last_shas.get(&trigger.name).map(String::as_str) == Some(delivery.sha.as_str()) {
            already_fired += 1;
            continue;
        }
        match crate::repo_trigger::fire_configured_trigger(
            state,
            now,
            &project_id,
            &project_root,
            project_mode,
            trigger,
            &delivery.sha,
        )
        .await
        {
            TriggerFireOutcome::Fired(run_id) => run_ids.push(run_id),
            TriggerFireOutcome::Deferred { reason, .. } => {
                deferred_reason = Some(reason);
                break;
            }
            TriggerFireOutcome::Busy => {
                deferred_reason = Some("project already has work in progress".to_string());
                break;
            }
            TriggerFireOutcome::Failed(error) => {
                deferred_reason = Some(format!("run creation failed: {error}"));
                break;
            }
        }
    }

    if deferred_reason.is_some() {
        // The delivery did not complete: a trigger was deferred, busy or failed. Release the claim
        // so the sender's retry runs again; triggers that already fired are skipped above through
        // their recorded SHA, so only the unfinished part is retried.
        release_claim(state, &key).await?;
    }
    if run_ids.is_empty() {
        if deferred_reason.is_none() && already_fired > 0 {
            return Ok(DeliveryOutcome::Duplicate);
        }
        return Ok(DeliveryOutcome::Deferred {
            reason: deferred_reason.unwrap_or_else(|| "no configured trigger fired".to_string()),
        });
    }

    Ok(DeliveryOutcome::Fired { run_ids })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::execution::runner::FakeCommandRunner;
    use std::sync::Arc;

    /// A state whose `machine_config_root` is a temporary directory standing in for `~/.nucleos`,
    /// returned beside it so the directory lives exactly as long as the test that holds it.
    async fn test_state() -> (AppState, tempfile::TempDir) {
        let home = tempfile::tempdir().expect("create a stand-in home");
        let pool = crate::testdb::fresh_pool().await;
        let state = AppState {
            token: Token("test-token".into()),
            pool,
            telegram_doctrine: None,
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: Some(home.path().to_path_buf()),
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: Arc::new(crate::state::EmailRuntime::default()),
            voice: Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        };
        (state, home)
    }

    async fn configured_project(state: &AppState) -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        crate::project::state::write_for_test(
            state
                .machine_config_root
                .as_deref()
                .expect("the test state has a stand-in home"),
            "configured",
            crate::project::state::AUTOPILOT_FILE,
            "repo_triggers:\n  - name: pushed-main\n    branch: main\n    prompt: \"review the push\"\n",
        );
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root)
             VALUES ('configured', 'shadow', ?)",
        )
        .bind(project.path().to_string_lossy().as_ref())
        .execute(&state.pool)
        .await
        .unwrap();
        project
    }

    fn delivery(id: &str) -> Delivery {
        Delivery {
            project_id: "configured".to_string(),
            branch: "main".to_string(),
            sha: "0123456789abcdef".to_string(),
            delivery_id: Some(id.to_string()),
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-07-29T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    async fn run_count(state: &AppState) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn configured_project_and_branch_fire_once_and_replay_is_deduplicated() {
        let (state, _home) = test_state().await;
        let _project = configured_project(&state).await;

        assert!(matches!(
            deliver(&state, delivery("delivery-1"), now())
                .await
                .unwrap(),
            DeliveryOutcome::Fired { .. }
        ));
        assert_eq!(run_count(&state).await, 1);

        assert_eq!(
            deliver(&state, delivery("delivery-1"), now())
                .await
                .unwrap(),
            DeliveryOutcome::Duplicate
        );
        assert_eq!(run_count(&state).await, 1);
    }

    #[tokio::test]
    async fn delivery_without_sender_id_uses_project_branch_and_sha_for_deduplication() {
        let (state, _home) = test_state().await;
        let _project = configured_project(&state).await;
        let mut first = delivery("unused");
        first.delivery_id = None;
        let mut replay = delivery("also-unused");
        replay.delivery_id = None;

        assert!(matches!(
            deliver(&state, first, now()).await.unwrap(),
            DeliveryOutcome::Fired { .. }
        ));
        assert_eq!(
            deliver(&state, replay, now()).await.unwrap(),
            DeliveryOutcome::Duplicate
        );
        assert_eq!(run_count(&state).await, 1);
    }

    #[tokio::test]
    async fn a_sha_seen_earlier_on_the_branch_fires_again_after_a_different_one() {
        let (state, _home) = test_state().await;
        let push = |sha: &str| Delivery {
            project_id: "configured".to_string(),
            branch: "main".to_string(),
            sha: sha.to_string(),
            delivery_id: None,
        };
        let claim = |d: Delivery| {
            let state = state.clone();
            async move {
                let key = dedupe_key(&d).unwrap();
                claim_delivery(&state, &d, &key, now()).await.unwrap()
            }
        };

        assert!(claim(push("aaa")).await);
        assert!(
            !claim(push("aaa")).await,
            "an immediate replay is a duplicate"
        );
        assert!(claim(push("bbb")).await);
        assert!(
            claim(push("aaa")).await,
            "A after B is a new push, not a replay"
        );
    }

    #[tokio::test]
    async fn a_deferred_delivery_is_retried_on_the_next_pass() {
        let (state, _home) = test_state().await;
        let _project = configured_project(&state).await;
        crate::governance::autopilot::set_kill_switch(&state.pool, true)
            .await
            .unwrap();
        assert!(matches!(
            deliver(&state, delivery("retry"), now()).await.unwrap(),
            DeliveryOutcome::Deferred { .. }
        ));
        crate::governance::autopilot::set_kill_switch(&state.pool, false)
            .await
            .unwrap();
        assert!(matches!(
            deliver(&state, delivery("retry"), now()).await.unwrap(),
            DeliveryOutcome::Fired { .. }
        ));
        assert_eq!(run_count(&state).await, 1);
    }

    #[tokio::test]
    async fn a_partial_fire_releases_the_claim_and_only_the_unfinished_trigger_retries() {
        let (state, _home) = test_state().await;
        let project = configured_project(&state).await;
        crate::project::state::write_for_test(
            state.machine_config_root.as_deref().unwrap(),
            "configured",
            crate::project::state::AUTOPILOT_FILE,
            "repo_triggers:
  - name: first
    branch: main
    prompt: \"one\"
  - name: second
    branch: main
    prompt: \"two\"
",
        );
        drop(project);
        let _project = tempfile::tempdir().unwrap();
        sqlx::query("UPDATE autopilot_state SET project_root = ? WHERE project_id = 'configured'")
            .bind(_project.path().to_string_lossy().as_ref())
            .execute(&state.pool)
            .await
            .unwrap();

        // The first trigger fires and finishes its run; the second then finds the project busy.
        let first = deliver(&state, delivery("partial"), now()).await.unwrap();
        let DeliveryOutcome::Fired { run_ids } = first else {
            panic!("the first trigger should fire, got {first:?}");
        };
        assert_eq!(run_ids.len(), 1);
        assert_eq!(run_count(&state).await, 1);

        // The claim was released, so the retry is not a Duplicate and does not re-fire `first`.
        let retry = deliver(&state, delivery("partial"), now()).await.unwrap();
        assert_ne!(
            retry,
            DeliveryOutcome::Duplicate,
            "partial fire kept its claim"
        );
        assert_eq!(
            run_count(&state).await,
            1,
            "the finished trigger fired again"
        );
    }

    #[tokio::test]
    async fn unconfigured_project_or_branch_is_refused() {
        let (state, _home) = test_state().await;
        let _project = configured_project(&state).await;

        let mut unknown_project = delivery("project");
        unknown_project.project_id = "unknown".to_string();
        assert!(matches!(
            deliver(&state, unknown_project, now()).await,
            Err(DeliveryError::Unconfigured)
        ));

        let mut unknown_branch = delivery("branch");
        unknown_branch.branch = "not-configured".to_string();
        assert!(matches!(
            deliver(&state, unknown_branch, now()).await,
            Err(DeliveryError::Unconfigured)
        ));
        assert_eq!(run_count(&state).await, 0);
    }

    #[tokio::test]
    async fn kill_switch_stops_a_delivery_before_it_fires() {
        let (state, _home) = test_state().await;
        let _project = configured_project(&state).await;
        crate::governance::autopilot::set_kill_switch(&state.pool, true)
            .await
            .unwrap();

        assert!(matches!(
            deliver(&state, delivery("killed"), now()).await.unwrap(),
            DeliveryOutcome::Deferred { .. }
        ));
        assert_eq!(run_count(&state).await, 0);
    }

    #[tokio::test]
    async fn attention_brake_stops_a_delivery_before_it_fires() {
        let (state, _home) = test_state().await;
        let _project = configured_project(&state).await;
        crate::attention::record_heartbeat(
            &state.pool,
            &crate::attention::AttentionScope::Project("configured".to_string()),
            now(),
        )
        .await
        .unwrap();

        assert!(matches!(
            deliver(&state, delivery("owner-present"), now())
                .await
                .unwrap(),
            DeliveryOutcome::Deferred { .. }
        ));
        assert_eq!(run_count(&state).await, 0);
    }
}
