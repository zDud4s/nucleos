//! Reverting a confirmed postgate culprit.

use super::*;

/// The feed line for a culprit whose revert has just been started: what is about to be undone, on
/// which branch, and that a correction run follows. Only the revert switch reaches it.
pub(super) fn reverting_summary(
    target: &str,
    red: &str,
    base: &str,
    culprit: &str,
    also_suspect: &[String],
) -> String {
    let suspects = if also_suspect.is_empty() {
        String::new()
    } else {
        format!(" (also suspect: {})", short_list(also_suspect))
    };
    format!(
        "post-merge gate red on {target} at {}: culprit {}{suspects}; range {}..{}; \
         reverting it on {target} through the queue, then a correction run follows on a \
         fix/ branch.",
        short(red),
        short(culprit),
        short(base),
        short(red)
    )
}

/// One action on the revert of a confirmed culprit: queue it, follow its ticket, or build the
/// correction branch once the queue has published it. Everything it needs is in `state`, so a
/// restart lands here and picks up where it stopped; a request is never submitted twice.
pub(super) async fn revert_step(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    state: &State,
) -> Step {
    let pool = &executor.pool;
    let Some(merge_sha) = state.revert_merge_sha.as_deref() else {
        return Step::Failed("a revert is pending without its culprit".to_owned());
    };
    if let Some(revert_sha) = state.revert_sha.as_deref() {
        return fix_branch_step(executor, project_id, project_root, state, revert_sha).await;
    }
    let Some(request) = state.revert_request_id else {
        return queue_revert(executor, project_id, state, merge_sha).await;
    };
    let ticket = match vcs::wait_for(pool, request, Duration::ZERO).await {
        Ok(ticket) => ticket,
        Err(sqlx::Error::RowNotFound) => {
            let reason = format!("revert request {request} vanished");
            return revert_failed(pool, project_id, state, merge_sha, reason).await;
        }
        Err(error) => {
            return Step::Failed(format!("cannot read revert request {request}: {error}"));
        }
    };
    if !vcs::TERMINAL_STATUSES.contains(&ticket.status.as_str()) {
        return Step::Waiting {
            sha: merge_sha.to_owned(),
        };
    }
    match (ticket.status.as_str(), ticket.result_sha) {
        ("succeeded", Some(revert_sha)) => {
            match verify_postgate::record_revert(pool, project_id, merge_sha, &revert_sha).await {
                Ok(true) => Step::Reverted {
                    merge_sha: merge_sha.to_owned(),
                    revert_sha,
                },
                Ok(false) => Step::Waiting {
                    sha: merge_sha.to_owned(),
                },
                Err(error) => Step::Failed(format!("cannot record the revert: {error}")),
            }
        }
        ("succeeded", None) => {
            let reason = "the queue reported success without a commit".to_owned();
            revert_failed(pool, project_id, state, merge_sha, reason).await
        }
        (status, _) => {
            let reason = ticket
                .failure_reason
                .unwrap_or_else(|| format!("the request ended {status}"));
            revert_failed(pool, project_id, state, merge_sha, reason).await
        }
    }
}

