//! SQL of the account pool: the `accounts` and `chat_accounts` tables and the account columns of
//! `runs`. Free async fns over the pool, like `runs.rs`: this module owns its SQL.

use sqlx::{Row, SqlitePool, sqlite::SqliteRow};

use super::{Account, AccountChoice, AccountId, AccountKind, AccountState};

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn kind_text(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Subscription => "subscription",
        AccountKind::ApiKey => "api_key",
    }
}

fn state_text(state: AccountState) -> &'static str {
    match state {
        AccountState::Active => "active",
        AccountState::Paused => "paused",
        AccountState::NeedsLogin => "needs_login",
    }
}

fn account_of_row(row: &SqliteRow) -> sqlx::Result<Account> {
    let kind: String = row.try_get("kind")?;
    let state: String = row.try_get("state")?;
    Ok(Account {
        id: AccountId(row.try_get("id")?),
        engine: row.try_get("engine")?,
        provider: row.try_get("provider")?,
        label: row.try_get("label")?,
        kind: if kind == "api_key" {
            AccountKind::ApiKey
        } else {
            AccountKind::Subscription
        },
        credential_locator: row.try_get("credential_locator")?,
        state: match state.as_str() {
            "paused" => AccountState::Paused,
            "needs_login" => AccountState::NeedsLogin,
            _ => AccountState::Active,
        },
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

const COLUMNS: &str =
    "id, engine, provider, label, kind, credential_locator, state, created_at, updated_at";

/// Every account, ordered by provider then label.
pub async fn list(db: &SqlitePool) -> sqlx::Result<Vec<Account>> {
    // COLUMNS is a fixed const list, no user input: audited as SQL-safe.
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM accounts ORDER BY provider, label"
    )))
    .fetch_all(db)
    .await?;
    rows.iter().map(account_of_row).collect()
}

pub async fn get(db: &SqlitePool, id: &AccountId) -> sqlx::Result<Option<Account>> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM accounts WHERE id = ?"
    )))
    .bind(id.as_str())
    .fetch_optional(db)
    .await?;
    row.as_ref().map(account_of_row).transpose()
}

pub async fn insert(db: &SqlitePool, account: &Account) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO accounts (id, engine, provider, label, kind, credential_locator, state, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(account.id.as_str())
    .bind(&account.engine)
    .bind(&account.provider)
    .bind(&account.label)
    .bind(kind_text(account.kind))
    .bind(&account.credential_locator)
    .bind(state_text(account.state))
    .bind(&account.created_at)
    .bind(&account.updated_at)
    .execute(db)
    .await?;
    Ok(())
}

