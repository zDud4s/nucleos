//! The auth watch per account (spec sections 6 and 11): a subscription account whose credential
//! file went missing is flagged `needs_login`, leaves the pool, and the owner is told through the
//! feed. Existence only: core never reads a credential. An account on the CLI's own directory
//! (no `credential_locator`) is not watched here; the run-time auth watch covers it.

use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;

use super::adapters::adapter_for;
use super::login::Lookup;
use super::pool::PoolState;
use super::{Account, AccountId, AccountKind, AccountState, NEEDS_LOGIN_KIND, startup, store};

const SWEEP_EVERY: Duration = Duration::from_secs(30);

pub fn start(db: SqlitePool, _pool: Arc<PoolState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SWEEP_EVERY).await;
            if sweep(&db, adapter_for).await > 0 {
                startup::accounts_changed(&db).await;
            }
        }
    });
}

/// The sweep reads the store on every tick, so a change needs no push: nothing to do here (and
/// calling `sweep` from this hook would loop through `accounts_changed`).
pub async fn on_accounts_changed(_db: &SqlitePool, _pool: &Arc<PoolState>) {}

/// Flags every active subscription account whose credential files are all gone. Returns how many
/// it flagged. The caller fans the change out.
pub async fn sweep(db: &SqlitePool, lookup: Lookup) -> usize {
    let accounts = match store::list(db).await {
        Ok(accounts) => accounts,
        Err(error) => {
            tracing::warn!(%error, "account auth watch could not list the accounts");
            return 0;
        }
    };
    let mut flagged = 0;
    for account in accounts {
        if account.state != AccountState::Active
            || account.kind != AccountKind::Subscription
            || account.credential_locator.is_none()
        {
            continue;
        }
        let Some(adapter) = lookup(&account.engine) else {
            continue;
        };
        if credentials_gone(&adapter.credential_paths(&account)).await
            && flag(db, &account, "its credentials are missing").await
        {
            flagged += 1;
        }
    }
    flagged
}

/// Every path is absent. No paths to look at means nothing to judge; an unreadable answer counts
/// as present, so a flaky disk never logs an account out.
async fn credentials_gone(paths: &[std::path::PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }
    for path in paths {
        if tokio::fs::try_exists(path).await.unwrap_or(true) {
            return false;
        }
    }
    true
}

/// A provider rejected an account's credential (a run saw it): same outcome as a missing file,
/// fanned out at once.
pub async fn report_rejected(db: &SqlitePool, id: &AccountId) {
    match store::get(db, id).await {
        Ok(Some(account)) if account.state == AccountState::Active => {
            if flag(db, &account, "the provider rejected its credentials").await {
                startup::accounts_changed(db).await;
            }
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "account auth watch could not read an account"),
    }
}

/// Moves the account to `needs_login` and says so in the feed. False when nothing changed.
async fn flag(db: &SqlitePool, account: &Account, why: &str) -> bool {
    match store::set_state(db, &account.id, AccountState::NeedsLogin).await {
        Ok(true) => {}
        Ok(false) => return false,
        Err(error) => {
            tracing::warn!(%error, "account auth watch could not flag an account");
            return false;
        }
    }
    let summary = format!(
        "Account {} ({}) needs a new login: {why}",
        account.label, account.engine
    );
    if let Err(error) = crate::feed::append(db, None, NEEDS_LOGIN_KIND, &summary, None, None).await
    {
        tracing::warn!(%error, "account auth watch could not write the feed");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::adapters::fake::FakeAdapter;
    use crate::accounts::provider::{ProviderAdapter, SessionPortability};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    static FAKE: FakeAdapter = FakeAdapter {
        engine: "fake",
        provider: "fakeco",
        portability: SessionPortability::NotPortable,
        cache: None,
    };

    fn lookup(engine: &str) -> Option<&'static dyn ProviderAdapter> {
        (engine == "fake").then_some(&FAKE as &'static dyn ProviderAdapter)
    }

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    async fn add(db: &SqlitePool, hex: &str, dir: Option<&std::path::Path>, state: AccountState) {
        store::insert(
            db,
            &Account {
                id: AccountId(format!("fake:{hex}")),
                engine: "fake".into(),
                provider: "fakeco".into(),
                label: hex.into(),
                kind: AccountKind::Subscription,
                credential_locator: dir.map(|d| d.to_string_lossy().into_owned()),
                state,
                created_at: "t".into(),
                updated_at: "t".into(),
            },
        )
        .await
        .unwrap();
    }

    async fn state_of(db: &SqlitePool, hex: &str) -> AccountState {
        store::get(db, &AccountId(format!("fake:{hex}")))
            .await
            .unwrap()
            .unwrap()
            .state
    }

    #[tokio::test]
    async fn a_missing_credential_file_flags_the_account_once_and_says_so_in_the_feed() {
        let db = pool().await;
        let present = tempfile::tempdir().unwrap();
        std::fs::write(present.path().join("fake-auth.json"), "{}").unwrap();
        let gone = tempfile::tempdir().unwrap();
        add(&db, "00000001", Some(present.path()), AccountState::Active).await;
        add(&db, "00000002", Some(gone.path()), AccountState::Active).await;
        add(&db, "00000003", Some(gone.path()), AccountState::Paused).await;
        add(&db, "00000004", None, AccountState::Active).await;

        assert_eq!(sweep(&db, lookup).await, 1);
        assert_eq!(state_of(&db, "00000001").await, AccountState::Active);
        assert_eq!(state_of(&db, "00000002").await, AccountState::NeedsLogin);
        assert_eq!(state_of(&db, "00000003").await, AccountState::Paused);
        assert_eq!(state_of(&db, "00000004").await, AccountState::Active);

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = ?")
            .bind(NEEDS_LOGIN_KIND)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(sweep(&db, lookup).await, 0, "already flagged: said once");
    }

    #[tokio::test]
    async fn a_rejected_credential_flags_an_active_account_only() {
        let db = pool().await;
        let dir = tempfile::tempdir().unwrap();
        add(&db, "00000001", Some(dir.path()), AccountState::Active).await;
        add(&db, "00000002", Some(dir.path()), AccountState::Paused).await;
        report_rejected(&db, &AccountId("fake:00000001".into())).await;
        report_rejected(&db, &AccountId("fake:00000002".into())).await;
        report_rejected(&db, &AccountId("fake:ffffffff".into())).await;
        assert_eq!(state_of(&db, "00000001").await, AccountState::NeedsLogin);
        assert_eq!(state_of(&db, "00000002").await, AccountState::Paused);
    }
}
