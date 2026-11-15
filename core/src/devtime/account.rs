//! Which account a transcript belongs to: the transcript path is matched against the Claude config
//! dir of each account (`accounts.credential_locator`; NULL = the CLI's default dir).
//! Pure: no I/O, no database. Only directory paths are compared, never credentials.

use crate::accounts::Account;
use std::path::{Component, Path};

/// Path components, lower-cased on Windows (case-insensitive) and with `\` and `/` treated alike.
fn parts(path: &str) -> Vec<String> {
    Path::new(&path.replace('\\', "/"))
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| {
            let text = c.as_os_str().to_string_lossy().into_owned();
            if cfg!(windows) { text.to_lowercase() } else { text }
        })
        .collect()
}

fn is_under(path: &[String], dir: &[String]) -> bool {
    !dir.is_empty() && path.len() > dir.len() && path[..dir.len()] == dir[..]
}

/// The id of the Claude account whose config dir holds `transcript`, or `None` when no account's
/// dir contains it. `default_dir` is the CLI's own config dir (`~/.claude`), which an account with
/// a NULL locator stands for. When dirs nest, the longest match wins.
pub fn resolve_account(
    transcript: &str,
    default_dir: &str,
    accounts: &[Account],
) -> Option<String> {
    let path = parts(transcript);
    let mut best: Option<(usize, &str)> = None;
    for account in accounts.iter().filter(|a| a.engine == "claude") {
        let dir = match account.credential_locator.as_deref() {
            Some(locator) if !locator.trim().is_empty() => parts(locator),
            _ => parts(default_dir),
        };
        if is_under(&path, &dir) && best.is_none_or(|(len, _)| dir.len() > len) {
            best = Some((dir.len(), account.id.as_str()));
        }
    }
    best.map(|(_, id)| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountId, AccountKind, AccountState};

    fn account(id: &str, locator: Option<&str>) -> Account {
        Account {
            id: AccountId(id.to_owned()),
            engine: "claude".into(),
            provider: "anthropic".into(),
            label: id.into(),
            kind: AccountKind::Subscription,
            credential_locator: locator.map(str::to_owned),
            state: AccountState::Active,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn pool() -> Vec<Account> {
        vec![
            account("default:claude", None),
            account("claude:ab12cd34", Some("/data/acc/work")),
        ]
    }

    #[test]
    fn default_dir_maps_to_the_default_account() {
        let got = resolve_account("/home/u/.claude/projects/p/s.jsonl", "/home/u/.claude", &pool());
        assert_eq!(got.as_deref(), Some("default:claude"));
    }

    #[test]
    fn custom_dir_maps_to_its_account() {
        let got = resolve_account(
            "/data/acc/work/projects/p/s.jsonl",
            "/home/u/.claude",
            &pool(),
        );
        assert_eq!(got.as_deref(), Some("claude:ab12cd34"));
    }

    #[test]
    fn unknown_path_is_none() {
        let got = resolve_account("/elsewhere/projects/p/s.jsonl", "/home/u/.claude", &pool());
        assert_eq!(got, None);
        // A sibling dir sharing a name prefix is not inside the account dir.
        let got = resolve_account("/data/acc/work2/projects/s.jsonl", "/home/u/.claude", &pool());
        assert_eq!(got, None);
    }
}
