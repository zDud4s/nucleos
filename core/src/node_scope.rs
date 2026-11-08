//! The scope fence of a work node: a run behind a `running` node may only write the files its
//! node owns (`work_nodes.owned_paths`), plus a short list of shared files every node may touch
//! (`DEFAULT_SHARED_FILES`, or `shared_files` in `.nucleos/workflow/speed.yaml`).
//!
//! The PreToolUse hook asks `for_run` before the classifier, so a write outside the node's globs
//! is refused at once with a sentence that tells the agent what to do instead (report it, or ask
//! its parent to widen the node), rather than surfacing later as a collision with a sibling.
//!
//! Fail-open by design: a node with no globs (the root, a reviewer), a run with no node, a path
//! outside the run's worktree, and any error reading the node all leave the decision to the rules
//! that follow.
//!
//! Known limit: only the file-writing tools (`Write`, `Edit`, `MultiEdit`, `NotebookEdit`) are
//! seen here. An edit made through a shell command (Bash or PowerShell, e.g. `sed -i` or a
//! redirect) is not; the land gate and the node containment checks still apply to it.

use serde_json::Value;
use sqlx::SqlitePool;

use crate::work_nodes::{NodeLimits, normalise_glob, path_contained};

/// The files every node may edit whatever it owns. A `/`-ending entry is a directory prefix (it
/// covers files created under it); any other entry is one exact file.
pub const DEFAULT_SHARED_FILES: &[&str] = &[
    "core/AGENTS.md",
    ".claude/hooks/ask_daemon.py",
    "core/hooks/ask_daemon.py",
    "core/migrations/",
];

/// The tools whose target file this fence judges.
const FENCED_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];

/// What the fence says about one written path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeVerdict {
    Allowed,
    /// The path, relative to the worktree root, that no owned glob or shared entry covers.
    Outside {
        path: String,
    },
}

/// The file a fenced tool call writes, `None` for any other tool or a call without a path.
pub fn edited_path<'a>(tool_name: &str, tool_input: &'a Value) -> Option<&'a str> {
    if !FENCED_TOOLS.contains(&tool_name) {
        return None;
    }
    ["file_path", "notebook_path"]
        .iter()
        .find_map(|key| tool_input.get(*key).and_then(Value::as_str))
        .filter(|path| !path.trim().is_empty())
}

/// Whether a path spelling is absolute: `/x`, `\x` or a drive letter (`C:`).
fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.as_bytes().get(1) == Some(&b':')
}

/// The comparison form of one segment: a drive letter is always folded (`C:` and `c:` are the
/// same drive), and on Windows every segment is, because its file system ignores case.
fn fold(segment: &str) -> String {
    if cfg!(windows) || (segment.len() == 2 && segment.ends_with(':')) {
        segment.to_lowercase()
    } else {
        segment.to_owned()
    }
}

/// The segments of a path with `\` read as `/`, empty and `.` segments dropped and `..` resolved
/// lexically. `None` when a `..` climbs above the first segment.
fn segments(path: &str) -> Option<Vec<String>> {
    let unified = path.trim().replace('\\', "/");
    let mut kept: Vec<String> = Vec::new();
    for segment in unified.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                kept.pop()?;
            }
            other => kept.push(other.to_owned()),
        }
    }
    Some(kept)
}

/// The file relative to `root`, in original case, or `None` when it lies outside the root (or the
/// root itself), which is not this fence's business.
fn relative_to_root(root: &str, file: &str) -> Option<String> {
    let root_segments = segments(root)?;
    let joined = if is_absolute(file) {
        file.to_owned()
    } else {
        format!("{}/{}", root.trim().replace('\\', "/"), file)
    };
    let file_segments = segments(&joined)?;
    if file_segments.len() <= root_segments.len() {
        return None;
    }
    let under_root = root_segments
        .iter()
        .zip(&file_segments)
        .all(|(left, right)| fold(left) == fold(right));
    under_root.then(|| file_segments[root_segments.len()..].join("/"))
}

/// The comparison form of a whole relative path or glob.
fn fold_path(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_owned()
    }
}

