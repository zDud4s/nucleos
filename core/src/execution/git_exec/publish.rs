//! Publishing a computed result onto the target branch, by update-ref or by fast-forward.

use super::*;

/// Publishes a computed merge onto `target`, by whichever of spec §6.3's routes applies.
///
/// Takes the `Computed` by value: it is consumed exactly once, and a publish that could be attempted
/// twice against the same `old` is a shape worth making impossible rather than documenting.
pub async fn publish(
    project_root: &Path,
    target: &str,
    computed: Computed,
    deadline: std::time::Instant,
) -> Outcome {
    // Nothing to publish: the merge found the target already contained the source.
    if computed.new == computed.old {
        return Outcome::Succeeded {
            sha: Some(computed.new),
            output_tail: computed.output_tail,
        };
    }

    let holder = match worktree_holding(project_root, target, deadline).await {
        Ok(holder) => holder,
        Err(outcome) => return outcome,
    };

    match holder {
        None => publish_by_update_ref(project_root, target, computed, deadline).await,
        Some(worktree) => {
            publish_by_fast_forward(project_root, &worktree, target, computed, deadline).await
        }
    }
}

pub(super) async fn publish_by_update_ref(
    project_root: &Path,
    target: &str,
    computed: Computed,
    deadline: std::time::Instant,
) -> Outcome {
    let reference = format!("refs/heads/{target}");
    // The third argument is git's own compare-and-swap. It is checked and applied inside git, so no
    // gap exists between reading the ref and moving it — which is the whole reason not to do this as
    // a rev-parse followed by an update.
    let updated = match git(
        project_root,
        &["update-ref", &reference, &computed.new, &computed.old],
        deadline,
    )
    .await
    {
        Ok(updated) => updated,
        Err(outcome) => return outcome,
    };
    if !updated.succeeded() {
        return failed(
            format!(
                "{target} moved while the merge was being computed, so it was not published; resubmit"
            ),
            &updated,
        );
    }
    Outcome::Succeeded {
        sha: Some(computed.new),
        output_tail: computed.output_tail,
    }
}

