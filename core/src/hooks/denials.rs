//! What follows a denial: counting them, stopping a prober, hard denies, parks and pausing for approval.

use super::*;

/// How many denied actions a run may attempt before it is stopped.
///
/// Not one: a run that reaches for something destructive, is told no, and takes a different route
/// is behaving exactly as the gate intends, and killing it there would turn the gate into a
/// tripwire. Not unbounded either, which is what it was. Three leaves room for an honest mistake
/// and a retry, and far too little to search a grammar with.
pub(super) const DENIAL_LIMIT: i64 = 3;

/// Spec B D9: what `count_denial_and_stop_a_prober` did. `#[must_use]` is how "every caller reads
/// the enum" is held: a caller that means to ignore it has to say `let _ =`, in view.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialCount {
    /// Counted, and the run is still below `DENIAL_LIMIT` — the only case spec B's E1 may ask the
    /// judge about: the judge may stop a run BEFORE the limit, never after it.
    Counted(i64),
    /// The allowance is spent. Returned whenever `denials >= DENIAL_LIMIT`, even when
    /// `finalize_termination` lost its race to another terminator.
    Stopped,
    /// The count could not be written (the SQLITE_BUSY arm). The action is still refused.
    NotCounted,
}

/// Records a denied attempt and, once a run has spent its allowance, stops it.
///
/// Terminated to `failed` rather than `awaiting_approval`: a denied action is destructive by
/// classification, and the pause path exists to make an action approvable. Offering a human an
/// "approve" button here would launder precisely the verdict that is supposed to be final — the
/// class-scoped grant deliberately only ever lifts a `pending_approval`.
pub(super) async fn count_denial_and_stop_a_prober(
    state: &AppState,
    run_id: i64,
    tool_name: &str,
) -> DenialCount {
    let denials: i64 = match sqlx::query_scalar(
        "UPDATE runs SET denials = denials + 1 WHERE id = ? RETURNING denials",
    )
    .bind(run_id)
    .fetch_one(&state.pool)
    .await
    {
        Ok(count) => count,
        // The action is still denied — that part never depended on this write. Only the allowance
        // does, and the pool this shares with feed appends and status writes makes SQLITE_BUSY an
        // ordinary event here. Sustained probing is many calls, of which lost counts are a
        // minority; treating one lost count as a reason to kill the run would make a busy database
        // indistinguishable from an attack.
        Err(error) => {
            tracing::warn!(run_id, %error, "could not count a denied action against the run");
            return DenialCount::NotCounted;
        }
    };

    tracing::warn!(
        run_id,
        tool = %tool_name,
        denials,
        "pretooluse-decision: denied action {denials}/{DENIAL_LIMIT} for this run"
    );
    if denials < DENIAL_LIMIT {
        return DenialCount::Counted(denials);
    }

    // Spawned and then awaited, for the same reason `pause_for_approval` is: terminating the run
    // kills the CLI whose hook script owns the connection this handler is answering, so the request
    // can vanish mid-handler and take an inline continuation with it.
    let state = state.clone();
    let tool_name = tool_name.to_owned();
    let _ = tokio::spawn(async move {
        if !finalize_termination(&state, run_id, "failed").await {
            return;
        }
        let _ = crate::feed::append(
            &state.pool,
            None,
            "run_stopped_probing",
            &format!("run {run_id} was stopped after {denials} denied actions (last: {tool_name})"),
            Some(run_id),
            Some(&crate::feed::run_subject(&state.pool, run_id).await),
        )
        .await;
    })
    .await;
    DenialCount::Stopped
}

/// Spec B D8: the judge stops a run by the road the denial limit already uses — the termination in
/// a task of its own and awaited, for `count_denial_and_stop_a_prober`'s reason, and the caller
/// then answers `deny`. The terminal write of `spawn_run` loses its CAS, so no E4 is born. A
/// proven pattern; a new run status would have to be taught to the worktree GC and its test.
pub(super) async fn stop_by_judge(
    state: &AppState,
    run_id: i64,
    project_id: Option<&str>,
    phrase: String,
) {
    let state = state.clone();
    let project_id = project_id.map(str::to_owned);
    let _ = tokio::spawn(async move {
        if !finalize_termination(&state, run_id, "failed").await {
            return;
        }
        let summary = format!("run {run_id} was stopped ({phrase})");
        let subject = crate::feed::run_subject(&state.pool, run_id).await;
        let _ = crate::feed::append(
            &state.pool,
            project_id.as_deref(),
            "run_stopped_by_judge",
            &summary,
            Some(run_id),
            Some(&subject),
        )
        .await;
    })
    .await;
}

