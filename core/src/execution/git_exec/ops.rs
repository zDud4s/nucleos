//! The other queued operations: rebase, fetch, branch delete, tag and push.

use super::*;

/// Replays `branch` onto `onto`, on a detached HEAD in the integration worktree.
///
/// **The holder check comes FIRST, before anything is computed, and that ordering is the operation
/// rather than an optimisation.** Every other executor here computes and then discovers whether it
/// can publish; this one cannot publish to a held branch at all — `Op::Rebase` argues why at length,
/// and the short version is that no git command moves a checkout across a divergence while refusing
/// to destroy. So a computed rebase nobody could publish would be minutes of work and a pile of
/// unreferenced objects, thrown away to say something that was knowable before it started.
///
/// `Blocked` rather than `Failed` for that case, and it is the one place in this module where
/// `Blocked`'s terminal-ness is exactly right: nothing about the repository will change on its own to
/// make this publishable. A person has to check that branch out somewhere else, and until they do a
/// retry would fail identically.
pub(super) async fn rebase(
    project_root: &Path,
    branch: &str,
    onto: &str,
    deadline: std::time::Instant,
) -> Outcome {
    match worktree_holding(project_root, branch, deadline).await {
        Ok(Some(holder)) => {
            return Outcome::Blocked {
                reason: format!(
                    "{branch} is checked out in {}, and a rebase rewrites it — this queue will not \
                     reset a worktree it does not own. Check that branch out somewhere else and \
                     resubmit.",
                    holder.display()
                ),
                output_tail: String::new(),
            };
        }
        Ok(None) => {}
        Err(outcome) => return outcome,
    }

    let integration = match prepare_integration_worktree(project_root, deadline).await {
        Ok(integration) => integration,
        Err(outcome) => return outcome,
    };

    // Detached, like `compute_merge` — and here it is not merely permitted but required: `git rebase`
    // moves whatever HEAD names, so a rebase on an attached HEAD would move the integration
    // worktree's own branch instead of computing a value to publish.
    let checkout = match git(&integration, &["checkout", "--detach", branch], deadline).await {
        Ok(checkout) => checkout,
        Err(outcome) => return outcome,
    };
    if !checkout.succeeded() {
        return failed(format!("could not check out {branch} to rebase"), &checkout);
    }

    let old = match revision(&integration, "HEAD", deadline).await {
        Ok(old) => old,
        Err(outcome) => return outcome,
    };

    let rebased = match git(
        &integration,
        &["rebase", "--end-of-options", onto],
        deadline,
    )
    .await
    {
        Ok(rebased) => rebased,
        Err(outcome) => return outcome,
    };
    if !rebased.succeeded() {
        // Best effort, and for `compute_merge`'s reason: the next operation resets this worktree
        // anyway, and a failure to abort must not replace the conflict the caller needs to read.
        let _ = git(&integration, &["rebase", "--abort"], deadline).await;
        return failed(format!("rebasing {branch} onto {onto} failed"), &rebased);
    }

    let new = match revision(&integration, "HEAD", deadline).await {
        Ok(new) => new,
        Err(outcome) => return outcome,
    };

    // Already on top of `onto`: git rebased nothing and HEAD did not move. Publishing would be a
    // no-op compare-and-swap, so say so instead.
    if new == old {
        return Outcome::Succeeded {
            sha: Some(new),
            output_tail: rebased.output_tail,
        };
    }

    // Asked again, because the rebase took time and somebody may have checked the branch out in the
    // meantime: moving the ref under that checkout would leave its files describing the old commit,
    // which reads as a dirty tree full of changes nobody made. This narrows the window to the
    // instant between this answer and the compare-and-swap; it cannot close it, since git offers no
    // ref update that is conditional on no worktree holding the branch.
    match worktree_holding(project_root, branch, deadline).await {
        Ok(Some(holder)) => {
            return Outcome::Blocked {
                reason: format!(
                    "{branch} was checked out in {} while it was being rebased, so the rebase \
                     was not published — this queue will not move a branch under a worktree it \
                     does not own. Check that branch out somewhere else and resubmit.",
                    holder.display()
                ),
                output_tail: String::new(),
            };
        }
        Ok(None) => {}
        Err(outcome) => return outcome,
    }

    // The same compare-and-swap `publish_by_update_ref` performs for a merge, and reached the same
    // way: nobody holds this branch — checked above, before and after the work — so there is no
    // worktree whose files have to move with the ref.
    publish_by_update_ref(
        project_root,
        branch,
        Computed {
            old,
            new,
            output_tail: rebased.output_tail,
        },
        deadline,
    )
    .await
}

