//! The code half of moving a chat: what the origin sends (`git bundle` of HEAD, `git diff --binary`,
//! the new non-ignored files) and what the destination does with it (a tree at the same commit, the
//! diff applied on top, the new files written).
//!
//! Built from an allow-list: only files git itself reports as tracked changes or as untracked and not
//! ignored travel, and a path leaves the `.git` directory and the repository alone. Ignored files are
//! listed as left behind, never sent. The git calls all go through `git_exec::run_git`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::execution::git_exec::{CommandResult, OPERATION_TIMEOUT, run_git};

/// One new, untracked and not ignored file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CodeFile {
    /// Relative to the repository root, `/`-separated.
    pub path: String,
    /// Standard base64 of the bytes.
    pub data: String,
}

/// The code of a chat, ready to be recreated.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Code {
    /// The project (roster id) the chat's directory belongs to; the destination finds its own copy by it.
    pub project_id: String,
    pub commit: String,
    pub branch: Option<String>,
    /// Base64 of a `git bundle` holding `HEAD`.
    pub bundle: String,
    /// Base64 of `git diff --binary HEAD`; empty when the tree is clean.
    pub diff: String,
    pub files: Vec<CodeFile>,
    pub left_behind: Vec<String>,
}

/// What the origin can say about the code before anything is built or frozen.
#[derive(Debug)]
pub struct Probe {
    pub project_id: String,
    pub top: PathBuf,
    pub commit: String,
    pub branch: Option<String>,
    pub new_files: Vec<String>,
    pub left_behind: Vec<String>,
}

/// Largest single file and largest package of new files that travel.
const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_FILES_BYTES: u64 = 100 * 1024 * 1024;

async fn git(dir: &Path, args: &[&str]) -> Result<CommandResult, String> {
    let argv: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    run_git(dir, &argv, OPERATION_TIMEOUT).await
}

async fn git_ok(dir: &Path, args: &[&str]) -> Result<String, String> {
    let result = git(dir, args).await?;
    if result.succeeded() {
        Ok(result.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            result.output_tail.trim()
        ))
    }
}

fn nul_list(text: &str) -> Vec<String> {
    text.split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

fn deadline() -> std::time::Instant {
    std::time::Instant::now() + OPERATION_TIMEOUT
}

/// A scratch file name no other move shares.
fn scratch(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "nucleos-move-{}-{}-{label}",
        std::process::id(),
        crate::auth::generate_uuid_v4()
    ))
}

/// Looks at the chat's directory and answers why it cannot move, or what would move. Changes nothing.
pub async fn probe(pool: &sqlx::SqlitePool, cwd: &Path) -> Result<Probe, String> {
    if !cwd.is_dir() {
        return Err(format!(
            "the chat's folder {} is not on this machine",
            cwd.display()
        ));
    }
    let top = crate::execution::git_exec::toplevel(cwd, deadline())
        .await
        .map_err(|_| {
            format!(
                "{} is not a git repository, so its code cannot move",
                cwd.display()
            )
        })?;
    let project_id = crate::execution::vcs::project_for_worktree(pool, &top, deadline())
        .await
        .map_err(|_| "this chat's folder belongs to no project of this machine".to_string())?;
    let commit = git_ok(&top, &["rev-parse", "--verify", "HEAD"])
        .await
        .map_err(|_| "the repository has no commit yet, so there is nothing to bundle".to_string())?
        .trim()
        .to_owned();
    if git_ok(&top, &["rev-parse", "--is-shallow-repository"])
        .await?
        .trim()
        == "true"
    {
        return Err("the repository is a shallow clone, which cannot be bundled".to_string());
    }
    if !git_ok(&top, &["ls-files", "-u", "-z"]).await?.is_empty() {
        return Err(
            "the working tree has unresolved merge conflicts; resolve them first".to_string(),
        );
    }
    let branch = git_ok(&top, &["symbolic-ref", "--short", "-q", "HEAD"])
        .await
        .ok()
        .map(|b| b.trim().to_owned())
        .filter(|b| !b.is_empty());
    let new_files =
        nul_list(&git_ok(&top, &["ls-files", "--others", "--exclude-standard", "-z"]).await?)
            .into_iter()
            .filter(|p| p != ".git" && !p.starts_with(".git/"))
            .collect();
    let left_behind = nul_list(
        &git_ok(
            &top,
            &[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                "-z",
            ],
        )
        .await?,
    );
    Ok(Probe {
        project_id,
        top,
        commit,
        branch,
        new_files,
        left_behind,
    })
}