/// Whether one shared entry covers the (folded) relative path.
fn shared_covers(entry: &str, path: &str) -> bool {
    let is_dir = entry.trim().replace('\\', "/").ends_with('/');
    let Ok(clean) = normalise_glob(entry) else {
        return false;
    };
    let clean = fold_path(&clean);
    if is_dir {
        path.strip_prefix(clean.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
    } else {
        path == clean
    }
}

/// **Pure.** Whether some shared entry covers every file the (`normalise_glob`-ed) `glob` can
/// name: the glob is an exact shared file, or lies under a shared directory. A widen request drops
/// such a glob, since the node may already edit it.
pub(crate) fn shared_covers_glob(shared: &[String], glob: &str) -> bool {
    let folded = fold_path(glob);
    shared.iter().any(|entry| shared_covers(entry, &folded))
}

/// **Pure.** Whether a node owning `owned` may write `file`. `root` is the run's worktree (its
/// cwd); a relative `file` is relative to it.
pub fn check(owned: &[String], shared: &[String], root: &str, file: &str) -> ScopeVerdict {
    if owned.is_empty() {
        return ScopeVerdict::Allowed;
    }
    let Some(relative) = relative_to_root(root, file) else {
        return ScopeVerdict::Allowed;
    };
    let folded = fold_path(&relative);
    if shared.iter().any(|entry| shared_covers(entry, &folded)) {
        return ScopeVerdict::Allowed;
    }
    // A glob that does not normalise covers nothing, as in the containment rule.
    let covered = owned
        .iter()
        .filter_map(|glob| normalise_glob(glob).ok())
        .any(|glob| path_contained(&folded, &fold_path(&glob)));
    if covered {
        ScopeVerdict::Allowed
    } else {
        ScopeVerdict::Outside { path: relative }
    }
}

/// The sentence a refused write is answered with.
pub fn refusal(owned: &[String], path: &str) -> String {
    format!(
        "`{path}` is outside the paths this node owns ({}). Do not edit it. Report the problem \
         with mcp__nucleos__task_report (key, title, paths) and carry on with your own work, or \
         ask your parent for the path with node_update widen_paths.",
        owned.join(", ")
    )
}

/// The node limits of a project, read from the project's stored root: the same lookup as
/// `/nodes` uses. The defaults when there is no project, no root, or the root cannot be read.
pub async fn project_limits(pool: &SqlitePool, project_id: Option<&str>) -> NodeLimits {
    let Some(project_id) = project_id else {
        return NodeLimits::load(None);
    };
    match crate::inspect::project_root(pool, project_id).await {
        Ok(Some(root)) => NodeLimits::load(Some(std::path::Path::new(&root))),
        Ok(None) => NodeLimits::load(None),
        Err(error) => {
            tracing::warn!(project = project_id, %error, "could not read the project root");
            NodeLimits::load(None)
        }
    }
}

/// The refusal for a fenced tool call of `run_id`, or `None` to let it through. The node is the
/// `running` one bound to the run (`work_nodes.run_id` holds the run id as text). Fail-open: any
/// lookup error, no node, or an unreadable `owned_paths` answers `None`.
pub async fn for_run(
    pool: &SqlitePool,
    run_id: i64,
    root: &str,
    shared: &[String],
    tool_name: &str,
    tool_input: &Value,
) -> Option<String> {
    let file = edited_path(tool_name, tool_input)?;
    let owned_json: Option<String> = match sqlx::query_scalar(
        "SELECT owned_paths FROM work_nodes WHERE run_id = ? AND status = 'running' LIMIT 1",
    )
    .bind(run_id.to_string())
    .fetch_optional(pool)
    .await
    {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(run_id, %error, "node scope: could not read the run's node, not fencing");
            return None;
        }
    };
    let owned: Vec<String> = serde_json::from_str(&owned_json?).ok()?;
    match check(&owned, shared, root, file) {
        ScopeVerdict::Allowed => None,
        ScopeVerdict::Outside { path } => Some(refusal(&owned, &path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = r"C:\work\repo";

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn shared() -> Vec<String> {
        strings(DEFAULT_SHARED_FILES)
    }

    fn outside(path: &str) -> ScopeVerdict {
        ScopeVerdict::Outside {
            path: path.to_owned(),
        }
    }

    #[test]
    fn a_path_inside_an_owned_glob_is_allowed() {
        let owned = strings(&["core/src/node_scope.rs", "core/src/hooks/**"]);
        assert_eq!(
            check(&owned, &shared(), ROOT, "core/src/node_scope.rs"),
            ScopeVerdict::Allowed
        );
        assert_eq!(
            check(&owned, &shared(), ROOT, "core/src/hooks/pretool.rs"),
            ScopeVerdict::Allowed
        );
    }

    #[test]
    fn a_path_outside_every_owned_glob_is_refused() {
        let owned = strings(&["core/src/hooks/**"]);
        assert_eq!(
            check(&owned, &shared(), ROOT, "core/src/hooksx/a.rs"),
            outside("core/src/hooksx/a.rs")
        );
        assert_eq!(
            check(&owned, &shared(), ROOT, r"C:\work\repo\shell\src\App.tsx"),
            outside("shell/src/App.tsx")
        );
        let text = refusal(&strings(&["a/**", "b.rs"]), "shell/src/App.tsx");
        assert_eq!(
            text,
            "`shell/src/App.tsx` is outside the paths this node owns (a/**, b.rs). Do not edit it. \
             Report the problem with mcp__nucleos__task_report (key, title, paths) and carry on \
             with your own work, or ask your parent for the path with node_update widen_paths."
        );
    }

    #[test]
    fn a_shared_file_is_allowed_for_every_node() {
        let owned = strings(&["shell/**"]);
        for file in [
            "core/AGENTS.md",
            ".claude/hooks/ask_daemon.py",
            r"C:\work\repo\core\hooks\ask_daemon.py",
        ] {
            assert_eq!(
                check(&owned, &shared(), ROOT, file),
                ScopeVerdict::Allowed,
                "{file}"
            );
        }
        // Exact file, not a prefix.
        assert_eq!(
            check(&owned, &shared(), ROOT, "core/AGENTS.md.bak"),
            outside("core/AGENTS.md.bak")
        );
    }

    #[test]
    fn a_shared_directory_prefix_covers_new_files() {
        let owned = strings(&["shell/**"]);
        assert_eq!(
            check(&owned, &shared(), ROOT, "core/migrations/0300_new.sql"),
            ScopeVerdict::Allowed
        );
        assert_eq!(
            check(&owned, &shared(), ROOT, "core/migrationsx/0300_new.sql"),
            outside("core/migrationsx/0300_new.sql")
        );
    }

    #[test]
    fn an_empty_owned_list_is_not_enforced() {
        assert_eq!(
            check(&[], &shared(), ROOT, "anything/at/all.rs"),
            ScopeVerdict::Allowed
        );
    }

    #[test]
    fn backslashes_dot_segments_and_absolute_paths_are_the_same_file() {
        let owned = strings(&["core/src/**"]);
        for file in [
            r"C:\work\repo\core\src\a.rs",
            "c:/work/repo/core/src/a.rs",
            r"core\src\a.rs",
            "./core/./src/a.rs",
            "C:/work/repo/./core//src/a.rs",
            "core/tests/../src/a.rs",
        ] {
            assert_eq!(
                check(&owned, &shared(), ROOT, file),
                ScopeVerdict::Allowed,
                "{file}"
            );
        }
        for file in [r"C:\work\repo\core\tests\a.rs", r".\core\tests\a.rs"] {
            assert_eq!(
                check(&owned, &shared(), ROOT, file),
                outside("core/tests/a.rs"),
                "{file}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_case_differences_are_the_same_file() {
        let owned = strings(&["core/src/**"]);
        assert_eq!(
            check(
                &owned,
                &shared(),
                r"C:\Work\Repo",
                "c:/work/repo/CORE/Src/a.rs"
            ),
            ScopeVerdict::Allowed
        );
        assert_eq!(
            check(&owned, &shared(), ROOT, "Core/Agents.md"),
            ScopeVerdict::Allowed
        );
    }

    #[test]
    fn a_path_outside_the_worktree_root_is_not_judged() {
        let owned = strings(&["core/src/**"]);
        for file in [
            r"C:\work\other\x.rs",
            r"C:\work\repo-2\core\x.rs",
            "../other/x.rs",
            "/tmp/x.rs",
            r"D:\elsewhere\x.rs",
        ] {
            assert_eq!(
                check(&owned, &shared(), ROOT, file),
                ScopeVerdict::Allowed,
                "{file}"
            );
        }
    }
}
