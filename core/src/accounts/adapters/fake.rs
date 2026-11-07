//! A scripted adapter for the decision, pool and login tests. Windows are named `1h` and `30d`,
//! so no test depends on a real provider's window names.

use std::path::PathBuf;

use crate::accounts::Account;
use crate::accounts::provider::{
    CacheModel, LimitHit, LoginCommand, LoginEvent, ProviderAdapter, SessionPortability,
};

pub struct FakeAdapter {
    pub engine: &'static str,
    pub provider: &'static str,
    pub portability: SessionPortability,
    pub cache: Option<CacheModel>,
}

impl FakeAdapter {
    pub fn new(engine: &'static str, provider: &'static str) -> Self {
        Self {
            engine,
            provider,
            portability: SessionPortability::NotPortable,
            cache: None,
        }
    }
}

impl ProviderAdapter for FakeAdapter {
    fn engine(&self) -> &'static str {
        self.engine
    }
    fn provider(&self) -> &'static str {
        self.provider
    }
    fn account_env(&self, account: &Account) -> Vec<(String, String)> {
        match &account.credential_locator {
            Some(dir) => vec![("FAKE_HOME".to_owned(), dir.clone())],
            None => Vec::new(),
        }
    }
    fn login_command(&self, account: &Account) -> Option<LoginCommand> {
        Some(LoginCommand {
            program: "fake-login".to_owned(),
            args: Vec::new(),
            env: self.account_env(account),
            needs_tty: false,
        })
    }
    /// The scripted login lines: `URL <url>`, `CODE?`, `OK`, `FAIL <why>`.
    fn parse_login_line(&self, line: &str) -> Option<LoginEvent> {
        let line = line.trim();
        if let Some(url) = line.strip_prefix("URL ") {
            return Some(LoginEvent::AuthUrl(url.to_owned()));
        }
        if line == "CODE?" {
            return Some(LoginEvent::CodePrompt);
        }
        if line == "OK" {
            return Some(LoginEvent::Succeeded);
        }
        line.strip_prefix("FAIL ")
            .map(|why| LoginEvent::Failed(why.to_owned()))
    }
    fn credential_paths(&self, account: &Account) -> Vec<PathBuf> {
        match &account.credential_locator {
            Some(dir) => vec![PathBuf::from(dir).join("fake-auth.json")],
            None => Vec::new(),
        }
    }
    fn usage_source(&self) -> crate::quota::Fidelity {
        crate::quota::Fidelity::Official
    }
    fn session_portability(&self) -> SessionPortability {
        self.portability
    }
    fn cache_model(&self) -> Option<CacheModel> {
        self.cache
    }
    /// Matches `FAKE_LIMIT <window>` on either stream.
    fn classify_failure(&self, stdout: &str, stderr: &str) -> Option<LimitHit> {
        stdout.lines().chain(stderr.lines()).find_map(|line| {
            let window = line.trim().strip_prefix("FAKE_LIMIT ")?;
            Some(LimitHit {
                window: Some(window.trim().to_owned()),
                resets_at: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountId, AccountKind, AccountState};

    fn account(locator: Option<&str>) -> Account {
        Account {
            id: AccountId("fake:00000001".into()),
            engine: "fake".into(),
            provider: "fakeco".into(),
            label: "a".into(),
            kind: AccountKind::Subscription,
            credential_locator: locator.map(str::to_owned),
            state: AccountState::Active,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn env_is_empty_without_a_locator() {
        let fake = FakeAdapter::new("fake", "fakeco");
        assert!(fake.account_env(&account(None)).is_empty());
        assert_eq!(
            fake.account_env(&account(Some("/x"))),
            vec![("FAKE_HOME".to_owned(), "/x".to_owned())]
        );
    }

    #[test]
    fn scripted_login_and_failure_lines() {
        let fake = FakeAdapter::new("fake", "fakeco");
        assert_eq!(
            fake.parse_login_line("URL http://x"),
            Some(LoginEvent::AuthUrl("http://x".into()))
        );
        assert_eq!(fake.parse_login_line("CODE?"), Some(LoginEvent::CodePrompt));
        assert_eq!(fake.parse_login_line("OK"), Some(LoginEvent::Succeeded));
        assert_eq!(
            fake.parse_login_line("FAIL nope"),
            Some(LoginEvent::Failed("nope".into()))
        );
        assert_eq!(fake.parse_login_line("noise"), None);
        let hit = fake.classify_failure("", "x\nFAKE_LIMIT 1h\n").unwrap();
        assert_eq!(hit.window.as_deref(), Some("1h"));
        assert!(fake.classify_failure("fine", "").is_none());
    }
}