/// Builds the code half of a package from a probe.
pub async fn capture(probe: Probe) -> Result<Code, String> {
    let top = &probe.top;
    let bundle_path = scratch("out.bundle");
    let diff_path = scratch("out.diff");
    let built = async {
        let bundle_arg = bundle_path.to_string_lossy().into_owned();
        git_ok(top, &["bundle", "create", "--quiet", &bundle_arg, "HEAD"]).await?;
        let diff_arg = format!("--output={}", diff_path.to_string_lossy());
        git_ok(
            top,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--binary",
                "HEAD",
                &diff_arg,
            ],
        )
        .await?;
        let bundle =
            std::fs::read(&bundle_path).map_err(|e| format!("could not read the bundle: {e}"))?;
        let diff = std::fs::read(&diff_path).unwrap_or_default();
        Ok::<_, String>((bundle, diff))
    }
    .await;
    let _ = std::fs::remove_file(&bundle_path);
    let _ = std::fs::remove_file(&diff_path);
    let (bundle, diff) = built?;

    let mut files = Vec::new();
    let mut left_behind = probe.left_behind.clone();
    let mut total = 0u64;
    for path in &probe.new_files {
        if !safe_relative(path) {
            left_behind.push(format!("{path} (unsafe path)"));
            continue;
        }
        let full = top.join(path);
        let meta = match std::fs::symlink_metadata(&full) {
            Ok(meta) if meta.is_file() => meta,
            _ => {
                left_behind.push(format!("{path} (not a regular file)"));
                continue;
            }
        };
        total += meta.len();
        if meta.len() > MAX_FILE_BYTES || total > MAX_FILES_BYTES {
            return Err(format!(
                "{path} is too large to move; add it to .gitignore or commit it"
            ));
        }
        let bytes = std::fs::read(&full).map_err(|e| format!("could not read {path}: {e}"))?;
        files.push(CodeFile {
            path: path.clone(),
            data: B64.encode(bytes),
        });
    }
    Ok(Code {
        project_id: probe.project_id,
        commit: probe.commit,
        branch: probe.branch,
        bundle: B64.encode(bundle),
        diff: B64.encode(diff),
        files,
        left_behind,
    })
}

/// A path that stays inside the repository: relative, no `..`, nothing under `.git`.
fn safe_relative(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty()
        && !p.is_absolute()
        && !path.contains(['\\', ':', '\0'])
        && p.components().all(|component| match component {
            std::path::Component::Normal(part) => {
                let name = part.to_string_lossy();
                !part.eq_ignore_ascii_case(".git")
                    && !name.eq_ignore_ascii_case("git~1")
                    && !name.ends_with('.')
                    && !name.ends_with(' ')
            }
            _ => false,
        })
}

fn diff_changes_symlink(diff: &[u8]) -> bool {
    String::from_utf8_lossy(diff)
        .split("diff --git ")
        .any(|section| {
            let lines: Vec<_> = section
                .lines()
                .map(|line| line.trim_end_matches('\r'))
                .collect();
            let deletes_symlink = lines.contains(&"deleted file mode 120000");
            lines.iter().any(|line| {
                matches!(
                    *line,
                    "new file mode 120000" | "old mode 120000" | "new mode 120000"
                ) || (!deletes_symlink && line.starts_with("index ") && line.ends_with(" 120000"))
            })
        })
}

/// The deterministic tree name shared by import retries and failed-import cleanup.
pub fn materialized_tree(project_root: &Path, chat_id: &str, transfer_id: &str) -> PathBuf {
    let safe: String = chat_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(16)
        .collect();
    let tag: String = transfer_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    crate::execution::worktree::worktree_root(project_root).join(format!("moved-{safe}-{tag}"))
}

async fn remove_tree(project_root: &Path, tree: &Path) {
    let _ = git(
        project_root,
        &["worktree", "remove", "--force", &tree.to_string_lossy()],
    )
    .await;
    let _ = std::fs::remove_dir_all(tree);
}

/// Drops only a deterministic tree left by a failed import, and only when no chat owns it.
pub async fn remove_unreferenced_tree(
    pool: &sqlx::SqlitePool,
    project_root: &Path,
    chat_id: &str,
    transfer_id: &str,
) -> Result<(), String> {
    let tree = materialized_tree(project_root, chat_id, transfer_id);
    match std::fs::symlink_metadata(&tree) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not inspect {}: {error}", tree.display())),
    }
    let referenced: Option<String> =
        sqlx::query_scalar("SELECT chat_id FROM chats WHERE cwd = ? LIMIT 1")
            .bind(tree.to_string_lossy().into_owned())
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("could not check the prior imported tree: {e}"))?;
    if referenced.is_none() {
        remove_tree(project_root, &tree).await;
    }
    Ok(())
}