/// Fast-forwards the worktree that has `target` open, so HEAD, the index and the files move
/// together.
///
/// It is always *possible* to fast-forward here, and that is not luck: the merge commit's first
/// parent is `computed.old` by construction (Task 3's `--no-ff`), so the branch tip is an ancestor of
/// the new commit. The ref is re-read first because it may no longer be there — and the re-read
/// matters in **both** directions, which is easy to get wrong:
///
/// - The target moved **forward**: the fast-forward is genuinely impossible and git would refuse
///   anyway, so the check only improves the message.
/// - The target was **rewound** — somebody ran `reset --hard` and discarded work: `old~1` is still
///   an ancestor of the merge commit, so the fast-forward is entirely *possible*, and without this
///   check the publish rolls straight over the rewind, restores what they threw away, and reports
///   success. This is the direction the check exists for, and the only one a test can distinguish
///   it by.
///
/// **The refusal is then classified by asking a second question, not by reading git's message.**
/// `Blocked` is terminal and its advice is "commit or stash", so it must mean the user's own work is
/// in the way and nothing else. It would be wrong for a file another process has locked open — the
/// ordinary Windows case, `unable to unlink old 'target/app.exe': Permission denied` — for a worktree
/// git lists but whose directory is gone, for `dubious ownership`, or for a full disk. None of those
/// is fixed by committing, and telling a human to stash would be actively misleading advice on a
/// state that is terminal.
///
/// So: `status --porcelain` in that worktree. Non-empty means the user has work there and `Blocked`
/// is the honest answer; empty, or a status that will not even run, means the fast-forward was
/// possible and something else refused — `Failed`, with git's own output as the diagnostic. It never
/// parses git's prose: `worktree.rs:321-327` is explicit that stderr is localised and
/// version-dependent and must not be classified on. The message is *carried*, so the human reading
/// the row sees which files git named, and never consulted.
///
/// **This is a one-directional guard, not an exact mapping, and it should not be read as one.** It
/// reliably keeps a *clean* worktree out of `Blocked` — which is the case that mattered, since a
/// clean worktree's refusal is never something committing would fix. It does not help when the
/// worktree is dirty for an unrelated reason *and* refuses for another: a locked file in a checkout
/// that also has ordinary uncommitted work still comes back `Blocked` with advice — "commit, stash
/// or move it" — that will not help. That gap is narrow, and it is not silent, because the output
/// tail carries what git actually said.
///
/// The mirror-image gap is *not* here, and that was measured rather than assumed: an **ignored** file
/// in the way would be invisible to `status --porcelain` and would land in `Failed`, but it never
/// reaches the classification — git overwrites an ignored file instead of refusing (checked on git
/// 2.50.1, with `merge.overwriteIgnore` both true and false). The publish therefore replaces such a
/// file silently, which is what a fast-forward does anywhere and not something this route adds.
///
/// `Failed` rather than `Blocked` is also the right way to be wrong: `Blocked` is terminal by
/// design, so a transient problem misfiled there would need a human to notice it and resubmit.
pub(super) async fn publish_by_fast_forward(
    project_root: &Path,
    worktree: &Path,
    target: &str,
    computed: Computed,
    deadline: std::time::Instant,
) -> Outcome {
    // The mirror of `prepare_integration_worktree`'s guard, and it runs before any git command is
    // pointed at this directory for the same reason: `git -C <dir>` walks UP until it finds a
    // repository. A worktree git still has a row for, whose directory survives but whose `.git` is
    // gone, therefore answers for whatever repository *encloses* it — measured: `worktree list`
    // still emits its `branch refs/heads/<target>` line (with a `prunable` line this module does not
    // read), and `status --porcelain` there exits 0 carrying the enclosing repository's dirt. That
    // lands the row in `Blocked`, terminal, telling a human to commit files in a repository nobody
    // named. `merge --ff-only` walks up too, so where the enclosing repository is the one that owns
    // the merge object, the fast-forward can land somewhere nobody asked us to touch.
    //
    // **Existence only, deliberately — NOT `is_file()`, which is what the integration guard uses.**
    // The two look like they should agree and must not: that guard only ever inspects the daemon's
    // own worktree, which is always linked and so always has a `.git` FILE. This one is handed
    // whatever holds the branch, and the commonest holder in the whole design is the user's main
    // checkout, whose `.git` is a DIRECTORY. `is_file()` here would reject the ordinary case on
    // every merge — `a_branch_somebody_has_open_is_fast_forwarded_in_place` is what goes red if
    // somebody ever "fixes" this into agreeing with the other one.
    //
    // **Both holder tests are needed and neither subsumes the other**, which is worth saying before
    // somebody consolidates them on the grounds that they look alike. Only the one named above can
    // catch that tightening: its holder is the MAIN checkout, so its `.git` is a directory and
    // `is_file()` turns it red. `a_merge_into_a_branch_somebody_has_open_moves_their_whole_worktree`
    // holds the branch in a LINKED worktree, whose `.git` is a file — it would sail through the same
    // change untouched, which is precisely why its subject is the composition end to end and not
    // this guard.
    if tokio::fs::metadata(worktree.join(".git")).await.is_err() {
        return Outcome::Failed {
            reason: format!(
                "{target} is checked out in {}, which is no longer a git worktree; run `git worktree prune` and resubmit",
                worktree.display()
            ),
            exit_code: None,
            output_tail: String::new(),
        };
    }

    let current = match revision(project_root, &format!("refs/heads/{target}"), deadline).await {
        Ok(current) => current,
        Err(outcome) => return outcome,
    };
    if current != computed.old {
        return Outcome::Failed {
            reason: format!(
                "{target} moved while the merge was being computed, so it was not published; resubmit"
            ),
            exit_code: None,
            output_tail: String::new(),
        };
    }

    let merged = match git(worktree, &["merge", "--ff-only", &computed.new], deadline).await {
        Ok(merged) => merged,
        Err(outcome) => return outcome,
    };
    if merged.succeeded() {
        return Outcome::Succeeded {
            sha: Some(computed.new),
            output_tail: computed.output_tail,
        };
    }

    let status = git(worktree, &["status", "--porcelain"], deadline).await;
    // A status that could not run leaves the question unanswered, and unanswered must fall to
    // `Failed`: claiming the user's files are in the way when that was never established is the one
    // wrong answer here.
    let user_work_in_the_way = matches!(
        &status,
        Ok(status) if status.succeeded() && !status.stdout.trim().is_empty()
    );

    if user_work_in_the_way {
        Outcome::Blocked {
            reason: format!(
                // "or move it", because the blocked case this chunk actually tests is an UNTRACKED
                // file, and `git stash` without `-u` does not move one — git's own message for it
                // says "please move or remove them". Advice that does not work on the case in the
                // test is advice that does not work.
                "{target} is checked out in {} with uncommitted work in the way; commit, stash or move it and resubmit",
                worktree.display()
            ),
            output_tail: merged.output_tail,
        }
    } else {
        Outcome::Failed {
            reason: format!(
                "{target} is checked out in {} and could not be fast-forwarded; git's output says why",
                worktree.display()
            ),
            exit_code: merged.exit_code,
            output_tail: merged.output_tail,
        }
    }
}
