//! Read-only questions about a repository: toplevel, origin and HEAD, branches, ancestry, first-parent history, the repository key and the current branch.

use super::*;

/// One git command, given whatever is left of the operation's budget.
///
/// A spent budget is refused **here, before anything is spawned**, and that is not the same as
/// letting `run_git` time it out. `tokio::process::Command::output()` spawns the child eagerly, as
/// the argument to `timeout` is evaluated — so handing `run_git` a `Duration::ZERO` would launch git
/// and immediately kill it. For `rev-parse` that is merely wasteful; for `merge` or `update-ref` it
/// is a mutation killed mid-flight, which is a worse thing to do to a repository than declining to
/// start. `run_git`'s own zero handling stays as the transport-level backstop it was written to be.
pub(super) async fn git(
    repo: &Path,
    args: &[&str],
    deadline: std::time::Instant,
) -> Result<CommandResult, Outcome> {
    let budget = remaining(deadline, &args.join(" "))?;
    let arguments: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    run_git(repo, &arguments, budget)
        .await
        // An `Outcome::Failed` with an empty tail, which is right whenever no command produced
        // output: git did not run, so there is nothing it printed. Not `Unexecutable` — that one
        // means the *row* could not be executed, and this row was fine; the subprocess was not.
        .map_err(|reason| Outcome::Failed {
            reason,
            exit_code: None,
            output_tail: String::new(),
        })
}

/// What is left of the operation's budget, or the outcome to record if it is already spent.
///
/// Separate from `git` because `add_worktree` needs it too — it is the one call that goes to
/// `run_git` directly, and it must not be the one place a spent budget still spawns a process.
pub(super) fn remaining(deadline: std::time::Instant, what: &str) -> Result<Duration, Outcome> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(Outcome::Failed {
            reason: format!("the operation ran out of time before `git {what}` could start"),
            exit_code: None,
            output_tail: String::new(),
        });
    }
    Ok(budget)
}

