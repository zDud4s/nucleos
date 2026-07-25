use sqlx::SqlitePool;
use std::path::{Component, Path, PathBuf};

const CAT_CAP: usize = 256 * 1024;
const GREP_CAP: usize = 200;
const GREP_LINE_CAP: usize = 400;
const SKIP_DIRS: [&str; 4] = [".git", "node_modules", "target", "__pycache__"];

#[derive(Debug)]
pub enum InspectError {
    NoRoot,
    UnsafePath,
    NotFound,
    Io(std::io::Error),
}

#[derive(serde::Serialize)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(serde::Serialize)]
pub struct Match {
    pub path: String,
    pub line: u64,
    pub text: String,
}

/// Returns the project's stored root, or None when the row is absent OR the root is NULL (off mode).
pub async fn project_root(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<String>> {
    let root: Option<Option<String>> =
        sqlx::query_scalar("SELECT project_root FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;
    Ok(root.flatten())
}

/// Join a caller-supplied relative path onto `root`, rejecting absolute paths and ANY `..`/root/prefix
/// component (conservative: even a `..` that would stay inside is rejected). Empty or "." -> root itself.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, InspectError> {
    let rel = rel.trim();
    if rel.is_empty() || rel == "." {
        return Ok(root.to_path_buf());
    }
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return Err(InspectError::UnsafePath);
    }
    for component in candidate.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(InspectError::UnsafePath);
            }
        }
    }
    Ok(root.join(candidate))
}

fn io_err(e: std::io::Error) -> InspectError {
    if e.kind() == std::io::ErrorKind::NotFound {
        InspectError::NotFound
    } else {
        InspectError::Io(e)
    }
}

pub fn ls(root: &Path, rel: &str) -> Result<Vec<Entry>, InspectError> {
    let dir = safe_join(root, rel)?;
    let read = std::fs::read_dir(&dir).map_err(io_err)?;
    let mut entries = Vec::new();
    for item in read {
        let item = item.map_err(InspectError::Io)?;
        let is_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push(Entry {
            name: item.file_name().to_string_lossy().into_owned(),
            is_dir,
        });
    }
    // Directories first, then files; each group alphabetical.
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(entries)
}

pub fn cat(root: &Path, rel: &str) -> Result<String, InspectError> {
    let file = safe_join(root, rel)?;
    let bytes = std::fs::read(&file).map_err(io_err)?;
    let truncated = bytes.len() > CAT_CAP;
    let slice = if truncated {
        &bytes[..CAT_CAP]
    } else {
        &bytes[..]
    };
    let mut text = String::from_utf8_lossy(slice).into_owned();
    if truncated {
        text.push_str("\n…[truncated]");
    }
    Ok(text)
}

pub fn grep(root: &Path, query: &str, rel: &str) -> Result<Vec<Match>, InspectError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let base = safe_join(root, rel)?;
    let mut out = Vec::new();
    let mut stack = vec![base];
    while let Some(dir) = stack.pop() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for item in read.flatten() {
            let file_type = match item.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let path = item.path();
            if file_type.is_dir() {
                let name = item.file_name().to_string_lossy().into_owned();
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                // read_to_string fails on binary/non-UTF8 files -> silently skipped.
                if let Ok(content) = std::fs::read_to_string(&path) {
                    for (index, line) in content.lines().enumerate() {
                        if line.contains(query) {
                            let rel_path = path
                                .strip_prefix(root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .replace('\\', "/");
                            out.push(Match {
                                path: rel_path,
                                line: index as u64 + 1,
                                text: line.chars().take(GREP_LINE_CAP).collect(),
                            });
                            if out.len() >= GREP_CAP {
                                return Ok(out);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

pub fn diff(root: &Path) -> Result<String, InspectError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("diff")
        .output()
        .map_err(InspectError::Io)?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn safe_join_accepts_normal_relative_paths() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        assert_eq!(
            safe_join(root, "src/main.rs").unwrap(),
            root.join("src/main.rs")
        );
        assert_eq!(safe_join(root, "").unwrap(), root);
        assert_eq!(safe_join(root, ".").unwrap(), root);
    }

    #[test]
    fn safe_join_rejects_traversal_and_absolute() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        for path in ["..", "../x", "a/../b", "/etc/passwd", "C:/x"] {
            assert!(matches!(
                safe_join(root, path),
                Err(InspectError::UnsafePath)
            ));
        }
    }

    #[test]
    fn ls_lists_dirs_first() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("a.txt"), "file").unwrap();
        std::fs::create_dir(root.join("zsub")).unwrap();

        let entries = ls(root, "").unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "zsub");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "a.txt");
        assert!(!entries[1].is_dir);
    }

    #[test]
    fn cat_reads_and_reports_missing() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("hello.txt"), "hi").unwrap();

        assert_eq!(cat(root, "hello.txt").unwrap(), "hi");
        assert!(matches!(cat(root, "nope.txt"), Err(InspectError::NotFound)));
    }

    #[test]
    fn grep_finds_across_files_skips_git_and_caps() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("keep.txt"), "before\nneedle here\nafter").unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "needle hidden").unwrap();

        let matches = grep(root, "needle", "").unwrap();
        assert!(matches.iter().any(|item| item.path == "keep.txt"));
        assert!(!matches.iter().any(|item| item.path.starts_with(".git/")));

        std::fs::write(root.join("many.txt"), "needle\n".repeat(250)).unwrap();
        assert_eq!(grep(root, "needle", "").unwrap().len(), GREP_CAP);
    }

    #[tokio::test]
    async fn project_root_reads_autopilot_state() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('p', 'active', '/some/root')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('off1', 'off')")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            project_root(&pool, "p").await.unwrap(),
            Some("/some/root".to_string())
        );
        assert_eq!(project_root(&pool, "off1").await.unwrap(), None);
        assert_eq!(project_root(&pool, "unknown").await.unwrap(), None);
    }
}
