//! Claude Code adapter. Session portability stays `NotPortable` (spec §9: spike S1 executed
//! nothing, so the shared-store mechanism below is documented but not the active verdict).

use std::path::{Path, PathBuf};

use crate::accounts::Account;
use crate::accounts::provider::{
    CacheModel, LimitHit, LoginCommand, LoginEvent, ProviderAdapter, SessionPortability,
    parse_cli_login_line,
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
    /// INFERRED (spec B8, spike S1; not executed): `claude auth login` prints an authorization
    /// URL and may ask for a pasted code.
    fn login_command(&self, account: &Account) -> Option<LoginCommand> {
        Some(LoginCommand {
            program: ENGINE.to_owned(),
            args: vec!["auth".to_owned(), "login".to_owned()],
            env: self.account_env(account),
            needs_tty: false,
        })
    }
    fn parse_login_line(&self, line: &str) -> Option<LoginEvent> {
        parse_cli_login_line(line)
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
    /// S1 mechanism, documented and unused: link `<account dir>/projects` to
    /// `<store>/claude/projects`. `.credentials.json`, `.claude.json` and settings are never linked.
    fn session_store_link(&self, account: &Account, store: &Path) -> Option<(PathBuf, PathBuf)> {
        let dir = account.credential_locator.as_ref()?;
        Some((
            PathBuf::from(dir).join("projects"),
            store.join(ENGINE).join("projects"),
        ))
    }
    fn cache_model(&self) -> Option<CacheModel> {
        None
    }
    fn classify_failure(&self, _stdout: &str, _stderr: &str) -> Option<LimitHit> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountId, AccountKind, AccountState};

    fn account(locator: Option<&str>) -> Account {
        Account {
            id: AccountId("claude:00000001".into()),
            engine: ENGINE.into(),
            provider: "anthropic".into(),
            label: "a".into(),
            kind: AccountKind::Subscription,
            credential_locator: locator.map(str::to_owned),
            state: AccountState::Active,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn env_and_credential_follow_the_locator() {
        let a = ClaudeAdapter;
        assert!(a.account_env(&account(None)).is_empty());
        assert!(a.credential_paths(&account(None)).is_empty());
        assert_eq!(
            a.account_env(&account(Some("/d"))),
            vec![("CLAUDE_CONFIG_DIR".to_owned(), "/d".to_owned())]
        );
        assert_eq!(
            a.credential_paths(&account(Some("/d"))),
            vec![PathBuf::from("/d").join(".credentials.json")]
        );
    }

    #[test]
    fn login_command_is_auth_login() {
        let cmd = ClaudeAdapter.login_command(&account(Some("/d"))).unwrap();
        assert_eq!(cmd.program, "claude");
        assert_eq!(cmd.args, vec!["auth", "login"]);
        assert_eq!(
            cmd.env,
            vec![("CLAUDE_CONFIG_DIR".to_owned(), "/d".to_owned())]
        );
        assert!(!cmd.needs_tty);
    }

    #[test]
    fn stays_not_portable_and_link_avoids_credentials() {
        let a = ClaudeAdapter;
        assert_eq!(a.session_portability(), SessionPortability::NotPortable);
        let (link, target) = a
            .session_store_link(&account(Some("/d")), Path::new("/s"))
            .unwrap();
        assert_eq!(link, PathBuf::from("/d").join("projects"));
        assert_eq!(target, PathBuf::from("/s").join("claude").join("projects"));
        assert!(
            a.credential_paths(&account(Some("/d")))
                .iter()
                .all(|c| !c.starts_with(&link))
        );
        assert!(a.session_store_link(&account(None), Path::new("/s")).is_none());
    }
}
