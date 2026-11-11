//! What the pool needs to know about a provider, as a trait. Everything provider-specific (env
//! var names, credential files, login output, cache pricing) lives behind it, in `adapters/`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::Account;

/// Whether a session started under one account can continue under another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPortability {
    Portable,
    SharedStore,
    NotPortable,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CacheModel {
    /// Provider default; a TTL the stream reports wins.
    pub ttl: Duration,
    /// Cache write price / plain input price.
    pub write_price_ratio: f64,
    /// Cache read price / plain input price.
    pub read_price_ratio: f64,
}

/// Used when neither the stream nor the adapter knows the cache TTL.
pub const CONSERVATIVE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq)]
pub struct LimitHit {
    pub window: Option<String>,
    pub resets_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoginCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub needs_tty: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoginEvent {
    AuthUrl(String),
    CodePrompt,
    /// A one-time code the owner types into the browser (device-code login); never pasted back.
    DeviceCode(String),
    Succeeded,
    Failed(String),
}

pub trait ProviderAdapter: Send + Sync {
    fn engine(&self) -> &'static str;
    fn provider(&self) -> &'static str;
    /// Env that makes the process use `account`. MUST be empty when `credential_locator` is None,
    /// so one account per provider launches exactly as before the pool.
    fn account_env(&self, account: &Account) -> Vec<(String, String)>;
    /// The guided-login command, run with `account_env`. None = login unsupported.
    fn login_command(&self, account: &Account) -> Option<LoginCommand>;
    /// One line of the login process' stdout/stderr, read as an event.
    fn parse_login_line(&self, line: &str) -> Option<LoginEvent>;
    /// Files whose presence means "logged in". Existence only: core never reads a credential.
    fn credential_paths(&self, account: &Account) -> Vec<PathBuf>;
    /// What the quota sidecar can measure for this engine.
    fn usage_source(&self) -> crate::quota::Fidelity;
    /// `NotPortable` until a spike says otherwise.
    fn session_portability(&self) -> SessionPortability;
    /// Session files to copy or share for a mid-conversation switch. Default: none.
    fn session_paths(&self, _account: &Account, _session_id: &str) -> Vec<PathBuf> {
        Vec::new()
    }
    /// `(link, target)` for the shared-store mechanism of spike S1: `link` (inside the account's
    /// dir) would point at `target` (inside `store`). Documented and unused: no caller creates
    /// the link while `session_portability` is `NotPortable`. Never covers credential or
    /// settings files. Default: none.
    fn session_store_link(&self, _account: &Account, _store: &Path) -> Option<(PathBuf, PathBuf)> {
        None
    }
    /// None = unknown, treated as a high switch cost.
    fn cache_model(&self) -> Option<CacheModel>;
    fn classify_failure(&self, stdout: &str, stderr: &str) -> Option<LimitHit>;
}

/// Line classifier shared by the CLI adapters. INFERRED from the CLIs' behaviour (spike S1,
/// nothing executed): an `https://` URL is the authorization URL, a "paste ... code" line is a
/// code prompt, and "logged in" / "error" / "failed" end the login.
pub fn parse_cli_login_line(line: &str) -> Option<LoginEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(start) = line.find("https://") {
        let url = line[start..].split_whitespace().next().unwrap_or_default();
        // Prose around the URL ("visit https://x/y.", "(https://x/y)") is not part of it.
        let url = url.trim_end_matches(['.', ',', ')', ']', '>', '"', '\'']);
        return Some(LoginEvent::AuthUrl(url.to_owned()));
    }
    if is_device_code(line) {
        return Some(LoginEvent::DeviceCode(line.to_owned()));
    }
    let lower = line.to_ascii_lowercase();
    if lower.contains("paste") && lower.contains("code") {
        return Some(LoginEvent::CodePrompt);
    }
    if lower.contains("logged in") || lower.contains("login successful") {
        return Some(LoginEvent::Succeeded);
    }
    if lower.starts_with("error") || lower.contains("login failed") {
        return Some(LoginEvent::Failed(line.to_owned()));
    }
    None
}

/// A line that is only a device code such as `ABCD-EFGH` (INFERRED from `codex login
/// --device-auth`, which prints the code alone on its line): two groups of 4 to 8 upper-case
/// letters or digits joined by one dash.
fn is_device_code(line: &str) -> bool {
    let group = |g: &str| {
        (4..=8).contains(&g.len())
            && g.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    };
    line.split_once('-')
        .is_some_and(|(left, right)| group(left) && group(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_that_is_only_a_device_code_is_surfaced() {
        assert_eq!(
            parse_cli_login_line("  ABCD-EFGH  "),
            Some(LoginEvent::DeviceCode("ABCD-EFGH".into()))
        );
        assert_eq!(
            parse_cli_login_line("K7Q2-93XZ4"),
            Some(LoginEvent::DeviceCode("K7Q2-93XZ4".into()))
        );
        // Prose, short groups and three groups are not codes.
        assert_eq!(parse_cli_login_line("one-time code"), None);
        assert_eq!(
            parse_cli_login_line("Enter this one-time code ABCD-EFGH"),
            None
        );
        assert_eq!(parse_cli_login_line("ABC-DEFG"), None);
        assert_eq!(parse_cli_login_line("ABCD-EFGH-IJKL"), None);
    }

    #[test]
    fn sentence_punctuation_after_the_url_is_not_part_of_it() {
        for line in [
            "Visit https://auth.example/x?y=1.",
            "Open (https://auth.example/x?y=1)",
            "Browser did not open, use \"https://auth.example/x?y=1\"",
        ] {
            assert_eq!(
                parse_cli_login_line(line),
                Some(LoginEvent::AuthUrl("https://auth.example/x?y=1".into())),
                "{line}"
            );
        }
    }

    #[test]
    fn classifies_login_lines() {
        assert_eq!(
            parse_cli_login_line("Open https://auth.example/x?y=1 to sign in"),
            Some(LoginEvent::AuthUrl("https://auth.example/x?y=1".into()))
        );
        assert_eq!(
            parse_cli_login_line("Paste code here if prompted >"),
            Some(LoginEvent::CodePrompt)
        );
        assert_eq!(
            parse_cli_login_line("Successfully logged in"),
            Some(LoginEvent::Succeeded)
        );
        assert!(matches!(
            parse_cli_login_line("Error: denied"),
            Some(LoginEvent::Failed(_))
        ));
        assert_eq!(parse_cli_login_line("   "), None);
        assert_eq!(parse_cli_login_line("hello"), None);
    }
}
