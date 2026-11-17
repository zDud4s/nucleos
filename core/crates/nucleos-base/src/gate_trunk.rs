//! Whether a gate script is the trunk's own, for the tamper checks of `gate` and the sandbox.

use std::path::Path;

/// Whether `bytes` is the script `rel` as the trunk has it, or had it where this branch forked.
///
/// The project root's working copy alone is the wrong yardstick: it is whatever the main checkout is
/// parked on, so a branch that merely lags behind a changed script (or runs ahead of the checkout)
/// would be refused though it never touched the file. A branch that edited the script matches none
/// of the trunk's tip, the fork point and the root's copy. Any git failure answers `false`, which
/// keeps the strict refusal.
pub fn script_matches_trunk(project_root: &Path, worktree: &Path, rel: &str, bytes: &[u8]) -> bool {
    let spec = |rev: &str| format!("{rev}:{}", rel.replace('\\', "/"));
    for trunk in ["master", "main"] {
        if git_stdout(project_root, &["show", &spec(trunk)]).is_some_and(|tip| tip == bytes) {
            return true;
        }
        let Some(base) = git_stdout(worktree, &["merge-base", "HEAD", trunk]) else {
            continue;
        };
        let base = String::from_utf8_lossy(&base).trim().to_owned();
        if !base.is_empty()
            && git_stdout(worktree, &["show", &spec(&base)]).is_some_and(|forked| forked == bytes)
        {
            return true;
        }
    }
    false
}

/// Stdout of a local, read-only `git -C dir args`; `None` when it fails to start or exits non-zero.
fn git_stdout(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::tampered_command_scripts;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            // Without this a Windows autocrlf checkout writes `v1\r\n` beside the `v1\n` blob.
            .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn commit_script(dir: &Path, body: &str, message: &str) {
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/g.sh"), body).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", message]);
    }

    /// A root on `master` holding `v2` committed over `v1`, plus a worktree branched at `v1`.
    fn lagging() -> (tempfile::TempDir, tempfile::TempDir) {
        let root = tempfile::tempdir().expect("root");
        git(root.path(), &["init", "-b", "master"]);
        commit_script(root.path(), "v1\n", "v1");
        let wt = tempfile::tempdir().expect("worktree");
        let wt_path = wt.path().join("wt");
        git(
            root.path(),
            &["worktree", "add", "-b", "feat", wt_path.to_str().unwrap()],
        );
        commit_script(root.path(), "v2\n", "v2");
        (root, wt)
    }

    const CMD: &str = "sh scripts/g.sh";

    #[test]
    fn a_branch_lagging_behind_master_is_accepted() {
        let (root, wt) = lagging();
        let wt = wt.path().join("wt");
        // The root's copy is v2, the branch still has the v1 it forked from.
        assert_eq!(tampered_command_scripts(root.path(), &wt, [CMD]), None);
    }

    #[test]
    fn a_script_matching_masters_tip_but_not_the_roots_copy_is_accepted() {
        let (root, wt) = lagging();
        let wt = wt.path().join("wt");
        // Root parked on another branch with a different script; the branch carries master's tip.
        git(root.path(), &["checkout", "-b", "other"]);
        std::fs::write(root.path().join("scripts/g.sh"), "other\n").unwrap();
        std::fs::write(wt.join("scripts/g.sh"), "v2\n").unwrap();
        assert!(script_matches_trunk(
            root.path(),
            &wt,
            "scripts/g.sh",
            b"v2\n"
        ));
        assert_eq!(tampered_command_scripts(root.path(), &wt, [CMD]), None);
    }

    #[test]
    fn a_branch_edited_script_is_refused() {
        let (root, wt) = lagging();
        let wt = wt.path().join("wt");
        commit_script(&wt, "exit 0\n", "rig the gate");
        let reason = tampered_command_scripts(root.path(), &wt, [CMD]).expect("refused");
        assert!(
            reason.contains("differs from the project root's copy")
                && reason.contains("scripts/g.sh"),
            "{reason}"
        );
    }

    #[test]
    fn a_git_failure_is_not_trusted() {
        let root = tempfile::tempdir().unwrap();
        assert!(!script_matches_trunk(
            root.path(),
            root.path(),
            "x.sh",
            b"x"
        ));
    }
}
