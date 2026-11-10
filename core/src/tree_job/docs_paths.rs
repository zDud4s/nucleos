//! Rules of a docs branch (spec R6): the paths it may write and the states it may end in.

/// The literal token that stands for the project's state dir in a docs branch's paths.
const STATE_DIR_TOKEN: &str = "<state_dir>";

/// The only directories under [`STATE_DIR_TOKEN`] a docs branch may write to.
const DOCS_DIRS: [&str; 4] = ["specs", "spikes", "plans", "notes"];

/// True when `path` matches an owned docs-path glob. `*` stays within one path segment and `**`
/// matches zero or more whole segments. Traversal is never a match, even when a glob would
/// otherwise consume it.
pub fn owned_glob_matches(glob: &str, path: &str) -> bool {
    let glob = glob.replace('\\', "/");
    let path = path.replace('\\', "/");
    let glob: Vec<&str> = glob.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    if glob
        .iter()
        .chain(path.iter())
        .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
    {
        return false;
    }
    owned_glob_segments_match(&glob, &path)
}

fn owned_glob_segments_match(glob: &[&str], path: &[&str]) -> bool {
    match glob {
        [] => path.is_empty(),
        ["**", rest @ ..] => {
            owned_glob_segments_match(rest, path)
                || (!path.is_empty() && owned_glob_segments_match(glob, &path[1..]))
        }
        [segment, rest @ ..] => {
            path.first()
                .is_some_and(|part| owned_glob_segment_matches(segment, part))
                && owned_glob_segments_match(rest, &path[1..])
        }
    }
}

fn owned_glob_segment_matches(glob: &str, path: &str) -> bool {
    let pieces: Vec<&str> = glob.split('*').collect();
    let first = pieces.first().copied().unwrap_or_default();
    if !path.starts_with(first) {
        return false;
    }
    let mut remainder = &path[first.len()..];
    for (index, piece) in pieces.iter().enumerate().skip(1) {
        if index + 1 == pieces.len() {
            return remainder.ends_with(piece);
        }
        if piece.is_empty() {
            continue;
        }
        let Some(index) = remainder.find(piece) else {
            return false;
        };
        remainder = &remainder[index + piece.len()..];
    }
    remainder.is_empty()
}

/// True when `path` names a file below one of [`DOCS_DIRS`] of the state dir, written as
/// `<state_dir>/<dir>/<rest>` with the literal [`STATE_DIR_TOKEN`] as its first segment.
///
/// The check is purely lexical and fails closed. Backslashes count as separators, the match is
/// case-sensitive even on Windows, and any `..` is refused, including one that would resolve
/// back inside an allowed directory. A directory named without a file is refused too, and so is
/// the retired `.ai/` layout.
pub fn is_allowed_docs_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let segments: Vec<&str> = normalized.split('/').collect();
    match segments.as_slice() {
        [token, dir, rest @ ..] if *token == STATE_DIR_TOKEN && !rest.is_empty() => {
            DOCS_DIRS.contains(dir) && rest.iter().all(|segment| is_plain_segment(segment))
        }
        _ => false,
    }
}

/// False for an empty, `.` or `..` segment, for the forms Windows would resolve to one of those
/// (`...`, `.. `), and for anything with a colon (a drive prefix or an alternate data stream).
fn is_plain_segment(segment: &str) -> bool {
    !segment.trim_end_matches(['.', ' ']).is_empty() && !segment.contains(':')
}

/// A docs branch only writes files outside git, so it runs without a worktree or a cost.
pub fn docs_branch_needs_worktree() -> bool {
    false
}

/// A docs branch has nothing tracked to land, so it never goes through a merge.
pub fn docs_branch_merges() -> bool {
    false
}

