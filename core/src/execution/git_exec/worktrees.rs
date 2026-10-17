//! Preparing the worktrees the queue works in: integration, postgate and fix branches, reset or create, and the outcomes a failure becomes.

use super::*;

/// The outcome for a git command that ran and refused: what was being attempted, plus git's own
/// answer, which is the only diagnostic the row will carry.
pub(super) fn failed(reason: String, result: &CommandResult) -> Outcome {
    Outcome::Failed {
        reason,
        exit_code: result.exit_code,
        output_tail: result.output_tail.clone(),
    }
}

/// `failed`'s sibling for the outcomes a person has to look at. Same columns, different status —
/// see `Outcome::Escalated` for why that distinction is the whole point rather than a label.
pub(super) fn escalated(reason: String, result: &CommandResult) -> Outcome {
    Outcome::Escalated {
        reason,
        exit_code: result.exit_code,
        output_tail: result.output_tail.clone(),
    }
}

/// Ensures the integration worktree exists and is clean, and returns its path.
///
/// Resetting is safe here in a way it never is in a user's copy, and that asymmetry is the whole
/// design: this directory is the daemon's property, always detached, and nobody edits anything in it.
/// Spec §7 says a dirty integration worktree is reset before the next operation, and this is where.
pub(super) async fn prepare_integration_worktree(
    project_root: &Path,
    deadline: std::time::Instant,
) -> Result<std::path::PathBuf, Outcome> {
    let integration = integration_worktree(project_root);
    // Before the hash suffix the directory was `integration-<leaf>`. Retire this repository's tree
    // there, if any, so an upgraded machine does not keep an orphaned registered checkout.
    if let Some(parent) = integration.parent() {
        let legacy = parent.join(format!(
            "{INTEGRATION_PREFIX}{}",
            project_root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unnamed".to_owned())
        ));
        crate::execution::worktree::retire_legacy_worktree(project_root, &legacy, &integration)
            .await;
    }
    reset_or_create_worktree(project_root, &integration, "integration", deadline).await
}

/// Ensures the post-merge gate's worktree exists, is clean, and is checked out detached at `sha`.
///
/// The directory is the daemon's property exactly like the integration tree, so it takes the same
/// guard and the same reset; the checkout then moves it to the tip the gate is about to measure.
/// It never goes through the vcs queue: no branch ref is touched.
pub(crate) async fn prepare_postgate_worktree(
    project_root: &Path,
    sha: &str,
    deadline: std::time::Instant,
) -> Result<std::path::PathBuf, String> {
    let path = crate::verify::postgate::postgate_worktree(project_root);
    let path = reset_or_create_worktree(project_root, &path, "postgate", deadline)
        .await
        .map_err(|outcome| match outcome {
            Outcome::Failed {
                reason,
                output_tail,
                ..
            } => format!("{reason}: {output_tail}"),
            other => format!("{other:?}"),
        })?;
    let moved = git(&path, &["checkout", "--detach", "--force", sha], deadline)
        .await
        .map_err(|outcome| format!("{outcome:?}"))?;
    if !moved.succeeded() {
        return Err(format!(
            "could not check out {sha} in {}: {}",
            path.display(),
            moved.output_tail
        ));
    }
    Ok(path)
}

/// A failed queue-style git step as the one-line reason `prepare_fix_branch` returns.
pub(super) fn outcome_reason(outcome: Outcome) -> String {
    match outcome {
        Outcome::Failed {
            reason,
            output_tail,
            ..
        } => format!("{reason}: {output_tail}"),
        other => format!("{other:?}"),
    }
}

