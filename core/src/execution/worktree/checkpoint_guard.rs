//! The look a checkpoint takes at its own index between `add -A` and `commit`.
//!
//! On 2026-11-12 a checkpoint taken while the machine came back from a restart committed "nucleos:
//! checkpoint a gate agreed with" with 2407 files deleted and nothing inserted. Every tracked file
//! outside the branch's own change left the index although it was still on disk, unchanged, and the
//! next gate could not find a `Cargo.toml` the branch had never touched. `add -A` only records what
//! git believes the tree is; when that belief is wrong, the commit makes it history.
//!
//! So the staged deletions are measured before the commit, and two shapes are refused: a path
//! staged as deleted that is still a regular file on disk (git's view is wrong, not the tree), and
//! a deletion of at least half of what `HEAD` tracks once it is past [`MASS_DELETION_FLOOR`] files.
//! A refusal unstages everything with `reset -q`, so the index is back on `HEAD` and the work stays
//! in the working tree, and answers an error: every caller already reads a failed checkpoint as "no
//! footing this time", which is far cheaper than a footing that deletes the repository.

use super::git;
use std::io;
use std::path::Path;

/// Below this many staged deletions the half-of-the-tree rule does not apply: a small repository
/// whose item really does replace most of its few files must still be able to commit.
const MASS_DELETION_FLOOR: usize = 50;

