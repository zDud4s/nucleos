//! `GitExecutor`, the queue's executor, and the merge gate it runs before publishing.

use super::*;

/// The `VcsExecutor` the daemon actually runs.
///
/// A deadline, and where project state lives: everything else it needs comes from the claimed
/// request, which is the only thing that knows which repository and which operation.
pub struct GitExecutor {
    pub timeout: Duration,
    /// `AppState::machine_config_root`, so the queue reads a project's `autopilot.yaml` from the
    /// same `~/.nucleos/projects/<id>/` every other reader does. `None` — what `Default` gives —
    /// reads every project as having no rules file, which is how a test that never configures one
    /// stays out of the real home directory.
    pub machine_root: Option<std::path::PathBuf>,
    /// Where a gated merge records its measurement (`verify_runs`). `None` - what `Default`
    /// gives - measures exactly as before and records nothing, which is every test that never
    /// asked for a database.
    pub pool: Option<sqlx::SqlitePool>,
    /// The verification executor a merge's `scope` gate is submitted to (spec 2026-10-05 §6.1).
    /// `None` - what `Default` gives - means every merge takes the full gate, which is how a test
    /// that never installed one behaves.
    pub verify: Option<std::sync::Arc<crate::verify::exec::Executor>>,
}

impl Default for GitExecutor {
    fn default() -> Self {
        Self {
            timeout: OPERATION_TIMEOUT,
            machine_root: None,
            pool: None,
            verify: None,
        }
    }
}

/// What `gate_the_merge` did, because the caller owes the publish a git budget when something ran
/// and owes the post-merge gate a cover mark when the FULL gate ran on a merge it would repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MergeGate {
    /// `gate_before_publish` is off: nothing was measured.
    Skipped,
    /// The full `gate_command` ran and passed; the post-merge gate is not in play.
    Full,
    /// The full `gate_command` ran and passed on a merge into the land target while
    /// `gate_after_land` is on: once published, that sha need not be gated again.
    FullCovering,
    /// A `scope` verification ran (or found nothing to run). The post-merge gate still owes the full
    /// gate on this sha.
    Scope,
}

impl MergeGate {
    pub(super) fn measured(self) -> bool {
        self != MergeGate::Skipped
    }
}