/// Creates the correction branch `branch` (`fix/...`) for a published revert: its tip is a revert
/// of `revert_sha`, so its first commit puts the reverted work back on top of the target and the
/// run that follows starts from the culprit's own tree (spec 2026-10-05 §6.2, D4).
///
/// The branch is only ever CREATED, and never through the queue: `update-ref` is given the zero id
/// as the old value, which makes git refuse when the ref exists, so a name that is taken is never
/// moved. A second call adopts the branch it made itself, recognised by its tip's first parent being
/// `revert_sha`, and returns that tip; any other branch under the name is an error and is left
/// exactly as it was. The work is done in the daemon's `postgate-` worktree, which is idle while no
/// gate runs; the integration worktree belongs to the queue.
pub(crate) async fn prepare_fix_branch(
    project_root: &Path,
    revert_sha: &str,
    branch: &str,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let full = format!("refs/heads/{branch}");
    let spec = format!("{full}^{{commit}}");
    let existing = git(
        project_root,
        &["rev-parse", "--verify", "--quiet", &spec],
        deadline,
    )
    .await
    .map_err(outcome_reason)?;
    if existing.succeeded() {
        let tip = existing.stdout.trim().to_owned();
        let parent_spec = format!("{tip}^1");
        let parent = git(
            project_root,
            &["rev-parse", "--verify", "--quiet", &parent_spec],
            deadline,
        )
        .await
        .map_err(outcome_reason)?;
        if parent.succeeded() && parent.stdout.trim() == revert_sha {
            return Ok(tip);
        }
        return Err(format!(
            "{branch} already exists and is not the correction branch of {revert_sha}; \
             it was left untouched"
        ));
    }

    let tree = prepare_postgate_worktree(project_root, revert_sha, deadline).await?;
    let reverted = git(
        &tree,
        &["revert", "--no-edit", "--end-of-options", revert_sha],
        deadline,
    )
    .await
    .map_err(outcome_reason)?;
    if !reverted.succeeded() {
        // Best-effort, like the revert in `compute_revert`: the next use resets this tree anyway.
        let _ = git(&tree, &["revert", "--abort"], deadline).await;
        return Err(format!(
            "could not revert {revert_sha} to build {branch}: {}",
            reverted.output_tail.trim()
        ));
    }
    let tip = revision(&tree, "HEAD", deadline)
        .await
        .map_err(outcome_reason)?;
    let zero = "0".repeat(tip.len());
    let created = git(project_root, &["update-ref", &full, &tip, &zero], deadline)
        .await
        .map_err(outcome_reason)?;
    if !created.succeeded() {
        return Err(format!(
            "could not create {branch}: {}",
            created.output_tail.trim()
        ));
    }
    Ok(tip)
}

/// The shared body of the daemon-owned detached trees: create `path` when it is absent, otherwise
/// guard it, reset it and clean it. `what` names the tree in the creation failure.
pub(super) async fn reset_or_create_worktree(
    project_root: &Path,
    path: &Path,
    what: &str,
    deadline: std::time::Instant,
) -> Result<std::path::PathBuf, Outcome> {
    let integration = path.to_path_buf();

    if tokio::fs::metadata(&integration).await.is_err() {
        return create_integration_worktree(project_root, &integration, what, deadline).await;
    }

    // **This check runs before any git command is pointed at that directory, and it is load-bearing.**
    // `git -C <dir>` walks UP until it finds a repository. So a directory that exists but is not a
    // worktree does not make `reset --hard` fail — it makes it *succeed* against whatever ancestor
    // repository encloses it — reverting every uncommitted tracked change in that repository, by way
    // of a command that reports success. That is precisely the runtime destruction this function is
    // written to avoid. The two commands below are not equally dangerous, and saying so exactly
    // matters: `reset --hard` is repository-wide, while the `clean -fd` is *cwd-relative* and would
    // remove only this directory's own contents. The reset is the half that destroys work. None of
    // this is hypothetical in the tests either: `space_free_tempdir` places the worktree root inside
    // this very checkout, which is why the test for this guard builds a sacrificial repository
    // between the two rather than letting the walk-up reach the real one.
    //
    // A linked worktree always has a `.git` FILE naming its admin directory, and `is_file` rather
    // than mere existence is deliberate: a `.git` *directory* at that path is a standalone
    // repository somebody put there, which would pass an existence check and then be reset and
    // cleaned. Every other state that reaches here is not a worktree either — an interrupted
    // `worktree add`, a hand-deleted `.git`, a directory somebody created by mistake.
    if !tokio::fs::metadata(integration.join(".git"))
        .await
        .is_ok_and(|entry| entry.is_file())
    {
        return Err(Outcome::Failed {
            reason: format!(
                "{} exists but is not a git worktree; remove that directory and resubmit",
                integration.display()
            ),
            exit_code: None,
            output_tail: String::new(),
        });
    }

    // It already existed, so it may be mid-merge, dirty, or both. `merge --abort` fails loudly when
    // there is no merge to abort, which is the ordinary case — its exit code is deliberately unread.
    let _ = git(&integration, &["merge", "--abort"], deadline).await;
    let reset = git(&integration, &["reset", "--hard"], deadline).await?;
    if !reset.succeeded() {
        // A `.git` that is present but no longer resolves — the project was re-cloned, so the admin
        // directory it names is gone. Reported rather than repaired, for the reason `worktree.rs`
        // guards its own deletes with `is_dangerous_removal_path`: a runtime `remove_dir_all` that
        // is wrong once is far worse than a merge that fails with an actionable message, and that
        // guard is not free to reproduce.
        return Err(failed(
            format!(
                "{} is a worktree but is not usable; remove that directory and resubmit",
                integration.display()
            ),
            &reset,
        ));
    }
    // `-fd`, not `-fdx`: ignored files are build output that costs nothing to keep and minutes to
    // rebuild, and nothing git ignores can affect a merge.
    //
    // Discarded like the abort above, but not for the same reason and not for free: a failing
    // `clean` is abnormal rather than ordinary — a file held open by an indexer or an editor is the
    // usual Windows cause — and what it then leaves behind resurfaces as the next command's "could
    // not check out <target> to merge into", which does not look related to it. Accepted anyway,
    // because the checkout is the step that actually knows whether the litter is in the way, and
    // refusing here would fail operations it would have completed.
    let _ = git(&integration, &["clean", "-fd"], deadline).await;
    Ok(integration)
}

