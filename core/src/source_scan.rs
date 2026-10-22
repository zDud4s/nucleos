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

/// `path` from the root of its source tree, with `/` between folders: `runs.rs`,
/// `council/store.rs`. What a scan names a file by, because a bare file name repeats as soon as
/// modules live in folders (`council/store.rs`, `verify/store.rs`).
pub fn module_path(path: &Path) -> String {
    let root = source_roots()
        .into_iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count());
    let below = root
        .as_deref()
        .and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path);
    below
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether `path` is a module its parent declares as `#[cfg(test)] mod <stem>;`, so the whole
/// file is test code. The parent is `mod.rs` (or `lib.rs`/`main.rs`) in the same directory, or
/// `<dir>.rs` beside that directory, or `app.rs` beside it: `nucleos-app` declares the modules it
/// adds under `channels` inside an inline `mod channels { ... }` there.
pub fn is_test_only_module(path: &Path) -> bool {
    let (Some(dir), Some(stem)) = (
        path.parent(),
        path.file_stem().and_then(|stem| stem.to_str()),
    ) else {
        return false;
    };
    let mut parents = vec![dir.join("mod.rs"), dir.join("lib.rs"), dir.join("main.rs")];
    if let (Some(grand), Some(name)) = (dir.parent(), dir.file_name()) {
        parents.push(grand.join(name).with_extension("rs"));
        parents.push(grand.join("app.rs"));
    }
    parents
        .iter()
        .filter_map(|parent| std::fs::read_to_string(parent).ok())
        .any(|source| declares_test_only_mod(&source, stem))
}

/// Whether `source` has `#[cfg(test)]` on the line right before `mod <stem>;`.
fn declares_test_only_mod(source: &str, stem: &str) -> bool {
    let lines: Vec<&str> = source.lines().map(str::trim).collect();
    lines.windows(2).any(|w| {
        w[0] == "#[cfg(test)]"
            && [
                format!("mod {stem};"),
                format!("pub(crate) mod {stem};"),
                format!("pub mod {stem};"),
            ]
            .contains(&w[1].to_owned())
    })
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
    fn an_out_of_line_cfg_test_module_is_test_only() {
        assert!(declares_test_only_mod(
            "#[cfg(test)]\nmod switch_tests;",
            "switch_tests"
        ));
        assert!(!declares_test_only_mod("mod switch_tests;", "switch_tests"));
        assert!(!declares_test_only_mod(
            "#[cfg(test)]\nmod other;\nmod x;",
            "x"
        ));
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert!(is_test_only_module(
            &src.join("channels").join("switch_tests.rs")
        ));
        assert!(!is_test_only_module(
            &src.join("channels").join("commands.rs")
        ));
    }

    #[test]
    fn a_file_is_named_by_its_path_from_its_root() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert_eq!(module_path(&src.join("runs.rs")), "runs.rs");
        assert_eq!(
            module_path(&src.join("council").join("store.rs")),
            "council/store.rs"
        );
    }
}