/// Spec B E1 in enforce (D3, D7, D8, D9). Reached only on `DenialCount::Counted`, so the judge may
/// stop a run BEFORE the denial limit and never after it. The answer to the hook is the
/// classifier's `deny` whatever comes of this (D3: "a hard refusal keeps blocking the action"):
/// the judge may only stop the run, tell the owner, or leave today's refusal — never an allow.
///
/// When the resolver stands aside (a resolution lineage, or a lineage read that failed or ran out
/// of time) nothing is written and today's refusal stands: D3 gives E1 the `deny` as its default,
/// and parking for a refusal nobody can approve would only end the run.
pub(super) async fn resolve_hard_deny(
    state: &AppState,
    run_id: i64,
    project_id: Option<&str>,
    action_class: &str,
    asked: &crate::judge::resolve::Asked,
    wait: Duration,
) {
    use crate::judge::resolve::{Event, Outcome};
    let Some(row) =
        crate::judge::resolve::ask_unless_resolution(&state.pool, &state.judge, asked, wait).await
    else {
        return;
    };
    // D3: E1's outcomes are deny, warn and stop, and nothing else, whatever the rule returned. No
    // opinion (a failure, the deadline, a missing probability) is today's refusal (D1).
    let applied = row
        .judge_outcome
        .filter(|outcome| Event::HardDeny.outcomes().contains(outcome))
        .unwrap_or(Outcome::Deny);
    let phrase = crate::judge::resolve::phrase(applied, row.p);
    let enforced = applied != Outcome::Deny;
    // Written BEFORE the stop, as `resolve_park` does (D13 cost, D11 review queue): the stop kills
    // the CLI that holds this request's connection, which can drop this handler future mid-call, and
    // a row written after it would be lost with its cost.
    crate::judge::resolve::record_later(&state.pool, row.settled(applied, enforced));
    match applied {
        Outcome::Stop => stop_by_judge(state, run_id, project_id, phrase).await,
        // D3: "contacting the owner" at E1 is a line and never a pause; the run goes on.
        Outcome::Warn => {
            let summary = format!(
                "run {run_id} was refused a {action_class} action its task seems to need ({phrase})"
            );
            let subject = crate::feed::run_subject(&state.pool, run_id).await;
            let _ = crate::feed::append(
                &state.pool,
                project_id,
                "judge_needs_owner",
                &summary,
                Some(run_id),
                Some(&subject),
            )
            .await;
        }
        _ => {}
    }
}

