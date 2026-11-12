//! The empty folder a run with no project of its own starts in.
//!
//! A team seat, a council seat and a `map_intent` question are not about any checkout, yet a CLI
//! started with no working directory inherits the daemon's — the repository the owner is
//! developing in. Giving each run a folder of its own, empty at the start, means a sandbox profile
//! has nothing of the owner's to widen over and a stray `ls` finds nothing to read.

use std::path::{Path, PathBuf};

/// Where every such folder lives: one directory under the OS temp dir, so no project folder and
/// nothing under `C:\Projects` ever holds them.
fn root() -> PathBuf {
    std::env::temp_dir().join("nucleos-empty-runs")
}

/// Creates (or empties) `<root>/<kind>-<id>` and returns it; `None` if it cannot be made, in which
/// case the caller keeps the working directory it had before — a run is never refused for this.
pub fn empty_run_dir(kind: &str, id: &str) -> Option<PathBuf> {
    empty_run_dir_in(&root(), kind, id)
}

/// An empty folder no other call shares, for a run with no stable id of its own (a one-shot
/// question). The caller removes it with [`remove_run_dir`] when the run ends.
pub fn unique_empty_run_dir(kind: &str) -> Option<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    empty_run_dir(kind, &format!("{}-{n}", std::process::id()))
}

/// Best-effort removal of a folder [`empty_run_dir`] made.
pub fn remove_run_dir(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn empty_run_dir_in(root: &Path, kind: &str, id: &str) -> Option<PathBuf> {
    // The id names a directory, so only characters a file name can hold survive.
    let clean: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = root.join(format!("{kind}-{clean}"));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).ok()?;
    }
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_is_empty_even_when_an_earlier_run_left_files() {
        let base = tempfile::tempdir().unwrap();
        let first = empty_run_dir_in(base.path(), "team", "run-1").unwrap();
        std::fs::write(first.join("left-over.txt"), "x").unwrap();
        let second = empty_run_dir_in(base.path(), "team", "run-1").unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read_dir(&second).unwrap().count(), 0);
    }

    #[test]
    fn an_id_cannot_climb_out_of_the_root() {
        let base = tempfile::tempdir().unwrap();
        let dir = empty_run_dir_in(base.path(), "council", "../../x").unwrap();
        assert_eq!(dir.parent(), Some(base.path()));
    }

    #[test]
    fn the_root_is_under_the_temp_dir_and_not_the_project() {
        assert!(root().starts_with(std::env::temp_dir()));
    }
}
