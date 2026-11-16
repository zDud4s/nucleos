//! Transient SQLite faults in the writers of the worktree feed lines.
//!
//! The pool (`nucleos_base::storage`: 12 connections, 3 s acquire timeout, 3 s `busy_timeout`) is
//! sized on purpose and kept inside the hook budget, so a burst (several trees starting in the same
//! second) is ridden out here instead: a short bounded retry, and when it still fails a `_held` feed
//! kind rather than a danger one, because "database is locked" says nothing about the worktree, the
//! gate or the root run.

use std::time::Duration;

/// Pauses between the attempts of [`retry_transient`]: three retries, 1.1 s in total at most.
const BACKOFF_MS: [u64; 3] = [100, 300, 700];

/// Whether an error text says the database was momentarily busy: SQLite's `database is locked`,
/// `database table is locked`, `SQLITE_BUSY`, or the pool out of connections (`pool timed out`).
pub fn is_transient_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "database is locked",
        "database table is locked",
        "sqlite_busy",
        "pool timed out",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

/// [`is_transient_message`] for a typed error: `PoolTimedOut` always, anything else by its text.
pub fn is_transient_error(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::PoolTimedOut) || is_transient_message(&error.to_string())
}

/// Runs `op`, retrying with a short bounded backoff while it fails with a transient error. Any
/// other error, or the last attempt's, is returned as it is. `op` must be safe to repeat.
pub async fn retry_transient<T, F, Fut>(mut op: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    let mut pauses = BACKOFF_MS.iter();
    loop {
        match op().await {
            Err(error) if is_transient_error(&error) => match pauses.next() {
                Some(ms) => tokio::time::sleep(Duration::from_millis(*ms)).await,
                None => return Err(error),
            },
            other => return other,
        }
    }
}

