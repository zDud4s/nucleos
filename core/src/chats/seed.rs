//! The workflow, carried into a git worktree a conversation is rooted in.
//!
//! A linked worktree is a checkout of TRACKED files, and the workflow lives in gitignored ones —
//! `.claude/skills`, `.claude/agents`, `.agents/skills` — so a chat rooted in a worktree finds no
//! skills, no phase cockpits and no classifier hook, and falls to `McpOnly` without a word (the
//! chats-as-a-development-environment design, P5). The owner's answer to its Q8 (decision of
//! 2026-10-11): the chat seeds the workflow itself, but only into a worktree that has NONE of it,
//! never over a version of its own, and never `.ai/` — that holds memory, plans and state only
//! that worktree may have, and a copy would overwrite them with no backup.
//!
//! The settings file is not copied. The main checkout's may register hooks that run scripts from
//! `.ai/`, which is exactly what is not seeded, and a hook that cannot run is non-blocking — the
//! call goes through unclassified. The classifier hook is wired instead, with the same
//! `wire_classifier_hook` every other door uses, and only when the main checkout has it wired: a
//! worktree gets the project's policy, never a wider one.

use std::path::{Path, PathBuf};

/// What is copied, relative to the checkout root. `.ai/` is deliberately absent.
const SEEDED: [&str; 3] = [".claude/skills", ".claude/agents", ".agents/skills"];

/// What happened when a turn asked for the seed. Returned rather than only logged, so a test can
/// tell the refusals apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seeded {
    /// `dir` is not a linked worktree: the main checkout, a plain folder, or a bare path.
    NotAWorktree,
    /// The worktree already has `.claude/` or `.agents/`; whatever is there is its own.
    AlreadyHasWorkflow,
    /// Copied these entries, and wired the hook when `hook` is true.
    Done { copied: Vec<String>, hook: bool },
}

/// The main checkout of the linked worktree at `dir`, or `None` when `dir` is not one.
///
/// Read off the files git leaves, without running git: a linked worktree's `.git` is a FILE
/// naming its private git dir, whose `commondir` names the shared one, whose parent is the main
/// checkout. A `.git` directory is a main checkout, and anything unreadable is not a worktree.
pub fn main_checkout_of(dir: &Path) -> Option<PathBuf> {
    let pointer = std::fs::read_to_string(dir.join(".git")).ok()?;
    let gitdir = PathBuf::from(pointer.trim().strip_prefix("gitdir:")?.trim());
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        dir.join(gitdir)
    };
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = PathBuf::from(common.trim());
    let common = if common.is_absolute() {
        common
    } else {
        gitdir.join(common)
    };
    let main = std::fs::canonicalize(common).ok()?.parent()?.to_path_buf();
    main.join(".git").is_dir().then_some(main)
}

/// Seeds the workflow into the worktree at `dir`, when it has none of it. Blocking: call it from
/// `spawn_blocking`.
pub fn seed_if_empty(dir: &Path) -> Result<Seeded, String> {
    let Some(main) = main_checkout_of(dir) else {
        return Ok(Seeded::NotAWorktree);
    };
    if dir.join(".claude").exists() || dir.join(".agents").exists() {
        return Ok(Seeded::AlreadyHasWorkflow);
    }
    let mut copied = Vec::new();
    for entry in SEEDED {
        let source = main.join(entry);
        if source.is_dir() {
            copy_tree(&source, &dir.join(entry)).map_err(|error| {
                format!("could not seed {entry} into {}: {error}", dir.display())
            })?;
            copied.push(entry.to_string());
        }
    }
    let hook = crate::governance::autopilot::classifier_hook_is_wired(&main);
    if hook {
        crate::governance::autopilot::wire_classifier_hook(dir)?;
    }
    Ok(Seeded::Done { copied, hook })
}

/// Copies a directory tree. Links are not followed: a junction inside the workflow folders would
/// copy whatever it points at, and nothing there is meant to be one.
fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let to = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A main checkout with the workflow and a linked worktree beside it, laid out the way git
    /// leaves them, without running git.
    fn checkout_and_worktree(hook: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::TempDir::new().unwrap();
        let main = root.path().join("main");
        let tree = root.path().join("tree");
        let private = main.join(".git/worktrees/tree");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(private.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            tree.join(".git"),
            format!("gitdir: {}\n", private.display()),
        )
        .unwrap();
        for entry in SEEDED {
            std::fs::create_dir_all(main.join(entry).join("orchestrate")).unwrap();
            std::fs::write(main.join(entry).join("orchestrate/SKILL.md"), entry).unwrap();
        }
        std::fs::create_dir_all(main.join(".ai")).unwrap();
        std::fs::write(main.join(".ai/memory.md"), "main's memory").unwrap();
        if hook {
            crate::governance::autopilot::wire_classifier_hook(&main).unwrap();
        }
        (root, main, tree)
    }

    #[test]
    fn an_empty_worktree_gets_the_workflow_and_the_hook_but_never_ai() {
        let (_root, _main, tree) = checkout_and_worktree(true);

        let seeded = seed_if_empty(&tree).unwrap();

        assert_eq!(
            seeded,
            Seeded::Done {
                copied: SEEDED.iter().map(|entry| entry.to_string()).collect(),
                hook: true
            }
        );
        for entry in SEEDED {
            assert!(
                tree.join(entry).join("orchestrate/SKILL.md").is_file(),
                "{entry}"
            );
        }
        assert!(crate::governance::autopilot::classifier_hook_is_wired(
            &tree
        ));
        assert!(!tree.join(".ai").exists(), ".ai/ is never seeded");
    }

    #[test]
    fn a_worktree_with_any_workflow_of_its_own_is_left_alone() {
        let (_root, _main, tree) = checkout_and_worktree(true);
        std::fs::create_dir_all(tree.join(".claude/skills/mine")).unwrap();

        assert_eq!(seed_if_empty(&tree).unwrap(), Seeded::AlreadyHasWorkflow);
        assert!(!tree.join(".claude/agents").exists());
        assert!(!tree.join(".agents").exists());
    }

    #[test]
    fn the_hook_is_wired_only_where_the_main_checkout_has_it() {
        let (_root, _main, tree) = checkout_and_worktree(false);

        assert!(matches!(
            seed_if_empty(&tree).unwrap(),
            Seeded::Done { hook: false, .. }
        ));
        assert!(!crate::governance::autopilot::classifier_hook_is_wired(
            &tree
        ));
    }

    #[test]
    fn a_main_checkout_or_a_plain_folder_is_not_a_worktree() {
        let (_root, main, _tree) = checkout_and_worktree(true);
        let plain = tempfile::TempDir::new().unwrap();

        assert_eq!(seed_if_empty(&main).unwrap(), Seeded::NotAWorktree);
        assert_eq!(seed_if_empty(plain.path()).unwrap(), Seeded::NotAWorktree);
    }
}
