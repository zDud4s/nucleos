//! The per-account usage feed: polls the quota sidecar and fills the pool. With no sidecar the
//! pool stays empty of usage and every pick is today's launch.
//!
//! Each round pushes the pool's measurable accounts to the sidecar (only when the set changed),
//! reads `GET /quota`, and hands every per-account reading to `PoolState::set_usage`. A failed
//! round leaves the usage already in the pool alone.

use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::Notify;

use super::pool::{AccountUsage, PoolState, WindowUse, window_length};
use super::{Account, AccountId, AccountState};
use crate::quota::Fidelity;
use crate::quota::client::{AccountDir, ProviderReading, QuotaClient};

/// How often a round runs when nothing wakes the loop sooner.
const ROUND_EVERY: Duration = Duration::from_secs(60);

/// Wakes the loop at once after an account change. `notify_one` stores a permit, so a change that
/// lands while a round is running is not lost.
static WAKE: Notify = Notify::const_new();

pub fn start(_db: SqlitePool, client: Option<QuotaClient>, pool: Arc<PoolState>) {
    let Some(client) = client else {
        return;
    };
    tokio::spawn(async move {
        // `None` until the first successful push; also reset when the sidecar forgets the list.
        let mut pushed: Option<Vec<AccountDir>> = None;
        // Whether the last round failed, so an outage is logged once rather than every minute.
        let mut failing = false;
        loop {
            let outcome = round(&client, &pool, &mut pushed).await;
            match outcome {
                Ok(()) => {
                    if failing {
                        tracing::debug!("the per-account usage feed recovered");
                    }
                    failing = false;
                }
                Err(error) => {
                    if !failing {
                        tracing::warn!(%error, "the per-account usage feed failed; keeping the last usage");
                    }
                    failing = true;
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(ROUND_EVERY) => {}
                _ = WAKE.notified() => {}
            }
        }
    });
}

pub async fn on_accounts_changed(_db: &SqlitePool, _pool: &Arc<PoolState>) {
    WAKE.notify_one();
}

async fn round(
    client: &QuotaClient,
    pool: &PoolState,
    pushed: &mut Option<Vec<AccountDir>>,
) -> Result<(), crate::quota::client::QuotaError> {
    let accounts: Vec<Account> = pool
        .view()
        .candidates
        .into_iter()
        .map(|c| c.account)
        .collect();
    let wanted = accounts_to_push(&accounts);
    if pushed.as_ref() != Some(&wanted) {
        client.put_accounts(&wanted).await?;
        *pushed = Some(wanted.clone());
    }
    let report = client.report().await?;
    if report.accounts.is_empty() && !wanted.is_empty() {
        // The sidecar answered without our accounts (restarted, list lost): push again next round.
        *pushed = None;
    }
    pool.set_usage(usage_from_readings(&report.accounts));
    Ok(())
}

/// PURE: the accounts the sidecar can read — Active, not a default, with a credential directory.
pub fn accounts_to_push(accounts: &[Account]) -> Vec<AccountDir> {
    accounts
        .iter()
        .filter(|a| a.state == AccountState::Active && !a.id.is_default())
        .filter_map(|a| {
            let dir = a.credential_locator.as_deref().filter(|d| !d.is_empty())?;
            Some(AccountDir {
                id: a.id.as_str().to_owned(),
                engine: a.engine.clone(),
                dir: dir.to_owned(),
            })
        })
        .collect()
}

/// PURE: the sidecar's per-account readings as pool usage. A reading with no `account_id` cannot
/// be attributed and is skipped.
pub fn usage_from_readings(readings: &[ProviderReading]) -> Vec<AccountUsage> {
    readings
        .iter()
        .filter_map(|reading| {
            let id = reading.account_id.as_deref().filter(|id| !id.is_empty())?;
            Some(AccountUsage {
                account_id: AccountId(id.to_owned()),
                fidelity: Fidelity::parse(&reading.fidelity),
                read_at: Some(reading.read_at),
                windows: reading
                    .windows
                    .iter()
                    .map(|w| WindowUse {
                        name: w.window.clone(),
                        used_fraction: w.used_fraction,
                        resets_at: w.resets_at,
                        length: window_length(&w.window),
                        stale: w.stale,
                    })
                    .collect(),
                detail: reading.detail.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::choose::test_support::account;
    use crate::quota::client::QuotaReport;

    fn with(id: &str, state: AccountState, locator: Option<&str>) -> Account {
        let mut a = account(id);
        a.engine = "claude".into();
        a.state = state;
        a.credential_locator = locator.map(str::to_owned);
        a
    }

    #[test]
    fn only_active_non_default_accounts_with_a_dir_are_pushed() {
        let accounts = vec![
            with("claude:ok", AccountState::Active, Some("/d/ok")),
            with("claude:paused", AccountState::Paused, Some("/d/p")),
            with("claude:login", AccountState::NeedsLogin, Some("/d/l")),
            with("default:claude", AccountState::Active, None),
            with("claude:nodir", AccountState::Active, None),
        ];

        let pushed = accounts_to_push(&accounts);

        assert_eq!(
            pushed,
            vec![AccountDir {
                id: "claude:ok".into(),
                engine: "claude".into(),
                dir: "/d/ok".into(),
            }]
        );
    }

    #[test]
    fn readings_map_to_usage_and_unattributed_ones_are_skipped() {
        let report: QuotaReport = serde_json::from_str(
            r#"{"providers":[],"accounts":[
                {"account_id":"claude:a","provider":"claude","fidelity":"official",
                 "read_at":"2026-09-19T05:00:00Z","detail":"ok",
                 "windows":[{"window":"5h","used_fraction":0.4,"resets_at":"2026-09-19T08:00:00Z"},
                            {"window":"7d","used_fraction":0.1,"resets_at":null,"stale":true}]},
                {"provider":"claude","fidelity":"official","read_at":"2026-09-19T05:00:00Z"},
                {"account_id":"claude:b","provider":"claude","fidelity":"weird",
                 "read_at":"2026-09-19T05:00:00Z","windows":null}]}"#,
        )
        .unwrap();

        let usage = usage_from_readings(&report.accounts);

        assert_eq!(usage.len(), 2);
        let a = &usage[0];
        assert_eq!(a.account_id, AccountId("claude:a".into()));
        assert_eq!(a.fidelity, Fidelity::Official);
        assert_eq!(a.detail, "ok");
        assert_eq!(a.windows.len(), 2);
        assert_eq!(a.windows[0].name, "5h");
        assert_eq!(a.windows[0].used_fraction, 0.4);
        assert_eq!(a.windows[0].length, Some(chrono::Duration::hours(5)));
        assert!(a.windows[0].resets_at.is_some());
        assert_eq!(a.windows[1].length, Some(chrono::Duration::days(7)));
        assert!(a.windows[1].stale);
        assert_eq!(usage[1].account_id, AccountId("claude:b".into()));
        assert_eq!(usage[1].fidelity, Fidelity::Unmeasured);
        assert!(usage[1].windows.is_empty());
    }
}
