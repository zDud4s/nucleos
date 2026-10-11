//! Deciding a session's own git operations: whether a queue operation may run, and why not.

use super::*;

/// The answer of the session-git route: a [`Decision`] plus, when the operation was queued, which
/// request it became and whether that request had reached a final state by the time of the answer.
/// Both extras are absent for every answer that did not queue anything, so those keep the exact
/// two-field shape the hook script already reads.
#[derive(Serialize, Deserialize)]
pub struct SessionGitDecision {
    pub decision: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled: Option<bool>,
}

impl From<Decision> for SessionGitDecision {
    fn from(decision: Decision) -> Self {
        Self {
            decision: decision.decision,
            reason: decision.reason,
            request_id: None,
            settled: None,
        }
    }
}

/// How long the session-git route waits for a queued request to settle before answering. The
/// hook's `urlopen` gives the daemon 30s; an answer that over-runs that fails closed as "daemon
/// unreachable", which is why this stays well under it.
pub const SESSION_SETTLE_WAIT: Duration = Duration::from_secs(20);

/// The ceiling for the whole decision, probes and submit included. The settle wait is cut down to
/// whatever of this is left, so the answer still lands inside the hook's 30s timeout.
pub(super) const SESSION_DECISION_BUDGET: Duration = Duration::from_secs(25);

/// A valid branch name that only stands in while a segment's SHAPE is matched, before the branch
/// it really runs on has been read. Never reaches an op that is queued.
pub(super) const QUEUE_OP_PROBE_BRANCH: &str = "probe";

/// The six parsers, in the order the session gate asks them, for one segment.
pub(super) fn queue_op(segment: &str, branch: &str) -> Option<crate::execution::vcs::Op> {
    crate::execution::vcs::merge_from_command(segment, branch)
        .or_else(|| crate::execution::vcs::push_from_command(segment, branch))
        .or_else(|| crate::execution::vcs::tag_from_command(segment, branch))
        .or_else(|| crate::execution::vcs::fetch_from_command(segment))
        .or_else(|| crate::execution::vcs::branch_delete_from_command(segment))
        .or_else(|| crate::execution::vcs::rebase_from_command(segment, branch))
}

#[derive(Deserialize)]
pub struct SessionGitPayload {
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
    /// Where the session is standing. Not necessarily a repository root — see `git_exec::toplevel`.
    pub cwd: String,
}

/// The decision for a session nobody launched: a person's own editor, in a worktree, with no run
/// behind it.
///
/// **This exists because the pillar was governing the wrong half of its own purpose.** The queue is
/// there to order git operations *between sessions*, and the sessions doing the most work are the
/// ones a person opens by hand. Those carry no `NUCLEOS_RUN_ID`, so `ask_daemon.py` expressed no
/// opinion and git ran directly — measured, not theorised: an editor session was asked to
/// `git merge master` and it merged, with no hook, no proposal and no row. Every guarantee the queue
/// offers is a guarantee about requests that reach the queue.
///
/// **It answers with `deny` or with nothing, never with `allow`**, and that is what makes it safe to
/// add where the old code chose silence. The comment it replaces was right about its own case:
/// registered repo-wide, an `allow` emitted by a daemon that does not know what the person is doing
/// would auto-approve their tools. A refusal grants nothing.
///
/// **Only what the queue can actually perform is refused** — the same six parsers that decide it for
/// runs, not the classifier's much wider `pending_approval` net. That distinction is the difference
/// between a gate and a wall: the classifier sends everything not provably read-only for approval, so
/// refusing on its verdict would stop a session at its second command. And a spelling the queue
/// declines (`git merge --squash`, `git branch -D`) must keep working directly, or it becomes
/// impossible rather than governed.
///
/// The refusal is not a redirect: the operation is admitted here, in the daemon, the daemon waits a
/// bounded time for the queue to settle it, and the session is told how it ended — or, if it is
/// still running, that it will be told later. The alternative was to answer "ask the queue
/// yourself", which would have meant telling every editor session how to obtain the control token —
/// handing out the master key to avoid one round trip.
pub async fn session_git_decision(
    State(state): State<AppState>,
    Json(payload): Json<SessionGitPayload>,
) -> Json<SessionGitDecision> {
    session_git_decision_within(&state, payload, SESSION_SETTLE_WAIT).await
}