/// True for the states a docs branch may end in: `done` and `dropped`.
pub fn is_docs_terminal_state(state: &str) -> bool {
    matches!(state, "done" | "dropped")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_glob_matches_declared_paths_and_rejects_traversal() {
        assert!(owned_glob_matches(
            "<state_dir>/notes/**",
            "<state_dir>/notes/design.md"
        ));
        assert!(owned_glob_matches(
            "<state_dir>/notes/**",
            "<state_dir>/notes/nested/design.md"
        ));
        assert!(owned_glob_matches(
            "<state_dir>/specs/*.md",
            "<state_dir>/specs/design.md"
        ));
        assert!(!owned_glob_matches(
            "<state_dir>/specs/*.md",
            "<state_dir>/specs/nested/design.md"
        ));
        assert!(!owned_glob_matches(
            "<state_dir>/notes/**",
            "<state_dir>/notes/../specs/design.md"
        ));
        assert!(!owned_glob_matches(
            "<state_dir>/notes/**",
            "../<state_dir>/notes/design.md"
        ));
    }

    #[test]
    fn docs_glob_matches_a_state_dir_token_glob() {
        assert!(owned_glob_matches(
            "<state_dir>/notes/**",
            "<state_dir>/notes/a/b.md"
        ));
        assert!(!owned_glob_matches(
            "<state_dir>/notes/**",
            "<state_dir>/specs/a.md"
        ));
        assert!(!owned_glob_matches(
            "<state_dir>/notes/**",
            "<state_dir>/notes/../specs/a.md"
        ));
        assert!(!owned_glob_matches(
            "<state_dir>/notes/**",
            "<state_dir>/notes/a/../../b.md"
        ));
    }

    #[test]
    fn docs_glob_anchors_the_last_piece_of_a_segment() {
        assert!(owned_glob_matches(
            "<state_dir>/specs/*.md",
            "<state_dir>/specs/design.md"
        ));
        assert!(owned_glob_matches(
            "<state_dir>/specs/*.md",
            "<state_dir>/specs/design.md.md"
        ));
        assert!(!owned_glob_matches(
            "<state_dir>/specs/*.md",
            "<state_dir>/specs/design.mdx"
        ));
    }

    #[test]
    fn docs_path_allows_a_file_under_each_of_the_four_directories() {
        for path in [
            "<state_dir>/specs/design.md",
            "<state_dir>/spikes/probe.md",
            "<state_dir>/plans/plan.md",
            "<state_dir>/notes/note.md",
            "<state_dir>/plans/nested/deeper/plan.md",
        ] {
            assert!(is_allowed_docs_path(path), "should allow {path}");
        }
    }

    #[test]
    fn docs_path_refuses_the_retired_ai_layout() {
        for path in [".ai/specs/x.md", r".ai\plans\x.md", ".ai/notes/n.md"] {
            assert!(!is_allowed_docs_path(path), "should refuse {path}");
        }
    }

    #[test]
    fn docs_path_refuses_a_tracked_path() {
        for path in ["core/src/lib.rs", "README.md", "docs/specs/design.md"] {
            assert!(!is_allowed_docs_path(path), "should refuse {path}");
        }
    }

    #[test]
    fn docs_path_refuses_a_sibling_prefix_of_an_allowed_directory() {
        for path in [
            "<state_dir>/specs-old/x.md",
            "<state_dir>/specsx.md",
            "<state_dir>/plans.md",
            "<state_dir>/notes2/x.md",
            "nested/<state_dir>/specs/x.md",
        ] {
            assert!(!is_allowed_docs_path(path), "should refuse {path}");
        }
    }

    #[test]
    fn docs_path_refuses_the_ai_directory_itself_and_its_other_children() {
        for path in [
            "<state_dir>",
            "<state_dir>/",
            "<state_dir>/memory.md",
            "<state_dir>/scripts/tool.py",
            "",
        ] {
            assert!(!is_allowed_docs_path(path), "should refuse {path:?}");
        }
    }

    #[test]
    fn docs_path_refuses_parent_traversal_out_of_an_allowed_directory() {
        for path in [
            "<state_dir>/specs/..",
            "<state_dir>/specs/../memory.md",
            "<state_dir>/specs/../../core/src/lib.rs",
            "<state_dir>/notes/deep/../../../core/src/lib.rs",
            "<state_dir>/plans/../../../outside.md",
        ] {
            assert!(!is_allowed_docs_path(path), "should refuse {path}");
        }
    }

    #[test]
    fn docs_path_refuses_absolute_paths() {
        for path in [
            "/etc/passwd",
            "/<state_dir>/specs/x.md",
            "C:/Projects/nucleos/<state_dir>/specs/x.md",
            r"C:\Projects\nucleos\<state_dir>\specs\x.md",
            r"\<state_dir>\specs\x.md",
            r"\\server\share\<state_dir>\specs\x.md",
        ] {
            assert!(!is_allowed_docs_path(path), "should refuse {path}");
        }
    }

    #[test]
    fn docs_path_treats_windows_backslashes_as_the_same_path() {
        for path in [
            r"<state_dir>\specs\design.md",
            r"<state_dir>\spikes\probe.md",
            r"<state_dir>\plans\nested\plan.md",
            r"<state_dir>\notes\note.md",
        ] {
            assert!(is_allowed_docs_path(path), "should allow {path}");
        }
        for path in [
            r"core\src\lib.rs",
            r"<state_dir>\specs-old\x.md",
            r"<state_dir>\",
            r"<state_dir>\specs\..\..\core\src\lib.rs",
            r"<state_dir>/specs\../../core/src/lib.rs",
        ] {
            assert!(!is_allowed_docs_path(path), "should refuse {path}");
        }
    }

    #[test]
    fn docs_branch_needs_no_worktree_and_never_merges() {
        assert!(!docs_branch_needs_worktree());
        assert!(!docs_branch_merges());
    }

    #[test]
    fn docs_terminal_states_are_done_and_dropped() {
        assert!(is_docs_terminal_state("done"));
        assert!(is_docs_terminal_state("dropped"));
    }

    #[test]
    fn docs_terminal_states_refuse_merge_and_non_terminal_states() {
        for state in ["merged", "landed", "running", "pending", "failed", ""] {
            assert!(!is_docs_terminal_state(state), "should refuse {state:?}");
        }
    }
}