/// False when no such account. A label already taken for the engine is a unique-index error.
pub async fn rename(db: &SqlitePool, id: &AccountId, label: &str) -> sqlx::Result<bool> {
    let done = sqlx::query("UPDATE accounts SET label = ?, updated_at = ? WHERE id = ?")
        .bind(label)
        .bind(now())
        .bind(id.as_str())
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

pub async fn set_state(db: &SqlitePool, id: &AccountId, state: AccountState) -> sqlx::Result<bool> {
    let done = sqlx::query("UPDATE accounts SET state = ?, updated_at = ? WHERE id = ?")
        .bind(state_text(state))
        .bind(now())
        .bind(id.as_str())
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Deletes the row; clears every chat pin naming it (choice -> 'auto') and returns those chat ids.
/// Refuses (`Ok(Err)`) for a `default:*` id.
pub async fn remove(
    db: &SqlitePool,
    id: &AccountId,
) -> sqlx::Result<Result<Vec<String>, &'static str>> {
    if id.is_default() {
        return Ok(Err("the default account cannot be removed"));
    }
    let pinned = AccountChoice::Account(id.clone()).to_string();
    let mut tx = db.begin().await?;
    let chats: Vec<String> =
        sqlx::query_scalar("SELECT chat_id FROM chat_accounts WHERE choice = ?")
            .bind(&pinned)
            .fetch_all(&mut *tx)
            .await?;
    sqlx::query("UPDATE chat_accounts SET choice = 'auto', updated_at = ? WHERE choice = ?")
        .bind(now())
        .bind(&pinned)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(id.as_str())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Ok(chats))
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatAccountRow {
    pub chat_id: String,
    pub choice: AccountChoice,
    pub current_account_id: Option<AccountId>,
    pub previous_account_id: Option<AccountId>,
    pub switched_at: Option<String>,
    pub last_cache_write_at: Option<String>,
    pub last_cache_tokens: Option<i64>,
}

pub async fn chat_account(db: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<ChatAccountRow>> {
    let row = sqlx::query(
        "SELECT chat_id, choice, current_account_id, previous_account_id, switched_at, \
         last_cache_write_at, last_cache_tokens FROM chat_accounts WHERE chat_id = ?",
    )
    .bind(chat_id)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let choice: String = row.try_get("choice")?;
    let id = |column: &str| -> sqlx::Result<Option<AccountId>> {
        Ok(row.try_get::<Option<String>, _>(column)?.map(AccountId))
    };
    Ok(Some(ChatAccountRow {
        chat_id: row.try_get("chat_id")?,
        // A stored text this build cannot read is treated as "auto", never as an error.
        choice: choice.parse().unwrap_or_default(),
        current_account_id: id("current_account_id")?,
        previous_account_id: id("previous_account_id")?,
        switched_at: row.try_get("switched_at")?,
        last_cache_write_at: row.try_get("last_cache_write_at")?,
        last_cache_tokens: row.try_get("last_cache_tokens")?,
    }))
}

pub async fn set_chat_choice(
    db: &SqlitePool,
    chat_id: &str,
    choice: &AccountChoice,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO chat_accounts (chat_id, choice, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(chat_id) DO UPDATE SET choice = excluded.choice, updated_at = excluded.updated_at",
    )
    .bind(chat_id)
    .bind(choice.to_string())
    .bind(now())
    .execute(db)
    .await?;
    Ok(())
}

/// Records the account a turn ran on. When it differs from the current one, the current becomes
/// the previous and `switched_at` is set to `now`.
pub async fn set_chat_current(
    db: &SqlitePool,
    chat_id: &str,
    account: &AccountId,
    now: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO chat_accounts (chat_id, current_account_id, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(chat_id) DO UPDATE SET \
           previous_account_id = CASE WHEN current_account_id IS NOT NULL AND current_account_id <> ?2 \
                                      THEN current_account_id ELSE previous_account_id END, \
           switched_at = CASE WHEN current_account_id IS NOT NULL AND current_account_id <> ?2 \
                              THEN ?3 ELSE switched_at END, \
           current_account_id = ?2, updated_at = ?3",
    )
    .bind(chat_id)
    .bind(account.as_str())
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn note_chat_cache(
    db: &SqlitePool,
    chat_id: &str,
    at: &str,
    cached_tokens: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO chat_accounts (chat_id, last_cache_write_at, last_cache_tokens, updated_at) \
         VALUES (?1, ?2, ?3, ?2) \
         ON CONFLICT(chat_id) DO UPDATE SET last_cache_write_at = ?2, last_cache_tokens = ?3, updated_at = ?2",
    )
    .bind(chat_id)
    .bind(at)
    .bind(cached_tokens)
    .execute(db)
    .await?;
    Ok(())
}

/// Stores what was asked for a run; `None` clears it (NULL = the settings default).
pub async fn set_run_choice(
    db: &SqlitePool,
    run_id: i64,
    choice: Option<&AccountChoice>,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE runs SET account_choice = ? WHERE id = ?")
        .bind(choice.map(|c| c.to_string()))
        .bind(run_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn run_choice(db: &SqlitePool, run_id: i64) -> sqlx::Result<Option<AccountChoice>> {
    let text: Option<Option<String>> =
        sqlx::query_scalar("SELECT account_choice FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(db)
            .await?;
    Ok(text.flatten().and_then(|t| t.parse().ok()))
}

/// Where the run actually ran.
pub async fn record_run_account(
    db: &SqlitePool,
    run_id: i64,
    account: &AccountId,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE runs SET account_id = ? WHERE id = ?")
        .bind(account.as_str())
        .bind(run_id)
        .execute(db)
        .await?;
    Ok(())
}

/// The account a CLI session lives in: the latest run with that session_id that has an account_id.
pub async fn account_of_session(
    db: &SqlitePool,
    session_id: &str,
) -> sqlx::Result<Option<AccountId>> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT account_id FROM runs WHERE session_id = ? AND account_id IS NOT NULL \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(id.map(AccountId))
}

/// Mean (cache_read + cache_creation + output) tokens and count of the last `n` completed runs on
/// an account: the projection input.
pub async fn recent_run_usage(
    db: &SqlitePool,
    account: &AccountId,
    n: i64,
) -> sqlx::Result<(f64, i64)> {
    let row = sqlx::query(
        "SELECT COALESCE(AVG(t), 0.0) AS mean, COUNT(*) AS n FROM ( \
           SELECT COALESCE(cache_read_tokens, 0) + COALESCE(cache_creation_tokens, 0) \
                  + COALESCE(output_tokens, 0) AS t \
           FROM runs WHERE account_id = ? AND completed_at IS NOT NULL \
           ORDER BY completed_at DESC LIMIT ?)",
    )
    .bind(account.as_str())
    .bind(n)
    .fetch_one(db)
    .await?;
    Ok((row.try_get("mean")?, row.try_get("n")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

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

    fn extra(engine: &str, hex: &str, label: &str) -> Account {
        Account {
            id: AccountId(format!("{engine}:{hex}")),
            engine: engine.into(),
            provider: "p".into(),
            label: label.into(),
            kind: AccountKind::Subscription,
            credential_locator: Some("/x".into()),
            state: AccountState::Active,
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    #[tokio::test]
    async fn the_migration_seeds_the_two_default_accounts() {
        let db = pool().await;
        let ids: Vec<String> = list(&db)
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.id.0)
            .collect();
        assert_eq!(ids, vec!["default:claude", "default:codex"]);
        let claude = get(&db, &AccountId::default_for("claude"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            claude.credential_locator, None,
            "the default login is the CLI's own dir"
        );
    }

    #[tokio::test]
    async fn insert_rename_state_and_label_uniqueness() {
        let db = pool().await;
        let a = extra("claude", "1a2b3c4d", "work");
        insert(&db, &a).await.unwrap();
        assert!(
            insert(&db, &extra("claude", "ffffffff", "work"))
                .await
                .is_err()
        );
        assert!(rename(&db, &a.id, "home").await.unwrap());
        assert!(!rename(&db, &AccountId("nope".into()), "x").await.unwrap());
        assert!(set_state(&db, &a.id, AccountState::Paused).await.unwrap());
        let got = get(&db, &a.id).await.unwrap().unwrap();
        assert_eq!(
            (got.label.as_str(), got.state),
            ("home", AccountState::Paused)
        );
    }

    #[tokio::test]
    async fn remove_unpins_chats_and_refuses_defaults() {
        let db = pool().await;
        let a = extra("claude", "1a2b3c4d", "work");
        insert(&db, &a).await.unwrap();
        set_chat_choice(&db, "c1", &AccountChoice::Account(a.id.clone()))
            .await
            .unwrap();
        set_chat_choice(&db, "c2", &AccountChoice::Auto)
            .await
            .unwrap();
        assert!(
            remove(&db, &AccountId::default_for("codex"))
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(remove(&db, &a.id).await.unwrap(), Ok(vec!["c1".to_owned()]));
        assert!(get(&db, &a.id).await.unwrap().is_none());
        assert_eq!(
            chat_account(&db, "c1").await.unwrap().unwrap().choice,
            AccountChoice::Auto
        );
    }

    #[tokio::test]
    async fn chat_current_moves_to_previous_when_it_changes() {
        let db = pool().await;
        let (a, b) = (
            AccountId("claude:aaaaaaaa".into()),
            AccountId("claude:bbbbbbbb".into()),
        );
        set_chat_current(&db, "c", &a, "t1").await.unwrap();
        let row = chat_account(&db, "c").await.unwrap().unwrap();
        assert_eq!(
            (
                row.current_account_id.as_ref(),
                row.previous_account_id.as_ref(),
                row.switched_at.as_deref()
            ),
            (Some(&a), None, None)
        );
        set_chat_current(&db, "c", &a, "t2").await.unwrap();
        assert_eq!(
            chat_account(&db, "c").await.unwrap().unwrap().switched_at,
            None
        );
        set_chat_current(&db, "c", &b, "t3").await.unwrap();
        let row = chat_account(&db, "c").await.unwrap().unwrap();
        assert_eq!(
            (
                row.current_account_id,
                row.previous_account_id,
                row.switched_at
            ),
            (Some(b), Some(a), Some("t3".into()))
        );
        note_chat_cache(&db, "c", "t4", 900).await.unwrap();
        let row = chat_account(&db, "c").await.unwrap().unwrap();
        assert_eq!(
            (row.last_cache_write_at.as_deref(), row.last_cache_tokens),
            (Some("t4"), Some(900))
        );
    }

    #[tokio::test]
    async fn run_columns_and_usage() {
        let db = pool().await;
        let acc = AccountId::default_for("claude");
        for (session, out) in [("s1", 100), ("s2", 300)] {
            sqlx::query(
                "INSERT INTO runs (prompt, status, created_at, session_id, output_tokens, completed_at) \
                 VALUES ('p', 'completed', 't', ?, ?, ?)",
            )
            .bind(session)
            .bind(out)
            .bind(format!("2026-01-0{out}"))
            .execute(&db)
            .await
            .unwrap();
        }
        let id: i64 = sqlx::query_scalar("SELECT id FROM runs WHERE session_id = 's1'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(run_choice(&db, id).await.unwrap(), None);
        let pin = AccountChoice::Provider("p".into());
        set_run_choice(&db, id, Some(&pin)).await.unwrap();
        assert_eq!(run_choice(&db, id).await.unwrap(), Some(pin));
        record_run_account(&db, id, &acc).await.unwrap();
        assert_eq!(
            account_of_session(&db, "s1").await.unwrap(),
            Some(acc.clone())
        );
        assert_eq!(account_of_session(&db, "s2").await.unwrap(), None);
        assert_eq!(recent_run_usage(&db, &acc, 5).await.unwrap(), (100.0, 1));
    }
}