pub(super) async fn session_git_decision_within(
    state: &AppState,
    payload: SessionGitPayload,
    settle_wait: Duration,
) -> Json<SessionGitDecision> {
    let started = Instant::now();
    let no_opinion = || {
        Json(SessionGitDecision::from(Decision {
            decision: "allow".to_owned(),
            reason: "not an operation this queue performs".to_owned(),
        }))
    };

    if !matches!(payload.tool_name.as_str(), "Bash" | "PowerShell") {
        return no_opinion();
    }
    let Some(command) = payload.tool_input.get("command").and_then(Value::as_str) else {
        return no_opinion();
    };

    // One deadline for the whole decision. A hook runs in front of every tool call, so this path
    // spends git subprocesses on a person's keystrokes — which is why the caller filters first and
    // only asks about commands that could possibly be queueable.
    let deadline = std::time::Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;

    // Per SEGMENT, not per command. Every parser below matches its whole token list as an exact
    // shape, so a shell operator in front of the git call made the list longer and the match fail —
    // and a failed match here is an ALLOW. `cd repo && git merge master` was permitted by the route
    // whose entire purpose is to refuse it, measured against the running daemon. The strictness of
    // the parsers is not what was wrong and is not touched; they are simply asked about each command
    // in the line rather than about the line.
    //
    // **And each segment in the directory it runs in.** The branch a merge moves was read from the
    // session's cwd alone, so `cd <worktree> && git merge master` from a session on `master` queued
    // `master into master` and reported it performed. The shape is matched first with a stand-in
    // branch (the parsers only copy the branch into the op), and the real one is read afterwards
    // from where the segment actually runs.
    let segments = crate::execution::vcs::directed_segments(command);
    let Some((segment, probe)) = segments.iter().find_map(|segment| {
        queue_op(&segment.command, QUEUE_OP_PROBE_BRANCH).map(|probe| (segment, probe))
    }) else {
        // Declined by the queue is not the same sentence as fine to run by hand, and reading them
        // as one left `git push --force` passing. A spelling that still writes something other
        // sessions share is refused with NOTHING queued — there is nothing to queue, because the
        // queue cannot perform that spelling either. Per segment, for the reason above.
        let Some(reason) = crate::execution::vcs::shell_segments(command)
            .into_iter()
            .find_map(crate::execution::vcs::unqueueable_but_shared)
        else {
            return no_opinion();
        };
        // Standing outside a working tree, so no git command from here reaches a repository this
        // queue serves. Silence rather than refusal: this hook is registered for one repository and
        // a session that has wandered out of it is not the case being governed.
        if crate::execution::git_exec::toplevel(Path::new(&payload.cwd), deadline)
            .await
            .is_err()
        {
            return no_opinion();
        }
        return Json(deny_with(&reason).into());
    };

    let kind = probe.kind();
    let cwd = match &segment.dir {
        crate::execution::vcs::SegmentDir::Session => std::path::PathBuf::from(&payload.cwd),
        crate::execution::vcs::SegmentDir::Steps(steps) => {
            crate::execution::vcs::resolve_segment_dir(Path::new(&payload.cwd), steps)
        }
        crate::execution::vcs::SegmentDir::Unfollowable(how) => {
            return Json(
                deny_with(&format!(
                    "{kind} goes through the queue, and the queue has to know which branch it \
                     moves — but this line changes directory with {how}, which this hook does not \
                     follow, so performing it from the session's own directory could move a branch \
                     nobody named. Nothing was queued. Name the directory plainly: \
                     `git -C <dir> {}` or `cd <dir> && {}`.",
                    segment.command.trim_start_matches("git ").trim(),
                    segment.command.trim(),
                ))
                .into(),
            );
        }
    };
    let explicit = segment.dir != crate::execution::vcs::SegmentDir::Session;
    let root = match crate::execution::git_exec::toplevel(&cwd, deadline).await {
        Ok(root) => root,
        // An explicit `cd`/`-C` into something that is not a working tree is a mistake to report,
        // not a session that wandered off; falling silent would let it run by hand.
        Err(reason) if explicit => {
            return Json(deny_with(&format!("{kind} goes through the queue, and {reason}")).into());
        }
        Err(_) => return no_opinion(),
    };
    let branch = match crate::execution::git_exec::current_branch(&root, deadline).await {
        Ok(branch) => branch,
        Err(reason) if explicit => {
            return Json(deny_with(&format!("{kind} goes through the queue, and {reason}")).into());
        }
        Err(_) => return no_opinion(),
    };
    let Some(op) = queue_op(&segment.command, &branch) else {
        // The shape matched with the stand-in and not with the real branch: the parsers refuse only
        // a detached HEAD on that ground, and it names no branch the queue could move.
        return Json(
            deny_with(&format!(
                "{kind} goes through the queue, and {} is on a detached HEAD, which names no \
                 branch the queue can move. Nothing was queued.",
                root.display()
            ))
            .into(),
        );
    };

    // From here the command IS one the queue performs, so every remaining failure refuses rather
    // than falls through. The direction is deliberately the opposite of `runs::queueable_operation`,
    // and the two are right for opposite reasons: there, a person has already approved an action and
    // being unable to queue it must not strand them holding it; here, nobody has approved anything,
    // and falling through would hand back the very bypass this function closes.
    let project_id =
        match crate::execution::vcs::project_for_worktree(&state.pool, &root, deadline).await {
            Ok(project_id) => project_id,
            Err(reason) => {
                return Json(
                    deny_with(&format!(
                        "{} goes through the queue, and {reason}",
                        op.kind()
                    ))
                    .into(),
                );
            }
        };
    let repo = match crate::execution::vcs::resolve_repo(&state.pool, &project_id).await {
        Ok(repo) => repo,
        Err(error) => {
            tracing::warn!(
                project_id,
                ?error,
                "session-git: could not resolve the repository"
            );
            return Json(
                deny_with(&format!(
                    "{} goes through the queue, and {project_id}'s repository could not be resolved",
                    op.kind()
                ))
                .into(),
            );
        }
    };

    match crate::execution::vcs::submit(
        &state.pool,
        &repo,
        &op,
        crate::execution::vcs::Origin::Shell,
    )
    .await
    {
        Ok(id) => {
            let wait = settle_wait.min(SESSION_DECISION_BUDGET.saturating_sub(started.elapsed()));
            let ticket = match crate::execution::vcs::wait_for(&state.pool, id, wait).await {
                Ok(ticket) => ticket,
                Err(error) => {
                    tracing::warn!(id, ?error, "session-git: could not read the request back");
                    crate::execution::vcs::Ticket {
                        id,
                        status: "queued".into(),
                        result_sha: None,
                        failure_reason: None,
                    }
                }
            };
            let settled =
                crate::execution::vcs::TERMINAL_STATUSES.contains(&ticket.status.as_str());
            Json(SessionGitDecision {
                decision: "deny".to_owned(),
                reason: session_git_reason(id, op.kind(), &ticket, settled),
                request_id: Some(id),
                settled: Some(settled),
            })
        }
        Err(error) => {
            tracing::error!(?error, "session-git: could not admit the request");
            Json(
                deny_with(&format!(
                    "{} goes through the queue, and admitting it failed",
                    op.kind()
                ))
                .into(),
            )
        }
    }
}