/// Spec B E3 in enforce (D3, D5, D8): what the resolver's answer at a park comes to. `Some` is the
/// hook's answer, always a `deny`: the run is stopped, or refused without parking (D5's redirect).
/// `None` is the park, as today — the default whenever the judge had no opinion, said "park", or
/// said "explain" where D5 does not allow a redirect. Never an allow.
pub(super) async fn resolve_park(
    state: &AppState,
    payload: &PreToolUsePayload,
    classification: &classifier::Classification,
    run: &RunSnapshot<'_>,
    row: crate::judge::resolve::ResolutionRow,
    wait: Option<Duration>,
) -> Option<Decision> {
    use crate::judge::resolve::{Event, Outcome, REDIRECTS_PER_LINEAGE};
    // D3: E3's outcomes are explain, park and stop, never a correction, whatever the rule returned.
    let opinion = row
        .judge_outcome
        .filter(|outcome| Event::Park.outcomes().contains(outcome))
        .unwrap_or(Outcome::Park);
    // D5: never for what spec A may never approve (the ONE predicate, `judge_may_allow`), and
    // never past the lineage's ceiling. The ceiling's read is bounded by what is left of the hook's
    // budget; a read that fails or runs out counts as the ceiling spent, and the run parks. So does
    // a budget already spent (`wait` is `None`): no read is attempted and the ceiling is taken as
    // reached, never a fresh floor of time.
    let may_redirect = opinion == Outcome::Explain
        && crate::judge::judge_may_allow(
            &payload.tool_name,
            &payload.tool_input,
            Some(Path::new(run.cwd)),
            classification.action_class,
        )
        && match wait {
            None => false,
            Some(wait) => {
                tokio::time::timeout(
                    wait,
                    crate::judge::resolve::redirects_in_lineage(&state.pool, run.lineage_root),
                )
                .await
                .unwrap_or(REDIRECTS_PER_LINEAGE)
                    < REDIRECTS_PER_LINEAGE
            }
        };
    let applied = crate::judge::resolve::applied_park(opinion, may_redirect);
    let phrase = crate::judge::resolve::phrase(applied, row.p);
    let enforced = applied != Outcome::Park;
    // D13: written off the response path. D5's ceiling is soft for exactly this reason: the next
    // call may count before this row lands, and the excess is one more refusal, never an action.
    crate::judge::resolve::record_later(&state.pool, row.settled(applied, enforced));
    match applied {
        Outcome::Stop => {
            stop_by_judge(state, run.run_id, run.project_id, phrase.clone()).await;
            Some(Decision {
                decision: "deny".to_owned(),
                reason: format!("{} ({phrase})", classification.reason),
            })
        }
        Outcome::Explain => {
            // Not counted (D5), like a refusal of spec A's judge over a park (A D7). Written down
            // only if none is open: `record_refused_action` swallows the one-open-per-run index.
            record_refused_action(
                state,
                payload,
                &payload.tool_name,
                // The owner reads this to decide whether to do it themselves, so it carries why the
                // classifier wanted approval (spec B D5), after the fixed sentence.
                &format!(
                    "{} Needed approval because: {}",
                    crate::execution::runs::CONTINUING_WITHOUT_IT,
                    classification.reason
                ),
            )
            .await;
            Some(Decision {
                decision: "deny".to_owned(),
                reason: crate::execution::runs::CONTINUING_WITHOUT_IT.to_owned(),
            })
        }
        _ => None,
    }
}

