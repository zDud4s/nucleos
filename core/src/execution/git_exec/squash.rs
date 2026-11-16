//! Squashing a tree leaf's checkpoint and autoformat commits into one, at merge time (`git.squash_checkpoints`).
//!
//! The leaf branch is never touched: its `checkpoint_sha` rollback points stay valid for as long as a job
//! references them. The merge is computed from a rebuilt copy of the chain, a few unreferenced commit
//! objects, and only the merge commit that comes out of it reaches the target.

use super::*;
use crate::execution::worktree::is_checkpoint_message;

/// The subject the autoformat pass commits under (`tree_job::autofmt`).
const AUTOFORMAT_SUBJECT: &str = "nucleos: autoformat";
/// Tree branches are `tree/<tree>`, and a leaf's is `tree/<tree>/<leaf>`.
const TREE_BRANCH_PREFIX: &str = "tree/";

/// One commit of the range, as `git log` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    sha: String,
    tree: String,
    message: String,
}

impl Entry {
    fn is_noise(&self) -> bool {
        is_checkpoint_message(&self.message)
            || self
                .message
                .lines()
                .next()
                .is_some_and(|subject| subject.trim() == AUTOFORMAT_SUBJECT)
    }
}

/// What the rebuilt chain keeps: a real commit as it was, or a run of checkpoint and autoformat
/// commits as one.
#[derive(Debug, PartialEq, Eq)]
enum Piece<'a> {
    Real(&'a Entry),
    Run(&'a [Entry]),
}

/// Splits the range into real commits and maximal runs of consecutive checkpoint/autoformat commits.
fn pieces(entries: &[Entry]) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < entries.len() {
        if entries[at].is_noise() {
            let end = entries[at..]
                .iter()
                .position(|entry| !entry.is_noise())
                .map_or(entries.len(), |offset| at + offset);
            out.push(Piece::Run(&entries[at..end]));
            at = end;
        } else {
            out.push(Piece::Real(&entries[at]));
            at += 1;
        }
    }
    out
}

/// The message of a run squashed into one commit: the first checkpoint's subject, and how many it covers.
fn squashed_message(run: &[Entry]) -> String {
    let subject = run
        .iter()
        .find(|entry| is_checkpoint_message(&entry.message))
        .or_else(|| run.first())
        .and_then(|entry| entry.message.lines().next())
        .unwrap_or(AUTOFORMAT_SUBJECT)
        .trim();
    format!(
        "{subject}\n\nSquashed {} checkpoint and autoformat commits into one.",
        run.len()
    )
}

/// Parses `git log --format=%H%x1f%P%x1f%T%x1f%B%x1e`, or `None` for anything that is not a linear chain.
fn parse_range(log: &str) -> Option<(String, Vec<Entry>)> {
    let mut base = None;
    let mut entries = Vec::new();
    for record in log.split('\x1e') {
        let record = record.trim_start_matches(['\n', '\r']);
        if record.trim().is_empty() {
            continue;
        }
        let mut fields = record.splitn(4, '\x1f');
        let sha = fields.next()?.trim();
        let parents = fields.next()?.trim();
        let tree = fields.next()?.trim();
        let message = fields.next()?.to_owned();
        // A merge commit in the range (a branch that took the target in) is not a chain to rebuild.
        if parents.split_whitespace().count() != 1 {
            return None;
        }
        if base.is_none() {
            base = Some(parents.to_owned());
        }
        entries.push(Entry {
            sha: sha.to_owned(),
            tree: tree.to_owned(),
            message,
        });
    }
    Some((base?, entries))
}

/// Whether this merge is one the setting applies to: a leaf (`tree/<tree>/<leaf>`) into a tree branch.
///
/// A tree branch landing on the trunk (`tree/<tree>` into `master`) is left alone, so that
/// `land`'s "already part of the target" check keeps reading the branch tip as an ancestor.
pub(super) fn squash_applies(source: &str, target: &str) -> bool {
    is_leaf_branch(source)
        && target.starts_with(TREE_BRANCH_PREFIX)
        && crate::settings::get::<crate::config::GitConfig>(&crate::settings::Scope::Machine)
            .squash_checkpoints
}

fn is_leaf_branch(branch: &str) -> bool {
    branch
        .strip_prefix(TREE_BRANCH_PREFIX)
        .is_some_and(|rest| matches!(rest.split_once('/'), Some((tree, leaf)) if !tree.is_empty() && !leaf.is_empty()))
}

