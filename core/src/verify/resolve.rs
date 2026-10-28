//! Resolving the repository state and paths to verify.

use super::*;
use crate::execution::vcs;

/// One git read inside a request's budget.
///
/// Calls `run_git` directly, past `git_exec`'s list of sanctioned entries, for the reason
/// `map_stamp::digest` gives: this is not a queue operation and writes nothing. It keeps the
/// property that list protects anyway — what is left of `deadline` is computed here, and an
/// already-spent budget is refused before a child is spawned.
pub(super) async fn git(
    repo: &Path,
    args: &[&str],
    deadline: Instant,
) -> Result<CommandResult, String> {
    let budget = deadline.saturating_duration_since(Instant::now());
    if budget.is_zero() {
        return Err("the request ran out of time before git could answer".to_owned());
    }
    let args: Vec<&OsStr> = args.iter().map(|arg| OsStr::new(*arg)).collect();
    git_exec::run_git(repo, &args, budget).await
}

/// Which worktree is verified, and the project it belongs to.
///
/// A job node verifies the worktree its run works in and nothing else: verification runs the
/// project's own commands, so naming another worktree would run them over code this run does not
/// own. The owner may name any worktree, but it has to be a registered worktree of a known
/// project — a path that merely holds a `.git` is not something the daemon has agreed to run.
pub async fn resolve_worktree(
    pool: &SqlitePool,
    caller: Caller,
    asked: Option<&str>,
    deadline: Instant,
) -> Result<(PathBuf, String), VerifyError> {
    let (root, run_project) = match caller {
        Caller::Run(id) => {
            let row: Option<(Option<String>, Option<String>)> =
                sqlx::query_as("SELECT project_id, cwd FROM runs WHERE id = ?")
                    .bind(id)
                    .fetch_optional(pool)
                    .await
                    .map_err(internal)?;
            let Some((project_id, Some(cwd))) = row else {
                return Err(VerifyError::Forbidden(format!(
                    "run {id} has no working directory to verify"
                )));
            };
            let own = git_exec::toplevel(Path::new(&cwd), deadline)
                .await
                .map_err(|error| {
                    VerifyError::Unprocessable(format!("{cwd} is not a git worktree: {error}"))
                })?;
            if let Some(asked) = asked {
                let own_key = git_exec::canonical(&own)
                    .await
                    .map_err(VerifyError::Unprocessable)?;
                // A path that cannot be canonicalised cannot be the run's own, which can.
                let asked_key = git_exec::canonical(Path::new(asked)).await.ok();
                if asked_key.as_deref() != Some(own_key.as_str()) {
                    return Err(VerifyError::Forbidden(format!(
                        "a job node verifies its own worktree, {}",
                        own.display()
                    )));
                }
            }
            (own, project_id)
        }
        // The daemon names the job's worktree itself, and it is held to the same registered-
        // worktree-of-a-known-project check as the owner's.
        Caller::Merge(_)
        | Caller::Owner
        | Caller::Job(_)
        | Caller::Postgate
        | Caller::FlakeCheck
        | Caller::Bisect => {
            let Some(asked) = asked else {
                return Err(VerifyError::BadRequest("worktree is required".to_owned()));
            };
            // A relative path would be resolved against the daemon's own directory, which is
            // never what a caller in another directory meant.
            if !Path::new(asked).is_absolute() {
                return Err(VerifyError::BadRequest(format!(
                    "worktree must be an absolute path, not {asked}"
                )));
            }
            let root = git_exec::toplevel(Path::new(asked), deadline)
                .await
                .map_err(|error| {
                    VerifyError::Unprocessable(format!("{asked} is not a git worktree: {error}"))
                })?;
            (root, None)
        }
    };

    let project_id = vcs::project_for_worktree(pool, &root, deadline)
        .await
        .map_err(VerifyError::NotFound)?;
    if let Some(run_project) = run_project.as_deref()
        && run_project != project_id
    {
        return Err(VerifyError::Forbidden(format!(
            "this run belongs to {run_project}, but its worktree is in {project_id}"
        )));
    }
    let project_root = inspect::project_root(pool, &project_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            VerifyError::NotFound(format!("project {project_id} has no root recorded"))
        })?;

    let listed = git(
        Path::new(&project_root),
        &["worktree", "list", "--porcelain"],
        deadline,
    )
    .await
    .map_err(VerifyError::Unprocessable)?;
    if !listed.succeeded() {
        return Err(VerifyError::Unprocessable(format!(
            "could not list {project_id}'s worktrees: {}",
            listed.output_tail
        )));
    }
    let root_key = git_exec::canonical(&root)
        .await
        .map_err(VerifyError::Unprocessable)?;
    let mut registered = false;
    for path in listed
        .stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
    {
        // A listed worktree whose directory is gone cannot canonicalise; it is simply not ours.
        if git_exec::canonical(Path::new(path)).await.ok().as_deref() == Some(root_key.as_str()) {
            registered = true;
            break;
        }
    }
    if !registered {
        return Err(VerifyError::NotFound(format!(
            "{} is not a worktree of {project_id}",
            root.display()
        )));
    }
    Ok((root, project_id))
}