/// Fetches from `remote`, using whatever refspec that remote is configured with.
///
/// The shortest executor here, and the only one that resolves nothing first — there is no object id
/// to record, because a fetch moves however many tracking refs the remote had news about. Its
/// `Succeeded` therefore carries `None`, which is what made `Outcome::Succeeded::sha` optional.
///
/// What it still needs from everything above it is the part that does not show in these seven lines:
/// this is the operation that hands git an ssh and a credential helper, so it depends on
/// `run_git`'s tree-kill, its `GIT_TERMINAL_PROMPT=0` and its drain grace exactly as `push` does. A
/// fetch against a dead network is the case spec §7's hung-command row was written about.
pub(super) async fn fetch(
    project_root: &Path,
    remote: &str,
    deadline: std::time::Instant,
) -> Outcome {
    let fetched = match git(
        project_root,
        &["fetch", "--end-of-options", remote],
        deadline,
    )
    .await
    {
        Ok(fetched) => fetched,
        Err(outcome) => return outcome,
    };
    if !fetched.succeeded() {
        return failed(
            format!("fetching from {remote} failed; git's output says why"),
            &fetched,
        );
    }
    Outcome::Succeeded {
        sha: None,
        output_tail: fetched.output_tail,
    }
}

/// Deletes `branch`, in the spelling that refuses to lose unmerged work.
///
/// **The sha is read BEFORE the delete and recorded, and that ordering is the operation's whole
/// value beyond running the command.** Afterwards the ref is gone and nothing can answer what it
/// pointed at; the row is then the only place holding the one string that undoes this
/// (`git branch <name> <sha>`). Every other executor here resolves first for a different reason — so
/// that what it publishes is a value rather than a re-read — and this one does it so that what it
/// destroys is recoverable.
///
/// `--delete` and never `-D`, which `vcs::branch_delete_from_command` argues at length: the refusal
/// on unmerged commits is git's own, computed from the commit graph, and it is what lets the queue
/// offer this without owning the question of what is safe to lose. A branch checked out in some
/// worktree is refused by git too, with `Cannot delete branch … used by worktree` — a guard this
/// module would otherwise have to reproduce against `worktree list`, and get wrong.
///
/// **Merged into what is `judged_against`, the project's integration branch, and never the main
/// checkout's HEAD.** `--delete` compares a branch with its upstream, and with HEAD only when it has
/// none. HEAD is the project root's, so a main checkout parked on a feature branch used to refuse
/// every landed branch that feature branch did not contain. So a branch with no upstream borrows
/// the integration branch as one for exactly as long as the delete takes. The judgement is still
/// git's, computed from the same commit graph, and so is the refusal of a checked-out branch; only
/// what it is compared with changes. A branch that already tracks something keeps it, because its
/// owner chose that comparison and it is not this queue's to replace.
pub(super) async fn branch_delete(
    project_root: &Path,
    branch: &str,
    judged_against: Option<&str>,
    deadline: std::time::Instant,
) -> Outcome {
    let reference = format!("refs/heads/{branch}");
    let sha = match revision(project_root, &reference, deadline).await {
        Ok(sha) => sha,
        Err(outcome) => return outcome,
    };

    let borrowed_upstream = match judged_against {
        Some(target) if target != branch => {
            let tracked = match git(
                project_root,
                &["for-each-ref", "--format=%(upstream)", &reference],
                deadline,
            )
            .await
            {
                Ok(tracked) if tracked.succeeded() => tracked,
                Ok(tracked) => {
                    return failed(format!("could not read what {branch} tracks"), &tracked);
                }
                Err(outcome) => return outcome,
            };
            if tracked.stdout.trim().is_empty() {
                let upstream = format!("--set-upstream-to={target}");
                let set = match git(
                    project_root,
                    &["branch", &upstream, "--end-of-options", branch],
                    deadline,
                )
                .await
                {
                    Ok(set) => set,
                    Err(outcome) => return outcome,
                };
                if !set.succeeded() {
                    return failed(format!("could not judge {branch} against {target}"), &set);
                }
                true
            } else {
                false
            }
        }
        _ => false,
    };

    let deleted = match git(
        project_root,
        &["branch", "--delete", "--end-of-options", branch],
        deadline,
    )
    .await
    {
        Ok(deleted) => deleted,
        Err(outcome) => return outcome,
    };
    if !deleted.succeeded() {
        // A refused delete leaves the branch exactly as it found it, and it found no upstream.
        if borrowed_upstream {
            let _ = git(
                project_root,
                &["branch", "--unset-upstream", "--end-of-options", branch],
                deadline,
            )
            .await;
        }
        return failed(
            format!("deleting {branch} failed; git's output says why"),
            &deleted,
        );
    }

    Outcome::Succeeded {
        sha: Some(sha),
        output_tail: deleted.output_tail,
    }
}