/// Creates it, retrying once behind a `worktree prune`.
///
/// The retry is not defensive padding — it is the only way out of one specific state. A directory
/// deleted by hand while git still holds a row for it makes `worktree add` refuse that path forever,
/// and `prune` is what drops rows whose directory is missing (`worktree.rs:368-373`): exactly that
/// row, and by construction only rows already describing something that is gone.
///
/// Pruning is deliberately **not** done up front on every operation. It rewrites the worktree
/// registry for the whole project, including the rows `worktree.rs` owns for run and job worktrees,
/// and doing that on every merge would quietly make this module a co-owner of a lifecycle it has no
/// business in. Reaching for it only after `worktree add` has actually failed keeps the blast radius
/// to the case that needs it.
pub(super) async fn create_integration_worktree(
    project_root: &Path,
    integration: &Path,
    what: &str,
    deadline: std::time::Instant,
) -> Result<std::path::PathBuf, Outcome> {
    if let Some(parent) = integration.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| Outcome::Failed {
                reason: format!("could not create the worktree root: {error}"),
                exit_code: None,
                output_tail: String::new(),
            })?;
    }

    // Hidden BEFORE `git worktree add` runs, same as `worktree::create_at` — this is the other
    // creation site, and a tree that cannot be hidden must not exist.
    crate::execution::worktree::hide_root_if_nested(
        project_root,
        &crate::execution::worktree::worktree_root(project_root),
    )
    .await
    .map_err(|error| Outcome::Failed {
        reason: format!("could not hide the worktree root from git: {error}"),
        exit_code: None,
        output_tail: String::new(),
    })?;

    let mut added = add_worktree(project_root, integration, deadline).await?;
    if !added.succeeded() {
        let _ = git(project_root, &["worktree", "prune"], deadline).await;
        added = add_worktree(project_root, integration, deadline).await?;
    }
    if !added.succeeded() {
        return Err(failed(
            format!("could not create the {what} worktree"),
            &added,
        ));
    }
    Ok(integration.to_path_buf())
}

/// The only argv in this module with a path in it, and therefore the only *production* caller of
/// `run_git` that is not `git`. The test module below calls it directly three more times, testing
/// the transport rather than going through this module's budget gate.
pub(super) async fn add_worktree(
    project_root: &Path,
    integration: &Path,
    deadline: std::time::Instant,
) -> Result<CommandResult, Outcome> {
    let budget = remaining(deadline, "worktree add")?;
    run_git(
        project_root,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--detach"),
            integration.as_os_str(),
        ],
        budget,
    )
    .await
    .map_err(|reason| Outcome::Failed {
        reason,
        exit_code: None,
        output_tail: String::new(),
    })
}
