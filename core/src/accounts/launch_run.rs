//! The account hook of a run launch. W0 returns today's launch; the run-wiring branch decides.

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
pub fn restrict_for_choice(_choice: &AccountChoice, _accounts: &[Account]) -> Option<Vec<String>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_choice_restricts_routing_in_the_skeleton() {
        assert_eq!(restrict_for_choice(&AccountChoice::Auto, &[]), None);
        assert_eq!(
            restrict_for_choice(&AccountChoice::Provider("anthropic".into()), &[]),
            None
        );
    }
}