/// The full object id `rev` names in `repo`, or why git would not resolve it to a commit.
pub async fn resolve_commit(repo: &Path, rev: &str, deadline: Instant) -> Result<String, String> {
    let spec = format!("{rev}^{{commit}}");
    let resolved = git(repo, &["rev-parse", "--verify", "--quiet", &spec], deadline).await?;
    let sha = resolved.stdout.trim();
    if !resolved.succeeded() || sha.is_empty() {
        return Err(format!("{rev} is not a commit here"));
    }
    Ok(sha.to_owned())
}

/// The tree object `rev` names in `repo`: what a commit's content is, whatever its history.
pub async fn tree_of(repo: &Path, rev: &str, deadline: Instant) -> Result<String, String> {
    let spec = format!("{rev}^{{tree}}");
    let resolved = git(repo, &["rev-parse", "--verify", "--quiet", &spec], deadline).await?;
    let tree = resolved.stdout.trim();
    if !resolved.succeeded() || tree.is_empty() {
        return Err(format!("{rev} has no tree here"));
    }
    Ok(tree.to_owned())
}

/// `HEAD`'s tree when `worktree` has nothing uncommitted, `None` when it has anything: a tracked
/// edit, a staged one or an untracked file (ignored files do not count, git does not list them).
pub(super) async fn clean_tree(
    worktree: &Path,
    deadline: Instant,
) -> Result<Option<String>, String> {
    let status = git(
        worktree,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
        deadline,
    )
    .await?;
    if !status.succeeded() {
        return Err(format!(
            "could not read the worktree's status: {}",
            status.output_tail
        ));
    }
    if !status.stdout.is_empty() {
        return Ok(None);
    }
    tree_of(worktree, "HEAD", deadline).await.map(Some)
}

