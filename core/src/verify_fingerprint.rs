//! The fingerprint of a verification unit: what makes a green result reusable (plan §5.2).
//!
//! A unit's fingerprint folds together the map's version, the group's own entry, the values of the
//! environment variables the group declares, and the content of the files it reads. Two runs with
//! the same fingerprint would execute the same command over the same bytes, so the first's green
//! stands for the second.
//!
//! The toolchain is deliberately outside scheme `fp1`: a compiler upgrade does not invalidate the
//! cache in v1. Variables written as `NAME=val` in the command travel with the group's entry,
//! which is hashed whole.

#![cfg_attr(not(test), allow(dead_code))]

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::tests_map::{self, Group};

/// The fingerprint scheme; bumping it orphans every cached green, which is the point of a bump.
pub(crate) const SCHEME: &str = "fp1";

/// Listing a large tree is the slow part of a fingerprint, so it gets more room than the usual
/// git operation.
pub(crate) const LIST_TIMEOUT: Duration = Duration::from_secs(60);

fn hex(hasher: Sha256) -> String {
    format!("{:x}", hasher.finalize())
}

/// Hex sha256 of the group's serialised entry: any edit to the map entry changes it.
pub(crate) fn entry_hash(group: &Group) -> String {
    // A `Group` always serialises; the fallback only keeps this infallible.
    let json = serde_json::to_string(group).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    hex(hasher)
}

/// The value of each named variable (`None` when unset), sorted by name so the order a map lists
/// them in never changes the fingerprint.
pub(crate) fn env_values(
    names: &[String],
    lookup: impl Fn(&str) -> Option<String>,
) -> Vec<(String, Option<String>)> {
    let mut values: Vec<(String, Option<String>)> = names
        .iter()
        .map(|name| (name.clone(), lookup(name)))
        .collect();
    values.sort_by(|a, b| a.0.cmp(&b.0));
    values
}

/// Folds the parts into `fp1:<hex>`. Every part is `\0`-terminated so two different splits of the
/// same bytes cannot collide, and an unset variable (`\x01unset`) differs from an empty one.
pub(crate) fn combine(
    map_version: u32,
    entry_hash: &str,
    env: &[(String, Option<String>)],
    content: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("map-version\0{map_version}\0entry\0{entry_hash}\0").as_bytes());
    for (name, value) in env {
        let value = value.as_deref().unwrap_or("\x01unset");
        hasher.update(format!("env\0{name}\0{value}\0").as_bytes());
    }
    hasher.update(format!("content\0{content}").as_bytes());
    format!("{SCHEME}:{}", hex(hasher))
}

/// Lets `std::io::copy` stream a file into the hasher without holding it in memory.
struct HashWriter<'a>(&'a mut Sha256);

impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Hex sha256 over the listed files, relative to `worktree`. Blocking: it reads every file.
///
/// Each path is followed by a marker saying what it is, so a file that became a symlink or a
/// directory changes the hash: `-` missing, `l<target>` symlink, `d` directory or gitlink,
/// `f<size>` then the bytes for a file.
pub(crate) fn hash_files(worktree: &Path, paths: &[String]) -> String {
    let mut hasher = Sha256::new();
    for path in paths {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        let full = worktree.join(path);
        match std::fs::symlink_metadata(&full) {
            Err(_) => hasher.update(b"-"),
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&full)
                    .map(|target| target.to_string_lossy().into_owned())
                    .unwrap_or_default();
                hasher.update(b"l");
                hasher.update(target.as_bytes());
            }
            Ok(meta) if meta.is_dir() => hasher.update(b"d"),
            Ok(meta) => {
                hasher.update(format!("f{}", meta.len()).as_bytes());
                // A file that vanishes or cannot be read mid-hash contributes what was read; the
                // size marker above still pins what it was when listed.
                if let Ok(mut file) = std::fs::File::open(&full) {
                    let _ = std::io::copy(&mut file, &mut HashWriter(&mut hasher));
                }
            }
        }
        hasher.update(b"\0");
    }
    hex(hasher)
}

/// A `reads` entry as a git pathspec: a glob stays a glob, anything else is a literal path with
/// its trailing `/` dropped so a directory names itself.
fn pathspec(read: &str) -> String {
    if read.contains('*') || read.contains('?') {
        format!(":(glob){read}")
    } else {
        format!(":(literal){}", read.trim_end_matches('/'))
    }
}