/// The whole `pending_approval` act: terminate the run, then record the proposal that makes the
/// pause actionable. These two belong together — a run parked in `awaiting_approval` with no
/// proposal can be neither approved nor rejected, and it goes on holding one of the project's
/// concurrency slots, permanently: the sweep spares `awaiting_approval`, and startup recovery only
/// reconciles rows left `running`. Hence the caller runs this as a detachable task rather than
/// inline in a request that may not survive its own side effects.
pub(super) async fn pause_for_approval(
    state: AppState,
    run_id: i64,
    tool_name: String,
    tool_input: String,
    reason: String,
) {
    // Active termination (spec §8.4 steps 2–3): drive the run to `awaiting_approval` via the same
    // atomic-handle-removal arbiter cancellation uses (`finalize_termination`, Chunk 2 Task 4).
    if !finalize_termination(&state, run_id, "awaiting_approval").await {
        return;
    }

    let (session_id, project_id, job_id, stage) =
        sqlx::query_as::<_, (Option<String>, Option<String>, Option<i64>, Option<String>)>(
            "SELECT session_id, project_id, job_id, stage FROM runs WHERE id = ?",
        )
        .bind(run_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .unwrap_or((None, None, None, None));

    // A node of a job takes the other road entirely, and three shapes of node take three roads.
    //
    // An **item's** node is put down and the job carries on to the next item. A **review** node owns
    // no item and does not need one: its verdict is advisory — §5.5 gives ship/no-ship to the gate —
    // and everything it was going to read is already written, gated and checkpointed. A **replan**
    // node owns none either, and ending it hands the job to `stop_after_replan`, which stops it
    // `Stopped` rather than `Failed` precisely so the rounds that DID run stay worth looking at.
    //
    // The rule underneath all three: **a node that asks already has an ending written for it**, and
    // the job's own handler picks it. What parking adds is not safety — it is a job that reads live,
    // holds a concurrency slot and does nothing, with no sign anywhere until a person goes looking.
    //
    // Measured, not reasoned into. Chunk 2 put review and plan on one road with a single sentence,
    // and four jobs falsified it in a day: 12 parked its review on `git reflog`, 13 parked its own
    // on a `for` loop, and 14 and 16 both parked their REPLAN nodes after every item had passed.
    //
    // The **plan** node is the one that still parks, and the argument for it is the one Chunk 2
    // made: the queue is what it produces, so there is nothing partial to preserve and nothing to
    // carry on to. Ending it would fail a job that a single answer turns into a night's work, to
    // save a slot that is now bounded by a ceiling rather than blocking the project.
    //
    // The mark is what decides for an item, and it is also step (2) of the skip — see
    // `skip_the_item`. Doing it here rather than inside keeps "did this run own an item?" and "put
    // it down" as one write: asking first and marking after would be a race with the same
    // cancel/reconcile the mark is already guarded against.
    //
    // Read HERE and not before the termination above, deliberately: `job_id` does not change when a
    // run ends, and moving the read earlier would put an extra query on the hot path of every
    // ordinary run's hook, which is nearly all of them.
    let road = match job_id {
        Some(job_id) if put_the_item_down(&state.pool, job_id, run_id).await => {
            Some((job_id, true))
        }
        Some(job_id) if NODES_THAT_GIVE_UP.contains(&stage.as_deref().unwrap_or_default()) => {
            Some((job_id, false))
        }
        _ => None,
    };
    if let Some((job_id, had_an_item)) = road {
        skip_the_item(
            state,
            SkippedItem {
                run_id,
                job_id,
                session_id,
                project_id,
                tool_name,
                tool_input,
                reason,
                had_an_item,
            },
        )
        .await;
        return;
    }

    let recorded = crate::proposals::create_action_approval(
        &state.pool,
        run_id,
        session_id.as_deref(),
        project_id.as_deref(),
        &tool_name,
        &reason,
        Some(&tool_input),
    )
    .await;

    if let Ok(proposal_id) = &recorded {
        advise_on_proposal(&state, *proposal_id, &tool_name, &reason, &tool_input);
    }

    if let Err(error) = recorded {
        tracing::warn!(
            run_id,
            %error,
            "pretooluse-decision: failed to record action-approval proposal"
        );
        let _ = crate::feed::append(
            &state.pool,
            project_id.as_deref(),
            "proposal_record_failed",
            &format!("failed to record action-approval proposal: {error}"),
            Some(run_id),
            Some(&crate::feed::run_subject(&state.pool, run_id).await),
        )
        .await;

        // Warning alone left the run parked in `awaiting_approval` with nothing to approve or
        // reject, holding one of the project's concurrency slots for good: the sweep spares that
        // status on purpose, because a run with a PENDING proposal is resumable.
        // `reconcile_stranded_approvals` is what tells the two apart — but only at startup, so the
        // project ran one slot narrower until someone restarted the daemon.
        //
        // Undo the pause instead. `interrupted` is the status startup recovery already uses for
        // exactly this shape, so a run that ends here reads the same either way, and the slot is
        // free immediately. Guarded on the status this function set, so a cancel that arrived in
        // the meantime keeps the last word.
        let rolled_back = sqlx::query(
            "UPDATE runs SET status = 'interrupted', completed_at = ?
             WHERE id = ? AND status = 'awaiting_approval'",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(run_id)
        .execute(&state.pool)
        .await;
        // The pause was undone: wake the runs topic (a spurious hint is harmless).
        nucleos_base::wake::bump(nucleos_base::wake::Topic::Runs);
        match rolled_back {
            Ok(result) if result.rows_affected() == 1 => tracing::warn!(
                run_id,
                "pretooluse-decision: rolled the unapprovable pause back to interrupted"
            ),
            Ok(_) => {}
            Err(rollback_error) => tracing::error!(
                run_id,
                %rollback_error,
                "pretooluse-decision: could not roll back an unapprovable pause — this project is blocked until restart"
            ),
        }
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;

    /// Rolling back an unapprovable pause wakes the runs topic.
    ///
    /// The wake bus is process-global, so the assertion is `>= 1`, never an exact delta.
    #[tokio::test]
    async fn wake_signal_pause_for_approval_bumps_runs() {
        let state = crate::channels::testkit::state().await;
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'running', 'worktree', '2026-10-19T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let task =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });
        state
            .run_handles
            .lock()
            .unwrap()
            .insert(run_id, task.abort_handle());
        // Make recording the proposal fail, so the pause is rolled back.
        sqlx::query("DROP TABLE proposal_events")
            .execute(&state.pool)
            .await
            .unwrap();

        let rx = nucleos_base::wake::subscribe(nucleos_base::wake::Topic::Runs);
        let before = *rx.borrow();

        pause_for_approval(
            state.clone(),
            run_id,
            "Bash".to_string(),
            "{}".to_string(),
            "wake test".to_string(),
        )
        .await;

        assert!(rx.borrow().wrapping_sub(before) >= 1);
    }
}