/// The feed kind to write for a failure whose reason is `reason`: a transient database fault turns
/// the three danger kinds of these writers into their `_held` twin; anything else keeps `kind`.
pub fn feed_kind_for(kind: &'static str, reason: &str) -> &'static str {
    if !is_transient_message(reason) {
        return kind;
    }
    match kind {
        "worktree_provision_failed" => "worktree_provision_held",
        "worktree_gate_failed" => "worktree_gate_held",
        "work_tree_root_start_failed" => "work_tree_root_start_held",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn transient_messages_are_recognised() {
        for text in [
            "error returned from database: (code: 5) database is locked",
            "pool timed out while waiting for an open connection",
            "could not read scope verification ticket 7: pool timed out while waiting",
            "SQLITE_BUSY",
            "database table is locked",
        ] {
            assert!(is_transient_message(text), "{text}");
        }
        for text in ["no space left on device", "branch main not found", ""] {
            assert!(!is_transient_message(text), "{text}");
        }
        assert!(is_transient_error(&sqlx::Error::PoolTimedOut));
        assert!(!is_transient_error(&sqlx::Error::RowNotFound));
    }

    #[test]
    fn a_transient_reason_picks_the_held_kind() {
        let locked = "database is locked";
        assert_eq!(
            feed_kind_for("worktree_provision_failed", locked),
            "worktree_provision_held"
        );
        assert_eq!(
            feed_kind_for(
                "worktree_gate_failed",
                "worktree gate errored: could not read scope verification ticket 3: pool timed out"
            ),
            "worktree_gate_held"
        );
        assert_eq!(
            feed_kind_for("work_tree_root_start_failed", locked),
            "work_tree_root_start_held"
        );
        assert_eq!(
            feed_kind_for("worktree_provision_failed", "disk full"),
            "worktree_provision_failed"
        );
        assert_eq!(feed_kind_for("other_kind", locked), "other_kind");
    }

    #[tokio::test]
    async fn retry_recovers_from_a_transient_error() {
        let calls = AtomicUsize::new(0);
        let result = retry_transient(|| async {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(sqlx::Error::PoolTimedOut)
            } else {
                Ok(7)
            }
        })
        .await;
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_does_not_repeat_a_permanent_error() {
        let calls = AtomicUsize::new(0);
        let result: Result<(), _> = retry_transient(|| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(sqlx::Error::RowNotFound)
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_gives_up_after_the_backoff_is_spent() {
        let calls = AtomicUsize::new(0);
        let result: Result<(), _> = retry_transient(|| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(sqlx::Error::PoolTimedOut)
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1 + BACKOFF_MS.len());
    }
}

/// Parking a run whose every account is at its usage limit: when it wakes, and the wait itself.
/// (Lives here, not in a file of its own, so the module map needs no new row.)
pub(in crate::execution::runs) mod account_park {
    use chrono::{DateTime, Duration, Utc};

    /// Most accounts one run walks through after limit hits, and most times it parks for a reset.
    pub(in crate::execution::runs) const MAX_LIMIT_SWITCHES: u32 = 8;
    pub(in crate::execution::runs) const MAX_LIMIT_PARKS: u32 = 4;
    /// Slack past a provider's reset time before the parked run asks for an account again.
    const PARK_MARGIN: Duration = Duration::seconds(60);
    /// How long a park waits when no reset time is known.
    const PARK_FALLBACK: Duration = Duration::minutes(30);
    /// A parked run is silent; the liveness sweep must see a sign of life at least this often.
    const PARK_HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(120);

    /// When a run parked at `now` wakes: the earliest reset plus a margin, else the fallback.
    pub(in crate::execution::runs) fn park_until(
        resets_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> DateTime<Utc> {
        resets_at.map_or(now + PARK_FALLBACK, |at| (at + PARK_MARGIN).max(now))
    }

    /// Parks a run whose every account is at its limit: says so (feed row and transcript line, so the
    /// run stays visibly "running, waiting") and sleeps until `until`. The run row stays `running`;
    /// a cancel aborts the task, which ends the sleep. The transcript and attempt clock are touched
    /// every `PARK_HEARTBEAT` so the liveness sweep does not read the wait as a stall.
    pub(in crate::execution::runs) async fn park_for_account_limit(
        pool: &sqlx::SqlitePool,
        id: i64,
        project_id: Option<&str>,
        transcript: &std::sync::Arc<std::sync::Mutex<String>>,
        engine: &str,
        until: DateTime<Utc>,
    ) {
        let notice = format!(
            "every {engine} account is at its usage limit; run {id} is parked until {}",
            until.to_rfc3339()
        );
        let _ = crate::feed::append(
            pool,
            project_id,
            "run_parked",
            &notice,
            Some(id),
            Some(&crate::feed::run_subject(pool, id).await),
        )
        .await;
        loop {
            let left = until - Utc::now();
            if left <= Duration::zero() {
                return;
            }
            if let Ok(mut shared) = transcript.lock() {
                shared.push_str(&format!("[parked] {notice}\n"));
            }
            crate::execution::runner::cli_pids::begin_attempt(id);
            tokio::time::sleep(left.to_std().unwrap_or_default().min(PARK_HEARTBEAT)).await;
        }
    }

    /// What a run asks of the pool after a limit hit, and what it gets back.
    pub(in crate::execution::runs) struct Relaunch {
        pub account: crate::execution::runs::spawn::HeldAccount,
        pub env: Vec<(String, String)>,
    }

    /// How many times one run has switched accounts and parked after limit hits.
    #[derive(Default)]
    pub(in crate::execution::runs) struct LimitCounts {
        pub switches: u32,
        pub parks: u32,
    }

    /// A finished attempt of a run holding `account` failed: if its output is a usage-limit error,
    /// marks the account exhausted (a limit error never gets a blind retry on it), frees its slot and
    /// finds the account the run relaunches on, cold, parking until the earliest reset when none has
    /// room. `Ok(None)`: not a limit error (the account stays held), or switches/parks ran out (the
    /// account is gone and the limit error stands as the run's failure). `Err(())`: the run was
    /// already failed (a pinned account cannot take work).
    #[allow(clippy::too_many_arguments)]
    pub(in crate::execution::runs) async fn relaunch_after_limit(
        pool: &sqlx::SqlitePool,
        id: i64,
        project_id: Option<&str>,
        transcript: &std::sync::Arc<std::sync::Mutex<String>>,
        account: &mut Option<crate::execution::runs::spawn::HeldAccount>,
        (stdout, stderr): (&str, &str),
        base_env: &[(String, String)],
        counts: &mut LimitCounts,
    ) -> Result<Option<Relaunch>, ()> {
        use crate::accounts::{adapters, pool as accounts_pool};
        let Some(held) = account.as_ref() else {
            return Ok(None);
        };
        let Some(hit) = adapters::adapter_for(&held.engine)
            .and_then(|adapter| adapter.classify_failure(stdout, stderr))
        else {
            return Ok(None);
        };
        let now = Utc::now();
        let outcome = accounts_pool::exhaust_and_repick(
            pool,
            &accounts_pool::PoolState::global(),
            project_id,
            held.reservation.account(),
            &held.engine,
            &hit,
            &crate::accounts::settings::current(),
            now,
        )
        .await;
        let engine = held.engine.clone();
        // Free the exhausted account's slot before asking for another.
        *account = None;
        let mut wake_at = match outcome {
            accounts_pool::Repick::Switch(_) => None,
            accounts_pool::Repick::AllExhausted { resets_at } => Some(park_until(resets_at, now)),
        };
        while counts.switches < MAX_LIMIT_SWITCHES && counts.parks < MAX_LIMIT_PARKS {
            if let Some(at) = wake_at.take() {
                counts.parks += 1;
                park_for_account_limit(pool, id, project_id, transcript, &engine, at).await;
            }
            // Cold: no resumed session, so a stuck session cannot block the new account.
            match crate::execution::runs::spawn::account_for_launch(
                pool,
                id,
                None,
                base_env.to_vec(),
                project_id,
            )
            .await?
            {
                (Some(account), env) => {
                    counts.switches += 1;
                    return Ok(Some(Relaunch { account, env }));
                }
                // Still nobody with room: wait for the next reset.
                (None, _) => wake_at = Some(park_until(None, Utc::now())),
            }
        }
        Ok(None)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use chrono::TimeZone;

        #[test]
        fn a_known_reset_wakes_the_run_after_it_plus_the_margin() {
            let now = Utc.with_ymd_and_hms(2026, 11, 15, 10, 0, 0).unwrap();
            let reset = now + Duration::hours(2);
            assert_eq!(park_until(Some(reset), now), reset + PARK_MARGIN);
        }

        #[test]
        fn an_unknown_reset_waits_the_fallback() {
            let now = Utc.with_ymd_and_hms(2026, 11, 15, 10, 0, 0).unwrap();
            assert_eq!(park_until(None, now), now + PARK_FALLBACK);
        }

        #[test]
        fn a_reset_already_past_does_not_wake_in_the_past() {
            let now = Utc.with_ymd_and_hms(2026, 11, 15, 10, 0, 0).unwrap();
            assert_eq!(park_until(Some(now - Duration::hours(1)), now), now);
        }
    }
}
