//! Names under a conversation's working directory, for completing an `@`.
//!
//! Its own module rather than a use of `files.rs`, because it answers a different question. That
//! pillar owns a managed root the app itself created, and every path through it is held to the
//! standard of an attachment filename. This one points at somebody's project — a real checkout,
//! with a `target/` in it that is bigger than everything else put together — and the question is
//! not "what is on disk" but "what would a person plausibly be about to name".
//!
//! So the walk skips what a project keeps and nobody mentions: build output, dependency trees, and
//! anything hidden. Skipping is not a performance trick here; a completion list where the first
//! four hundred hits are `node_modules` is a completion list nobody can use.
//!
//! Measured on this repository, so the next person does not have to guess: the top level answers in
//! about 5ms, and a query that walks the whole tree — including one matching nothing, which is the
//! worst case — lands between 55 and 95ms. That is a keystroke's worth of work and no more, which
//! is the budget this has to stay inside. It is also why the skips are not optional: `target/`
//! alone holds more entries than the rest of the checkout together.

use std::path::{Path, PathBuf};

/// Directory names never descended into.
///
/// By name and not by pattern, because the cost of being wrong is asymmetric. Missing one means a
/// slower walk and some noise; wrongly skipping a real folder means a file the person can see in
/// their editor and cannot mention here, with nothing on screen to say why.
const NEVER: &[&str] = &[
    "node_modules",
    "target",
    "target-test",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
];

/// How many names come back at most. Past this the list is not a list, it is a wall.
const HITS: usize = 40;

/// How many directory entries the walk will look at before giving up.
///
/// A ceiling on the WORK, distinct from the ceiling on the answer: a query matching almost nothing
/// in a large checkout would otherwise walk the whole thing to say so, on a keystroke.
const VISITS: usize = 20_000;

/// One name a person could be about to mention.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Mention {
    /// Relative to the conversation's directory, forward-slashed on every platform.
    ///
    /// Relative because it is the only form that means anything to both ends: the window shows it,
    /// the person picks it, and what goes into the prompt is a path the model can open from where
    /// its turn already runs.
    pub path: String,
    /// The last component, which is what the person is typing at.
    pub name: String,
    pub is_dir: bool,
}

/// What the walk found, and whether it stopped early.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Found {
    pub hits: Vec<Mention>,
    /// True when a ceiling was reached, so the window can say the list is a beginning rather than
    /// letting somebody conclude their file is not there.
    pub truncated: bool,
}

/// The names under `root` worth offering for `query`.
///
/// An empty query answers with the top level rather than with nothing: pressing `@` and being shown
/// where you are is how every editor does this, and "type something first" is a worse first move.
///
/// Ranking is by where the query lands in the name — a name that STARTS with what you typed before
/// one that merely contains it — then directories, then alphabetically. Case-insensitive
/// throughout, because nobody types the capital letters in a filename.
pub fn matching(root: &Path, query: &str) -> Found {
    let needle = query.trim().to_lowercase();
    let mut hits: Vec<Mention> = Vec::new();
    let mut visits = 0usize;
    let mut truncated = false;

    // Breadth-first, so a shallow file beats a deep one when the ceiling cuts the walk short. A
    // depth-first walk would spend the whole budget in whichever subtree happened to sort first.
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(directory) = pending.first().cloned() {
        pending.remove(0);
        let Ok(reader) = std::fs::read_dir(&directory) else {
            // A folder that vanished or refuses to open mid-walk is skipped rather than fatal: the
            // rest of the tree is still a useful answer.
            continue;
        };
        for item in reader.flatten() {
            visits += 1;
            if visits > VISITS {
                truncated = true;
                return finish(hits, &needle, truncated);
            }
            let Some(name) = item.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(metadata) = item.metadata() else {
                continue;
            };
            let is_dir = metadata.is_dir();
            let full = item.path();

            // A symlink is never followed. Not a walk-cost decision: a link inside the directory
            // pointing anywhere else is how a walk rooted at a project ends up reading a home
            // directory, and the containment this module promises has to survive the filesystem
            // rather than the path string.
            let is_link = std::fs::symlink_metadata(&full)
                .map(|link| link.is_symlink())
                .unwrap_or(false);

            let Ok(under_root) = full.strip_prefix(root) else {
                continue;
            };
            let path = under_root.to_string_lossy().replace('\\', "/");

            if is_dir && !is_link && !skipped(&name) {
                // The top level only when nothing was typed: `@` shows where you are, and a query
                // searches. Descending on an empty query would answer a question nobody asked and
                // walk a whole checkout to do it.
                if !needle.is_empty() {
                    pending.push(full.clone());
                }
            }

            if !needle.is_empty() && !name.to_lowercase().contains(&needle) {
                continue;
            }
            if needle.is_empty() && skipped(&name) {
                continue;
            }
            hits.push(Mention { path, name, is_dir });
        }
    }

    finish(hits, &needle, truncated)
}

/// Whether a directory is one nobody mentions: build output, dependencies, or hidden.
fn skipped(name: &str) -> bool {
    name.starts_with('.') || NEVER.contains(&name)
}