async fn list(
    worktree: &Path,
    flags: &[&str],
    pathspecs: &[String],
) -> Result<Vec<String>, String> {
    let mut args: Vec<OsString> = vec!["ls-files".into(), "-z".into()];
    args.extend(flags.iter().map(OsString::from));
    args.push("--".into());
    args.extend(pathspecs.iter().map(OsString::from));
    let refs: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
    let result = crate::git_exec::run_git(worktree, &refs, LIST_TIMEOUT).await?;
    if !result.succeeded() {
        return Err(format!(
            "git ls-files failed ({:?}): {}",
            result.exit_code, result.output_tail
        ));
    }
    Ok(result
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

/// The content hash of what a group reads: tracked and untracked-but-not-ignored files, plus
/// ignored ones when `include_ignored` and `reads` say so. Without `reads` it is the whole tree.
pub(crate) async fn content_hash(
    worktree: &Path,
    reads: Option<&[String]>,
    include_ignored: bool,
) -> Result<String, String> {
    let pathspecs: Vec<String> = reads
        .map(|reads| reads.iter().map(|read| pathspec(read)).collect())
        .unwrap_or_default();
    let mut files = list(
        worktree,
        &["--cached", "--others", "--exclude-standard"],
        &pathspecs,
    )
    .await?;
    // Ignored files only count when the group names what to read: with no `reads` this would be
    // the whole build output (the map parser refuses that combination too).
    if include_ignored && reads.is_some() {
        files.extend(
            list(
                worktree,
                &["--others", "--ignored", "--exclude-standard"],
                &pathspecs,
            )
            .await?,
        );
    }
    if let Some(reads) = reads {
        // A pathspec is looser than the map's own matcher; refilter so both agree on what is read.
        files.retain(|path| reads.iter().any(|read| tests_map::matches(read, path)));
    }
    files.sort();
    files.dedup();
    let root = worktree.to_path_buf();
    tokio::task::spawn_blocking(move || hash_files(&root, &files))
        .await
        .map_err(|error| format!("hashing the files panicked: {error}"))
}

/// The group's fingerprint from an already computed content hash; the environment comes from the
/// daemon's own process.
pub(crate) fn group_fingerprint_from(map_version: u32, group: &Group, content: &str) -> String {
    combine(
        map_version,
        &entry_hash(group),
        &env_values(&group.env, |name| std::env::var(name).ok()),
        content,
    )
}

pub(crate) async fn group_fingerprint(
    worktree: &Path,
    map_version: u32,
    group: &Group,
) -> Result<String, String> {
    let content = content_hash(worktree, group.reads.as_deref(), group.include_ignored).await?;
    Ok(group_fingerprint_from(map_version, group, &content))
}

/// The fingerprint of the project's `gate_command` unit, which has no map entry: the command text
/// and the content of the whole tree.
pub(crate) fn gate_fingerprint_from(command: &str, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("gate\0{command}\0content\0{content}").as_bytes());
    format!("{SCHEME}:{}", hex(hasher))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn group() -> Group {
        Group {
            paths: vec!["core/".into()],
            check: None,
            command: "run-tests core".into(),
            select: None,
            reads: None,
            cache: true,
            env: Vec::new(),
            include_ignored: false,
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn write(dir: &Path, path: &str, text: &str) {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    /// A committed repo with `core/a.rs`, `docs/b.md` and `ignored/x.txt` (ignored by git).
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        write(dir.path(), ".gitignore", "ignored/\n");
        write(dir.path(), "core/a.rs", "fn a() {}\n");
        write(dir.path(), "docs/b.md", "# b\n");
        write(dir.path(), "ignored/x.txt", "x1\n");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    fn reads(paths: &[&str]) -> Option<Vec<String>> {
        Some(paths.iter().map(|path| (*path).to_string()).collect())
    }

    #[test]
    fn entry_hash_is_stable_for_the_same_group() {
        assert_eq!(entry_hash(&group()), entry_hash(&group()));
        assert_eq!(entry_hash(&group()).len(), 64);
    }

    #[test]
    fn entry_hash_changes_with_each_field() {
        let variants = [
            Group {
                paths: vec!["shell/".into()],
                ..group()
            },
            Group {
                check: Some("run-check".into()),
                ..group()
            },
            Group {
                command: "run-tests all".into(),
                ..group()
            },
            Group {
                select: Some("select.py".into()),
                ..group()
            },
            Group {
                reads: reads(&["core/"]),
                ..group()
            },
            Group {
                cache: false,
                ..group()
            },
            Group {
                env: vec!["FOO".into()],
                ..group()
            },
            Group {
                include_ignored: true,
                ..group()
            },
        ];
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(entry_hash(&group()));
        for variant in &variants {
            assert!(
                seen.insert(entry_hash(variant)),
                "a field did not move the hash: {variant:?}"
            );
        }
    }

    #[test]
    fn combine_changes_with_the_map_version_and_an_env_value() {
        let env = vec![("A".to_string(), Some("1".to_string()))];
        let base = combine(1, "e", &env, "c");
        assert_eq!(base, combine(1, "e", &env, "c"));
        assert_ne!(base, combine(2, "e", &env, "c"));
        assert_ne!(base, combine(1, "other", &env, "c"));
        assert_ne!(base, combine(1, "e", &env, "other"));
        let changed = vec![("A".to_string(), Some("2".to_string()))];
        assert_ne!(base, combine(1, "e", &changed, "c"));
        // Unset differs from set-but-empty.
        let unset = vec![("A".to_string(), None)];
        let empty = vec![("A".to_string(), Some(String::new()))];
        assert_ne!(combine(1, "e", &unset, "c"), combine(1, "e", &empty, "c"));
    }

    #[test]
    fn env_values_reports_set_and_unset_names_sorted() {
        let names = vec!["B".to_string(), "A".to_string(), "C".to_string()];
        let values = env_values(&names, |name| (name != "B").then(|| format!("v{name}")));
        assert_eq!(
            values,
            vec![
                ("A".to_string(), Some("vA".to_string())),
                ("B".to_string(), None),
                ("C".to_string(), Some("vC".to_string())),
            ]
        );
    }

    #[test]
    fn the_fingerprint_carries_the_scheme_prefix() {
        let combined = combine(1, "e", &[], "c");
        let hex = combined.strip_prefix("fp1:").expect("fp1 prefix");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(group_fingerprint_from(1, &group(), "c").starts_with("fp1:"));
        assert!(gate_fingerprint_from("make", "c").starts_with("fp1:"));
        assert_eq!(SCHEME, "fp1");
    }

    #[tokio::test]
    async fn a_change_inside_reads_moves_the_fingerprint() {
        let dir = repo();
        let g = Group {
            reads: reads(&["core/"]),
            ..group()
        };
        let before = group_fingerprint(dir.path(), 1, &g).await.unwrap();
        assert_eq!(before, group_fingerprint(dir.path(), 1, &g).await.unwrap());
        write(dir.path(), "core/a.rs", "fn a() { changed(); }\n");
        let after = group_fingerprint(dir.path(), 1, &g).await.unwrap();
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn a_change_outside_reads_keeps_it() {
        let dir = repo();
        let g = Group {
            reads: reads(&["core/"]),
            ..group()
        };
        let before = group_fingerprint(dir.path(), 1, &g).await.unwrap();
        write(dir.path(), "docs/b.md", "# b, edited\n");
        write(dir.path(), "docs/new.md", "untracked and outside\n");
        let after = group_fingerprint(dir.path(), 1, &g).await.unwrap();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn without_reads_an_untracked_file_moves_it() {
        let dir = repo();
        let g = group();
        let before = group_fingerprint(dir.path(), 1, &g).await.unwrap();
        write(dir.path(), "docs/new.md", "untracked\n");
        let after = group_fingerprint(dir.path(), 1, &g).await.unwrap();
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn a_deleted_tracked_file_moves_it() {
        let dir = repo();
        let g = Group {
            reads: reads(&["core/"]),
            ..group()
        };
        let before = group_fingerprint(dir.path(), 1, &g).await.unwrap();
        std::fs::remove_file(dir.path().join("core/a.rs")).unwrap();
        let after = group_fingerprint(dir.path(), 1, &g).await.unwrap();
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn an_ignored_file_moves_it_only_with_include_ignored() {
        let dir = repo();
        let plain = Group {
            reads: reads(&["core/", "ignored/"]),
            ..group()
        };
        let inclusive = Group {
            include_ignored: true,
            ..plain.clone()
        };
        let plain_before = group_fingerprint(dir.path(), 1, &plain).await.unwrap();
        let inclusive_before = group_fingerprint(dir.path(), 1, &inclusive).await.unwrap();
        write(dir.path(), "ignored/x.txt", "x2, changed\n");
        let plain_after = group_fingerprint(dir.path(), 1, &plain).await.unwrap();
        let inclusive_after = group_fingerprint(dir.path(), 1, &inclusive).await.unwrap();
        assert_eq!(plain_before, plain_after);
        assert_ne!(inclusive_before, inclusive_after);
    }

    #[test]
    fn gate_fingerprint_changes_with_the_command() {
        let base = gate_fingerprint_from("make test", "c");
        assert_eq!(base, gate_fingerprint_from("make test", "c"));
        assert_ne!(base, gate_fingerprint_from("make check", "c"));
        assert_ne!(base, gate_fingerprint_from("make test", "other"));
    }
}
