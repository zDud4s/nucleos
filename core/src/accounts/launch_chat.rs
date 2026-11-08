//! The account hook of a chat turn, and the launch plumbing a run launch shares with it.
//!
//! With an empty pool, or an engine outside it, every function here answers today's launch: no
//! env, no reservation, no database write (P7).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use super::adapters::adapter_for;
use super::choose::{Decision, SessionView};
use super::pool::{Holder, PickRequest, PoolState};
use super::provider::{LimitHit, ProviderAdapter};
use super::settings::AccountPoolDoc;
use super::store::{self, ChatAccountRow};
use super::{
    ACCOUNT_ENV_MARKER, Account, AccountChoice, AccountId, AccountKind, AccountPick, token, watch,
};

fn parse_time(text: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text?)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// A chat's or run's own choice, else the settings default. `Auto` is "no choice made", so it
/// defers to the default too.
pub fn effective_choice(own: Option<AccountChoice>, default: &AccountChoice) -> AccountChoice {
    match own {
        Some(choice) if choice != AccountChoice::Auto => choice,
        _ => default.clone(),
    }
}

/// The env of a launch on `account`: the adapter's, plus the marker that names the account.
fn env_for(adapter: &dyn ProviderAdapter, account: &Account) -> Vec<(String, String)> {
    let mut env = adapter.account_env(account);
    env.push((
        ACCOUNT_ENV_MARKER.to_owned(),
        account.id.as_str().to_owned(),
    ));
    env
}

/// Adds a token account's token to a launch env (replacing any earlier entry of the same name).
fn add_token(env: &mut Vec<(String, String)>, secret: &str) {
    env.retain(|(key, _)| key != token::ENV_VAR);
    env.push(token::env_pair(secret));
}

/// How many times a pick is redone after a token account turned out to have no token. Each redo
/// follows the account being flagged `needs_login`, so it cannot be chosen again; the bound only
/// guards a flag that did not stick.
const TOKEN_REPICKS: usize = 4;

/// `pick_with`, then the credential a token account needs: its token is read from the credential
/// store (on a blocking thread) and added to the env. A token account whose token is gone is
/// flagged `needs_login` and the pick is redone without it; one whose store cannot be read is not
/// flagged (the failure may be passing) and yields no pick at all. A launch never starts on a token
/// account without its token.
async fn pick_with_credentials(
    db: &SqlitePool,
    pool: &Arc<PoolState>,
    adapter: &dyn ProviderAdapter,
    doc: &AccountPoolDoc,
    holder: Holder,
    session: SessionView,
    now: DateTime<Utc>,
) -> AccountPick {
    for _ in 0..TOKEN_REPICKS {
        let pick = pick_with(pool, adapter, doc, holder.clone(), session.clone(), now);
        let Some(id) = pick.account_id.clone() else {
            return pick;
        };
        let is_token = pool
            .view()
            .candidates
            .iter()
            .any(|c| c.account.id == id && c.account.kind == AccountKind::Token);
        if !is_token {
            return pick;
        }
        match token::load(&id).await {
            Ok(Some(secret)) => {
                let mut pick = pick;
                add_token(&mut pick.env, &secret);
                return pick;
            }
            Ok(None) => {
                // Dropping the pick releases its reservation.
                drop(pick);
                watch::report_token_missing(db, &id).await;
            }
            Err(error) => {
                drop(pick);
                tracing::warn!(%error, account = %id.as_str(), "could not read an account token");
                return AccountPick {
                    blocked: Some(Decision::NoCandidate {
                        why: "the token of the account could not be read".to_owned(),
                    }),
                    ..AccountPick::default_launch()
                };
            }
        }
    }
    AccountPick::default_launch()
}

/// Decides and reserves under the pool's one lock, then turns the decision into a launch.
///
/// `Use` carries the account env and the reservation. A blocked decision (`PinnedBlocked`,
/// `SessionStuck`, `NoCandidate`) hands back no env and no reservation, only the decision, for
/// the caller to act on. `Default` is today's launch.
pub fn pick_with(
    pool: &Arc<PoolState>,
    adapter: &dyn ProviderAdapter,
    doc: &AccountPoolDoc,
    holder: Holder,
    session: SessionView,
    now: DateTime<Utc>,
) -> AccountPick {
    let request = PickRequest {
        engine: adapter.engine().to_owned(),
        provider: adapter.provider().to_owned(),
        session,
        holder,
        // Whether a session may move is the adapter's word: NotPortable until a spike says
        // otherwise (S1), which keeps `choose_account` from switching mid-conversation.
        portability: adapter.session_portability(),
        cache_model: adapter.cache_model(),
    };
    let (decision, reservation) = pool.decide_and_reserve(&request, doc, now);
    match decision {
        Decision::Default => AccountPick::default_launch(),
        Decision::Use { account, .. } => {
            let env = pool
                .view()
                .candidates
                .iter()
                .find(|c| c.account.id == account)
                .map(|c| env_for(adapter, &c.account))
                .unwrap_or_default();
            AccountPick {
                account_id: Some(account),
                env,
                reservation,
                blocked: None,
            }
        }
        blocked => AccountPick {
            blocked: Some(blocked),
            ..AccountPick::default_launch()
        },
    }
}

