//! A node whose run died because its ACCOUNT ran out (a usage limit, or a 429 that says "this
//! subscription, right now") is not a node that failed, and waiting on a blind backoff against the
//! same account helps nobody. The account is marked exhausted and another is picked at once:
//!
//! - another account has room: the node starts again now, cold. The CLI session lives in the first
//!   account's store and cannot follow (`NotPortable`), so the relaunch never resumes it;
//! - none has (or the pool has only this one): the node waits, visibly, until the earliest reset
//!   plus a margin, and starts again by itself then.
//!
//! The launch itself re-picks the account (`spawn::account_for_launch`), and the mark made here is
//! what makes that pick skip the exhausted one.

use chrono::{DateTime, Duration, Utc};

use crate::accounts::pool::{PoolState, Repick, exhaust_and_repick};
use crate::accounts::provider::LimitHit;
use crate::execution::runner::{failed_on_rate_limit, usage_limit_reset};

/// Feed kind of the "waiting for an account" row (one per dead run).
pub(super) const WAIT_KIND: &str = "account_limit_wait";
/// How long a 429 holds an account out, from the run's end (the pool's own mark).
const RATE_LIMIT_COOLDOWN_SECS: i64 = crate::accounts::adapters::RATE_LIMIT_MARK.num_seconds();
/// How long a usage limit whose message names no readable reset holds an account out.
const UNKNOWN_RESET_SECS: i64 = crate::accounts::adapters::USAGE_LIMIT_FALLBACK.num_seconds();
/// Added to a reset, so the relaunch does not land in the second the limit lifts.
const RESET_MARGIN_SECS: i64 = 60;

/// What a node does about its dead run.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Verdict {
    /// The run did not die on an account limit (or under no pooled account): the other chains judge it.
    NotAccountLimit,
    /// Start the node again now: another account has room, or the limit has already lifted.
    Relaunch,
    /// Every account is out: leave the node alone until `resume`.
    Wait { resume: DateTime<Utc> },
}

/// The limit `stdout` ends on, read against `ended` (when the run died), so a run that died long
/// ago is not given a reset that drifted with the clock.
fn limit_of(stdout: &str, ended: DateTime<Utc>, window: Option<String>) -> Option<LimitHit> {
    let resets_at = match usage_limit_reset(stdout, ended) {
        Some(Some(reset)) => reset,
        Some(None) => ended + Duration::seconds(UNKNOWN_RESET_SECS),
        None if failed_on_rate_limit(stdout) => ended + Duration::seconds(RATE_LIMIT_COOLDOWN_SECS),
        None => return None,
    };
    Some(LimitHit {
        window,
        resets_at: Some(resets_at),
    })
}

/// Judges the dead run `run_id` of `node_id`. `accounts` is the pool the launch will pick from.
pub(super) async fn judge(
    db: &sqlx::SqlitePool,
    accounts: &PoolState,
    node_id: &str,
    run_id: &str,
) -> Result<Verdict, String> {
    let Ok(number) = run_id.parse::<i64>() else {
        return Ok(Verdict::NotAccountLimit);
    };
    let row: Option<(Option<String>, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT stdout, stderr, account_id, COALESCE(completed_at, created_at) \
         FROM runs WHERE id = ? AND status = 'failed'",
    )
    .bind(number)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    let Some((Some(stdout), stderr, Some(account_id), ended)) = row else {
        return Ok(Verdict::NotAccountLimit);
    };
    // A run under an account the pool does not hold has nothing to switch away from.
    let Some(account) = accounts
        .view()
        .candidates
        .into_iter()
        .map(|c| c.account)
        .find(|a| a.id.as_str() == account_id)
    else {
        return Ok(Verdict::NotAccountLimit);
    };
    let Some(adapter) = crate::accounts::adapters::adapter_for(&account.engine) else {
        return Ok(Verdict::NotAccountLimit);
    };
    // The adapter decides whether this is a limit at all (and names its window); the reset is read
    // against the run's end.
    let Some(read) = adapter.classify_failure(&stdout, stderr.as_deref().unwrap_or("")) else {
        return Ok(Verdict::NotAccountLimit);
    };
    let ended = DateTime::parse_from_rfc3339(&ended)
        .map(|at| at.with_timezone(&Utc))
        .unwrap_or_default();
    let hit = limit_of(&stdout, ended, read.window.clone()).unwrap_or(read);
    let now = Utc::now();
    let margin = Duration::seconds(RESET_MARGIN_SECS);
    let reset = hit.resets_at.unwrap_or(now) + margin;
    if reset <= now {
        // The limit lifted while the node waited (or before this tick saw the death).
        return Ok(Verdict::Relaunch);
    }
    let doc = crate::accounts::settings::current();
    let repick = exhaust_and_repick(
        db,
        accounts,
        None,
        &account.id,
        &account.engine,
        &hit,
        &doc,
        now,
    )
    .await;
    match repick {
        Repick::Switch(_) => Ok(Verdict::Relaunch),
        Repick::AllExhausted { resets_at } => {
            let resume = resets_at.map_or(reset, |at| at.max(now) + margin);
            announce_wait(db, node_id, number, &account.id, resume).await;
            Ok(Verdict::Wait { resume })
        }
    }
}