#[async_trait::async_trait]
impl crate::execution::vcs::VcsExecutor for GitExecutor {
    async fn execute(&self, request: &crate::execution::vcs::ClaimedRequest) -> Outcome {
        let project_root = Path::new(&request.project_root);
        // The budget starts here, once. Every git command inside the operation spends from it.
        let deadline = std::time::Instant::now() + self.timeout;
        // Also the only production reader of `ClaimedRequest::project_id` — see Task 7 Step 4, which
        // is where its absence would otherwise surface as a dead-code error. It earns its place
        // regardless: a merge in the daemon log without the repository it ran against is unreadable
        // the moment two projects are busy.
        tracing::info!(
            vcs_request_id = request.id,
            project_id = %request.project_id,
            op = request.op.kind(),
            "vcs: executing"
        );
        // **The third `.git` guard, on the one directory this module neither creates nor reads out
        // of git.** `prepare_integration_worktree` guards the directory it makes and
        // `publish_by_fast_forward` guards the one `worktree list` named; `project_root` arrives on
        // the queue row, and `worktree add`, `worktree list` and `update-ref` all run with `-C`
        // pointed at it. `git -C <dir>` walks UP, so a root that exists and is not a repository does
        // not fail — the whole operation runs against whatever repository ENCLOSES it. Measured: the
        // merge computed, the enclosing repository's `master` moved, and the row came back
        // `Succeeded` naming a sha from a repository nobody named.
        //
        // **A backstop now, and no longer the first line of defence.** `vcs::resolve_repo` puts the
        // root through `repo_key` before a row can be inserted at all, and that check is strictly
        // stronger than this one: it requires the root to BE the repository's top level, where this
        // only requires it to contain a `.git`. What is left for this guard is the window between
        // the two — a root that stopped being a repository between submitting and running, which a
        // request queued behind two slow merges has plenty of time to do — and any future caller
        // that reaches `execute` without having gone through `resolve_repo`. Both are real, and
        // naming them is the point: a guard nobody can say what it still catches is a guard somebody
        // eventually deletes.
        //
        // **Existence, not `is_file()`** — a project root is a main checkout, whose `.git` is a
        // DIRECTORY. `publish_by_fast_forward`'s guard states that distinction at length and this is
        // the same case for the same reason; it is not repeated here so there is one place to fix if
        // it is ever wrong.
        if tokio::fs::metadata(project_root.join(".git"))
            .await
            .is_err()
        {
            return Outcome::Failed {
                reason: format!(
                    "{} is not a git repository; check the project's root and resubmit",
                    project_root.display()
                ),
                exit_code: None,
                output_tail: String::new(),
            };
        }
        // Deliberately exhaustive with no `_` arm: a wildcard here would let a variant ship with no
        // executor and no compile error — a request that queues, claims the repository, and reports
        // success having done nothing.
        match &request.op {
            crate::execution::vcs::Op::Merge { source, target } => {
                // A resolver's output is verified BEFORE it is merged, and only a resolver's. See
                // `verify_resolution` for what is checked and why the checks are shaped that way.
                if request.from_resolution
                    && let Err(outcome) =
                        verify_resolution(project_root, source.as_str(), deadline).await
                {
                    return outcome;
                }
                match compute_merge(project_root, source.as_str(), target.as_str(), deadline).await
                {
                    Ok(computed) => {
                        // Spec 2026-10-05 §3.4, defence 1: a merge that changes the test map
                        // waits for the owner, before the gate, which is the very thing the map
                        // configures. Passes only when the owner approved exactly this content.
                        // Only a merge INTO the integration branch asks: master merged into an
                        // agent's branch carries a map the owner already approved.
                        match map_change(project_root, &computed, deadline).await {
                            Err(outcome) => return outcome,
                            Ok(Some(blob))
                                if lands_on_the_integration_branch(request, target)
                                    && request.approved_map_blob.as_deref()
                                        != Some(blob.as_str()) =>
                            {
                                return Outcome::AwaitingOwner {
                                    reason: format!(
                                        "this merge changes {}; it lands once the owner approves it",
                                        crate::tests_map::MAP_FILE
                                    ),
                                    map_blob: blob,
                                };
                            }
                            Ok(_) => {}
                        }
                        // Measured on the commit `compute_merge` left at the integration worktree's
                        // HEAD — the tree `target` is about to become — and BEFORE `publish` moves
                        // anything.
                        //
                        // `integration_worktree` and not `prepare_integration_worktree`: the first
                        // is a pure path function, and the second would `merge --abort`, `reset
                        // --hard` and `clean` the very checkout being measured.
                        //
                        // Only a merge INTO the integration branch the request names can take the
                        // combined rule: elsewhere there is no post-merge gate to compensate for a
                        // lighter measurement.
                        let land_target = request
                            .integration_branch
                            .as_ref()
                            .map(|branch| branch.as_str())
                            .filter(|branch| *branch == target.as_str());
                        let measured = match gate_the_merge(
                            self.machine_root.as_deref(),
                            self.pool.as_ref(),
                            request.id,
                            &request.project_id,
                            project_root,
                            &integration_worktree(project_root),
                            crate::state::DEFAULT_GATE_TIMEOUT,
                            crate::state::DEFAULT_GATE_TIMEOUT,
                            land_target,
                            self.verify.as_ref(),
                        )
                        .await
                        {
                            Ok(measured) => measured,
                            Err(outcome) => return outcome,
                        };
                        // A gate that RAN buys the publish a fresh git budget, and one that did not
                        // changes nothing. The arithmetic is the reason rather than the taste:
                        // `deadline` was fixed at `now + OPERATION_TIMEOUT` (300s) when this
                        // operation started, and a gate may legitimately outlast it
                        // (`DEFAULT_GATE_TIMEOUT` is 2700s). Letting a measurement — which spawns no
                        // git at all — spend the git budget would make every gated merge die at
                        // `update-ref` saying the budget was spent, which reads as a broken queue
                        // rather than as a slow suite. An UNGATED merge keeps the one deadline it
                        // always had, so nothing about this loosens the budget for anybody who did
                        // not ask to be measured.
                        let publishing = if measured.measured() {
                            std::time::Instant::now() + self.timeout
                        } else {
                            deadline
                        };
                        let outcome =
                            publish(project_root, target.as_str(), computed, publishing).await;
                        // Marked only AFTER the publish: a lost CAS leaves the measured sha off
                        // the target, and a mark on it would break the post-merge gate's range.
                        if measured == MergeGate::FullCovering
                            && let Outcome::Succeeded { sha: Some(sha), .. } = &outcome
                            && let Some(pool) = self.pool.as_ref()
                            && let Err(error) = crate::verify::postgate::mark_covered(
                                pool,
                                &request.project_id,
                                target.as_str(),
                                sha,
                            )
                            .await
                        {
                            tracing::warn!(
                                vcs_request_id = request.id,
                                %error,
                                "could not mark the published merge as covered; the post-merge \
                                 gate will run again"
                            );
                        }
                        outcome
                    }
                    // Computing failed, which IS how this request ended.
                    Err(outcome) => outcome,
                }
            }
            crate::execution::vcs::Op::Push { remote, branch } => {
                self.push_when_gated_green(request, remote.as_str(), branch.as_str(), deadline)
                    .await
            }
            crate::execution::vcs::Op::Tag { name, at } => {
                tag(project_root, name.as_str(), at.as_str(), deadline).await
            }
            crate::execution::vcs::Op::Fetch { remote } => {
                fetch(project_root, remote.as_str(), deadline).await
            }
            crate::execution::vcs::Op::BranchDelete { branch } => {
                let judged_against = request
                    .integration_branch
                    .as_ref()
                    .map(|integration| integration.as_str());
                branch_delete(project_root, branch.as_str(), judged_against, deadline).await
            }
            crate::execution::vcs::Op::Rebase { branch, onto } => {
                rebase(project_root, branch.as_str(), onto.as_str(), deadline).await
            }
            // No gate here: a revert returns a tree that has been green before, and the post-merge
            // gate measures the new tip like any other. A revert that changes the test map still
            // waits for the owner, exactly as the merge it undoes did.
            crate::execution::vcs::Op::Revert { merge_sha, target } => {
                let computed = match compute_revert(
                    project_root,
                    merge_sha.as_str(),
                    target.as_str(),
                    deadline,
                )
                .await
                {
                    Ok(computed) => computed,
                    Err(outcome) => return outcome,
                };
                match map_change(project_root, &computed, deadline).await {
                    Err(outcome) => return outcome,
                    Ok(Some(blob))
                        if lands_on_the_integration_branch(request, target)
                            && request.approved_map_blob.as_deref() != Some(blob.as_str()) =>
                    {
                        return Outcome::AwaitingOwner {
                            reason: format!(
                                "this revert changes {}; it lands once the owner approves it",
                                crate::tests_map::MAP_FILE
                            ),
                            map_blob: blob,
                        };
                    }
                    Ok(_) => {}
                }
                publish(project_root, target.as_str(), computed, deadline).await
            }
        }
    }
}