/// What the session is told about a request it caused, by where the request stands. Pure, so every
/// outcome can be asserted without a queue. The sentence always opens the same way, and never names
/// a route the session cannot read: an editor session holds no control token.
pub(super) fn session_git_reason(
    id: i64,
    kind: &str,
    ticket: &crate::execution::vcs::Ticket,
    settled: bool,
) -> String {
    let head = format!("queued as vcs request #{id} ({kind}) — ");
    let why = ticket
        .failure_reason
        .as_deref()
        .unwrap_or("no reason recorded");
    let tail = match ticket.status.as_str() {
        "succeeded" => format!(
            "the queue performed it: landed at {}. Do not run it again; your worktree may have \
             moved under you, so re-read `git status`/`git log` before going on.",
            ticket.result_sha.as_deref().unwrap_or("an unrecorded sha")
        ),
        "escalated" => "it hit a conflict and was escalated to a person. Do not retry it or \
                        resolve it by hand; the resolution arrives as a new request."
            .to_owned(),
        "blocked" => format!(
            "the queue refused to start it: {why}. Nothing was changed. Clear what that names, \
             then run the same command again to queue it anew."
        ),
        status @ ("failed" | "interrupted") => {
            format!("it {status}: {why}. Check the repository state before asking again.")
        }
        "rejected" if ticket.failure_reason.as_deref() == Some(crate::execution::vcs::OWNER_REFUSED_REASON) => {
            "the owner refused the change it makes to the test map, and nothing was performed.              Do not run it again."
                .to_owned()
        }
        status @ ("cancelled" | "rejected") => format!(
            "it was {status} and nothing was performed. Run the command again only if you still \
             want it."
        ),
        "awaiting_owner" => "it changes the test map, so it is held until the owner approves \
                             it. Do not run it again; the owner's decision is the next step."
            .to_owned(),
        status => {
            debug_assert!(
                !settled,
                "a settled status the reason does not know: {status}"
            );
            format!(
                "still {status} after the wait. The queue will perform it; do not run it again and \
                 do not run it another way. You will be told how it ended on a later action of \
                 this session."
            )
        }
    };
    format!("{head}{tail}")
}

/// Spec A: the one call this hook puts to the judge, built once from what the hook already holds.
/// Owned, because an observation outlives the request that started it.
pub(super) fn judge_asked(
    state: &AppState,
    run_id: i64,
    shadow_decision_id: Option<i64>,
    project_id: Option<&str>,
    cwd: &str,
    payload: &PreToolUsePayload,
    classification: &classifier::Classification,
) -> crate::judge::Asked {
    crate::judge::Asked {
        run_id,
        shadow_decision_id,
        project_id: project_id.map(str::to_owned),
        machine_root: state.machine_config_root.clone(),
        tool_name: payload.tool_name.clone(),
        tool_input: payload.tool_input.clone(),
        cwd: cwd.to_owned(),
        action_class: classification.action_class,
        classifier_decision: classification.decision.decision.clone(),
    }
}
