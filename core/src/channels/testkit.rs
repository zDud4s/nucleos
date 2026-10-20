//! Test helpers: a real AppState over a fresh pool (test-only).

use std::sync::Arc;

use crate::auth::Token;
use crate::execution::runner::FakeCommandRunner;
use crate::state::AppState;

/// An `AppState` over a fresh migrated pool, with the inert channel hub and fake collaborators.
pub(crate) async fn state() -> AppState {
    let pool = crate::testdb::fresh_pool().await;
    AppState {
        startup_config: Default::default(),
        token: Token("test-token".to_string()),
        pool,
        telegram_doctrine: None,
        channels: Default::default(),
        runner: Arc::new(FakeCommandRunner::default()),
        triage_runner: None,
        local_triage_disabled: None,
        assistants: Arc::new(crate::assistants::NoAssistants),
        run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        run_tails: Default::default(),
        utility_model: None,
        files_root: None,
        files_trash: None,
        workflow_library: None,
        machine_config_root: None,
        secrets: Arc::new(crate::secrets::InMemorySecrets::default()),
        email: Arc::new(crate::state::EmailRuntime::default()),
        voice: Arc::new(crate::integrations::voice::VoiceRuntime::default()),
        browser: Arc::new(crate::browser::BrowserRuntime::disabled()),
        github: Arc::new(crate::integrations::github::GithubRuntime::default()),
        web: Arc::new(crate::integrations::web::WebRuntime::disabled()),
        quota: Arc::new(crate::quota::QuotaRuntime::disabled()),
        judge: Arc::new(crate::judge::JudgeRuntime::disabled()),
        calendar: Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
        council: Arc::new(crate::council::CouncilRuntime::default()),
        progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
        run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn state_carries_an_inert_hub() {
        assert!(state().await.channels.config("telegram").is_none());
    }
}