impl GitExecutor {
    /// A push never waits in its caller's face: with `gate_after_land` on, a push of the project's
    /// integration branch goes out at once when the post-land gate has judged the sha green, and
    /// otherwise parks (`Outcome::AwaitingGate`) until the gate turns green - the gate's green writes
    /// requeue the row (`vcs::release_awaiting_gate`) and the push then re-resolves and re-decides.
    ///
    /// Any other push, a project without `gate_after_land`, an executor without a database, and any
    /// state that cannot be read behave exactly as before: this only ever DELAYS a push on positive
    /// evidence that the project asked for it, and a failure to find that evidence must not hold a
    /// push back. The sha is resolved once and decided on, and that same sha is what is pushed.
    async fn push_when_gated_green(
        &self,
        request: &crate::execution::vcs::ClaimedRequest,
        remote: &str,
        branch: &str,
        deadline: std::time::Instant,
    ) -> Outcome {
        let project_root = Path::new(&request.project_root);
        let Some(pool) = self.pool.as_ref() else {
            return push(project_root, remote, branch, deadline).await;
        };
        // Fails open like an unreadable rules file: with no `gate_command` no gate will ever run, so
        // a push parked for it would wait for ever.
        let gate_after_land =
            crate::config::load_schedule_rules(self.machine_root.as_deref(), &request.project_id)
                .map(|rules| rules.gate_after_land && rules.gate_command.is_some())
                .unwrap_or(false);
        if !gate_after_land {
            return push(project_root, remote, branch, deadline).await;
        }
        let on_the_integration_branch = matches!(
            crate::execution::land::integration_branch(
                pool,
                &request.project_id,
                project_root,
                deadline,
            )
            .await,
            Ok(integration) if integration.as_str() == branch
        );
        if !on_the_integration_branch {
            return push(project_root, remote, branch, deadline).await;
        }
        let sha = match revision(project_root, &format!("refs/heads/{branch}"), deadline).await {
            Ok(sha) => sha,
            Err(outcome) => return outcome,
        };
        let green = match crate::verify::postgate::load(pool, &request.project_id).await {
            Ok(state) => state.is_some_and(|state| state.is_green(&sha)),
            Err(error) => {
                tracing::warn!(
                    vcs_request_id = request.id,
                    %error,
                    "could not read the post-land gate's state; parking the push of the \
                     integration branch until it can be"
                );
                false
            }
        };
        if green {
            return push_sha(project_root, remote, branch, sha, deadline).await;
        }
        Outcome::AwaitingGate {
            sha: sha.clone(),
            reason: format!(
                "{branch} is at {sha}, which the post-land gate has not judged green; the push \
                 waits for the post-land gate and goes out by itself once it is"
            ),
        }
    }
}