/// The commit `own` and `scope` diff against.
///
/// In order: what the caller named; the base the daemon recorded when it created this worktree
/// (exact, survives the integration branch moving on); else the merge base with the project's
/// integration branch, which is what a hand-made worktree branched from.
pub async fn resolve_base(
    pool: &SqlitePool,
    project_id: &str,
    project_root: &Path,
    worktree_root: &Path,
    explicit: Option<&str>,
    deadline: Instant,
) -> Result<String, VerifyError> {
    if let Some(explicit) = explicit {
        // Hex only: whatever is passed here reaches git's argv, and a ref name could be an option.
        let well_formed =
            (7..=40).contains(&explicit.len()) && explicit.chars().all(|c| c.is_ascii_hexdigit());
        if !well_formed {
            return Err(VerifyError::BadRequest(format!(
                "base must be a commit id (7 to 40 hex digits), not {explicit}"
            )));
        }
        return resolve_commit(worktree_root, explicit, deadline)
            .await
            .map_err(VerifyError::Unprocessable);
    }

    let mut reasons = Vec::new();

    let recorded: Vec<(String, String)> = sqlx::query_as(
        "SELECT path, base_sha FROM worktrees WHERE project_id = ? AND removed_at IS NULL \
         AND base_sha IS NOT NULL ORDER BY created_at DESC",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(internal)?;
    let root_key = git_exec::canonical(worktree_root).await.ok();
    if let Some(root_key) = root_key.as_deref() {
        for (path, base_sha) in recorded {
            if git_exec::canonical(Path::new(&path)).await.ok().as_deref() == Some(root_key) {
                return Ok(base_sha);
            }
        }
    }
    reasons.push("the daemon recorded no base for this worktree".to_owned());

    match land::integration_branch(pool, project_id, project_root, deadline).await {
        Ok(branch) => {
            let target = format!("refs/heads/{}", branch.as_str());
            match git(worktree_root, &["merge-base", "HEAD", &target], deadline).await {
                Ok(result) if result.succeeded() && !result.stdout.trim().is_empty() => {
                    return Ok(result.stdout.trim().to_owned());
                }
                Ok(result) => reasons.push(format!(
                    "no merge base with {}: {}",
                    branch.as_str(),
                    result.output_tail.trim()
                )),
                Err(error) => reasons.push(error),
            }
        }
        Err(error) => reasons.push(error),
    }

    Err(VerifyError::Unprocessable(format!(
        "cannot resolve a base: {}",
        reasons.join("; ")
    )))
}

/// The base a `cover` request diffs from: the post-merge gate's newest proof for the project's land
/// target that the worktree descends from (F3-13).
///
/// The candidates are `covered_sha` and `last_green_sha`; each must resolve here and be an ancestor
/// of the worktree's `HEAD`. When both qualify the newer one wins. A diff from an older green is a
/// superset of the branch's own diff, so a base that is too old only ever selects more groups.
pub(super) async fn cover_base(
    pool: &SqlitePool,
    project_id: &str,
    project_root: &Path,
    worktree_root: &Path,
    deadline: Instant,
) -> Result<String, VerifyError> {
    let Some(state) = verify_postgate::load(pool, project_id)
        .await
        .map_err(internal)?
    else {
        return Err(VerifyError::Unprocessable(format!(
            "the post-merge gate holds no green for {project_id}"
        )));
    };
    let branch = land::integration_branch(pool, project_id, project_root, deadline)
        .await
        .map_err(VerifyError::Unprocessable)?;
    if state.target != branch.as_str() {
        return Err(VerifyError::Unprocessable(format!(
            "the post-merge gate tracks {}, not {}; there is no green to cover from",
            state.target,
            branch.as_str()
        )));
    }

    newest_proof(&state, worktree_root, deadline)
        .await?
        .ok_or_else(|| {
            VerifyError::Unprocessable(format!(
                "no green or covered commit of {branch} is in this worktree's history; merge {branch} first",
                branch = branch.as_str()
            ))
        })
}

/// The newest of the gate's `covered_sha` / `last_green_sha` that resolves in `worktree_root` and
/// is an ancestor of its `HEAD`, or `None` when neither qualifies.
async fn newest_proof(
    state: &verify_postgate::State,
    worktree_root: &Path,
    deadline: Instant,
) -> Result<Option<String>, VerifyError> {
    let mut kept: Vec<String> = Vec::new();
    for candidate in [&state.covered_sha, &state.last_green_sha]
        .into_iter()
        .flatten()
    {
        let Ok(sha) = resolve_commit(worktree_root, candidate, deadline).await else {
            continue;
        };
        if kept.contains(&sha) {
            continue;
        }
        if is_ancestor(worktree_root, &sha, "HEAD", deadline).await? {
            kept.push(sha);
        }
    }

    match kept.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        [first, second] => {
            // The newer of the two is the one the other is an ancestor of; unrelated proofs are
            // both valid bases, and the first (`covered_sha`) is the more recently measured.
            if is_ancestor(worktree_root, first, second, deadline).await? {
                Ok(Some(second.clone()))
            } else {
                Ok(Some(first.clone()))
            }
        }
        _ => unreachable!("there are two candidates at most"),
    }
}