/// `Ok(())` when the staged deletions look like work, or an error naming the counts after the index
/// was reset. A failure to measure is refused too: the guard exists for a moment git cannot be
/// trusted, and a commit nobody measured is the one it would let through.
pub(super) async fn refuse_implausible_deletions(worktree_path: &Path) -> io::Result<()> {
    let measured = async {
        let tracked = git_paths(worktree_path, &["ls-tree", "-r", "-z", "--name-only", "HEAD"])
            .await?
            .len();
        let deleted = git_paths(
            worktree_path,
            &["diff", "--cached", "--name-only", "-z", "--diff-filter=D"],
        )
        .await?;
        Ok::<_, io::Error>((tracked, deleted))
    }
    .await;
    let refusal = match measured {
        Ok((tracked, deleted)) => refusal(worktree_path, tracked, &deleted),
        Err(error) => Some(format!(
            "refusing to checkpoint: the staged deletions could not be measured: {error}"
        )),
    };
    let Some(reason) = refusal else {
        return Ok(());
    };

    let reset = git()
        .arg("-C")
        .arg(worktree_path)
        .arg("reset")
        .arg("-q")
        .output()
        .await;
    match reset {
        Ok(output) if output.status.success() => Err(io::Error::other(reason)),
        Ok(output) => Err(io::Error::other(format!(
            "{reason}; and git reset failed, so the index may still stage it: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
        Err(error) => Err(io::Error::other(format!(
            "{reason}; and git reset could not start, so the index may still stage it: {error}"
        ))),
    }
}

/// Why this set of staged deletions must not be committed, or `None` when it may.
fn refusal(worktree_path: &Path, tracked: usize, deleted: &[String]) -> Option<String> {
    let on_disk: Vec<&String> = deleted
        .iter()
        .filter(|path| {
            worktree_path
                .join(path)
                .symlink_metadata()
                .is_ok_and(|meta| meta.is_file())
        })
        .collect();
    if let Some(first) = on_disk.first() {
        return Some(format!(
            "refusing to checkpoint: {} of the {} staged deletions are still regular files on disk \
             (the first is {first}), so git's view of the tree is wrong, not the tree",
            on_disk.len(),
            deleted.len()
        ));
    }
    if deleted.len() > MASS_DELETION_FLOOR && deleted.len() * 2 >= tracked {
        return Some(format!(
            "refusing to checkpoint: it would delete {} of the {tracked} files tracked at HEAD",
            deleted.len()
        ));
    }
    None
}

/// The NUL-separated paths a git command prints, relative to the worktree root.
async fn git_paths(worktree_path: &Path, args: &[&str]) -> io::Result<Vec<String>> {
    let output = git()
        .arg("-C")
        .arg(worktree_path)
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use crate::execution::worktree::{checkpoint, head_sha};
    use std::path::Path;
    use std::process::Command;

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
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
        String::from_utf8(output.stdout)
            .expect("git output should be UTF-8")
            .trim()
            .to_owned()
    }

    /// A repository whose one commit tracks `files` files under `src/`.
    fn repo_tracking(files: usize) -> tempfile::TempDir {
        let repo =
            crate::execution::git_exec::testkit::space_free_tempdir("nucleos-checkpoint-guard-");
        let dir = repo.path();
        git_out(dir, &["init", "-q"]);
        git_out(dir, &["config", "user.email", "test@x"]);
        git_out(dir, &["config", "user.name", "test"]);
        std::fs::create_dir(dir.join("src")).expect("create src");
        for at in 0..files {
            std::fs::write(dir.join(format!("src/f{at}.txt")), format!("file {at}\n"))
                .expect("write a tracked file");
        }
        git_out(dir, &["add", "-A"]);
        git_out(dir, &["commit", "-q", "-m", "seed"]);
        repo
    }

    fn tracked_in_index(dir: &Path) -> usize {
        git_out(dir, &["ls-files"]).lines().count()
    }

    /// The incident's shape: the index ends up staging the deletion of most of the tree. Nothing
    /// may be committed, HEAD stays where it was, and the index is back on HEAD with every entry.
    #[tokio::test(flavor = "current_thread")]
    async fn a_checkpoint_that_would_delete_most_tracked_files_is_refused_and_the_index_restored() {
        let repo = repo_tracking(120);
        let dir = repo.path();
        let before = head_sha(dir).await.expect("read HEAD before");
        for at in 0..100 {
            std::fs::remove_file(dir.join(format!("src/f{at}.txt"))).expect("drop a file");
        }

        let error = checkpoint(dir)
            .await
            .expect_err("a checkpoint deleting most of the tree must be refused");

        assert!(
            error.to_string().contains("100 of the 120"),
            "the refusal has to name the counts: {error}"
        );
        assert_eq!(
            head_sha(dir).await.expect("read HEAD after"),
            before,
            "no commit may be made"
        );
        assert_eq!(
            git_out(dir, &["diff", "--cached", "--name-only"]),
            "",
            "nothing may be left staged"
        );
        assert_eq!(
            tracked_in_index(dir),
            120,
            "the index must hold every file HEAD tracks again"
        );
    }

    /// The everyday case the guard must not get in the way of.
    #[tokio::test(flavor = "current_thread")]
    async fn a_checkpoint_with_an_ordinary_deletion_still_commits() {
        let repo = repo_tracking(120);
        let dir = repo.path();
        let before = head_sha(dir).await.expect("read HEAD before");
        std::fs::remove_file(dir.join("src/f0.txt")).expect("drop a file");
        std::fs::remove_file(dir.join("src/f1.txt")).expect("drop another");

        let sha = checkpoint(dir)
            .await
            .expect("deleting two files is ordinary work");

        assert_ne!(sha, before, "the deletion must be committed");
        assert_eq!(
            git_out(dir, &["diff", "--name-only", "--diff-filter=D", &before, &sha])
                .lines()
                .count(),
            2
        );
        assert_eq!(git_out(dir, &["status", "--porcelain"]), "");
    }

    /// A path staged as deleted that is still a regular file on disk is git being wrong about the
    /// tree, refused however few files it is. Here the files leave the index (`rm --cached`) and an
    /// exclude keeps `add -A` from noticing they are still there.
    #[tokio::test(flavor = "current_thread")]
    async fn a_checkpoint_staging_the_deletion_of_a_file_still_on_disk_is_refused() {
        let repo = repo_tracking(10);
        let dir = repo.path();
        let before = head_sha(dir).await.expect("read HEAD before");
        git_out(dir, &["rm", "-q", "--cached", "src/f0.txt", "src/f1.txt"]);
        std::fs::create_dir_all(dir.join(".git/info")).expect("create .git/info");
        std::fs::write(dir.join(".git/info/exclude"), "src/f0.txt\nsrc/f1.txt\n")
            .expect("write the exclude");

        let error = checkpoint(dir)
            .await
            .expect_err("a deletion of a file still on disk must be refused");

        assert!(
            error.to_string().contains("2 of the 2 staged deletions"),
            "the refusal has to name the counts: {error}"
        );
        assert_eq!(head_sha(dir).await.expect("read HEAD after"), before);
        assert_eq!(git_out(dir, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(tracked_in_index(dir), 10);
    }
}
