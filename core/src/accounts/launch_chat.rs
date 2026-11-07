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
use super::{ACCOUNT_ENV_MARKER, Account, AccountChoice, AccountId, AccountPick};

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
    env.push((ACCOUNT_ENV_MARKER.to_owned(), account.id.as_str().to_owned()));
    env
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
    pick_with(
        pool,
        adapter,
        &doc,
        Holder::Chat(chat_id.to_owned()),
        session,
        now,
    )
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
    pick_with(pool, adapter, &doc, Holder::Run(run_id), session, now)
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

/// Records the turn's cache usage and classifies its output for a limit hit.
#[allow(clippy::too_many_arguments)]
pub async fn after_chat_turn(
    db: &SqlitePool,
    pool: &Arc<PoolState>,
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
    if let Some(hit) = note_limit(pool, engine, account, stdout, stderr, now) {
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
        pool.set_usage(ids.iter().map(|id| usage(id, &[("1h", 0.1, 600)], now)).collect());
        pool
    }

    fn view(current: Option<&str>, choice: AccountChoice) -> SessionView {
        SessionView {
            current: current.map(|id| AccountId(id.into())),
            choice,
            ..SessionView::default()
        }
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
        assert_eq!(adapter.session_portability(), SessionPortability::NotPortable);
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
        assert_eq!(effective_choice(Some(AccountChoice::Auto), &default), default);
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