fn reject_symlink_ancestors(tree: &Path, target: &Path) -> Result<(), String> {
    let relative = target.strip_prefix(tree).map_err(|_| {
        format!(
            "could not verify {} stays under {}",
            target.display(),
            tree.display()
        )
    })?;
    let mut ancestor = tree.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        ancestor.push(component.as_os_str());
        match std::fs::symlink_metadata(&ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "refusing to write {}: ancestor {} is a symlink",
                    target.display(),
                    ancestor.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not inspect {}: {error}", ancestor.display())),
        }
    }
    Ok(())
}

/// What the destination checks before it changes anything: the project is on this machine.
pub async fn destination_root(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> Result<PathBuf, String> {
    let root: Option<Option<String>> =
        sqlx::query_scalar("SELECT project_root FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("could not read the project roster: {e}"))?;
    match root.flatten().map(PathBuf::from) {
        Some(root) if root.is_dir() => Ok(root),
        _ => Err(format!("the project {project_id} is not on this machine")),
    }
}

/// Recreates the tree: fetches the commit from the bundle when it is missing, adds a detached
/// worktree at it, applies the diff and writes the new files. Returns the new directory. On any
/// failure the half-built tree is removed.
pub async fn materialize(
    pool: &sqlx::SqlitePool,
    code: &Code,
    project_root: &Path,
    chat_id: &str,
    transfer_id: &str,
) -> Result<PathBuf, String> {
    let bundle = B64
        .decode(&code.bundle)
        .map_err(|e| format!("the bundle is damaged: {e}"))?;
    let diff = B64
        .decode(&code.diff)
        .map_err(|e| format!("the diff is damaged: {e}"))?;
    if diff_changes_symlink(&diff) {
        return Err("the package diff changes a symlink, which cannot be moved safely".to_string());
    }
    let mut files = Vec::new();
    for file in &code.files {
        if !safe_relative(&file.path) {
            return Err(format!(
                "the package names a path outside the project: {}",
                file.path
            ));
        }
        files.push((
            &file.path,
            B64.decode(&file.data)
                .map_err(|e| format!("{} is damaged: {e}", file.path))?,
        ));
    }
    if code.commit.len() < 7 || !code.commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("the package carries no valid commit".to_string());
    }
    let tree = materialized_tree(project_root, chat_id, transfer_id);
    remove_unreferenced_tree(pool, project_root, chat_id, transfer_id).await?;
    let have = git(
        project_root,
        &["cat-file", "-e", &format!("{}^{{commit}}", code.commit)],
    )
    .await?;
    if !have.succeeded() {
        let bundle_path = scratch("in.bundle");
        std::fs::write(&bundle_path, &bundle)
            .map_err(|e| format!("could not stage the bundle: {e}"))?;
        let refspec = format!("HEAD:refs/nucleos/moved/{}", code.commit);
        let fetched = git_ok(
            project_root,
            &["fetch", "--quiet", &bundle_path.to_string_lossy(), &refspec],
        )
        .await;
        let _ = std::fs::remove_file(&bundle_path);
        fetched?;
    }
    if let Some(parent) = tree.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let built = async {
        git_ok(
            project_root,
            &[
                "worktree",
                "add",
                "--detach",
                &tree.to_string_lossy(),
                &code.commit,
            ],
        )
        .await?;
        if !diff.is_empty() {
            let diff_path = scratch("in.diff");
            std::fs::write(&diff_path, &diff)
                .map_err(|e| format!("could not stage the diff: {e}"))?;
            let applied = git_ok(
                &tree,
                &[
                    "apply",
                    "--binary",
                    "--whitespace=nowarn",
                    &diff_path.to_string_lossy(),
                ],
            )
            .await;
            let _ = std::fs::remove_file(&diff_path);
            applied?;
        }
        for (path, bytes) in &files {
            let target = tree.join(path);
            reject_symlink_ancestors(&tree, &target)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
            }
            reject_symlink_ancestors(&tree, &target)?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| format!("could not create {path}: {e}"))?;
            use std::io::Write as _;
            output
                .write_all(bytes)
                .map_err(|e| format!("could not write {path}: {e}"))?;
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(error) = built {
        remove_tree(project_root, &tree).await;
        return Err(error);
    }
    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::git_exec::testkit::{
        git_ok as git_command_ok, init_contained_repo, space_free_tempdir,
    };
    use std::ffi::OsStr;

    async fn head(repo: &Path) -> String {
        git(repo, &["rev-parse", "HEAD"])
            .await
            .unwrap()
            .stdout
            .trim()
            .to_string()
    }

    fn code(commit: String) -> Code {
        Code {
            project_id: "project".into(),
            commit,
            branch: None,
            bundle: B64.encode([]),
            diff: B64.encode([]),
            files: vec![],
            left_behind: vec![],
        }
    }

    #[test]
    fn safe_relative_rejects_git_components_case_insensitively() {
        assert!(!safe_relative(".GIT/config"));
        assert!(!safe_relative("nested/.Git/hooks/post-checkout"));
    }

    #[test]
    fn safe_relative_rejects_windows_aliases_and_trailing_components() {
        assert!(!safe_relative("nested/GIT~1/config"));
        assert!(!safe_relative("nested/file."));
        assert!(!safe_relative("nested/file "));
    }

    #[tokio::test]
    async fn capture_lists_an_unsafe_new_file_as_left_behind() {
        let (_keep, repo) = init_contained_repo("nucleos-move-unsafe-");
        let package = capture(Probe {
            project_id: "project".into(),
            top: repo.clone(),
            commit: head(&repo).await,
            branch: None,
            new_files: vec!["unsafe:name".into()],
            left_behind: vec![],
        })
        .await
        .unwrap();

        assert!(
            package
                .left_behind
                .iter()
                .any(|entry| entry == "unsafe:name (unsafe path)")
        );
    }

    #[tokio::test]
    async fn capture_ignores_external_diff_configuration() {
        let (_keep, repo) = init_contained_repo("nucleos-move-diff-config-");
        std::fs::write(repo.join("seed.txt"), "changed\n").unwrap();
        assert!(git_command_ok(
            &repo,
            &[
                OsStr::new("config"),
                OsStr::new("diff.external"),
                OsStr::new("definitely-not-a-real-diff-command"),
            ]
        ));

        let package = capture(Probe {
            project_id: "project".into(),
            top: repo.clone(),
            commit: head(&repo).await,
            branch: None,
            new_files: vec![],
            left_behind: vec![],
        })
        .await
        .unwrap();

        assert!(!B64.decode(package.diff).unwrap().is_empty());
    }

    #[tokio::test]
    async fn materialize_refuses_a_diff_that_changes_a_symlink() {
        let (_keep, repo) = init_contained_repo("nucleos-move-diff-symlink-");
        let mut package = code(head(&repo).await);
        package.diff = B64.encode(
            b"diff --git a/link b/link\nnew file mode 120000\nindex 0000000..1111111\n--- /dev/null\n+++ b/link\n@@ -0,0 +1 @@\n+/outside\n",
        );
        let tree = materialized_tree(&repo, "chat", "transfer");

        let pool = crate::testdb::fresh_pool().await;
        let error = materialize(&pool, &package, &repo, "chat", "transfer")
            .await
            .unwrap_err();

        assert!(error.contains("changes a symlink"), "{error}");
        assert!(std::fs::symlink_metadata(tree).is_err());
    }

    #[test]
    fn diff_changes_symlink_allows_deleting_a_symlink() {
        let diff = b"diff --git a/link b/link\ndeleted file mode 120000\nindex 1111111..0000000 120000\n--- a/link\n+++ /dev/null\n@@ -1 +0,0 @@\n-/outside\n";

        assert!(!diff_changes_symlink(diff));
    }

    #[test]
    fn diff_changes_symlink_refuses_a_hunk_that_retargets_a_symlink() {
        let diff = b"diff --git a/link b/link\nindex 1111111..2222222 120000\n--- a/link\n+++ b/link\n@@ -1 +1 @@\n-/old\n+/new\n";

        assert!(diff_changes_symlink(diff));
    }

    #[cfg(unix)]
    fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[tokio::test]
    async fn materialize_refuses_writing_below_a_symlink_ancestor() {
        let (_keep, repo) = init_contained_repo("nucleos-move-ancestor-symlink-");
        let outside = space_free_tempdir("nucleos-move-outside-");
        if let Err(error) = link_dir(outside.path(), &repo.join("linked")) {
            // Windows refuses symlinks without Developer Mode (os error 1314).
            eprintln!("skipped: this platform refuses to create symlinks: {error}");
            return;
        }
        let mut package = code(head(&repo).await);
        package.files.push(CodeFile {
            path: "linked/escaped.txt".into(),
            data: B64.encode(b"must stay in the tree"),
        });

        let pool = crate::testdb::fresh_pool().await;
        let error = materialize(&pool, &package, &repo, "chat", "transfer")
            .await
            .unwrap_err();

        assert!(
            error.contains("ancestor") && error.contains("symlink"),
            "{error}"
        );
        assert!(!outside.path().join("escaped.txt").exists());
    }
}