/// What the chat's row says about the session, as the decision reads it.
fn chat_session(row: Option<&ChatAccountRow>, default: &AccountChoice) -> SessionView {
    let Some(row) = row else {
        return SessionView {
            choice: default.clone(),
            ..SessionView::default()
        };
    };
    SessionView {
        current: row.current_account_id.clone(),
        choice: effective_choice(Some(row.choice.clone()), default),
        previous: row
            .previous_account_id
            .clone()
            .zip(parse_time(row.switched_at.as_deref())),
        last_cache_write_at: parse_time(row.last_cache_write_at.as_deref()),
        cached_tokens: row.last_cache_tokens,
        stream_ttl: None,
    }
}

/// Picks the account for one chat turn on `engine`. `resuming` is the CLI session the turn
/// continues, if any.
pub async fn pick_for_chat_turn(
    db: &SqlitePool,
    pool: &Arc<PoolState>,
    chat_id: &str,
    engine: &str,
    resuming: Option<&str>,
    now: DateTime<Utc>,
) -> AccountPick {
    let Some(adapter) = adapter_for(engine) else {
        return AccountPick::default_launch();
    };
    if pool.view().candidates.is_empty() {
        return AccountPick::default_launch();
    }
    let doc = super::settings::current();
    let row = store::chat_account(db, chat_id).await.ok().flatten();
    let mut session = chat_session(row.as_ref(), &doc.default_choice);
    if session.current.is_none()
        && let Some(session_id) = resuming
    {
        // A chat with no recorded account whose session already lives in one stays there.
        session.current = store::account_of_session(db, session_id)
            .await
            .ok()
            .flatten();
    }
    pick_with_credentials(
        db,
        pool,
        adapter,
        &doc,
        Holder::Chat(chat_id.to_owned()),
        session,
        now,
    )
    .await
}