/// The tip of `source` rebuilt with its checkpoint and autoformat runs collapsed, or `None` when
/// there is nothing to collapse or anything about the rebuild is not clean.
///
/// Fails open on purpose: a squash that cannot be made leaves the merge to go ahead as it would have
/// without the setting, so the worst case is the history the repository had before this existed.
pub(super) async fn squashed_tip(
    repo: &Path,
    source: &str,
    target: &str,
    deadline: std::time::Instant,
) -> Option<String> {
    let range = format!("{target}..{source}");
    let log = git(
        repo,
        &[
            "log",
            "--reverse",
            "--format=%H%x1f%P%x1f%T%x1f%B%x1e",
            &range,
        ],
        deadline,
    )
    .await
    .ok()?;
    if !log.succeeded() {
        return None;
    }
    let (base, entries) = parse_range(&log.stdout)?;
    let tip = entries.last()?.sha.clone();
    let pieces = pieces(&entries);
    if !pieces
        .iter()
        .any(|piece| matches!(piece, Piece::Run(run) if run.len() > 1))
    {
        return None;
    }
    let mut parent = base;
    // While nothing before a commit was rewritten, the commit keeps its sha: it is the leaf's own.
    let mut intact = true;
    for piece in &pieces {
        let (tree, message, original) = match piece {
            Piece::Real(entry) => (&entry.tree, entry.message.clone(), Some(&entry.sha)),
            Piece::Run(run) if run.len() == 1 => {
                (&run[0].tree, run[0].message.clone(), Some(&run[0].sha))
            }
            Piece::Run(run) => (&run[run.len() - 1].tree, squashed_message(run), None),
        };
        if intact && let Some(sha) = original {
            parent = sha.clone();
            continue;
        }
        intact = false;
        let made = git(
            repo,
            &["commit-tree", tree, "-p", &parent, "-m", &message],
            deadline,
        )
        .await
        .ok()?;
        if !made.succeeded() {
            return None;
        }
        parent = made.stdout.trim().to_owned();
        if parent.is_empty() {
            return None;
        }
    }
    (parent != tip).then_some(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::git_exec::testkit;
    use std::path::PathBuf;

    fn entry(sha: &str, message: &str) -> Entry {
        Entry {
            sha: sha.to_owned(),
            tree: format!("t-{sha}"),
            message: message.to_owned(),
        }
    }

    const CHECKPOINT: &str = "nucleos(a): item\n\nNucleos-Checkpoint: gated";

    #[test]
    fn runs_of_checkpoints_and_autoformat_are_grouped_and_real_commits_stay_alone() {
        let entries = vec![
            entry("1", CHECKPOINT),
            entry("2", "nucleos: autoformat"),
            entry("3", "real work"),
            entry("4", CHECKPOINT),
        ];
        let pieces = pieces(&entries);
        assert_eq!(pieces.len(), 3);
        assert!(matches!(&pieces[0], Piece::Run(run) if run.len() == 2));
        assert!(matches!(&pieces[1], Piece::Real(e) if e.sha == "3"));
        assert!(matches!(&pieces[2], Piece::Run(run) if run.len() == 1));
    }

    #[test]
    fn a_range_with_a_merge_commit_is_not_rebuilt() {
        let log = "a\x1fp1 p2\x1ft\x1fmerge\x1e";
        assert!(parse_range(log).is_none());
    }

    fn git_text(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// Commits a new file with `subject`, plus a checkpoint trailer when `checkpoint`.
    fn commit(dir: &Path, n: usize, subject: &str, checkpoint: bool) {
        std::fs::write(dir.join(format!("f{n}.txt")), format!("{n}\n")).expect("write");
        git_text(dir, &["add", "-A"]);
        if checkpoint {
            git_text(
                dir,
                &["commit", "-m", subject, "-m", "Nucleos-Checkpoint: gated"],
            );
        } else {
            git_text(dir, &["commit", "-m", subject]);
        }
    }

    fn budget() -> std::time::Instant {
        std::time::Instant::now() + std::time::Duration::from_secs(120)
    }

    /// `tree/feat` (the target) and `tree/job/leaf` forked from it, the leaf holding
    /// checkpoint, checkpoint, autoformat, real, checkpoint, checkpoint.
    fn leaf_with_checkpoints() -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = testkit::init_contained_repo("squash-");
        commit(&repo, 0, "base", false);
        git_text(&repo, &["branch", "tree/feat"]);
        git_text(&repo, &["checkout", "-b", "tree/job/leaf"]);
        commit(&repo, 1, "nucleos(a): one", true);
        commit(&repo, 2, "nucleos(a): two", true);
        commit(&repo, 3, "nucleos: autoformat", false);
        commit(&repo, 4, "real work", false);
        commit(&repo, 5, "nucleos(b): three", true);
        commit(&repo, 6, "nucleos(b): four", true);
        git_text(&repo, &["checkout", "-"]);
        (container, repo)
    }

    fn subjects(repo: &Path, range: &str) -> Vec<String> {
        git_text(repo, &["log", "--no-merges", "--format=%s", range])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn setting(on: bool) -> crate::settings::testing::Scoped {
        crate::settings::testing::scope(crate::settings::Settings::from_snapshot(
            crate::settings::Snapshot::default().with_document(
                &crate::settings::Scope::Machine,
                crate::config::GitConfig {
                    squash_checkpoints: on,
                },
            ),
        ))
    }

    #[tokio::test]
    async fn squash_on_collapses_each_run_and_keeps_the_real_commit() {
        let (_container, repo) = leaf_with_checkpoints();
        let _setting = setting(true);
        let leaf_before = git_text(&repo, &["rev-parse", "tree/job/leaf"]);
        assert!(squash_applies("tree/job/leaf", "tree/feat"));

        let squashed = squashed_tip(&repo, "tree/job/leaf", "tree/feat", budget())
            .await
            .expect("a leaf with checkpoints is squashed");
        let computed = compute_merge_of(&repo, "tree/job/leaf", &squashed, "tree/feat", budget())
            .await
            .expect("the squashed leaf merges");

        let range = format!("{}..{}", computed.old, computed.new);
        let subjects = subjects(&repo, &range);
        assert_eq!(subjects.len(), 3, "{subjects:?}");
        assert!(subjects.contains(&"real work".to_owned()), "{subjects:?}");
        assert!(
            !subjects.iter().any(|s| s == "nucleos: autoformat"),
            "{subjects:?}"
        );
        // The merge says what it merged, not the sha it merged.
        let top = git_text(&repo, &["log", "-1", "--format=%s", &computed.new]);
        assert_eq!(top, "merge tree/job/leaf into tree/feat");
        // Same content as the unsquashed leaf, and the leaf branch (its rollback points) untouched.
        assert_eq!(
            git_text(&repo, &["rev-parse", &format!("{}^{{tree}}", computed.new)]),
            git_text(&repo, &["rev-parse", &format!("{leaf_before}^{{tree}}")]),
        );
        assert_eq!(
            git_text(&repo, &["rev-parse", "tree/job/leaf"]),
            leaf_before
        );
    }

    #[tokio::test]
    async fn squash_off_leaves_the_history_as_it_is() {
        let (_container, repo) = leaf_with_checkpoints();
        let _setting = setting(false);
        assert!(!squash_applies("tree/job/leaf", "tree/feat"));

        let computed = compute_merge(&repo, "tree/job/leaf", "tree/feat", budget())
            .await
            .expect("the plain merge");
        let range = format!("{}..{}", computed.old, computed.new);
        assert_eq!(subjects(&repo, &range).len(), 6);
    }

    #[tokio::test]
    async fn a_leaf_of_real_commits_is_not_rewritten() {
        let (container, repo) = testkit::init_contained_repo("squash-real-");
        let _keep = container;
        commit(&repo, 0, "base", false);
        git_text(&repo, &["branch", "tree/feat"]);
        git_text(&repo, &["checkout", "-b", "tree/job/leaf"]);
        commit(&repo, 1, "real one", false);
        commit(&repo, 2, "real two", false);
        let _setting = setting(true);
        assert!(
            squashed_tip(&repo, "tree/job/leaf", "tree/feat", budget())
                .await
                .is_none()
        );
    }

    #[test]
    fn only_a_leaf_into_a_tree_branch_is_squashed() {
        let _setting = setting(true);
        assert!(squash_applies("tree/job/leaf", "tree/feat"));
        assert!(!squash_applies("tree/feat", "master"), "the trunk landing");
        assert!(!squash_applies("tree/job/leaf", "master"));
        assert!(!squash_applies("feat/x", "tree/feat"));
    }

    #[test]
    fn a_squashed_message_keeps_the_first_checkpoint_subject() {
        let run = vec![
            entry("1", "nucleos: autoformat"),
            entry("2", CHECKPOINT),
            entry("3", CHECKPOINT),
        ];
        let message = squashed_message(&run);
        assert!(message.starts_with("nucleos(a): item\n"), "{message}");
        assert!(message.contains("Squashed 3"), "{message}");
    }
}
