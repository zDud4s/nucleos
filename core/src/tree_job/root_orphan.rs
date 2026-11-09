//! The worktree a failed root start left behind.
//!
//! The root branch is `queued` until `open_branch` succeeds, so a worktree already standing at the
//! root's path belongs to an earlier attempt that died half way (a crash between `git worktree add`
//! and the manifest write). `open_branch` refuses such a path, so every retry would fail the same
//! way. `clear` removes it before the retry.
//!
//! The links a worktree carries (`shell/node_modules`, a junction on Windows) are taken off first,
//! without following them: `git worktree remove` and `remove_dir_all` both follow a junction on
//! Windows and would empty the main checkout's `node_modules`.

use std::path::{Path, PathBuf};

use super::open;
use super::root::ROOT_BRANCH;

/// Removes the leftover root worktree of `tree_id` under `project_root`, then its git branch.
/// Nothing is done when no worktree stands at the path. Best effort: a step that fails is logged
/// and the start goes on to report whatever it then meets.
pub(super) async fn clear(project_root: &str, tree_id: &str, links: &[String]) {
    let root = PathBuf::from(project_root);
    let worktree = open::worktree_path(&root, tree_id, ROOT_BRANCH);
    if worktree.symlink_metadata().is_err() {
        return;
    }
    tracing::warn!(worktree = %worktree.display(), "removing the worktree an earlier root start left");
    unlink_all(&worktree, links);
    let path = worktree.to_string_lossy().replace('\\', "/");
    if let Err(error) = open::git(project_root, &["worktree", "remove", "--force", &path]).await {
        tracing::warn!(%error, "orphan root worktree: git could not remove it");
    }
    // Git may leave the directory (untracked files, an unregistered path): the links are gone, so
    // deleting it cannot reach the main checkout.
    if worktree.symlink_metadata().is_ok()
        && let Err(error) = std::fs::remove_dir_all(&worktree)
    {
        tracing::warn!(%error, "orphan root worktree: directory not removed");
    }
    let _ = open::git(project_root, &["worktree", "prune"]).await;
    let branch = open::git_branch_name(tree_id, ROOT_BRANCH);
    let reference = format!("refs/heads/{branch}");
    if open::git(project_root, &["rev-parse", "--verify", "--quiet", &reference])
        .await
        .is_ok()
        && let Err(error) = open::git(project_root, &["branch", "-D", &branch]).await
    {
        tracing::warn!(%error, "orphan root worktree: branch not removed");
    }
}

/// Takes each configured link off the worktree without following it.
fn unlink_all(worktree: &Path, links: &[String]) {
    for name in links {
        let link = worktree.join(name);
        if link.symlink_metadata().is_ok()
            && let Err(error) = open::unlink(&link)
        {
            tracing::warn!(link = %link.display(), %error, "orphan root worktree: link not removed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};

    fn git_text(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// An orphan root worktree with a `shell/node_modules` link to a directory of the main checkout.
    fn orphan(repo: &Path) -> (PathBuf, PathBuf) {
        let worktree = open::worktree_path(repo, "job-1", ROOT_BRANCH);
        git_text(
            repo,
            &[
                "worktree",
                "add",
                "-b",
                &open::git_branch_name("job-1", ROOT_BRANCH),
                &worktree.to_string_lossy(),
            ],
        );
        let modules = repo.join("shell/node_modules");
        std::fs::create_dir_all(&modules).unwrap();
        std::fs::write(modules.join("kept.txt"), "x").unwrap();
        open::make_link(&modules, &worktree.join("shell/node_modules")).unwrap();
        (worktree, modules)
    }

    #[tokio::test]
    async fn an_orphan_root_worktree_is_removed_and_its_linked_node_modules_survive() {
        let container = space_free_tempdir("tree-root-orphan-");
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        let (worktree, modules) = orphan(&repo);
        let root = repo.to_string_lossy().replace('\\', "/");

        clear(&root, "job-1", &["shell/node_modules".to_owned()]).await;

        assert!(worktree.symlink_metadata().is_err(), "the worktree is gone");
        assert!(modules.join("kept.txt").exists(), "the link target is intact");
        let branches = git_text(&repo, &["branch", "--list", "tree/job-1/root"]);
        assert!(branches.is_empty(), "the root branch is gone: {branches}");
        assert!(
            !git_text(&repo, &["worktree", "list"]).contains("job-1-root"),
            "git no longer lists it"
        );
    }

    #[tokio::test]
    async fn clearing_with_no_worktree_does_nothing() {
        let container = space_free_tempdir("tree-root-orphan-");
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        clear(&repo.to_string_lossy().replace('\\', "/"), "job-1", &[]).await;
        assert!(!open::worktree_path(&repo, "job-1", ROOT_BRANCH).exists());
    }
}
