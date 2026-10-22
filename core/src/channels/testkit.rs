//! Test helpers: a real AppState over a fresh pool (test-only).

use std::sync::Arc;

use crate::auth::Token;
use crate::execution::runner::FakeCommandRunner;
use crate::state::AppState;

/// An `AppState` over a fresh migrated pool, with the inert channel hub and fake collaborators.
pub async fn state() -> AppState {
    let pool = crate::testdb::fresh_pool().await;
    AppState {
        startup_config: Default::default(),
        token: Token("test-token".to_string()),
        pool,
        channels: Default::default(),
        runner: Arc::new(FakeCommandRunner::default()),
        triage_runner: None,
        local_triage_disabled: None,
        assistants: Arc::new(crate::assistants::NoAssistants),
        run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        run_tails: Default::default(),
        fleet: Default::default(),
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

/// A state whose `telegram` has a routing row and said hello with the given capabilities.
pub async fn hello_state(typing: bool, edit: bool) -> AppState {
    let state = state().await;
    let mut conn = state.pool.acquire().await.unwrap();
    crate::channels::routing::seed(&mut conn, "telegram")
        .await
        .unwrap();
    drop(conn);
    let hello: crate::channels::contract::Hello = serde_json::from_value(serde_json::json!({
        "kind": crate::channels::registry::by_id("telegram").unwrap().kind,
        "version": "1.0.0",
        "capabilities": { "max_text": 4096, "typing": typing, "edit": edit },
    }))
    .unwrap();
    crate::channels::registry::record_hello(&state, "telegram", &hello, chrono::Utc::now())
        .await
        .unwrap();
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn state_carries_an_inert_hub() {
        assert!(state().await.channels.config("telegram").is_none());
    }
}
