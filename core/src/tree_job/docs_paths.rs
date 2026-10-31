//! Rules of a docs branch (spec R6): the paths it may write and the states it may end in.

/// The only directories under `.ai/` a docs branch may write to.
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
    let mut pieces = glob.split('*');
    let first = pieces.next().unwrap_or_default();
    if !path.starts_with(first) {
        return false;
    }
    let mut remainder = &path[first.len()..];
    for piece in pieces {
        if piece.is_empty() {
            continue;
        }
        let Some(index) = remainder.find(piece) else {
            return false;
        };
        remainder = &remainder[index + piece.len()..];
    }
    glob.ends_with('*') || remainder.is_empty()
}

/// True when `path` names a file below one of [`DOCS_DIRS`], relative to the repository root.
///
/// The check is purely lexical and fails closed. Backslashes count as separators, the match is
/// case-sensitive even on Windows, and any `..` is refused, including one that would resolve
/// back inside an allowed directory. A directory named without a file is refused too.
pub fn is_allowed_docs_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let segments: Vec<&str> = normalized.split('/').collect();
    match segments.as_slice() {
        [".ai", dir, rest @ ..] if !rest.is_empty() => {
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

/// A docs branch only writes gitignored files, so it runs without a worktree or a cost.
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
        assert!(owned_glob_matches(".ai/notes/**", ".ai/notes/design.md"));
        assert!(owned_glob_matches(
            ".ai/notes/**",
            ".ai/notes/nested/design.md"
        ));
        assert!(owned_glob_matches(".ai/specs/*.md", ".ai/specs/design.md"));
        assert!(!owned_glob_matches(
            ".ai/specs/*.md",
            ".ai/specs/nested/design.md"
        ));
        assert!(!owned_glob_matches(
            ".ai/notes/**",
            ".ai/notes/../specs/design.md"
        ));
        assert!(!owned_glob_matches(
            ".ai/notes/**",
            "../.ai/notes/design.md"
        ));
    }

    #[test]
    fn docs_path_allows_a_file_under_each_of_the_four_directories() {
        for path in [
            ".ai/specs/design.md",
            ".ai/spikes/probe.md",
            ".ai/plans/plan.md",
            ".ai/notes/note.md",
            ".ai/plans/nested/deeper/plan.md",
        ] {
            assert!(is_allowed_docs_path(path), "should allow {path}");
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
            ".ai/specs-old/x.md",
            ".ai/specsx.md",
            ".ai/plans.md",
            ".ai/notes2/x.md",
            "nested/.ai/specs/x.md",
        ] {
            assert!(!is_allowed_docs_path(path), "should refuse {path}");
        }
    }

    #[test]
    fn docs_path_refuses_the_ai_directory_itself_and_its_other_children() {
        for path in [".ai", ".ai/", ".ai/memory.md", ".ai/scripts/tool.py", ""] {
            assert!(!is_allowed_docs_path(path), "should refuse {path:?}");
        }
    }

    #[test]
    fn docs_path_refuses_parent_traversal_out_of_an_allowed_directory() {
        for path in [
            ".ai/specs/..",
            ".ai/specs/../memory.md",
            ".ai/specs/../../core/src/lib.rs",
            ".ai/notes/deep/../../../core/src/lib.rs",
            ".ai/plans/../../../outside.md",
        ] {
            assert!(!is_allowed_docs_path(path), "should refuse {path}");
        }
    }

    #[test]
    fn docs_path_refuses_absolute_paths() {
        for path in [
            "/etc/passwd",
            "/.ai/specs/x.md",
            "C:/Projects/nucleos/.ai/specs/x.md",
            r"C:\Projects\nucleos\.ai\specs\x.md",
            r"\.ai\specs\x.md",
            r"\\server\share\.ai\specs\x.md",
        ] {
            assert!(!is_allowed_docs_path(path), "should refuse {path}");
        }
    }

    #[test]
    fn docs_path_treats_windows_backslashes_as_the_same_path() {
        for path in [
            r".ai\specs\design.md",
            r".ai\spikes\probe.md",
            r".ai\plans\nested\plan.md",
            r".ai\notes\note.md",
        ] {
            assert!(is_allowed_docs_path(path), "should allow {path}");
        }
        for path in [
            r"core\src\lib.rs",
            r".ai\specs-old\x.md",
            r".ai\",
            r".ai\specs\..\..\core\src\lib.rs",
            r".ai/specs\../../core/src/lib.rs",
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