/// The canonical identity of the repository at `path`, for the queue to take its lock on.
///
/// `--git-common-dir` rather than `--git-dir`: a linked worktree's `--git-dir` is its own private
/// `.git/worktrees/<name>`, so keying on it would give every worktree of one repository a key of its
/// own, and the queue would run two merges against the same refs at once — the single thing it
/// exists to prevent. `--git-common-dir` is one value for all of them.
///
/// **`--show-toplevel` is checked as well, and it is the guard rather than decoration.** `git -C
/// <dir>` walks UP until it finds a repository, so `rev-parse` inside an ordinary subdirectory
/// succeeds and answers about the enclosing one. Requiring the caller's own path to BE the top level
/// turns "found a repository" into "found this repository".
///
/// **What that guard costs, stated because it is a restriction and not a free check:** the queue
/// serves projects whose recorded root is the root of a working-tree repository. A project rooted at
/// a package inside a monorepo is refused, and so is a bare repository — `--show-toplevel` exits 128
/// there ("this operation must be run in a work tree"). Both are correct refusals today, because
/// this module computes merges in a worktree it adds under that root; both are also the first thing
/// to revisit if a project ever needs to be rooted deeper.
///
/// Both paths are canonicalised before being compared, and that comparison is where canonicalisation
/// is load-bearing: git answers with forward slashes and the caller holds a Windows path, so a raw
/// comparison would reject every valid root. Canonicalising the RETURNED key buys less than it looks
/// — git already normalises drive-letter case and `.`/`..` itself (measured) — and is kept for
/// junctions and symlinks, where two spellings genuinely reach one directory.
///
/// The root of the working tree `path` sits in, whether `path` is that root or a directory under it.
///
/// **The one question `repo_key` and `current_branch` deliberately refuse to answer**, and it exists
/// because a caller arrived that genuinely does not know: an interactive session's `cwd` is wherever
/// the person happened to be standing, which is a subdirectory far more often than not. Both of the
/// others demand the path already BE the top level, and that demand is right for them — it turns
/// "found a repository" into "found this repository". This function is how a caller earns the path
/// that satisfies it, rather than each caller inventing its own `rev-parse`.
///
/// Skipping it is the hole, not a shortcut. `git -C <dir>` walks UP, so `current_branch` called on a
/// subdirectory does not fail — it refuses with "not the root", and a caller that reads that refusal
/// as "not a repository, nothing to govern here" hands the session exactly the bypass the gate
/// exists to close. Measured: every one of this repo's own worktrees answers a different toplevel and
/// the SAME common dir.
///
/// **Named `toplevel` and not `worktree_root`, which is taken and means the opposite end of the
/// same word.** `worktree::worktree_root(project_root)` answers "where does this project's worktrees
/// get CREATED" and returns a directory inside the project by default (a sibling only under a
/// `NUCLEOS_WORKTREE_ROOT` override that predates this move); this answers "which working tree am I
/// standing IN" and returns a checkout. Two functions in one crate under one name, differing only by
/// module, is a misreading waiting to happen — and it nearly did: the collision surfaced only because a grep for
/// this function's name found the other one in a worktree it had never been written to. `toplevel`
/// is git's own word for the thing (`--show-toplevel`), so it borrows a name that is already exact.
///
/// A fifth sanctioned entry to `run_git` (see its doc comment): it computes what is left of the
/// budget and refuses before spawning, which is the property that comment protects.
pub async fn toplevel(
    path: &Path,
    deadline: std::time::Instant,
) -> Result<std::path::PathBuf, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(
            "the operation ran out of time before the working tree could be located".to_owned(),
        );
    }

    let result = run_git(
        path,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--show-toplevel"),
        ],
        budget,
    )
    .await?;
    if !result.succeeded() {
        return Err(format!(
            "{} is not inside a git repository: {}",
            path.display(),
            result.output_tail.trim()
        ));
    }
    let Some(toplevel) = result.stdout.lines().next() else {
        return Err(format!(
            "git reported no top level for {} — a bare repository has none",
            path.display()
        ));
    };
    Ok(std::path::PathBuf::from(
        canonical(Path::new(toplevel.trim())).await?,
    ))
}

/// Where a repository points and what it last did, for a folder nobody has registered yet.
///
/// A sixth sanctioned entry to `run_git`: it computes what is left of the budget before each spawn
/// and returns without spawning when there is none, which is the property that list protects.
///
/// **Both halves are optional and neither absence is an error.** A repository with no `origin` is
/// ordinary — a local-only project is a project — and one with no commits is a repository somebody
/// made this morning. The caller is a wizard showing a person what is in a folder, and "there is no
/// remote" is information rather than a failure to look.
pub async fn origin_and_head(
    path: &Path,
    deadline: std::time::Instant,
) -> (Option<String>, Option<String>) {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return (None, None);
    }
    let remote = run_git(
        path,
        &[
            OsStr::new("remote"),
            OsStr::new("get-url"),
            OsStr::new("origin"),
        ],
        budget,
    )
    .await
    .ok()
    .filter(|result| result.succeeded())
    .and_then(|result| {
        result
            .stdout
            .lines()
            .next()
            .map(|line| line.trim().to_owned())
    })
    .filter(|line| !line.is_empty());

    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return (remote, None);
    }
    let head = run_git(
        path,
        &[
            OsStr::new("log"),
            OsStr::new("-1"),
            OsStr::new("--date=short"),
            OsStr::new("--format=%h %ad %s"),
        ],
        budget,
    )
    .await
    .ok()
    .filter(|result| result.succeeded())
    .and_then(|result| {
        result
            .stdout
            .lines()
            .next()
            .map(|line| line.trim().to_owned())
    })
    .filter(|line| !line.is_empty());

    (remote, head)
}

