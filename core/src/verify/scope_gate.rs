//! Applying the scoped verification gate.

use super::*;

/// What an item gate makes of a `scope` verification.
#[derive(Debug)]
pub enum ScopeVerdict {
    /// The ticket reached a verdict the gate can record.
    Measured(crate::gate::GateOutcome),
    /// No unit agreed with the work: nothing was measured, so there is nothing to bless.
    NothingRan,
    /// The request could not even be submitted (refused, no ticket): nothing was measured and
    /// nothing is queued, so the caller falls back to the measurement it made before `scope`.
    Unavailable,
}

/// Translates a ticket into the outcome an item gate records. A failed ticket carries the tail of
/// each failed unit, so the retry prompt still receives the end of the output as it does today.
pub fn scope_outcome(ticket: &Ticket) -> ScopeVerdict {
    use crate::gate::GateOutcome;

    if !ticket.done {
        return ScopeVerdict::Measured(GateOutcome::Errored {
            reason: format!(
                "scope verification did not finish in time ({} of {} units still queued)",
                ticket.progress.queued, ticket.progress.total
            ),
        });
    }
    match ticket.verdict.as_deref() {
        Some("passed") => ScopeVerdict::Measured(GateOutcome::Passed),
        Some("nothing_ran") => ScopeVerdict::NothingRan,
        Some("failed") => {
            let failed: Vec<&verify_plan::UnitReport> = ticket
                .units
                .iter()
                .filter(|unit| unit.status == "failed")
                .collect();
            let exit_code = failed
                .first()
                .and_then(|unit| unit.exit_code)
                .and_then(|code| i32::try_from(code).ok())
                .unwrap_or(1);
            let output = failed
                .iter()
                .map(|unit| {
                    let name = unit.group.clone().unwrap_or_else(|| unit.argv.join(" "));
                    format!(
                        "== {name} ({}) ==
{}",
                        unit.why,
                        unit.output_tail.as_deref().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            ScopeVerdict::Measured(GateOutcome::Failed { exit_code, output })
        }
        Some("inherited") => ScopeVerdict::Measured(GateOutcome::Errored {
            reason: "every red unit is inherited from a master incident; wait for it to close".to_owned(),
        }),
        other => ScopeVerdict::Measured(GateOutcome::Errored {
            reason: format!(
                "scope verification ended {}",
                other.unwrap_or("without a verdict")
            ),
        }),
    }
}

/// The one sentence the feed carries when a final gate (a job's last item, a run) tried `verify
/// scope` instead of the full gate, so the owner can tell which gate measured the work. `subject`
/// names what was gated ("job 3 at item 2", "run 7").
pub fn scoped_final_note(subject: &str, verdict: &ScopeVerdict) -> String {
    use crate::gate::GateOutcome;

    match verdict {
        ScopeVerdict::Measured(GateOutcome::Passed) => format!(
            "{subject} was gated by `verify scope` over its diff instead of the full gate, and it passed; the full gate runs on the target after landing (gate_after_land)"
        ),
        ScopeVerdict::Measured(GateOutcome::Failed { exit_code, .. }) => format!(
            "{subject} was gated by `verify scope` over its diff instead of the full gate, and it failed with exit code {exit_code}"
        ),
        ScopeVerdict::Measured(GateOutcome::Errored { reason }) => format!(
            "{subject} was gated by `verify scope` over its diff instead of the full gate, but `verify scope` could not measure it: {reason}"
        ),
        ScopeVerdict::NothingRan => format!(
            "{subject}: the test map selects no group for its diff, so `verify scope` measured nothing here; the full gate runs on the target after landing (gate_after_land)"
        ),
        ScopeVerdict::Unavailable => format!(
            "{subject}: `verify scope` could not be submitted, so the full gate measures it"
        ),
    }
}

/// Gates a job item over its worktree's diff from its recorded base: `scope` of kind `test`,
/// cached, at autonomous priority. A ticket that does not finish within `wait` is an `Errored`
/// outcome, never a pass; the units it queued keep running and may still fill the cache. A request
/// that cannot be submitted at all is `Unavailable`, and the caller measures the old way instead.
/// `wait` counts from the submit, so time spent queued behind other work eats into it.
pub async fn gate_job_scope(
    executor: &Arc<Executor>,
    job_id: i64,
    worktree: &Path,
    wait: Duration,
) -> ScopeVerdict {
    gate_scope(executor, Caller::Job(job_id), worktree, wait).await
}

/// The `scope` gate behind `gate_job_scope`, for any daemon-side caller that names itself: the job
/// item gate as `Caller::Job`, the merge queue's gate before publish as `Caller::Merge`. The
/// caller's label is what lands in `verify_requests.caller`.
pub async fn gate_scope(
    executor: &Arc<Executor>,
    caller: Caller,
    worktree: &Path,
    wait: Duration,
) -> ScopeVerdict {
    gate_scope_ticketed(executor, caller, worktree, wait)
        .await
        .1
}

/// `gate_scope` that also returns the ticket it submitted (`None` when it was `Unavailable`), for a
/// caller that may need to cancel the units that ticket queued.
pub async fn gate_scope_ticketed(
    executor: &Arc<Executor>,
    caller: Caller,
    worktree: &Path,
    wait: Duration,
) -> (Option<i64>, ScopeVerdict) {
    use crate::gate::GateOutcome;

    let args = VerifyArgs {
        kind: Kind::Test,
        scope: ScopeArg::Scope,
        worktree: Some(worktree.to_string_lossy().into_owned()),
        files: None,
        base: None,
        wait: false,
        cover: false,
        task_id: None,
    };
    // F3-12 (decision 3): the merge queue's integration worktree can vanish right after its ticket
    // ends, so the confirmation is awaited here as well as detached in `submit`. Idempotent. The
    // tree is read before the submit, so it is the one the ticket is about to measure.
    let before = clean_tree(worktree, Instant::now() + OPERATION_TIMEOUT)
        .await
        .ok()
        .flatten();
    let (id, map_trusted) = match submit_checked(executor, caller, &args).await {
        Ok(submitted) => submitted,
        Err(error) => {
            tracing::warn!(
                caller = %caller.label(),
                reason = error.message(),
                "scope verification could not be submitted; the caller takes the full gate"
            );
            return (None, ScopeVerdict::Unavailable);
        }
    };
    let verdict = match wait_ticket(&executor.pool, id, wait).await {
        Ok(Some((ticket, _))) => {
            // Only a map that was the root's at submit time may be vouched for later; see
            // `submit_checked`.
            if ticket.done
                && map_trusted
                && let Some(tree) = before
            {
                confirm_measured_tree(executor.pool.clone(), id, worktree.to_path_buf(), tree)
                    .await;
            }
            scope_outcome(&ticket)
        }
        Ok(None) => ScopeVerdict::Measured(GateOutcome::Errored {
            reason: format!("scope verification ticket {id} vanished"),
        }),
        Err(error) => ScopeVerdict::Measured(GateOutcome::Errored {
            reason: format!("could not read scope verification ticket {id}: {error}"),
        }),
    };
    (Some(id), verdict)
}

/// Ends the still-queued units a verification ticket created, so they stop competing with a
/// measurement that replaced them. Units the ticket joined from another request are not its own and
/// are left alone.
pub async fn cancel_queued_units(
    pool: &SqlitePool,
    ticket: i64,
    reason: &str,
) -> sqlx::Result<u64> {
    verify_runs::cancel_queued(pool, ORIGIN_VERIFY, ticket, reason).await
}
