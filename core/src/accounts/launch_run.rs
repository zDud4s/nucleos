//! The account hook of a run launch. W0 returns today's launch; the run-wiring branch decides.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use super::pool::PoolState;
use super::{Account, AccountChoice, AccountId, AccountPick};

/// Reads the run's `account_choice` (NULL -> the settings default) and picks an account.
pub async fn pick_for_run(
    _db: &SqlitePool,
    _pool: &Arc<PoolState>,
    _run_id: i64,
    _engine: &str,
    _resume_session_id: Option<&str>,
    _now: DateTime<Utc>,
) -> AccountPick {
    AccountPick::default_launch()
}

/// Classifies the run's output after it ends: marks a window exhausted when the provider said so.
#[allow(clippy::too_many_arguments)]
pub async fn after_run(
    _db: &SqlitePool,
    _pool: &Arc<PoolState>,
    _run_id: i64,
    _account: &AccountId,
    _engine: &str,
    _stdout: &str,
    _stderr: &str,
    _now: DateTime<Utc>,
) {
}

/// The routing restriction a run's choice implies (a provider or account pin -> that provider's
/// engines). None = no restriction.
///
/// Engines come back sorted and de-duplicated. A choice that names no known provider or account
/// restricts nothing (never an empty list, which would park the run): launching then fails or
/// falls back where the choice is resolved, not here.
pub fn restrict_for_choice(choice: &AccountChoice, accounts: &[Account]) -> Option<Vec<String>> {
    let engines: BTreeSet<String> = match choice {
        AccountChoice::Auto => return None,
        AccountChoice::Provider(provider) => accounts
            .iter()
            .filter(|a| &a.provider == provider)
            .map(|a| a.engine.clone())
            .collect(),
        AccountChoice::Account(id) => accounts
            .iter()
            .filter(|a| &a.id == id)
            .map(|a| a.engine.clone())
            .collect(),
    };
    (!engines.is_empty()).then(|| engines.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::choose::test_support::account;

    fn on(id: &str, engine: &str, provider: &str) -> Account {
        Account {
            engine: engine.into(),
            provider: provider.into(),
            ..account(id)
        }
    }

    #[test]
    fn auto_and_an_empty_pool_restrict_nothing() {
        assert_eq!(restrict_for_choice(&AccountChoice::Auto, &[]), None);
        let accounts = [on("a", "claude", "anthropic")];
        assert_eq!(restrict_for_choice(&AccountChoice::Auto, &accounts), None);
        assert_eq!(
            restrict_for_choice(&AccountChoice::Provider("anthropic".into()), &[]),
            None
        );
    }

    #[test]
    fn a_provider_pin_restricts_to_its_engines_sorted_and_unique() {
        let accounts = [
            on("a", "claude", "anthropic"),
            on("b", "claude", "anthropic"),
            on("c", "codex", "openai"),
        ];
        assert_eq!(
            restrict_for_choice(&AccountChoice::Provider("anthropic".into()), &accounts),
            Some(vec!["claude".to_owned()])
        );
    }

    #[test]
    fn an_account_pin_restricts_to_that_accounts_engine() {
        let accounts = [on("a", "claude", "anthropic"), on("c", "codex", "openai")];
        assert_eq!(
            restrict_for_choice(&AccountChoice::Account(AccountId("c".into())), &accounts),
            Some(vec!["codex".to_owned()])
        );
    }

    #[test]
    fn an_unknown_pin_restricts_nothing_instead_of_emptying_the_list() {
        let accounts = [on("a", "claude", "anthropic")];
        assert_eq!(
            restrict_for_choice(&AccountChoice::Provider("nobody".into()), &accounts),
            None
        );
        assert_eq!(
            restrict_for_choice(&AccountChoice::Account(AccountId("zz".into())), &accounts),
            None
        );
    }
}
