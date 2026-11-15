//! Parking a run whose every account is at its usage limit: when it wakes, and the wait itself.

use chrono::{DateTime, Duration, Utc};

/// Most accounts one run walks through after limit hits, and most times it parks for a reset.
pub(super) const MAX_LIMIT_SWITCHES: u32 = 8;
pub(super) const MAX_LIMIT_PARKS: u32 = 4;
/// Slack past a provider's reset time before the parked run asks for an account again.
const PARK_MARGIN: Duration = Duration::seconds(60);
/// How long a park waits when no reset time is known.
const PARK_FALLBACK: Duration = Duration::minutes(30);
/// A parked run is silent; the liveness sweep must see a sign of life at least this often.
const PARK_HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(120);

/// When a run parked at `now` wakes: the earliest reset plus a margin, else the fallback.
pub(super) fn park_until(resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> DateTime<Utc> {
    resets_at.map_or(now + PARK_FALLBACK, |at| (at + PARK_MARGIN).max(now))
}

/// Parks a run whose every account is at its limit: says so (feed row and transcript line, so the
/// run stays visibly "running, waiting") and sleeps until `until`. The run row stays `running`;
/// a cancel aborts the task, which ends the sleep. The transcript and attempt clock are touched
/// every `PARK_HEARTBEAT` so the liveness sweep does not read the wait as a stall.
pub(super) async fn park_for_account_limit(
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
pub(super) struct Relaunch {
    pub account: super::HeldAccount,
    pub env: Vec<(String, String)>,
}

/// How many times one run has switched accounts and parked after limit hits.
#[derive(Default)]
pub(super) struct LimitCounts {
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
pub(super) async fn relaunch_after_limit(
    pool: &sqlx::SqlitePool,
    id: i64,
    project_id: Option<&str>,
    transcript: &std::sync::Arc<std::sync::Mutex<String>>,
    account: &mut Option<super::HeldAccount>,
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
        match super::account_for_launch(pool, id, None, base_env.to_vec(), project_id).await? {
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
