//! Per-worktree warm state for verification units — spec §4.6.
//!
//! Layout: `<root>/warm/<project>/<wt-id>/<dir>` per worktree and
//! `<root>/warm/<project>/shared/<dir>` for state a project shares. Every function takes `root`
//! (the resolved `~/.nucleos`) so tests can point it at a temp dir. Nothing here ever touches a
//! path outside `<root>/warm/`.
// Nothing outside the tests calls this until the F2a-2 executor does; core is a binary crate, so
// clippy's `dead_code` would otherwise fail the gate.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::tests_map::{Seed, Warm};

pub(crate) const MARKER_WORKTREE: &str = ".worktree";
pub(crate) const MARKER_USED: &str = ".last_used";
const SHARED: &str = "shared";
const SHARED_PREFIX: &str = "shared:";

/// Stable id of a worktree path: first 16 hex of sha256 of its canonical, lowercase,
/// '/'-separated form.
pub(crate) fn worktree_id(worktree: &Path) -> String {
    let canon = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    let mut text = canon.to_string_lossy().replace('\\', "/").to_lowercase();
    if let Some(rest) = text.strip_prefix("//?/") {
        text = rest.to_string();
    }
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

fn check_project_id(project_id: &str) -> io::Result<()> {
    if project_id.is_empty()
        || project_id.contains('/')
        || project_id.contains('\\')
        || project_id.contains("..")
    {
        return Err(invalid("invalid project id"));
    }
    Ok(())
}

/// A relative path made only of normal components.
fn check_relative(dir: &str) -> io::Result<()> {
    if dir.is_empty() {
        return Err(invalid("empty warm dir"));
    }
    for c in Path::new(dir).components() {
        if !matches!(c, Component::Normal(_)) {
            return Err(invalid("warm dir must be a plain relative path"));
        }
    }
    Ok(())
}

fn project_dir(root: &Path, project_id: &str) -> io::Result<PathBuf> {
    check_project_id(project_id)?;
    Ok(root.join("warm").join(project_id))
}

/// The per-worktree directory (the unit the LRU and cleanup delete).
pub(crate) fn worktree_dir(root: &Path, project_id: &str, worktree: &Path) -> PathBuf {
    root.join("warm")
        .join(project_id)
        .join(worktree_id(worktree))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Creates (if needed) the directories `warm:` declares and returns the env to hand the child.
/// Touches `.last_used`, writes `.worktree`. `seed: copy` is treated as `none` with a warning.
pub(crate) fn prepare(
    root: &Path,
    project_id: &str,
    worktree: &Path,
    warm: &BTreeMap<String, Warm>,
) -> io::Result<Vec<(String, PathBuf)>> {
    let pdir = project_dir(root, project_id)?;
    let wt_dir = pdir.join(worktree_id(worktree));
    let mut env = Vec::with_capacity(warm.len());
    for (var, w) in warm {
        let path = match w.dir.strip_prefix(SHARED_PREFIX) {
            Some(rest) => {
                check_relative(rest)?;
                pdir.join(SHARED).join(rest)
            }
            None => {
                check_relative(&w.dir)?;
                wt_dir.join(&w.dir)
            }
        };
        if w.seed == Seed::Copy {
            tracing::warn!(var = %var, dir = %w.dir, "warm seed: copy is not implemented yet, treated as none");
        }
        std::fs::create_dir_all(&path)?;
        env.push((var.clone(), path));
    }
    std::fs::create_dir_all(&wt_dir)?;
    let canon = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    std::fs::write(
        wt_dir.join(MARKER_WORKTREE),
        canon.to_string_lossy().replace('\\', "/"),
    )?;
    std::fs::write(wt_dir.join(MARKER_USED), now_ms().to_string())?;
    Ok(env)
}

/// Deletes one worktree's state. Missing is fine.
pub(crate) fn forget_worktree(root: &Path, project_id: &str, worktree: &Path) -> io::Result<()> {
    check_project_id(project_id)?;
    match std::fs::remove_dir_all(worktree_dir(root, project_id, worktree)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub(crate) struct Caps {
    pub machine_bytes: u64,
    pub project_bytes: HashMap<String, u64>,
    pub default_project_bytes: u64,
}

fn is_link(md: &std::fs::Metadata) -> bool {
    if md.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT: junctions and other reparse points.
        if md.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

/// Recursive sum of file sizes; links are skipped, never followed.
fn dir_size(path: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in rd.flatten() {
        let Ok(md) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if is_link(&md) {
            continue;
        }
        if md.is_dir() {
            total = total.saturating_add(dir_size(&entry.path()));
        } else {
            total = total.saturating_add(md.len());
        }
    }
    total
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| !is_link(&m) && m.is_dir())
        .unwrap_or(false)
}

fn last_used(dir: &Path) -> u128 {
    std::fs::read_to_string(dir.join(MARKER_USED))
        .ok()
        .and_then(|s| s.trim().parse::<u128>().ok())
        .unwrap_or(0)
}

struct Entry {
    path: PathBuf,
    size: u64,
    used: u128,
}

fn remove(path: &Path) -> bool {
    match std::fs::remove_dir_all(path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "warm sweep could not remove a directory");
            false
        }
    }
}

/// Removes worktree dirs whose `.worktree` path no longer exists, then enforces the caps,
/// least recently used first, never touching a dir in `in_use`. `shared/` counts, never deleted.
/// Returns the dirs removed.
pub(crate) fn sweep(root: &Path, caps: &Caps, in_use: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let warm_root = root.join("warm");
    let Ok(projects) = std::fs::read_dir(&warm_root) else {
        return removed;
    };
    // (project id, shared bytes, live worktree entries)
    let mut kept: Vec<(String, u64, Vec<Entry>)> = Vec::new();
    for p in projects.flatten() {
        let ppath = p.path();
        if !is_real_dir(&ppath) {
            continue;
        }
        let pid = p.file_name().to_string_lossy().to_string();
        let Ok(children) = std::fs::read_dir(&ppath) else {
            continue;
        };
        let mut shared = 0u64;
        let mut entries = Vec::new();
        for c in children.flatten() {
            let cpath = c.path();
            if !is_real_dir(&cpath) {
                continue;
            }
            if c.file_name() == SHARED {
                shared = dir_size(&cpath);
                continue;
            }
            let orphan = std::fs::read_to_string(cpath.join(MARKER_WORKTREE))
                .map(|s| !Path::new(s.trim()).exists())
                .unwrap_or(false);
            if orphan && !in_use.contains(&cpath) && remove(&cpath) {
                removed.push(cpath);
                continue;
            }
            entries.push(Entry {
                size: dir_size(&cpath),
                used: last_used(&cpath),
                path: cpath,
            });
        }
        kept.push((pid, shared, entries));
    }

    // Per-project caps, least recently used first.
    for (pid, shared, entries) in kept.iter_mut() {
        let cap = caps
            .project_bytes
            .get(pid)
            .copied()
            .unwrap_or(caps.default_project_bytes);
        let mut total = entries
            .iter()
            .fold(*shared, |acc, e| acc.saturating_add(e.size));
        entries.sort_by_key(|e| e.used);
        let mut i = 0;
        while total > cap && i < entries.len() {
            if in_use.contains(&entries[i].path) || !remove(&entries[i].path) {
                i += 1;
                continue;
            }
            total = total.saturating_sub(entries[i].size);
            removed.push(entries.remove(i).path);
        }
    }

    // Machine cap over what is left.
    let mut total = kept.iter().fold(0u64, |acc, (_, shared, entries)| {
        entries
            .iter()
            .fold(acc.saturating_add(*shared), |a, e| a.saturating_add(e.size))
    });
    if total > caps.machine_bytes {
        let mut all: Vec<&Entry> = kept.iter().flat_map(|(_, _, es)| es.iter()).collect();
        all.sort_by_key(|e| e.used);
        for e in all {
            if total <= caps.machine_bytes {
                break;
            }
            if in_use.contains(&e.path) || !remove(&e.path) {
                continue;
            }
            total = total.saturating_sub(e.size);
            removed.push(e.path.clone());
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warm(dir: &str) -> Warm {
        Warm {
            dir: dir.to_string(),
            seed: Seed::None,
        }
    }

    /// Hand-made worktree state: marker pointing at `wt`, a last-used stamp and `bytes` of data.
    fn mk(
        root: &Path,
        proj: &str,
        id: &str,
        wt: &Path,
        used: Option<&str>,
        bytes: usize,
    ) -> PathBuf {
        let d = root.join("warm").join(proj).join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(MARKER_WORKTREE), wt.to_string_lossy().as_bytes()).unwrap();
        if let Some(u) = used {
            std::fs::write(d.join(MARKER_USED), u).unwrap();
        }
        std::fs::write(d.join("data"), vec![0u8; bytes]).unwrap();
        d
    }

    fn caps(machine: u64, default_project: u64) -> Caps {
        Caps {
            machine_bytes: machine,
            project_bytes: HashMap::new(),
            default_project_bytes: default_project,
        }
    }

    #[test]
    fn the_same_worktree_gets_the_same_id() {
        let t = tempfile::tempdir().unwrap();
        let a = worktree_id(t.path());
        assert_eq!(a, worktree_id(t.path()));
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn two_worktrees_get_different_ids() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_ne!(worktree_id(a.path()), worktree_id(b.path()));
    }

    #[test]
    fn prepare_creates_the_declared_dirs_and_returns_their_env() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let mut w = BTreeMap::new();
        w.insert("CARGO_TARGET_DIR".to_string(), warm("target"));
        w.insert("npm_config_cache".to_string(), warm("shared:npm"));
        let env = prepare(root.path(), "proj", wt.path(), &w).unwrap();
        let get = |k: &str| env.iter().find(|(n, _)| n == k).unwrap().1.clone();
        let wt_dir = worktree_dir(root.path(), "proj", wt.path());
        assert_eq!(get("CARGO_TARGET_DIR"), wt_dir.join("target"));
        assert_eq!(
            get("npm_config_cache"),
            root.path().join("warm/proj/shared/npm")
        );
        assert!(get("CARGO_TARGET_DIR").is_dir());
        assert!(get("npm_config_cache").is_dir());
    }

    #[test]
    fn prepare_marks_the_worktree_and_its_use() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let mut w = BTreeMap::new();
        w.insert("X".to_string(), warm("target"));
        prepare(root.path(), "proj", wt.path(), &w).unwrap();
        let d = worktree_dir(root.path(), "proj", wt.path());
        let marked = std::fs::read_to_string(d.join(MARKER_WORKTREE)).unwrap();
        assert!(Path::new(&marked).exists());
        let used: u128 = std::fs::read_to_string(d.join(MARKER_USED))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(used > 0);
    }

    #[test]
    fn forget_removes_only_that_worktree() {
        let root = tempfile::tempdir().unwrap();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let mut w = BTreeMap::new();
        w.insert("X".to_string(), warm("target"));
        w.insert("Y".to_string(), warm("shared:npm"));
        prepare(root.path(), "proj", a.path(), &w).unwrap();
        prepare(root.path(), "proj", b.path(), &w).unwrap();
        forget_worktree(root.path(), "proj", a.path()).unwrap();
        assert!(!worktree_dir(root.path(), "proj", a.path()).exists());
        assert!(worktree_dir(root.path(), "proj", b.path()).exists());
        assert!(root.path().join("warm/proj/shared/npm").exists());
        // Missing is fine.
        forget_worktree(root.path(), "proj", a.path()).unwrap();
    }

    #[test]
    fn a_sweep_removes_state_whose_worktree_is_gone() {
        let root = tempfile::tempdir().unwrap();
        let live = tempfile::tempdir().unwrap();
        let gone = root.path().join("no-such-worktree");
        let d_live = mk(root.path(), "p", "aaaa", live.path(), Some("5"), 10);
        let d_gone = mk(root.path(), "p", "bbbb", &gone, Some("5"), 10);
        let removed = sweep(root.path(), &caps(u64::MAX, u64::MAX), &HashSet::new());
        assert_eq!(removed, vec![d_gone.clone()]);
        assert!(d_live.exists());
        assert!(!d_gone.exists());
    }

    #[test]
    fn a_sweep_enforces_the_project_cap_least_recent_first() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let old = mk(root.path(), "p", "d1", wt.path(), Some("10"), 1000);
        let mid = mk(root.path(), "p", "d2", wt.path(), Some("20"), 1000);
        let new = mk(root.path(), "p", "d3", wt.path(), Some("30"), 1000);
        let removed = sweep(root.path(), &caps(u64::MAX, 2500), &HashSet::new());
        assert_eq!(removed, vec![old.clone()]);
        assert!(mid.exists() && new.exists());

        // A missing or unreadable stamp counts as 0: the oldest.
        let root = tempfile::tempdir().unwrap();
        let none = mk(root.path(), "p", "d1", wt.path(), None, 1000);
        let a = mk(root.path(), "p", "d2", wt.path(), Some("20"), 1000);
        let junk = mk(root.path(), "p", "d3", wt.path(), Some("garbage"), 1000);
        let removed = sweep(root.path(), &caps(u64::MAX, 1500), &HashSet::new());
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&none) && removed.contains(&junk));
        assert!(a.exists());
    }

    #[test]
    fn a_sweep_never_removes_a_dir_in_use() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let old = mk(root.path(), "p", "d1", wt.path(), Some("10"), 1000);
        let new = mk(root.path(), "p", "d2", wt.path(), Some("30"), 1000);
        let in_use: HashSet<PathBuf> = [old.clone()].into_iter().collect();
        let removed = sweep(root.path(), &caps(u64::MAX, 1500), &in_use);
        assert_eq!(removed, vec![new.clone()]);
        assert!(old.exists());
        assert!(!new.exists());
    }

    #[test]
    fn a_sweep_enforces_the_machine_cap_across_projects() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let a_old = mk(root.path(), "a", "d1", wt.path(), Some("10"), 1000);
        let a_new = mk(root.path(), "a", "d2", wt.path(), Some("40"), 1000);
        let b_mid = mk(root.path(), "b", "d1", wt.path(), Some("20"), 1000);
        let b_new = mk(root.path(), "b", "d2", wt.path(), Some("30"), 1000);
        let removed = sweep(root.path(), &caps(3500, u64::MAX), &HashSet::new());
        assert_eq!(removed, vec![a_old.clone()]);
        assert!(a_new.exists() && b_mid.exists() && b_new.exists());
    }

    #[test]
    fn shared_state_is_never_swept() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let shared = root.path().join("warm/p/shared/npm");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("blob"), vec![0u8; 5000]).unwrap();
        let d = mk(root.path(), "p", "d1", wt.path(), Some("10"), 1000);
        let removed = sweep(root.path(), &caps(100, 100), &HashSet::new());
        assert_eq!(removed, vec![d]);
        assert!(shared.join("blob").exists());
    }

    #[test]
    fn a_project_id_that_escapes_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let mut w = BTreeMap::new();
        w.insert("X".to_string(), warm("target"));
        for bad in ["", "..", "a/b", "a\\b", "../x"] {
            let e = prepare(root.path(), bad, wt.path(), &w).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
            let e = forget_worktree(root.path(), bad, wt.path()).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        // A dir that escapes is refused too.
        let mut w = BTreeMap::new();
        w.insert("X".to_string(), warm("../out"));
        let e = prepare(root.path(), "p", wt.path(), &w).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }
}