/// Submits the revert of `merge_sha` to the vcs queue and stores its ticket. It goes in as the
/// owner's standing order (`Origin::Daemon`): turning `revert_on_red` on is that order.
///
/// A request already queued for this culprit (a crash between the submit and the store of its
/// ticket) is adopted instead of submitted again. Before the first submit the kill switch is read
/// fail-closed, and `revert_on_red` is read again: the owner may have turned it off since the
/// report, and an autonomous publish to the target must honour that.
pub(super) async fn queue_revert(
    executor: &Arc<Executor>,
    project_id: &str,
    state: &State,
    merge_sha: &str,
) -> Step {
    let pool = &executor.pool;
    let waiting = || Step::Waiting {
        sha: merge_sha.to_owned(),
    };
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM vcs_requests \
         WHERE project_id = ? AND op = 'revert' AND json_extract(args, '$.merge_sha') = ? \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(merge_sha)
    .fetch_optional(pool)
    .await;
    match existing {
        Ok(Some(request)) => {
            return match verify_postgate::set_revert_request(pool, project_id, merge_sha, request)
                .await
            {
                Ok(true) => Step::Reverting {
                    merge_sha: merge_sha.to_owned(),
                    request,
                },
                Ok(false) => waiting(),
                Err(error) => {
                    Step::Failed(format!("cannot store revert request {request}: {error}"))
                }
            };
        }
        Ok(None) => {}
        Err(error) => return Step::Failed(format!("cannot look for a queued revert: {error}")),
    }
    if crate::governance::autopilot::kill_switch_engaged(pool)
        .await
        .unwrap_or(true)
    {
        return waiting();
    }
    let still_on =
        match crate::config::load_schedule_rules(executor.machine_root.as_deref(), project_id) {
            Ok(rules) => rules.revert_on_red,
            Err(error) => {
                // Unreadable is not "off": hold the revert and say so, so it goes ahead once the
                // rules can be read again and the switch is still on.
                let rules_path = crate::project::state::display_path(
                    project_id,
                    crate::project::state::AUTOPILOT_FILE,
                );
                let summary = format!(
                    "post-merge gate on {target}: the revert of {} is held because {rules_path} \
                     could not be read ({error}). Nothing was reverted yet; it goes ahead once \
                     the rules can be read and revert_on_red is still on.",
                    short(merge_sha),
                    target = state.target
                );
                if let Err(error) = crate::feed::append(
                    pool,
                    Some(project_id),
                    crate::verify::postgate::POSTGATE_RED_KIND,
                    &summary,
                    None,
                    None,
                )
                .await
                {
                    tracing::warn!(%error, "postgate: cannot tell the owner the revert is held");
                }
                return waiting();
            }
        };
    if !still_on {
        let summary = format!(
            "post-merge gate on {target}: the revert of {} was NOT made because revert_on_red \
             was turned off before it was queued. Nothing was reverted and no correction branch \
             was made.",
            short(merge_sha),
            target = state.target
        );
        let reason = "revert_on_red was turned off before the revert was queued".to_owned();
        return settle_failed_revert(
            pool,
            project_id,
            merge_sha,
            &summary,
            reason,
            incident::RevertEnd::Dropped,
        )
        .await;
    }
    let (Ok(sha), Ok(target)) = (CommitSha::new(merge_sha), Branch::new(&state.target)) else {
        let reason = "the culprit or the target is not something the queue accepts".to_owned();
        return revert_failed(pool, project_id, state, merge_sha, reason).await;
    };
    let repo = match vcs::resolve_repo(pool, project_id).await {
        Ok(repo) => repo,
        Err(error) => return Step::Failed(format!("cannot resolve the repository: {error:?}")),
    };
    let op = Op::Revert {
        merge_sha: sha,
        target,
    };
    let request = match vcs::submit_declared(pool, &repo, &op, Origin::Daemon).await {
        Ok(id) => id,
        Err(error) => return Step::Failed(format!("cannot queue the revert: {error}")),
    };
    match verify_postgate::set_revert_request(pool, project_id, merge_sha, request).await {
        Ok(true) => Step::Reverting {
            merge_sha: merge_sha.to_owned(),
            request,
        },
        // Settled by someone else between submit and now; the next tick reads it.
        Ok(false) => Step::Waiting {
            sha: merge_sha.to_owned(),
        },
        Err(error) => Step::Failed(format!("cannot store revert request {request}: {error}")),
    }
}

/// The revert of `merge_sha` did not happen: tell the owner why and free the state.
pub(super) async fn revert_failed(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    state: &State,
    merge_sha: &str,
    reason: String,
) -> Step {
    let summary = format!(
        "post-merge gate on {target}: the revert of {}{} failed ({reason}). No correction \
         branch was made; check {target} before acting.",
        short(merge_sha),
        ticket_note(state),
        target = state.target
    );
    settle_failed_revert(
        pool,
        project_id,
        merge_sha,
        &summary,
        reason,
        incident::RevertEnd::Failed,
    )
    .await
}

/// ` (vcs request #N)` when the state holds the revert's ticket, nothing otherwise.
pub(super) fn ticket_note(state: &State) -> String {
    state
        .revert_request_id
        .map(|id| format!(" (vcs request #{id})"))
        .unwrap_or_default()
}

/// Clears the revert state of `merge_sha` and writes `summary` to the feed.
pub(super) async fn settle_failed_revert(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    merge_sha: &str,
    summary: &str,
    reason: String,
    end: incident::RevertEnd,
) -> Step {
    // The marker first: see `incident::mark_revert_failed`.
    if !incident::mark_revert_failed(pool, project_id, merge_sha, end).await {
        return Step::Failed("cannot mark the incident with the failed revert".to_owned());
    }
    match verify_postgate::fail_revert(pool, project_id, merge_sha, summary).await {
        Ok(true) => {
            incident::settle_after_revert(pool, project_id).await;
            Step::RevertFailed {
                merge_sha: merge_sha.to_owned(),
                reason,
            }
        }
        Ok(false) => Step::Waiting {
            sha: merge_sha.to_owned(),
        },
        Err(error) => Step::Failed(format!("cannot record the failed revert: {error}")),
    }
}