/// Whether `refs/heads/<branch>` exists in the repository at `path`.
///
/// A seventh sanctioned entry to `run_git`, for the reason the others are: it computes what is
/// left of the budget and refuses before spawning. `land.rs` is the one caller — it is what stands
/// between `autopilot_state.integration_branch` and a name that used to resolve and does not any
/// more, so a landing refuses by name instead of failing three git commands deep inside a merge.
///
/// `show-ref --verify --quiet` rather than `rev-parse --verify`: the latter also accepts a sha, a
/// tag, or anything else that resolves, and what this answers is narrower — is there a LOCAL
/// BRANCH by this name, the one thing a landing may target.
pub async fn branch_exists(
    path: &Path,
    branch: &str,
    deadline: std::time::Instant,
) -> Result<bool, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err("the operation ran out of time before the branch could be checked".to_owned());
    }
    let reference = format!("refs/heads/{branch}");
    let result = run_git(
        path,
        &[
            OsStr::new("show-ref"),
            OsStr::new("--verify"),
            OsStr::new("--quiet"),
            OsStr::new(&reference),
        ],
        budget,
    )
    .await?;
    Ok(result.succeeded())
}

/// Whether `ancestor` is reachable from `descendant` — `merge-base --is-ancestor`, read as a bool
/// rather than an `Outcome`, because the one caller (`land.rs`, decision #3) uses this to decide
/// whether to submit at all and has no row yet to write an outcome onto.
///
/// An eighth sanctioned entry to `run_git`, for the reason the others are.
///
/// Non-zero is read as "no" without inspecting which non-zero. `--is-ancestor` uses 1 for a plain
/// "not an ancestor" and something else for "not even a commit", and both branches named here have
/// already been resolved by the caller before this runs — `land::submit` reads `source` off
/// `current_branch` and `target` off `integration_branch`, neither of which hands this a name git
/// cannot find. A future caller that cannot make the same guarantee owes its own check first.
pub async fn is_ancestor(
    path: &Path,
    ancestor: &str,
    descendant: &str,
    deadline: std::time::Instant,
) -> Result<bool, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(
            "the operation ran out of time before the merge base could be checked".to_owned(),
        );
    }
    let result = run_git(
        path,
        &[
            OsStr::new("merge-base"),
            OsStr::new("--is-ancestor"),
            OsStr::new(ancestor),
            OsStr::new(descendant),
        ],
        budget,
    )
    .await?;
    Ok(result.succeeded())
}

/// The first-parent commits of `(base, tip]`, oldest first, each with a flag for whether it
/// changed `tests_map::MAP_FILE` relative to its first parent. On the target branch these are the
/// merges the queue made, which is what the post-merge bisection (spec 2026-10-05 §6.2 step 2)
/// walks. A merged side branch's own commits are not listed.
///
/// Another sanctioned entry to `run_git`, for the reason the others are. The budget is recomputed
/// before every spawn, as `is_ancestor` does, so a long range cannot outlive the deadline. Any git
/// failure, including a base git cannot resolve, is an `Err`; an empty range is `Ok(vec![])`.
pub async fn first_parent_commits(
    repo: &Path,
    base: &str,
    tip: &str,
    deadline: std::time::Instant,
) -> Result<Vec<(String, bool)>, String> {
    const OUT_OF_TIME: &str = "the operation ran out of time before the range could be listed";
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(OUT_OF_TIME.to_owned());
    }
    let range = format!("{base}..{tip}");
    let listed = run_git(
        repo,
        &[
            OsStr::new("rev-list"),
            OsStr::new("--first-parent"),
            OsStr::new("--reverse"),
            OsStr::new(&range),
        ],
        budget,
    )
    .await?;
    if !listed.succeeded() {
        return Err(format!(
            "could not list {range}: {}",
            listed.output_tail.trim()
        ));
    }
    let shas: Vec<String> = listed
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    let mut commits = Vec::with_capacity(shas.len());
    for sha in shas {
        let budget = deadline.saturating_duration_since(std::time::Instant::now());
        if budget.is_zero() {
            return Err(OUT_OF_TIME.to_owned());
        }
        let parent = format!("{sha}^1");
        let changed = run_git(
            repo,
            &[
                OsStr::new("diff"),
                OsStr::new("--name-only"),
                OsStr::new(&parent),
                OsStr::new(&sha),
                OsStr::new("--"),
                OsStr::new(crate::tests_map::MAP_FILE),
            ],
            budget,
        )
        .await?;
        if !changed.succeeded() {
            return Err(format!(
                "could not diff {sha} against its first parent: {}",
                changed.output_tail.trim()
            ));
        }
        let touches_map = !changed.stdout.trim().is_empty();
        commits.push((sha, touches_map));
    }
    Ok(commits)
}