/// The `NUCLEOS_GATE_BASE` for a unit the post-merge gate runs in `worktree_root`: what the
/// project's `gate_command` (`gate-diff.sh`) diffs `HEAD` from to choose its targets.
///
/// The newest green or covered commit that `HEAD` descends from; with none, the repository's root
/// commit, so the diff spans the whole tree and every target is selected. Never empty on success:
/// the gate worktree sits at the target's tip, where `gate-diff.sh`'s own default base (the merge
/// base with `master`) is `HEAD` itself and selects nothing. `None` only when git cannot answer.
/// Read from the stored state at run time because a unit is started again by crash recovery and by
/// bisect probes, not only by the tick that first queued it.
pub(super) async fn postgate_gate_base(
    pool: &SqlitePool,
    project_id: &str,
    worktree_root: &Path,
    deadline: Instant,
) -> Option<String> {
    let state = verify_postgate::load(pool, project_id).await.ok().flatten();
    if let Some(state) = state
        && let Ok(Some(sha)) = newest_proof(&state, worktree_root, deadline).await
    {
        return Some(sha);
    }
    let roots = git(
        worktree_root,
        &["rev-list", "--max-parents=0", "HEAD"],
        deadline,
    )
    .await
    .ok()?;
    if !roots.succeeded() {
        return None;
    }
    roots
        .stdout
        .lines()
        .next()
        .map(|root| root.trim().to_owned())
}

/// Whether `ancestor` is an ancestor of `descendant` (or the same commit) in `repo`.
pub(super) async fn is_ancestor(
    repo: &Path,
    ancestor: &str,
    descendant: &str,
    deadline: Instant,
) -> Result<bool, VerifyError> {
    git(
        repo,
        &["merge-base", "--is-ancestor", ancestor, descendant],
        deadline,
    )
    .await
    .map(|result| result.succeeded())
    .map_err(VerifyError::Unprocessable)
}

/// Splits git's `-z` output into its non-empty entries.
pub(super) fn nul_separated(stdout: &str) -> impl Iterator<Item = &str> {
    stdout.split('\0').filter(|entry| !entry.is_empty())
}

/// Every path that differs from `base`: committed or not, and untracked files too, because a new
/// file nobody has added yet is still code the caller is about to hand in.
pub async fn changed_paths(
    worktree_root: &Path,
    base: &str,
    deadline: Instant,
) -> Result<Vec<String>, VerifyError> {
    let diff = git(
        worktree_root,
        &["diff", "--name-only", "-z", "--no-renames", base, "--"],
        deadline,
    )
    .await
    .map_err(VerifyError::Unprocessable)?;
    if !diff.succeeded() {
        return Err(VerifyError::Unprocessable(format!(
            "could not diff against {base}: {}",
            diff.output_tail
        )));
    }
    let untracked = git(
        worktree_root,
        &["ls-files", "-z", "--others", "--exclude-standard"],
        deadline,
    )
    .await
    .map_err(VerifyError::Unprocessable)?;
    if !untracked.succeeded() {
        return Err(VerifyError::Unprocessable(format!(
            "could not list untracked files: {}",
            untracked.output_tail
        )));
    }
    let paths: BTreeSet<String> = nul_separated(&diff.stdout)
        .chain(nul_separated(&untracked.stdout))
        .map(str::to_owned)
        .collect();
    Ok(paths.into_iter().collect())
}
