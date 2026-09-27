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
pub(crate) const WEBHOOK_BODY_LIMIT: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
pub(crate) struct Delivery {
    pub project_id: String,
    pub branch: String,
    pub sha: String,
    #[serde(default)]
    pub delivery_id: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum DeliveryOutcome {
    Fired { run_ids: Vec<i64> },
    Duplicate,
    Deferred { reason: String },
}

#[derive(Debug)]
pub(crate) enum DeliveryError {
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
pub(crate) async fn deliver(
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

    let projects = crate::autopilot::autopilot_projects(&state.pool)
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

    let mut run_ids = Vec::new();
    let mut deferred_reason = None;
    for trigger in &triggers {
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

    if run_ids.is_empty() {
        // No autonomous work was created, so a sender retry is not a duplicate run attempt. Release
        // the claim and let the same delivery try again after the fail-closed brake clears.
        release_claim(state, &key).await?;
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
    use crate::runner::FakeCommandRunner;
    use std::sync::Arc;

    /// A state whose `machine_config_root` is a temporary directory standing in for `~/.nucleos`,
    /// returned beside it so the directory lives exactly as long as the test that holds it.
    async fn test_state() -> (AppState, tempfile::TempDir) {
        let home = tempfile::tempdir().expect("create a stand-in home");
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
            workflow_library: None,
            machine_config_root: Some(home.path().to_path_buf()),
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: Arc::new(crate::state::EmailRuntime::default()),
            voice: Arc::new(crate::voice::VoiceRuntime::default()),
            browser: Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: Arc::new(crate::github::GithubRuntime::default()),
            web: Arc::new(crate::web::WebRuntime::disabled()),
            quota: Arc::new(crate::quota::QuotaRuntime::disabled()),
            calendar: Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        };
        (state, home)
    }

    async fn configured_project(state: &AppState) -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        crate::project_state::write_for_test(
            state
                .machine_config_root
                .as_deref()
                .expect("the test state has a stand-in home"),
            "configured",
            crate::project_state::AUTOPILOT_FILE,
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
        crate::autopilot::set_kill_switch(&state.pool, true)
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