/// Picks the account for one run launch on `engine`: the run's own `account_choice`, else the
/// settings default.
pub async fn pick_for_run_launch(
    db: &SqlitePool,
    pool: &Arc<PoolState>,
    run_id: i64,
    engine: &str,
    resume_session_id: Option<&str>,
    now: DateTime<Utc>,
) -> AccountPick {
    let Some(adapter) = adapter_for(engine) else {
        return AccountPick::default_launch();
    };
    if pool.view().candidates.is_empty() {
        return AccountPick::default_launch();
    }
    let doc = super::settings::current();
    let own = store::run_choice(db, run_id).await.ok().flatten();
    let current = match resume_session_id {
        Some(session_id) => store::account_of_session(db, session_id)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let session = SessionView {
        current,
        choice: effective_choice(own, &doc.default_choice),
        ..SessionView::default()
    };
    pick_with_credentials(db, pool, adapter, &doc, Holder::Run(run_id), session, now).await
}

/// Reads a launch's output for a limit error and marks the window exhausted when there is one,
/// so the next pick skips the account. `None` when the output is not a limit hit.
pub fn note_limit(
    pool: &PoolState,
    engine: &str,
    account: &AccountId,
    stdout: &str,
    stderr: &str,
    now: DateTime<Utc>,
) -> Option<LimitHit> {
    pool.note_failure(adapter_for(engine)?, account, stdout, stderr, now)
}

/// Account + window pairs already announced, with the time their notice is good until.
static ANNOUNCED: std::sync::Mutex<Vec<(String, String, DateTime<Utc>)>> =
    std::sync::Mutex::new(Vec::new());

/// How long a notice with no known reset time holds off a repeat.
const NOTICE_FALLBACK: chrono::Duration = chrono::Duration::hours(5);

/// When a hit's mark and notice stop holding: the provider's reset, else the fallback.
fn notice_until(hit: &LimitHit, now: DateTime<Utc>) -> DateTime<Utc> {
    hit.resets_at.unwrap_or(now + NOTICE_FALLBACK)
}

/// Seeds the dedup set from the marks stored before a restart, so their notices are not repeated.
pub(super) fn seed_announced(marks: &[store::Exhaustion], now: DateTime<Utc>) {
    let Ok(mut seen) = ANNOUNCED.lock() else {
        return;
    };
    seen.retain(|(_, _, until)| *until > now);
    for mark in marks.iter().filter(|m| m.until > now) {
        if !seen
            .iter()
            .any(|(id, w, _)| id == mark.account_id.as_str() && *w == mark.window)
        {
            seen.push((
                mark.account_id.as_str().to_owned(),
                mark.window.clone(),
                mark.until,
            ));
        }
    }
}

/// True once per account per window: the first hit of a window says so, the rest stay quiet
/// until the window resets.
pub(super) fn limit_notice_due(account: &AccountId, hit: &LimitHit, now: DateTime<Utc>) -> bool {
    let window = hit.window.clone().unwrap_or_default();
    let until = notice_until(hit, now);
    let Ok(mut seen) = ANNOUNCED.lock() else {
        return false;
    };
    seen.retain(|(_, _, until)| *until > now);
    if seen
        .iter()
        .any(|(id, w, _)| id == account.as_str() && *w == window)
    {
        return false;
    }
    seen.push((account.as_str().to_owned(), window, until));
    true
}

/// `note_limit`, plus one feed row (`LIMIT_HIT_KIND`) the first time an account's window is
/// found exhausted. The mark is stored too (`account_exhaustions`), so a restart restores it and
/// does not repeat the notice. Returns the hit like `note_limit`.
pub async fn note_limit_and_announce(
    db: &SqlitePool,
    project_id: Option<&str>,
    engine: &str,
    account: &AccountId,
    stdout: &str,
    stderr: &str,
    now: DateTime<Utc>,
) -> Option<LimitHit> {
    let hit = note_limit(&PoolState::global(), engine, account, stdout, stderr, now)?;
    if limit_notice_due(account, &hit, now) {
        let window = hit.window.as_deref().unwrap_or("a usage window");
        let summary = format!(
            "Account {} ({engine}) hit its limit on {window}; new work goes to other accounts",
            account.as_str()
        );
        if let Err(error) =
            crate::feed::append(db, project_id, super::LIMIT_HIT_KIND, &summary, None, None).await
        {
            tracing::warn!(%error, "could not write the account limit notice");
        }
    }
    // Every hit refreshes the stored mark; the first `announced_at` is kept by the upsert.
    let mark = store::Exhaustion {
        account_id: account.clone(),
        window: hit.window.clone().unwrap_or_default(),
        resets_at: hit.resets_at,
        until: notice_until(&hit, now),
        announced_at: now,
    };
    if let Err(error) = store::upsert_exhaustion(db, &mark).await {
        tracing::warn!(%error, "could not store the account exhaustion mark");
    }
    Some(hit)
}

/// Records the turn's cache usage and classifies its output for a limit hit.
#[allow(clippy::too_many_arguments)]
pub async fn after_chat_turn(
    db: &SqlitePool,
    chat_id: &str,
    account: &AccountId,
    engine: &str,
    cache_creation_tokens: i64,
    cache_read_tokens: i64,
    stdout: &str,
    stderr: &str,
    now: DateTime<Utc>,
) {
    let stamp = now.to_rfc3339();
    if let Err(error) = store::set_chat_current(db, chat_id, account, &stamp).await {
        tracing::warn!(%error, chat_id, "could not record the account a turn ran on");
    }
    // A cache write makes the prefix warm, and its size is what a later switch would pay for.
    if cache_creation_tokens > 0
        && let Err(error) = store::note_chat_cache(
            db,
            chat_id,
            &stamp,
            cache_creation_tokens.saturating_add(cache_read_tokens.max(0)),
        )
        .await
    {
        tracing::warn!(%error, chat_id, "could not record a turn's cache write");
    }
    if let Some(hit) = note_limit_and_announce(db, None, engine, account, stdout, stderr, now).await
    {
        tracing::info!(chat_id, account = account.as_str(), window = ?hit.window, "account limit hit");
    }
}

/// The routing restriction an account choice adds to a node's own, as engine names.
///
/// `existing` is the node launcher's restriction. The two are intersected; an intersection that
/// would be empty keeps `existing` (never an empty list, which would park the run). With an
/// empty pool or an `Auto` choice nothing changes.
pub fn narrow_by_choice(
    existing: Option<Vec<String>>,
    choice: &AccountChoice,
    accounts: &[Account],
) -> Option<Vec<String>> {
    let Some(by_choice) = super::launch_run::restrict_for_choice(choice, accounts) else {
        return existing;
    };
    match existing {
        None => Some(by_choice),
        Some(existing) => {
            let both: Vec<String> = existing
                .iter()
                .filter(|engine| by_choice.contains(engine))
                .cloned()
                .collect();
            Some(if both.is_empty() { existing } else { both })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::adapters::fake::FakeAdapter;
    use crate::accounts::choose::test_support::{account, usage};
    use crate::accounts::provider::SessionPortability;

    fn adapter() -> FakeAdapter {
        FakeAdapter::new("fake", "fakeco")
    }

    fn pool_of(ids: &[&str], now: DateTime<Utc>) -> Arc<PoolState> {
        let pool = PoolState::new();
        pool.set_accounts(ids.iter().map(|id| account(id)).collect());
        pool.set_usage(
            ids.iter()
                .map(|id| usage(id, &[("1h", 0.1, 600)], now))
                .collect(),
        );
        pool
    }

    fn view(current: Option<&str>, choice: AccountChoice) -> SessionView {
        SessionView {
            current: current.map(|id| AccountId(id.into())),
            choice,
            ..SessionView::default()
        }
    }

    async fn db() -> SqlitePool {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        crate::storage::MIGRATOR.run(&db).await.unwrap();
        db
    }

    fn token_account(id: &str) -> Account {
        let mut built = account(id);
        built.kind = AccountKind::Token;
        built.credential_locator = Some("/d".into());
        built
    }

    #[test]
    fn the_token_joins_the_env_of_the_launch_once() {
        let account = token_account("fake:t");
        let mut env = env_for(&adapter(), &account);
        add_token(&mut env, "sk-ant-oat01-fake");
        add_token(&mut env, "sk-ant-oat01-fake-2");
        let named = |name: &str| -> Vec<&str> {
            env.iter()
                .filter(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
                .collect()
        };
        assert_eq!(named(token::ENV_VAR), vec!["sk-ant-oat01-fake-2"]);
        assert_eq!(named("FAKE_HOME"), vec!["/d"]);
        assert_eq!(crate::accounts::account_of(&env), Some("fake:t"));
    }

    #[tokio::test]
    async fn a_token_account_launches_with_its_token() {
        let db = db().await;
        let now = Utc::now();
        let account = token_account("fake:token-launch");
        token::store(&account.id, "sk-ant-oat01-fake")
            .await
            .unwrap();
        let pool = PoolState::new();
        pool.set_accounts(vec![account.clone()]);
        pool.set_usage(vec![usage("fake:token-launch", &[("1h", 0.1, 600)], now)]);

        let pick = pick_with_credentials(
            &db,
            &pool,
            &adapter(),
            &AccountPoolDoc::default(),
            Holder::Run(11),
            SessionView::default(),
            now,
        )
        .await;

        assert_eq!(pick.account_id.as_ref(), Some(&account.id));
        assert!(
            pick.env
                .contains(&(token::ENV_VAR.to_owned(), "sk-ant-oat01-fake".to_owned()))
        );
        token::delete(&account.id).await.unwrap();
    }

    #[tokio::test]
    async fn a_token_account_without_its_token_is_flagged_and_never_launched() {
        let db = db().await;
        let now = Utc::now();
        let account = token_account("fake:token-gone");
        store::insert(&db, &account).await.unwrap();
        let pool = PoolState::new();
        pool.set_accounts(vec![account.clone()]);
        pool.set_usage(vec![usage("fake:token-gone", &[("1h", 0.1, 600)], now)]);

        let pick = pick_with_credentials(
            &db,
            &pool,
            &adapter(),
            &AccountPoolDoc::default(),
            Holder::Run(12),
            SessionView::default(),
            now,
        )
        .await;

        assert!(pick.account_id.is_none() && pick.env.is_empty());
        assert_eq!(
            pool.live_sessions(&account.id),
            0,
            "the reservation is released"
        );
        let stored = store::get(&db, &account.id).await.unwrap().unwrap();
        assert_eq!(stored.state, crate::accounts::AccountState::NeedsLogin);
    }

    #[test]
    fn a_limit_notice_is_due_once_per_account_and_window() {
        let now = Utc::now();
        let id = AccountId("notice-test:1".into());
        let hit = LimitHit {
            window: Some("5h".into()),
            resets_at: Some(now + chrono::Duration::hours(1)),
        };
        assert!(limit_notice_due(&id, &hit, now));
        assert!(!limit_notice_due(&id, &hit, now));
        let other = LimitHit {
            window: Some("7d".into()),
            ..hit.clone()
        };
        assert!(limit_notice_due(&id, &other, now));
        let later = now + chrono::Duration::hours(2);
        assert!(limit_notice_due(&id, &hit, later), "the window reset");
    }

    #[test]
    fn an_empty_pool_launches_exactly_as_today() {
        let pool = PoolState::new();
        let pick = pick_with(
            &pool,
            &adapter(),
            &AccountPoolDoc::default(),
            Holder::Chat("c".into()),
            SessionView::default(),
            Utc::now(),
        );
        assert!(pick.account_id.is_none() && pick.env.is_empty());
        assert!(pick.reservation.is_none() && pick.blocked.is_none());
    }

    #[test]
    fn a_pick_carries_the_marker_and_holds_a_reservation_until_dropped() {
        let now = Utc::now();
        let pool = pool_of(&["fake:a"], now);
        let pick = pick_with(
            &pool,
            &adapter(),
            &AccountPoolDoc::default(),
            Holder::Run(7),
            SessionView::default(),
            now,
        );
        let id = AccountId("fake:a".into());
        assert_eq!(pick.account_id.as_ref(), Some(&id));
        assert_eq!(crate::accounts::account_of(&pick.env), Some("fake:a"));
        assert_eq!(pool.live_sessions(&id), 1);
        drop(pick);
        assert_eq!(pool.live_sessions(&id), 0);
    }

    #[test]
    fn a_not_portable_session_stays_on_its_account() {
        let now = Utc::now();
        let pool = pool_of(&["fake:a", "fake:b"], now);
        // `a` is nearly spent, `b` is idle: a portable session would move.
        pool.set_usage(vec![
            usage("fake:a", &[("1h", 0.9, 600)], now),
            usage("fake:b", &[("1h", 0.0, 600)], now),
        ]);
        let adapter = adapter();
        assert_eq!(
            adapter.session_portability(),
            SessionPortability::NotPortable
        );
        let pick = pick_with(
            &pool,
            &adapter,
            &AccountPoolDoc::default(),
            Holder::Chat("c".into()),
            view(Some("fake:a"), AccountChoice::Auto),
            now,
        );
        assert_eq!(pick.account_id, Some(AccountId("fake:a".into())));
    }

    #[test]
    fn a_pin_on_a_paused_account_is_blocked_not_replaced() {
        let now = Utc::now();
        let pool = PoolState::new();
        let mut paused = account("fake:a");
        paused.state = crate::accounts::AccountState::Paused;
        pool.set_accounts(vec![paused, account("fake:b")]);
        let pick = pick_with(
            &pool,
            &adapter(),
            &AccountPoolDoc::default(),
            Holder::Chat("c".into()),
            view(None, AccountChoice::Account(AccountId("fake:a".into()))),
            now,
        );
        assert!(matches!(pick.blocked, Some(Decision::PinnedBlocked { .. })));
        assert!(pick.account_id.is_none() && pick.env.is_empty());
    }

    #[test]
    fn auto_defers_to_the_default_choice() {
        let default = AccountChoice::Provider("fakeco".into());
        assert_eq!(effective_choice(None, &default), default);
        assert_eq!(
            effective_choice(Some(AccountChoice::Auto), &default),
            default
        );
        let pinned = AccountChoice::Account(AccountId("fake:a".into()));
        assert_eq!(effective_choice(Some(pinned.clone()), &default), pinned);
    }

    #[test]
    fn a_choice_narrows_a_nodes_restriction_but_never_empties_it() {
        let mut claude = account("a");
        claude.engine = "claude".into();
        claude.provider = "anthropic".into();
        let accounts = [claude];
        let provider = AccountChoice::Provider("anthropic".into());
        assert_eq!(
            narrow_by_choice(None, &provider, &accounts),
            Some(vec!["claude".to_owned()])
        );
        let node = Some(vec!["claude".to_owned(), "codex".to_owned()]);
        assert_eq!(
            narrow_by_choice(node.clone(), &provider, &accounts),
            Some(vec!["claude".to_owned()])
        );
        let codex_only = Some(vec!["codex".to_owned()]);
        assert_eq!(
            narrow_by_choice(codex_only.clone(), &provider, &accounts),
            codex_only
        );
        assert_eq!(
            narrow_by_choice(node.clone(), &AccountChoice::Auto, &accounts),
            node
        );
    }
}
