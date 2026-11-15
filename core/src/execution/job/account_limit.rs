//! A job's node that died on an account's usage limit: switch to another account now, or park
//! until the earliest reset. Never a blind retry against the same exhausted account.

use super::*;
use crate::accounts::AccountId;
use crate::accounts::choose::{Decision, SessionView};
use crate::accounts::pool::{Holder, PickRequest, PoolState, Repick, exhaust_and_repick};

/// What the reason column says while every account of the job's engine is at its limit.
///
/// One word, like the other wait reasons, because `park` says it to the feed only when the reason
/// changes; the resume time lives in the detail.
pub(super) const WAIT_ACCOUNT_LIMIT: &str = "account-limit";

/// Added to the earliest reset before the job starts again, so a node does not launch in the
/// second the window opens and meet a provider that has not caught up.
pub(super) const RESUME_MARGIN: chrono::Duration = chrono::Duration::seconds(60);

/// What the pool says about a failed node.
#[derive(Debug, PartialEq)]
pub(super) enum LimitOutcome {
    /// The run did not die on an account limit (or its account is unknown): not ours to judge.
    NotALimit,
    /// The account was marked exhausted and another one has room: run again now, cold, there.
    Switch(AccountId),
    /// Every account is at its limit: the job's brake parks it until `resets_at` plus the margin.
    AllExhausted { resets_at: Option<DateTime<Utc>> },
}

/// The engine a run ran on: the router's `runs.runner`, else the active one.
fn engine_or_active(runner: Option<String>) -> String {
    runner.unwrap_or_else(|| {
        crate::config::models_config_now()
            .active_runner()
            .to_owned()
    })
}

/// Reads a failed run's output for an account limit and, when there is one, marks that account
/// exhausted (memory, store and the once-per-window feed row) and re-picks.
pub(super) async fn note_limit_of_run(
    pool: &SqlitePool,
    job: &JobRow,
    run_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<LimitOutcome> {
    let row: Option<(Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT runner, account_id, stdout FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await?;
    let Some((runner, Some(account), Some(stdout))) = row else {
        return Ok(LimitOutcome::NotALimit);
    };
    let engine = engine_or_active(runner);
    let Some(adapter) = crate::accounts::adapters::adapter_for(&engine) else {
        return Ok(LimitOutcome::NotALimit);
    };
    let Some(hit) = adapter.classify_failure(&stdout, "") else {
        return Ok(LimitOutcome::NotALimit);
    };
    let outcome = exhaust_and_repick(
        pool,
        &PoolState::global(),
        Some(job.project_id.as_str()),
        &AccountId(account),
        &engine,
        &hit,
        &crate::accounts::settings::current(),
        now,
    )
    .await;
    Ok(match outcome {
        Repick::Switch(next) => LimitOutcome::Switch(next),
        Repick::AllExhausted { resets_at } => LimitOutcome::AllExhausted { resets_at },
    })
}

/// When every account of the job's engine is at its limit: the earliest moment one is usable again
/// (`None` inside when no reset is known). `None` when some account can take the node, or the
/// pool holds none for the engine, which is today's launch.
pub(super) fn all_accounts_exhausted(now: DateTime<Utc>) -> Option<Option<DateTime<Utc>>> {
    let engine = engine_or_active(None);
    let adapter = crate::accounts::adapters::adapter_for(&engine)?;
    let req = PickRequest {
        engine,
        provider: adapter.provider().to_owned(),
        session: SessionView::default(),
        holder: Holder::Chat(String::new()),
        portability: adapter.session_portability(),
        cache_model: adapter.cache_model(),
    };
    // The margin is applied by asking the pool a little in the past: a mark that resets at T still
    // holds at T + margin - 1s, so the job resumes at T + margin.
    match PoolState::global().decide(
        &req,
        &crate::accounts::settings::current(),
        now - RESUME_MARGIN,
    ) {
        Decision::AllExhausted { earliest_resets_at } => {
            Some(earliest_resets_at.map(|at| at + RESUME_MARGIN))
        }
        _ => None,
    }
}

/// The brake's verdict on `all_accounts_exhausted`: park (with the resume time in the detail), or
/// stop when the reset falls beyond the job's own lifetime.
pub(super) fn account_limit_brake(job: &JobRow, now: DateTime<Utc>) -> Option<Brake> {
    let resets_at = all_accounts_exhausted(now)?;
    let beyond_the_job = resets_at.is_some_and(|reset| {
        DateTime::parse_from_rfc3339(&job.created_at)
            .map(|created| reset >= created.with_timezone(&Utc) + lifetime_ceiling(job))
            .unwrap_or(false)
    });
    Some(match resets_at {
        Some(reset) if beyond_the_job => Brake::Stop {
            detail: format!(
                "every account is at its usage limit and the first resets at {}, after this job's \
                 lifetime",
                reset.to_rfc3339()
            ),
        },
        Some(reset) => Brake::Park {
            reason: WAIT_ACCOUNT_LIMIT,
            detail: format!(
                "every account is at its usage limit; the job resumes at {}",
                reset.to_rfc3339()
            ),
        },
        None => Brake::Park {
            reason: WAIT_ACCOUNT_LIMIT,
            detail: "every account is at its usage limit and no reset time is known".to_owned(),
        },
    })
}