/// Every local branch and remote-tracking ref, as full refnames (`refs/heads/...`,
/// `refs/remotes/...`).
///
/// Another sanctioned entry to `run_git`, for the reason the others are. `land.rs` is the caller: it
/// decides which cargo target directories a landing may delete, and a directory name another live
/// branch also derives is not the landed branch's alone to delete.
pub async fn branch_refs(path: &Path, deadline: std::time::Instant) -> Result<Vec<String>, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err("the operation ran out of time before the branches could be listed".to_owned());
    }
    let result = run_git(
        path,
        &[
            OsStr::new("for-each-ref"),
            OsStr::new("--format=%(refname)"),
            OsStr::new("refs/heads"),
            OsStr::new("refs/remotes"),
        ],
        budget,
    )
    .await?;
    if !result.succeeded() {
        return Err(format!(
            "could not list the branches: {}",
            result.output_tail.trim()
        ));
    }
    Ok(result
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The branch `origin`'s `HEAD` points at, or `None` when there is no `origin` or no such symbolic
/// ref — never an error, because "cannot derive this way" is not "something is broken", and
/// `land::integration_branch` has two more ways to answer before it has to refuse.
///
/// A ninth sanctioned entry to `run_git`, for the reason the others are.
pub async fn default_remote_branch(
    path: &Path,
    deadline: std::time::Instant,
) -> Result<Option<String>, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(
            "the operation ran out of time before origin's default branch could be read".to_owned(),
        );
    }
    let result = run_git(
        path,
        &[
            OsStr::new("symbolic-ref"),
            OsStr::new("refs/remotes/origin/HEAD"),
        ],
        budget,
    )
    .await?;
    if !result.succeeded() {
        return Ok(None);
    }
    Ok(result
        .stdout
        .lines()
        .next()
        .and_then(|line| line.trim().strip_prefix("refs/remotes/origin/"))
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned))
}

/// A third sanctioned entry to `run_git` (see its doc comment, which names all three): it
/// computes what is left of the budget and refuses before spawning, which is the property that
/// comment exists to protect.
pub async fn repo_key(path: &Path, deadline: std::time::Instant) -> Result<String, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(
            "the operation ran out of time before the repository could be identified".to_owned(),
        );
    }

    let result = run_git(
        path,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--show-toplevel"),
            OsStr::new("--git-common-dir"),
        ],
        budget,
    )
    .await?;
    // Two refusals, deliberately worded apart. This one is "git found no repository from here at
    // all"; the one below the parse is "git found one, and it is not this directory". They shared a
    // sentence until a mutation pass showed what that cost: with one wording, a test naming either
    // case passed through whichever branch happened to run, and deleting this block outright left
    // the whole suite green. A message is a test's only way to say WHICH guard answered.
    if !result.succeeded() {
        return Err(format!(
            "{} is not inside a git repository: {}",
            path.display(),
            result.output_tail.trim()
        ));
    }

    // Order matters and is fixed by the argv above: `--show-toplevel` first, `--git-common-dir`
    // second. `rev-parse` prints its answers in the order it was asked for them.
    let mut lines = result.stdout.lines();
    let (Some(toplevel), Some(common_dir)) = (lines.next(), lines.next()) else {
        return Err(format!(
            "git did not report both a top level and a common directory for {}",
            path.display()
        ));
    };

    let toplevel = canonical(Path::new(toplevel.trim())).await?;
    if canonical(path).await? != toplevel {
        return Err(format!(
            "{} is not the root of a repository — it sits inside {toplevel}",
            path.display()
        ));
    }

    canonical(Path::new(common_dir.trim())).await
}

