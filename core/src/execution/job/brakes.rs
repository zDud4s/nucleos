//! What stops a job that is still allowed to run: budget, lifetime and the other brakes.

use super::*;

/// Whether the brakes let this job start another node right now.
pub(super) enum Brake {
    Go,
    /// Comes back on its own; park and try again.
    Park {
        reason: &'static str,
        detail: String,
    },
    /// Will not come back inside this job's life; stop and hand back what is on the branch.
    Stop {
        detail: String,
    },
}

/// Whether starting one more node would take this job past its own allowance, and how to say so.
///
/// Asks about the NEXT node, never the one in flight: the reserve is what a node is expected to cost,
/// so the test is "would starting another cross the line" rather than "has the line been crossed".
/// Killing a node mid-flight would throw away what it has already spent and leave the tree in a state
/// no gate has measured — the identical decision the global budget took on 2026-07-20, for the
/// identical reason.
pub(super) async fn job_over_budget(
    pool: &SqlitePool,
    job: &JobRow,
    limit: f64,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<String>> {
    let spent = crate::governance::budget::job_spend(pool, job.id, now).await?;
    let reserve = crate::governance::budget::load_budget_config(pool)
        .await?
        .per_run_reserve_usd;
    if spent + reserve <= limit {
        return Ok(None);
    }
    Ok(Some(format!(
        "this job has spent ${spent:.2} of its ${limit:.2} allowance, and the next node reserves \
         ${reserve:.2}"
    )))
}

pub(super) async fn brakes(state: &AppState, job: &JobRow, now: DateTime<Utc>) -> Brake {
    // Re-read per node rather than once per pass. A pass can spend a whole gate timeout inside one
    // job, and a stop that keeps starting work for another fifteen minutes is not a stop. Fails
    // closed: a switch that cannot be read holds the job rather than releasing it.
    match crate::governance::autopilot::kill_switch_engaged(&state.pool).await {
        Ok(false) => {}
        _ => {
            return Brake::Park {
                reason: "kill-switch",
                detail: "the emergency stop is engaged".to_owned(),
            };
        }
    }

    // **The house budget paces PROACTIVE autonomy, and a job the owner asked for with an allowance
    // of its own is not that.** The same argument `http::create_job` already makes about the scoped
    // kills, the budget and the WIP limit on the way in — "a person asking for a job through the
    // shell or the Telegram assistant is not that" — and it stopped at the door. Past it, the daily
    // and hourly house limits governed a job somebody had explicitly commissioned and given a
    // number to, which is the owner's spend decision being overruled by the daemon's own pacing.
    //
    // Measured 2026-08-30: job 23 was created for $25, spent $10.36 doing real work, and the next
    // node was about to be parked by an $8/hour house limit — a limit whose whole purpose is to
    // stop the daemon spending fast on things nobody asked for.
    //
    // **The exemption is conditional on there being another bound, and that is the whole of its
    // safety.** A requested job with no `budget_usd` of its own has nothing else holding it, so it
    // keeps the house limits exactly as before; the ceiling it escapes is replaced by the one the
    // owner named, never by nothing. A life ceiling bounds both cases regardless — a different one
    // each, chosen by this same predicate, which is why the predicate lives on `JobRow`.
    if !job.commissioned_with_an_allowance() {
        // Decision 9. Which of the two limits fired decides whether waiting is patience or a hang.
        match crate::governance::budget::budget_permits_new_run(&state.pool, now).await {
            crate::governance::budget::BudgetDecision::Allow => {}
            crate::governance::budget::BudgetDecision::Pause {
                reason,
                kind: crate::governance::budget::PauseKind::Transient,
                source,
            } => {
                return Brake::Park {
                    reason: source,
                    detail: reason,
                };
            }
            crate::governance::budget::BudgetDecision::Pause {
                reason,
                kind: crate::governance::budget::PauseKind::Window,
                ..
            } => return Brake::Stop { detail: reason },
        }
    }

    match crate::quota::quota_permits_new_run(
        &state.pool,
        Some(&state.quota),
        &state.quota.provider,
        now,
    )
    .await
    {
        crate::quota::QuotaDecision::Allow => {}
        crate::quota::QuotaDecision::Pause { reason, resets_at }
            if resets_at.is_some_and(|reset| {
                DateTime::parse_from_rfc3339(&job.created_at)
                    .map(|created| reset < created.with_timezone(&Utc) + lifetime_ceiling(job))
                    .unwrap_or(false)
            }) =>
        {
            return Brake::Park {
                reason: crate::quota::PAUSE_SOURCE,
                detail: reason,
            };
        }
        crate::quota::QuotaDecision::Pause { reason, .. } => return Brake::Stop { detail: reason },
    }

    // The job's own ceiling — under the house's for work nobody asked for, and INSTEAD of it for
    // work somebody did. Two different questions and both worth asking: a job can be stopped by
    // what it was given or by what is left in the till.
    //
    // `Stop` and never `Park`, which is the whole difference from the window brake above. A calendar
    // window reopens; a task's allowance does not, and a job parked on it would sit there until the
    // four-hour ceiling swept it up with `waiting` on its row and no reason a reader could act on.
    if let Some(limit) = job.budget_usd {
        match job_over_budget(&state.pool, job, limit, now).await {
            // Fails CLOSED, like every other brake here: a spend that cannot be read stops the job
            // rather than letting it keep spending against a number nobody could check.
            Err(error) => {
                return Brake::Stop {
                    detail: format!("this job's spend could not be read: {error}"),
                };
            }
            Ok(Some(detail)) => return Brake::Stop { detail },
            Ok(None) => {}
        }
    }

    // Somebody asked that this job and another not run at the same time, and the other one is
    // running. `Park` and never `Stop`: the reason lifts by itself the moment the partner ends, and
    // stopping would throw away a job for a wait measured in minutes.
    //
    // Placed after the budget and before the attention check, and the order is what the person
    // reads. Two of these can be true at once — the owner at the keyboard AND a partner holding a
    // slot — and "job 42 is running and the two are excluded" is the more useful of the two,
    // because it names something that will resolve on its own and says what to watch for.
    //
    // Fails CLOSED like its neighbours, and the choice is nearly moot: a pool that cannot answer
    // this could not answer the kill switch at the top of the chain either, so the job would already
    // be parked before reaching here.
    match crate::governance::exclusion::blocking_partner(&state.pool, job.id).await {
        Ok(None) => {}
        Ok(Some(partner)) => {
            return Brake::Park {
                reason: "excluded",
                detail: format!("job {partner} is running and the two are excluded"),
            };
        }
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not read this job's exclusions");
            return Brake::Park {
                reason: "excluded",
                detail: format!("this job's exclusions could not be read: {error}"),
            };
        }
    }

    // Decision 10, and the whole of what it adds: the brake was a check made once at admission, and
    // a job admitted at 03:00 could otherwise keep starting nodes at 08:00 with the owner at the
    // keyboard. The item in flight is never killed — its gate has already run or is about to — so
    // this only ever refuses to start the NEXT one.
    // Switched off per project in the project's `autopilot.yaml`, and read HERE rather than once at
    // admission for the same reason decision 10 gives about the check itself: a job admitted under
    // one setting must not go on starting nodes under it after the owner changed their mind.
    //
    // A rules file that cannot be read leaves the brake ON. That is the direction every other arm
    // of this chain fails in, and the asymmetry is the argument: being wrong this way costs a job
    // that waits, and being wrong the other way starts a node in a worktree somebody is using.
    let brake_applies = match crate::config::load_schedule_rules(
        state.machine_config_root.as_deref(),
        &job.project_id,
    ) {
        Ok(rules) => rules.attention_brake(),
        Err(error) => {
            tracing::warn!(
                job_id = job.id,
                %error,
                "could not read this project's autopilot rules; keeping the attention brake on"
            );
            true
        }
    };
    if !brake_applies {
        return Brake::Go;
    }

    match crate::attention::attention_permits_new_run(
        &state.pool,
        &state.run_handles,
        &job.project_id,
        now,
    )
    .await
    {
        crate::attention::AttentionDecision::Allow => Brake::Go,
        crate::attention::AttentionDecision::Defer { reason, .. } => Brake::Park {
            reason: "attention",
            detail: reason,
        },
    }
}

pub(super) fn lifetime_ceiling(job: &JobRow) -> chrono::Duration {
    if job.commissioned_with_an_allowance() {
        COMMISSIONED_JOB_LIFETIME
    } else {
        MAX_JOB_LIFETIME
    }
}
