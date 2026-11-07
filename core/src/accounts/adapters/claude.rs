//! Claude Code adapter. A stub in W0: the account env and credential path are real, login and
//! failure parsing are filled in by the adapters branch.

use std::path::PathBuf;

use crate::accounts::Account;
use crate::accounts::provider::{
    CacheModel, LimitHit, LoginCommand, LoginEvent, ProviderAdapter, SessionPortability,
};

pub const ENGINE: &str = "claude";

pub struct ClaudeAdapter;

impl ProviderAdapter for ClaudeAdapter {
    fn engine(&self) -> &'static str {
        ENGINE
    }
    fn provider(&self) -> &'static str {
        "anthropic"
    }
    fn account_env(&self, account: &Account) -> Vec<(String, String)> {
        match &account.credential_locator {
            Some(dir) => vec![("CLAUDE_CONFIG_DIR".to_owned(), dir.clone())],
            None => Vec::new(),
        }
    }
    fn login_command(&self, _account: &Account) -> Option<LoginCommand> {
        None
    }
    fn parse_login_line(&self, _line: &str) -> Option<LoginEvent> {
        None
    }
    fn credential_paths(&self, account: &Account) -> Vec<PathBuf> {
        match &account.credential_locator {
            Some(dir) => vec![PathBuf::from(dir).join(".credentials.json")],
            None => Vec::new(),
        }
    }
    fn usage_source(&self) -> crate::quota::Fidelity {
        crate::quota::Fidelity::Unmeasured
    }
    fn session_portability(&self) -> SessionPortability {
        SessionPortability::NotPortable
    }
    fn cache_model(&self) -> Option<CacheModel> {
        None
    }
    fn classify_failure(&self, _stdout: &str, _stderr: &str) -> Option<LimitHit> {
        None
    }
}