/// Measures the computed merge before it is published, when the project asked to be.
///
/// **This is the answer to how a target branch catches somebody else's red.** The queue merged and
/// nothing measured the result: a branch green on its own, merged into a target green on its own,
/// produces a tree neither of them ever built — and the first thing to notice was the next person's
/// build. `gate_before_publish` is a project saying it would rather wait.
///
/// **Where it runs is the whole design.** The commit under measurement is HEAD of the integration
/// worktree, left detached there by `compute_merge`, and `target` has not moved. So a red gate costs
/// a refusal and nothing else — no revert, no reset of a branch other people have already pulled, no
/// history rewritten. The two obvious alternatives are both worse: gating the SOURCE before merging
/// measures something other than what breaks, and merging first and undoing afterwards is a
/// destructive act on shared history that this queue will not take on its own.
///
/// Returns which measurement happened (`MergeGate`), because the caller owes a publish that ran
/// after a long gate its own git budget and owes one that did not exactly the budget it already had.
///
/// **With `gate_after_land` on and a merge into the land target (`land_target`), spec §6.1 applies:**
/// a test map that `job::map_is_trusted` accepts against the project root (valid, byte-equal to the
/// root's, and running no script that differs from the root's) means a cached `verify scope` stands
/// in for the full gate, because the post-merge worker runs the full gate on the published sha
/// anyway. No trusted map, no executor, a `scope` that cannot be submitted, or one that reaches no
/// verdict in time (`scope_wait`) falls back to the full `gate_command`, and that result is
/// `FullCovering`: the caller marks the published sha as covered. The fallback after a verdict that
/// never came also cancels the scope units still queued, so they do not run beside the full gate.
/// With the switch off nothing here changes.
///
/// **Three refusals rather than one, and each is a different sentence to its reader.**
/// - The rules file exists and will not parse: we cannot tell whether this repository wanted its
///   merges measured, and publishing unmeasured is the failure itself. A project with NO file is
///   untouched — `load_schedule_rules` answers `Ok(default)` for an absent one — so this only
///   refuses a repository that has the file and broke it, which is a repository whose scheduler,
///   triggers and run gate are already all dead for the same reason.
/// - `gate_before_publish` with no `gate_command`: the configuration asks for a measurement and
///   names none. Publishing anyway would make the key decorative, which is the exact failure it was
///   added to end.
/// - The gate could not run (`Errored`): "we could not measure" is not "it passed". `gate.rs` keeps
///   those two apart precisely so that callers do not collapse them, and this is a caller.
///
/// **A consequence worth finding by reading rather than by surprise: while this key is on, a branch
/// that CHANGES the gate script cannot land.** `run_gate` compares every script the command names
/// against the project root's copy and refuses to measure when they differ — which is exactly right
/// where it was written (an agent's worktree, where a run could green itself) and reads oddly here,
/// because a branch improving `scripts/gates.sh` is not tampering with anything. It arrives as an
/// `Errored`, so nothing is published and the reason names the file. The remedy is to land that one
/// change with the key off; the alternative — measuring with a script the merge itself supplied —
/// is the door that check exists to hold shut, and it is not worth opening for the convenience.
///
/// `Failed` and never `Escalated` for a red gate. `Escalated` means a person now owns something the
/// queue cannot resolve; a red suite is owned by whoever wrote the branch, and it is fixed where
/// every other red suite is fixed — in their own worktree, on their own branch.
#[allow(clippy::too_many_arguments)]
pub(super) async fn gate_the_merge(
    machine_root: Option<&Path>,
    pool: Option<&sqlx::SqlitePool>,
    request_id: i64,
    project_id: &str,
    project_root: &Path,
    integration: &Path,
    timeout: Duration,
    scope_wait: Duration,
    land_target: Option<&str>,
    verify: Option<&std::sync::Arc<crate::verify::exec::Executor>>,
) -> Result<MergeGate, Outcome> {
    let refuse = |reason: String| Outcome::Failed {
        reason,
        exit_code: None,
        output_tail: String::new(),
    };

    let rules_path =
        crate::project::state::display_path(project_id, crate::project::state::AUTOPILOT_FILE);
    let rules = match crate::config::load_schedule_rules(machine_root, project_id) {
        Ok(rules) => rules,
        Err(error) => {
            return Err(refuse(format!(
                "{rules_path} could not be read, so whether this repository wants its merges \
                 measured is unknown and nothing was published: {error}"
            )));
        }
    };
    if !rules.gate_before_publish {
        return Ok(MergeGate::Skipped);
    }
    let Some(command) = rules.gate_command else {
        return Err(refuse(format!(
            "{rules_path} asks for merges to be gated and names no gate_command, so there is \
             nothing to measure this merge with and nothing was published"
        )));
    };

    let combine = rules.gate_after_land && land_target.is_some();
    if combine && let Some(executor) = verify {
        let tree = integration.to_path_buf();
        let root = project_root.to_path_buf();
        let trusted = tokio::task::spawn_blocking(move || {
            crate::execution::job::map_is_trusted(&tree, &root)
        })
        .await
        .unwrap_or(false);
        if !trusted {
            tracing::info!(
                vcs_request_id = request_id,
                "the merge carries no test map the project root vouches for (absent, invalid, or the map or a script its groups run differs from the root's copy); measuring with the full gate"
            );
        } else {
            let (ticket, verdict) = crate::verify::gate_scope_ticketed(
                executor,
                crate::verify::Caller::Merge(request_id),
                integration,
                scope_wait,
            )
            .await;
            match verdict {
                crate::verify::ScopeVerdict::Measured(crate::gate::GateOutcome::Passed)
                | crate::verify::ScopeVerdict::NothingRan => return Ok(MergeGate::Scope),
                crate::verify::ScopeVerdict::Measured(crate::gate::GateOutcome::Failed {
                    exit_code,
                    output,
                }) => return Err(merge_gate_failed(exit_code, output)),
                crate::verify::ScopeVerdict::Measured(crate::gate::GateOutcome::Errored {
                    reason,
                }) => {
                    // "Not measured" is not "passed", but it is no reason to refuse either while
                    // the full gate can still measure the merge.
                    tracing::warn!(
                        vcs_request_id = request_id,
                        %reason,
                        "the merge's scope verification reached no verdict; measuring with the full gate instead"
                    );
                    if let Some(ticket) = ticket {
                        let why = "cancelled: the merge's scope verification reached no verdict, so the full gate measured the merge instead";
                        match crate::verify::cancel_queued_units(&executor.pool, ticket, why).await
                        {
                            Ok(cancelled) => tracing::info!(
                                vcs_request_id = request_id,
                                cancelled,
                                "cancelled the scope units still queued behind the full gate"
                            ),
                            Err(error) => tracing::warn!(
                                vcs_request_id = request_id,
                                %error,
                                "could not cancel the scope units still queued; they may run beside the full gate"
                            ),
                        }
                    }
                }
                crate::verify::ScopeVerdict::Unavailable => {}
            }
        }
    }

    let measured = crate::verify::runs::timed_gate(
        pool,
        crate::verify::runs::GateContext {
            project_id: Some(project_id),
            origin: crate::verify::runs::ORIGIN_MERGE,
            origin_id: Some(request_id),
            ordinal: None,
        },
        integration,
        project_root,
        &command,
        timeout,
    )
    .await;
    match measured {
        crate::gate::GateOutcome::Passed => Ok(if combine {
            MergeGate::FullCovering
        } else {
            MergeGate::Full
        }),
        crate::gate::GateOutcome::Failed { exit_code, output } => {
            Err(merge_gate_failed(exit_code, output))
        }
        crate::gate::GateOutcome::Errored { reason } => Err(refuse(format!(
            "the merge could not be measured, so nothing was published: {reason}"
        ))),
    }
}

/// A red gate on a merge. Says WHERE the failure lives, because the asker's first instinct will be
/// that their branch is fine - and it may well be. What was measured is the junction, which is a
/// tree neither side had ever built.
pub(crate) fn merge_gate_failed(exit_code: i32, output: String) -> Outcome {
    Outcome::Failed {
        reason: "the merge does not pass this project's gate, so nothing was published. What \
                 was measured is the two branches TOGETHER: each can be green on its own and \
                 still make a tree that is not."
            .to_owned(),
        exit_code: Some(exit_code),
        output_tail: output,
    }
}