/// Ranks what was found and cuts it to the ceiling.
///
/// The cut happens after the sort, never during the walk: taking the first forty names a directory
/// reader happened to hand over would answer a query with whatever sorted early on disk, which is
/// not an answer to the query at all.
fn finish(mut hits: Vec<Mention>, needle: &str, truncated: bool) -> Found {
    hits.sort_by(|a, b| {
        rank(&a.name, needle)
            .cmp(&rank(&b.name, needle))
            .then_with(|| b.is_dir.cmp(&a.is_dir))
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
    let over = hits.len() > HITS;
    hits.truncate(HITS);
    Found {
        hits,
        truncated: truncated || over,
    }
}

/// Where the query lands in a name: 0 for a name that starts with it, 1 for anywhere else.
///
/// Two buckets rather than the exact offset, so that within each bucket the alphabetical order
/// stands. Sorting by offset would interleave unrelated folders by an accident of how long their
/// names happen to be.
fn rank(name: &str, needle: &str) -> u8 {
    if needle.is_empty() || name.to_lowercase().starts_with(needle) {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project with the shape that makes this hard: real source, and a build directory holding
    /// more files than the source does with the same words in their names.
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for folder in [
            "core/src",
            "shell/src/pages",
            "target/debug",
            "node_modules/parse",
        ] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        for file in [
            "core/src/parser.rs",
            "core/src/runner.rs",
            "shell/src/pages/Chats.tsx",
            "target/debug/parser.d",
            "node_modules/parse/index.js",
            "README.md",
        ] {
            std::fs::write(root.join(file), "x").unwrap();
        }
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::write(root.join(".git/objects/parser"), "x").unwrap();
        dir
    }

    #[test]
    fn a_query_finds_a_file_by_part_of_its_name_relative_to_the_root() {
        let dir = project();

        let found = matching(dir.path(), "parser");

        let paths: Vec<&str> = found.hits.iter().map(|hit| hit.path.as_str()).collect();
        // Forward slashes, so the same string works on the window and in a prompt on any platform.
        assert!(paths.contains(&"core/src/parser.rs"), "{paths:?}");
    }

    /// The reason this module exists rather than a call into `files.rs`.
    ///
    /// A checkout's build output and dependency tree hold more files than its source, with the same
    /// words in their names. A completion list that included them would bury every real answer, and
    /// on this repository would walk a `target/` bigger than the rest of the tree to do it.
    #[test]
    fn the_walk_never_descends_into_build_output_dependencies_or_anything_hidden() {
        let dir = project();

        let found = matching(dir.path(), "parse");

        let paths: Vec<&str> = found.hits.iter().map(|hit| hit.path.as_str()).collect();
        assert!(paths.contains(&"core/src/parser.rs"), "{paths:?}");
        assert!(
            !paths.iter().any(|path| path.starts_with("target/")),
            "build output reached the list: {paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.starts_with("node_modules/")),
            "dependencies reached the list: {paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.starts_with(".git/")),
            "a hidden folder reached the list: {paths:?}"
        );
    }

    /// Pressing `@` and being told nothing is the wrong first move. Every editor answers it by
    /// showing where you are, and that is a listing rather than a search.
    #[test]
    fn an_empty_query_answers_with_the_top_level_and_does_not_walk_the_tree() {
        let dir = project();

        let found = matching(dir.path(), "");

        let paths: Vec<&str> = found.hits.iter().map(|hit| hit.path.as_str()).collect();
        assert!(paths.contains(&"core"), "{paths:?}");
        assert!(paths.contains(&"README.md"), "{paths:?}");
        // Nothing below the top level, and none of what is never mentioned.
        assert!(!paths.iter().any(|path| path.contains('/')), "{paths:?}");
        assert!(!paths.contains(&"target"), "{paths:?}");
        assert!(!paths.contains(&".git"), "{paths:?}");
    }

    /// What you typed at the START of a name is a better guess than what you typed in the middle
    /// of one, and a list that ignores the difference makes you read all of it every time.
    #[test]
    fn a_name_that_starts_with_the_query_comes_before_one_that_merely_contains_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("my-parser.rs"), "x").unwrap();
        std::fs::write(dir.path().join("parser.rs"), "x").unwrap();

        let found = matching(dir.path(), "parser");

        let paths: Vec<&str> = found.hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(paths, vec!["parser.rs", "my-parser.rs"]);
    }

    /// Nobody types the capital letters in a filename.
    #[test]
    fn matching_ignores_case() {
        let dir = project();

        let found = matching(dir.path(), "chats");

        assert!(
            found
                .hits
                .iter()
                .any(|hit| hit.path == "shell/src/pages/Chats.tsx"),
            "{:?}",
            found.hits
        );
    }

    /// A cut list must SAY it was cut. Forty names with nothing to mark the edge reads as "your
    /// file is not here", and the next thing a person does is stop looking.
    #[test]
    fn a_list_cut_at_the_ceiling_says_so() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..HITS + 5 {
            std::fs::write(dir.path().join(format!("parser-{index}.rs")), "x").unwrap();
        }

        let found = matching(dir.path(), "parser");

        assert_eq!(found.hits.len(), HITS);
        assert!(found.truncated);
    }

    /// The containment this module promises, asserted at the only place it can fail: a name that
    /// tries to climb out is a name that never matches anything, because the walk only ever reads
    /// what it descended into.
    #[test]
    fn nothing_outside_the_root_can_be_reached() {
        let outer = tempfile::tempdir().unwrap();
        std::fs::write(outer.path().join("secret.env"), "x").unwrap();
        let root = outer.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("open.rs"), "x").unwrap();

        for query in ["secret", "..", "../secret.env"] {
            let found = matching(&root, query);
            assert!(
                found.hits.is_empty(),
                "{query} reached outside the root: {:?}",
                found.hits
            );
        }
    }
}