/// The visible half of the wait: one feed row per dead run, so the owner sees why the node is idle
/// and until when. A row that cannot be written is logged and changes nothing.
async fn announce_wait(
    db: &sqlx::SqlitePool,
    node_id: &str,
    run: i64,
    account: &crate::accounts::AccountId,
    resume: DateTime<Utc>,
) {
    let seen: Result<i64, _> =
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = ? AND run_id = ?")
            .bind(WAIT_KIND)
            .bind(run)
            .fetch_one(db)
            .await;
    if !matches!(seen, Ok(0)) {
        return;
    }
    let summary = format!(
        "Node {node_id} is waiting: account {} hit its limit and no other account has room; \
         it starts again at {}",
        account.as_str(),
        resume.format("%Y-%m-%d %H:%M UTC")
    );
    // The feed has no node subject; the dead run is the story this row belongs to.
    let subject = nucleos_base::feed::Subject::Run(run);
    if let Err(error) =
        crate::feed::append(db, None, WAIT_KIND, &summary, Some(run), Some(&subject)).await
    {
        tracing::warn!(%error, "could not write the account wait notice");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use super::*;
    use crate::accounts::{Account, AccountId, AccountKind, AccountState};

    const CREATED: &str = "2026-10-31T00:00:00+00:00";
    const RATE_LIMITED: &str = r#"{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","api_error_status":429,"result":"API Error: 429"}"#;
    const OVERLOADED: &str = r#"{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","api_error_status":529,"result":"API Error: 529"}"#;

    async fn db() -> sqlx::SqlitePool {
        let (state, _runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(StdDuration::from_secs(60)),
            StdDuration::from_secs(600),
        )
        .await;
        state.pool.clone()
    }

    fn account(id: &str) -> Account {
        Account {
            id: AccountId(id.into()),
            engine: "claude".into(),
            provider: "anthropic".into(),
            label: id.into(),
            kind: AccountKind::Subscription,
            credential_locator: None,
            state: AccountState::Active,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn pool_of(ids: &[&str]) -> std::sync::Arc<PoolState> {
        let pool = PoolState::new();
        pool.set_accounts(ids.iter().map(|id| account(id)).collect());
        pool
    }

    /// A run that ended `failed` `ago` before now with `stdout`, under `account_id`.
    async fn seed_run(
        db: &sqlx::SqlitePool,
        id: i64,
        stdout: &str,
        account_id: Option<&str>,
        ago: chrono::Duration,
    ) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, stdout, account_id, created_at, completed_at) \
             VALUES (?, 'p', 'failed', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(stdout)
        .bind(account_id)
        .bind(CREATED)
        .bind((Utc::now() - ago).to_rfc3339())
        .execute(db)
        .await
        .unwrap();
    }

    async fn waits(db: &sqlx::SqlitePool, run: i64) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = ? AND run_id = ?")
            .bind(WAIT_KIND)
            .bind(run)
            .fetch_one(db)
            .await
            .unwrap()
    }

    /// A 429 on one account with another to spare: relaunch now, and the first one is marked so
    /// the launch's own pick skips it.
    #[tokio::test]
    async fn a_429_with_another_account_relaunches_now_and_marks_the_first() {
        let db = db().await;
        let pool = pool_of(&["claude:a", "claude:b"]);
        seed_run(
            &db,
            1,
            RATE_LIMITED,
            Some("claude:a"),
            chrono::Duration::seconds(5),
        )
        .await;

        let verdict = judge(&db, &pool, "n", "1").await.unwrap();

        assert_eq!(verdict, Verdict::Relaunch);
        let view = pool.view();
        let a = view
            .candidates
            .iter()
            .find(|c| c.account.id.as_str() == "claude:a")
            .unwrap();
        assert_eq!(a.exhausted.len(), 1, "the dead run's account is marked");
        assert_eq!(waits(&db, 1).await, 0, "nothing to wait for");
    }

    /// The only account hit a 429: no blind backoff against it. The node waits for the cooldown
    /// plus a margin, with one visible row, however often the tick looks again.
    #[tokio::test]
    async fn a_429_on_the_only_account_waits_for_its_reset_visibly() {
        let db = db().await;
        let pool = pool_of(&["claude:a"]);
        seed_run(
            &db,
            2,
            RATE_LIMITED,
            Some("claude:a"),
            chrono::Duration::seconds(5),
        )
        .await;

        let first = judge(&db, &pool, "n", "2").await.unwrap();
        let again = judge(&db, &pool, "n", "2").await.unwrap();

        let Verdict::Wait { resume } = first else {
            panic!("expected a wait, got {first:?}");
        };
        let from_now = (resume - Utc::now()).num_seconds();
        assert!(
            (RATE_LIMIT_COOLDOWN_SECS - 30..=RATE_LIMIT_COOLDOWN_SECS + RESET_MARGIN_SECS)
                .contains(&from_now),
            "resumes after cooldown + margin, not a backoff: {from_now}s"
        );
        assert!(matches!(again, Verdict::Wait { .. }));
        assert_eq!(waits(&db, 2).await, 1, "one row, not one per tick");
    }

    /// Every account out: the wait ends at the EARLIEST reset among them.
    #[tokio::test]
    async fn all_accounts_out_wait_for_the_earliest_reset() {
        let db = db().await;
        let pool = pool_of(&["claude:a", "claude:b"]);
        // b is already out until further ahead than a's 429 will be.
        pool.mark_exhausted(
            &AccountId("claude:b".into()),
            &LimitHit {
                window: Some("weekly".into()),
                resets_at: Some(Utc::now() + chrono::Duration::days(2)),
            },
            Utc::now(),
        );
        seed_run(
            &db,
            3,
            RATE_LIMITED,
            Some("claude:a"),
            chrono::Duration::seconds(5),
        )
        .await;

        let verdict = judge(&db, &pool, "n", "3").await.unwrap();

        let Verdict::Wait { resume } = verdict else {
            panic!("expected a wait, got {verdict:?}");
        };
        assert!(
            resume < Utc::now() + chrono::Duration::hours(1),
            "a's cooldown, not b's two days: {resume}"
        );
    }

    /// A limit that already lifted needs no wait and no switch.
    #[tokio::test]
    async fn a_lifted_limit_relaunches_without_marking_anything() {
        let db = db().await;
        let pool = pool_of(&["claude:a"]);
        seed_run(
            &db,
            4,
            RATE_LIMITED,
            Some("claude:a"),
            chrono::Duration::hours(2),
        )
        .await;

        let verdict = judge(&db, &pool, "n", "4").await.unwrap();

        assert_eq!(verdict, Verdict::Relaunch);
        assert!(pool.view().candidates[0].exhausted.is_empty());
    }

    /// Deaths that say nothing about the account stay with the other chains: the service being
    /// busy, a run under no account, an account the pool does not hold, a run still marked live.
    #[tokio::test]
    async fn other_deaths_are_not_account_limits() {
        let db = db().await;
        let pool = pool_of(&["claude:a"]);
        seed_run(
            &db,
            5,
            OVERLOADED,
            Some("claude:a"),
            chrono::Duration::seconds(5),
        )
        .await;
        seed_run(&db, 6, RATE_LIMITED, None, chrono::Duration::seconds(5)).await;
        seed_run(
            &db,
            7,
            RATE_LIMITED,
            Some("claude:zzz"),
            chrono::Duration::seconds(5),
        )
        .await;

        for run in ["5", "6", "7", "999", "not-a-number"] {
            assert_eq!(
                judge(&db, &pool, "n", run).await.unwrap(),
                Verdict::NotAccountLimit,
                "run {run}"
            );
        }
        assert!(pool.view().candidates[0].exhausted.is_empty());
    }

    /// A usage limit that names its reset waits for that reset, not for the 429 cooldown.
    #[tokio::test]
    async fn a_usage_limit_on_the_only_account_waits_for_the_reset_it_names() {
        let db = db().await;
        let pool = pool_of(&["claude:a"]);
        let ended = Utc::now() - chrono::Duration::seconds(5);
        let reset = (ended + chrono::Duration::hours(3))
            .with_timezone(&Utc)
            .trunc_subsecs(0);
        let message = format!(
            "You've hit your weekly limit · resets {} (UTC)",
            reset.format("%I:%M%P")
        );
        let stdout = serde_json::json!({
            "type": "result", "subtype": "success", "is_error": true,
            "result": message,
        })
        .to_string();
        seed_run(
            &db,
            8,
            &stdout,
            Some("claude:a"),
            chrono::Duration::seconds(5),
        )
        .await;

        let verdict = judge(&db, &pool, "n", "8").await.unwrap();

        let Verdict::Wait { resume } = verdict else {
            panic!("expected a wait, got {verdict:?}");
        };
        let hours = (resume - Utc::now()).num_minutes();
        assert!(
            (150..=190).contains(&hours),
            "about three hours to the reset, not the 429 cooldown: {hours} min"
        );
    }

    use chrono::SubsecRound;
}
