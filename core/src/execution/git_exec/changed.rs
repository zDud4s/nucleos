//! Which files a commit or a branch changed. Read-only; used by the task-claims code (spec
//! 2026-10-22 §3): the postgate's `hint_paths` and the land gate's changed-file set.

use super::resolution::{git_read, worktree_holding};

/// Splits git's `-z` output into its non-empty entries.
fn nul_paths(stdout: &str) -> Vec<String> {
    stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The files `sha` changed against its first parent. On the target branch a merge commit's first
/// parent is the target before the merge, so this is what the merge brought in.
pub async fn changed_by_commit(
    project_root: &std::path::Path,
    sha: &str,
    deadline: std::time::Instant,
) -> Result<Vec<String>, String> {
    let parent = format!("{sha}^1");
    let listed = git_read(
        project_root,
        &["diff", "--name-only", "-z", "--no-renames", &parent, sha, "--"],
        deadline,
    )
    .await?;
    if !listed.succeeded() {
        return Err(format!(
            "could not list what {sha} changed: {}",
            listed.output_tail
        ));
    }
    Ok(nul_paths(&listed.stdout))
}

/// The files `source` changed since it parted from `target` (`git diff target...source`: against
/// the merge base, not against `target`'s tip, so work that landed on `target` meanwhile is not
/// counted as the branch's own).
pub async fn changed_since_merge_base(
    project_root: &std::path::Path,
    target: &str,
    source: &str,
    deadline: std::time::Instant,
) -> Result<Vec<String>, String> {
    let range = format!("{target}...{source}");
    let listed = git_read(
        project_root,
        &["diff", "--name-only", "-z", "--no-renames", &range, "--"],
        deadline,
    )
    .await?;
    if !listed.succeeded() {
        return Err(format!(
            "could not list what {source} changed since {target}: {}",
            listed.output_tail
        ));
    }
    Ok(nul_paths(&listed.stdout))
}

/// The worktree that has `branch` checked out, if any (the main checkout counts).
pub async fn worktree_of_branch(
    project_root: &std::path::Path,
    branch: &str,
    deadline: std::time::Instant,
) -> Result<Option<std::path::PathBuf>, String> {
    worktree_holding(project_root, branch, deadline)
        .await
        .map_err(|outcome| format!("could not list the worktrees: {outcome:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn deadline() -> std::time::Instant {
        std::time::Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT
    }

    /// `master` (one commit `m.txt` past the fork) and `feat` (one commit `a.txt` past the fork).
    fn forked() -> (tempfile::TempDir, std::path::PathBuf) {
        let container = crate::execution::git_exec::testkit::space_free_tempdir("nucleos-changed-");
        let repo = container.path().join("repo");
        crate::execution::git_exec::testkit::initialize_repo(&repo);
        git(&repo, &["branch", "-M", "master"]);
        git(&repo, &["checkout", "-q", "-b", "feat"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "a"]);
        git(&repo, &["checkout", "-q", "master"]);
        std::fs::write(repo.join("m.txt"), "m\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "m"]);
        (container, repo)
    }

    #[tokio::test]
    async fn the_merge_base_diff_counts_only_what_the_branch_added() {
        let (_container, repo) = forked();
        let files = changed_since_merge_base(&repo, "master", "feat", deadline())
            .await
            .unwrap();
        assert_eq!(files, vec!["a.txt".to_owned()], "m.txt is master's, not feat's");
    }

    #[tokio::test]
    async fn a_commit_lists_the_files_it_changed_against_its_first_parent() {
        let (_container, repo) = forked();
        let tip = git(&repo, &["rev-parse", "feat"]);
        let files = changed_by_commit(&repo, &tip, deadline()).await.unwrap();
        assert_eq!(files, vec!["a.txt".to_owned()]);
        assert!(
            changed_by_commit(&repo, "0000000000000000000000000000000000000000", deadline())
                .await
                .is_err(),
            "an unknown sha is an error, never an empty list"
        );
    }

    #[tokio::test]
    async fn the_worktree_of_a_branch_is_the_checkout_that_holds_it() {
        let (_container, repo) = forked();
        let held = worktree_of_branch(&repo, "master", deadline()).await.unwrap();
        assert!(held.is_some(), "the main checkout holds master");
        assert_eq!(worktree_of_branch(&repo, "feat", deadline()).await.unwrap(), None);
    }
}
