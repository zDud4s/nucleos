//! One-time, non-destructive migration of the app data written under the old bundle
//! identifier to the new one. Copies, never moves or deletes; the old directories stay.

use std::fs;
use std::path::{Path, PathBuf};

/// The identifier the shell shipped under before the rename.
pub const OLD_ID: &str = "com.pc-multimedia.shell";
/// The identifier the shell ships under now.
pub const NEW_ID: &str = "com.nucleos.app";

/// What a migration pass did. Never an error type: the shell must start regardless.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Bases whose `old` directory was copied completely and marked.
    pub migrated: Vec<PathBuf>,
    /// One message per file or directory that could not be copied.
    pub errors: Vec<String>,
}

/// Copy `b/old` into `b/new` for every base `b`, skipping what already exists in `b/new`,
/// and write the `.migrated-from-<old>` marker last, only when no error occurred.
pub fn migrate_in(bases: &[PathBuf], old: &str, new: &str) -> Report {
    let mut report = Report::default();
    for base in bases {
        let src = base.join(old);
        let dst = base.join(new);
        let marker = dst.join(format!(".migrated-from-{old}"));
        if !src.is_dir() || marker.exists() {
            continue;
        }
        let before = report.errors.len();
        copy_tree(&src, &dst, &mut report.errors);
        if report.errors.len() != before {
            continue;
        }
        match fs::write(&marker, b"") {
            Ok(()) => report.migrated.push(base.clone()),
            Err(e) => report
                .errors
                .push(format!("write marker {}: {e}", marker.display())),
        }
    }
    report
}