/// Creates a lightweight tag at the tip of `at`.
///
/// The sha is resolved first and the tag written AT that object id rather than at the branch name,
/// for the reason `push` gives at length: what the row records has to be a statement about what
/// happened, not a re-reading of a ref that other queued operations are moving. Here it buys one
/// thing more — the tag and the row cannot disagree even if the branch advances between the two git
/// commands, which on a busy repository is a window this queue exists to have opinions about.
///
/// **No `-f`, ever, and a tag that already exists is git's refusal to report rather than ours to
/// overrule.** Moving an existing tag is the tag-shaped force push: it invalidates what everybody who
/// already fetched believes, and unlike a branch there is no expectation that it ever moves. So the
/// row comes back `Failed` carrying git's own `tag 'v1' already exists`.
///
/// `Failed` rather than `Blocked` for every refusal, on `publish_by_fast_forward`'s reasoning:
/// `Blocked` is terminal and means a human must clear something out of the way here and now, which
/// describes none of the ways this can fail.
///
/// **`--end-of-options` earns its place HERE, unlike in `push` where it is labelled unobservable —
/// measured, both ways.** Everything after it is a positional, so a flag that reached this argv
/// would be read as a tag name rather than as a flag: moving `-f` from before it to after it turns a
/// silent `Updated tag 'v1.0' (was 129eccf)` — somebody's release tag relocated — into
/// `fatal: too many arguments`. The difference from `push` is the argv shape rather than the care
/// taken: `git push <remote> <refspec>` has no option that could masquerade as either, and
/// `git tag -f <name>` is one keystroke from the operation this performs.
pub(super) async fn tag(
    project_root: &Path,
    name: &str,
    at: &str,
    deadline: std::time::Instant,
) -> Outcome {
    // `refs/heads/<at>` rather than `<at>` bare, which is what makes `at` a BRANCH rather than a
    // commit-ish the type merely calls one. A sha, a tag or `HEAD~1` fails here, naming the ref it
    // could not resolve — a refusal the caller can read, rather than a tag quietly written somewhere
    // the row's own type says it could not have been.
    let sha = match revision(project_root, &format!("refs/heads/{at}"), deadline).await {
        Ok(sha) => sha,
        Err(outcome) => return outcome,
    };

    let tagged = match git(
        project_root,
        &["tag", "--end-of-options", name, &sha],
        deadline,
    )
    .await
    {
        Ok(tagged) => tagged,
        Err(outcome) => return outcome,
    };
    if !tagged.succeeded() {
        return failed(
            format!("tagging {at} as {name} failed; git's output says why"),
            &tagged,
        );
    }

    Outcome::Succeeded {
        sha: Some(sha),
        output_tail: tagged.output_tail,
    }
}

/// Publishes `branch` to `remote`, by object id rather than by name.
///
/// **The sha is resolved first and then pushed as `<sha>:refs/heads/<branch>`, which is the whole
/// design of this function rather than a flourish.** Pushing the NAME would publish whatever the
/// branch points at in the instant git gets round to reading it, which is not necessarily what it
/// pointed at when the operation was admitted — the queue exists precisely because other things are
/// moving refs. Pushing the id makes the row's `result_sha` a statement about what is on the remote
/// instead of a guess, and it is what lets a human read the queue backwards afterwards.
///
/// A branch that does not exist fails at the `rev-parse`, before anything reaches the network, with a
/// message naming the ref rather than git's `src refspec … does not match any`.
///
/// **`--end-of-options` is here rather than argued away, and NO TEST CAN SEE IT — measured, not
/// assumed.** `Merge`'s arguments survive without one by a chain of accidents `Op` documents; none of
/// that chain is about `push`, whose argv is a different shape, so the flag was written rather than
/// reasoned around. Deleting it leaves every push test green, and that is not a coverage gap to be
/// filled: `Remote` and `Branch` both refuse a leading dash and the third argument is hexadecimal, so
/// there is no input that reaches here and needs it. It is the second lock on a door whose first lock
/// cannot be picked — worth having for the day a third argv shape arrives with a looser type, and
/// worth labelling as unobservable so nobody later mistakes its survival for dead weight.
///
/// **A rejected push is `Failed`, never `Blocked`.** `Blocked` is terminal and means the user's own
/// work is in the way here and now; a non-fast-forward means the remote moved, which is fixed by
/// bringing it in and resubmitting — and which the next queued operation may already be about to do.
/// `publish_by_fast_forward` makes the same call for the same reason: `Blocked` is the wrong way to
/// be wrong, because it needs a human to notice before anything can proceed.
pub(super) async fn push(
    project_root: &Path,
    remote: &str,
    branch: &str,
    deadline: std::time::Instant,
) -> Outcome {
    let reference = format!("refs/heads/{branch}");
    let sha = match revision(project_root, &reference, deadline).await {
        Ok(sha) => sha,
        Err(outcome) => return outcome,
    };
    push_sha(project_root, remote, branch, sha, deadline).await
}

/// Pushes `sha` as `refs/heads/<branch>`. Split from [`push`] so the executor can resolve the tip
/// first, decide on that sha (the post-land gate's green check) and then push exactly the sha it
/// decided on, instead of resolving a second time and risking a different one.
pub(super) async fn push_sha(
    project_root: &Path,
    remote: &str,
    branch: &str,
    sha: String,
    deadline: std::time::Instant,
) -> Outcome {
    let reference = format!("refs/heads/{branch}");
    let refspec = format!("{sha}:{reference}");
    let pushed = match git(
        project_root,
        &["push", "--end-of-options", remote, &refspec],
        deadline,
    )
    .await
    {
        Ok(pushed) => pushed,
        Err(outcome) => return outcome,
    };
    if !pushed.succeeded() {
        return failed(
            format!("pushing {branch} to {remote} failed; git's output says why"),
            &pushed,
        );
    }

    Outcome::Succeeded {
        sha: Some(sha),
        output_tail: pushed.output_tail,
    }
}
