//! The routing restriction of a run launch. The pick itself lives in `launch_chat`.

use std::collections::BTreeSet;

use super::{Account, AccountChoice};

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
    use crate::accounts::AccountId;
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