/// Recursively copy `src` into `dst`, never overwriting an existing file and never
/// following symlinks. Failures are appended to `errors`.
fn copy_tree(src: &Path, dst: &Path, errors: &mut Vec<String>) {
    if let Err(e) = fs::create_dir_all(dst) {
        errors.push(format!("create {}: {e}", dst.display()));
        return;
    }
    let entries = match fs::read_dir(src) {
        Ok(entries) => entries,
        Err(e) => {
            errors.push(format!("read {}: {e}", src.display()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                errors.push(format!("read entry in {}: {e}", src.display()));
                continue;
            }
        };
        let from = entry.path();
        let to = dst.join(entry.file_name());
        match entry.file_type() {
            Ok(t) if t.is_dir() => copy_tree(&from, &to, errors),
            Ok(t) if t.is_file() => {
                if to.exists() {
                    continue;
                }
                if let Err(e) = fs::copy(&from, &to) {
                    errors.push(format!("copy {}: {e}", from.display()));
                }
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("stat {}: {e}", from.display())),
        }
    }
}

/// The base directories to migrate in, for `os` (a `std::env::consts::OS` value), reading
/// environment variables through `env`.
pub fn roots_for(os: &str, env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let var = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let mut roots = Vec::new();
    match os {
        "windows" => {
            roots.extend(var("APPDATA"));
            roots.extend(var("LOCALAPPDATA"));
        }
        "macos" => {
            if let Some(home) = var("HOME") {
                let lib = home.join("Library");
                for dir in ["Application Support", "WebKit", "Caches", "HTTPStorages"] {
                    roots.push(lib.join(dir));
                }
            }
        }
        "linux" => {
            let home = var("HOME");
            let pick =
                |key: &str, rel: &str| var(key).or_else(|| home.as_ref().map(|h| h.join(rel)));
            roots.extend(pick("XDG_CONFIG_HOME", ".config"));
            roots.extend(pick("XDG_DATA_HOME", ".local/share"));
            roots.extend(pick("XDG_CACHE_HOME", ".cache"));
        }
        _ => {}
    }
    roots
}

/// Run the migration for the real environment. Never panics; failures are only logged.
pub fn run_at_startup() {
    let roots = roots_for(std::env::consts::OS, &|k| std::env::var(k).ok());
    let report = migrate_in(&roots, OLD_ID, NEW_ID);
    for error in &report.errors {
        eprintln!("legacy identity migration: {error}");
    }
    #[cfg(target_os = "macos")]
    if let Ok(home) = std::env::var("HOME") {
        let prefs = PathBuf::from(home).join("Library/Preferences");
        let from = prefs.join(format!("{OLD_ID}.plist"));
        let to = prefs.join(format!("{NEW_ID}.plist"));
        if from.is_file() && !to.exists() {
            if let Err(e) = fs::copy(&from, &to) {
                eprintln!("legacy identity migration: copy {}: {e}", from.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn marker(base: &Path) -> PathBuf {
        base.join(NEW_ID).join(format!(".migrated-from-{OLD_ID}"))
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn copies_nested_tree_and_writes_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        write(&base.join(OLD_ID).join("a.txt"), "A");
        write(&base.join(OLD_ID).join("sub/deep/b.txt"), "B");

        let report = migrate_in(std::slice::from_ref(&base), OLD_ID, NEW_ID);

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            fs::read_to_string(base.join(NEW_ID).join("a.txt")).unwrap(),
            "A"
        );
        assert_eq!(
            fs::read_to_string(base.join(NEW_ID).join("sub/deep/b.txt")).unwrap(),
            "B"
        );
        assert!(marker(&base).exists());
    }

    #[test]
    fn second_call_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        write(&base.join(OLD_ID).join("a.txt"), "A");
        migrate_in(std::slice::from_ref(&base), OLD_ID, NEW_ID);

        // Mutate the new side and add a file to the old side; a no-op must keep both as-is.
        write(&base.join(NEW_ID).join("a.txt"), "changed");
        write(&base.join(OLD_ID).join("late.txt"), "L");
        migrate_in(std::slice::from_ref(&base), OLD_ID, NEW_ID);

        assert_eq!(
            fs::read_to_string(base.join(NEW_ID).join("a.txt")).unwrap(),
            "changed"
        );
        assert!(!base.join(NEW_ID).join("late.txt").exists());
    }

    #[test]
    fn never_overwrites_an_existing_destination_file() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        write(&base.join(OLD_ID).join("a.txt"), "old");
        write(&base.join(OLD_ID).join("b.txt"), "B");
        write(&base.join(NEW_ID).join("a.txt"), "new");

        migrate_in(std::slice::from_ref(&base), OLD_ID, NEW_ID);

        assert_eq!(
            fs::read_to_string(base.join(NEW_ID).join("a.txt")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(base.join(NEW_ID).join("b.txt")).unwrap(),
            "B"
        );
    }

    #[test]
    fn old_tree_is_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        write(&base.join(OLD_ID).join("a.txt"), "A");
        write(&base.join(OLD_ID).join("sub/b.txt"), "B");

        migrate_in(std::slice::from_ref(&base), OLD_ID, NEW_ID);

        assert_eq!(
            fs::read_to_string(base.join(OLD_ID).join("a.txt")).unwrap(),
            "A"
        );
        assert_eq!(
            fs::read_to_string(base.join(OLD_ID).join("sub/b.txt")).unwrap(),
            "B"
        );
        assert!(!base
            .join(OLD_ID)
            .join(format!(".migrated-from-{OLD_ID}"))
            .exists());
    }

    #[test]
    fn missing_old_dir_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();

        let report = migrate_in(std::slice::from_ref(&base), OLD_ID, NEW_ID);

        assert!(report.errors.is_empty());
        assert!(!base.join(NEW_ID).exists());
        assert!(!marker(&base).exists());
    }

    #[test]
    fn destination_that_is_a_file_reports_an_error_and_no_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        write(&base.join(OLD_ID).join("a.txt"), "A");
        // `b/new` is a plain file, so nothing can be copied into it.
        fs::write(base.join(NEW_ID), "not a directory").unwrap();

        let report = migrate_in(std::slice::from_ref(&base), OLD_ID, NEW_ID);

        assert!(!report.errors.is_empty());
        assert!(!marker(&base).exists());
    }

    #[test]
    fn roots_for_returns_the_bases_of_each_os() {
        let env = |k: &str| -> Option<String> {
            match k {
                "APPDATA" => Some("/ad".into()),
                "LOCALAPPDATA" => Some("/lad".into()),
                "HOME" => Some("/home/u".into()),
                "XDG_CONFIG_HOME" => Some("/xc".into()),
                "XDG_DATA_HOME" => Some("/xd".into()),
                "XDG_CACHE_HOME" => Some("/xk".into()),
                _ => None,
            }
        };
        assert_eq!(
            roots_for("windows", &env),
            vec![PathBuf::from("/ad"), PathBuf::from("/lad")]
        );
        assert_eq!(
            roots_for("macos", &env),
            vec![
                PathBuf::from("/home/u/Library/Application Support"),
                PathBuf::from("/home/u/Library/WebKit"),
                PathBuf::from("/home/u/Library/Caches"),
                PathBuf::from("/home/u/Library/HTTPStorages"),
            ]
        );
        assert_eq!(
            roots_for("linux", &env),
            vec![
                PathBuf::from("/xc"),
                PathBuf::from("/xd"),
                PathBuf::from("/xk")
            ]
        );
        // XDG variables unset: fall back to the conventional paths under HOME.
        let only_home = |k: &str| -> Option<String> { (k == "HOME").then(|| "/home/u".into()) };
        assert_eq!(
            roots_for("linux", &only_home),
            vec![
                PathBuf::from("/home/u/.config"),
                PathBuf::from("/home/u/.local/share"),
                PathBuf::from("/home/u/.cache"),
            ]
        );
    }
}