/// The branch a worktree has checked out, or why that question has no usable answer.
///
/// Approving a paused run's `git merge X` has to know what X would have been merged INTO, and the
/// answer is the branch the run's own worktree stands on. Nothing else in the system records it:
/// `worktrees.path` is where, not what.
///
/// **`--show-toplevel` is asked for and checked for the reason `repo_key` documents at length**, and
/// the consequence of skipping it is worse here than there. `git -C <dir>` walks UP until it finds a
/// repository, so a path that has been removed — and a resumed run's worktree can be — answers with
/// the enclosing checkout's branch, cheerfully and with exit code 0. The queue would then be handed
/// a merge into whatever branch the MAIN checkout happens to have open. A wrong answer that looks
/// exactly like a right one is the failure mode this guard exists for.
///
/// A detached HEAD is `Ok("HEAD")` rather than an error: it is a fact about the worktree, not a
/// failure to find one out, and `vcs::merge_from_command` is where the decision to refuse it lives —
/// alongside the other reasons a command is not queueable, rather than split across two modules.
///
/// A fourth sanctioned entry to `run_git` (see its doc comment): it computes what is left of the
/// budget and refuses before spawning, which is the property that comment protects.
pub async fn current_branch(path: &Path, deadline: std::time::Instant) -> Result<String, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err("the operation ran out of time before the branch could be read".to_owned());
    }

    let result = run_git(
        path,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--show-toplevel"),
            OsStr::new("--abbrev-ref"),
            OsStr::new("HEAD"),
        ],
        budget,
    )
    .await?;
    if !result.succeeded() {
        return Err(format!(
            "{} is not inside a git repository: {}",
            path.display(),
            result.output_tail.trim()
        ));
    }

    // Order is fixed by the argv above: the top level first, then HEAD abbreviated.
    let mut lines = result.stdout.lines();
    let (Some(toplevel), Some(branch)) = (lines.next(), lines.next()) else {
        return Err(format!(
            "git did not report both a top level and a branch for {}",
            path.display()
        ));
    };

    let toplevel = canonical(Path::new(toplevel.trim())).await?;
    if canonical(path).await? != toplevel {
        return Err(format!(
            "{} is not the root of a worktree — it sits inside {toplevel}",
            path.display()
        ));
    }

    Ok(branch.trim().to_owned())
}

/// A path in the one spelling the filesystem itself uses.
///
/// On Windows this returns a verbatim path (`\\?\C:\…`). That is fine for a key, whose only job is
/// to compare equal to itself, and it is worth knowing before anyone compares a stored repository
/// key against a stored `project_root` — they are in different spellings on purpose.
pub async fn canonical(path: &Path) -> Result<String, String> {
    tokio::fs::canonicalize(path)
        .await
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| format!("could not canonicalise {}: {error}", path.display()))
}

/// The object id `rev` names, or the outcome to record when git will not resolve it.
///
/// Reads `stdout` rather than `output_tail`: the tail is a diagnostic and may carry a truncation
/// marker, while this is a value the caller compare-and-swaps against.
pub(super) async fn revision(
    repo: &Path,
    rev: &str,
    deadline: std::time::Instant,
) -> Result<String, Outcome> {
    let resolved = git(repo, &["rev-parse", rev], deadline).await?;
    if !resolved.succeeded() {
        return Err(failed(
            format!("could not resolve {rev} in {}", repo.display()),
            &resolved,
        ));
    }
    Ok(resolved.stdout.trim().to_owned())
}
