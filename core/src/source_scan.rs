//! Where the core's Rust sources live, for the tests that scan them.
//!
//! The core is being split into crates under `core/crates/<name>/src/`. A test that walks
//! `core/src` alone would keep passing after a module moved out of it, because the scan would
//! simply stop seeing that file: an invariant checked against nothing is a green test that
//! protects nothing. Every scan therefore asks this module for its roots instead of joining
//! `"src"` itself, so a scan sees every crate the core is split into.
//!
//! Today `core/crates` does not exist and the only root is `core/src`. `#[cfg(test)]` (or the
//! `testkit` feature), so none of it is compiled into the daemon.

use std::path::{Path, PathBuf};

/// `core/src`, plus the `src` of every crate directory under `core/crates/`, sorted.
///
/// Rooted at the directory this binary was compiled in; callers keep their own
/// compiled-in-versus-running-in guard.
pub fn source_roots() -> Vec<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut roots = vec![manifest.join("src")];
    let mut crates: Vec<PathBuf> = std::fs::read_dir(manifest.join("crates"))
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("src"))
        .filter(|path| path.is_dir())
        .collect();
    crates.sort();
    roots.extend(crates);
    roots
}

/// Every `*.rs` file directly inside one of the roots, sorted. Subdirectories are not entered.
///
/// For scans that only ever covered the top level of `core/src`.
pub fn top_level_rust_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in source_roots() {
        let entries = std::fs::read_dir(&root).expect("core source directory must be readable");
        for entry in entries {
            let path = entry.expect("core source entry must be readable").path();
            if is_rust(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Every `*.rs` file under all roots, recursively, sorted.
pub fn rust_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in source_roots() {
        let mut pending = vec![root];
        while let Some(dir) = pending.pop() {
            let entries = std::fs::read_dir(&dir).expect("core source directory must be readable");
            for entry in entries {
                let path = entry.expect("core source entry must be readable").path();
                if path.is_dir() {
                    pending.push(path);
                } else if is_rust(&path) {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}

/// Whether `path` is the root of a source tree, so a file directly in it is a top-level file.
pub fn is_top_level(path: &Path) -> bool {
    path.parent()
        .is_some_and(|parent| source_roots().iter().any(|root| root == parent))
}

fn is_rust(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("rs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_roots_start_with_the_cores_own_src_and_all_exist() {
        let roots = source_roots();
        assert_eq!(roots[0], Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        assert!(roots.iter().all(|root| root.is_dir()), "{roots:?}");
    }

    #[test]
    fn every_rust_file_is_found_recursively_and_in_order() {
        let files = rust_files();
        let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
        assert!(files.contains(&lib));
        assert!(
            files.iter().any(|path| !is_top_level(path)),
            "a module directory such as council/ must be walked"
        );
        assert!(files.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn the_top_level_files_are_a_subset_without_subdirectories() {
        let all = rust_files();
        let top = top_level_rust_files();
        assert!(!top.is_empty());
        assert!(top.len() < all.len());
        assert!(
            top.iter()
                .all(|path| all.contains(path) && is_top_level(path))
        );
    }
}
