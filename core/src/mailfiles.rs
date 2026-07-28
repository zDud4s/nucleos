//! The mail organization folder: one directory a person can arrange, and nothing outside it.
//!
//! This module exists because of a single question — given a path that came from somewhere else,
//! does it name a place inside the root? Every route here is a thin wrapper around
//! [`resolve_within`], and the wrappers are uninteresting on purpose: the whole design is that
//! there is exactly ONE function deciding what is reachable, so a mistake has one place to be.
//!
//! It matters more than an ordinary file browser would, because the names arriving here are chosen
//! by whoever sent the mail, and later by an agent that has read what they wrote.

use std::path::{Component, Path, PathBuf};

/// Why a path was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// The path names somewhere outside the root — `..`, an absolute path, a drive letter, or a
    /// symlink pointing out.
    Escapes,
    /// A component a filesystem should not be asked to store.
    Unsafe,
    NotFound,
    NotADirectory,
    Io(String),
}

/// One entry in the folder.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    /// Zero for a directory: the size of a folder is a different question, asked by walking it.
    pub size_bytes: i64,
    /// RFC 3339, or absent when the platform will not say.
    pub modified: Option<String>,
}

/// Where the folder lives, under the daemon's own data directory rather than anywhere a person
/// keeps their own files — nothing here should ever land in a synced folder by default.
pub fn root_for(data_local_dir: &Path) -> PathBuf {
    data_local_dir.join("mail")
}

/// Creates the root if it is missing, and returns it canonicalised.
///
/// Canonicalised ONCE, at startup, because every later containment check compares against it: a
/// root that is still a symlink or a short path (`C:\PROGRA~1`) would compare unequal to the
/// resolved children it actually contains, and the check would reject everything or — worse, if
/// written the other way round — accept everything.
pub fn ensure_root(data_local_dir: &Path) -> std::io::Result<PathBuf> {
    let root = root_for(data_local_dir);
    std::fs::create_dir_all(&root)?;
    std::fs::canonicalize(&root)
}

/// PURE-ish: resolves a relative path against the root, or refuses.
///
/// The rule is whitelist, not blacklist. Only ordinary named components are allowed through —
/// `..`, absolute roots, Windows drive prefixes and UNC shares are all ways of naming somewhere
/// else, and a folder inside the mail root never needs one. Stripping `..` instead of refusing it
/// would be the classic mistake: `....//` survives one pass of that and becomes `../`.
///
/// Each name is then held to the same standard as an attachment's filename, because it usually IS
/// one. A component that would be rewritten by that rule is refused rather than quietly renamed:
/// silently storing `report.docx` when asked for `report.docx.` means the caller and the disk
/// disagree about what exists.
///
/// Finally the result is canonicalised and checked against the root, which is what catches a
/// symlink placed inside the folder pointing out of it. The check runs against the deepest existing
/// ancestor, since the path being created does not exist yet.
pub fn resolve_within(root: &Path, relative: &str) -> Result<PathBuf, PathError> {
    let mut resolved = root.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => {
                let name = name.to_str().ok_or(PathError::Unsafe)?;
                if name != crate::email::safe_filename(name) {
                    return Err(PathError::Unsafe);
                }
                resolved.push(name);
            }
            _ => return Err(PathError::Escapes),
        }
    }

    // Everything above is textual. This is the part that survives a filesystem where a name is not
    // just a name: an existing symlink inside the root pointing anywhere else.
    let mut existing = resolved.as_path();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(real) => {
                if !real.starts_with(root) {
                    return Err(PathError::Escapes);
                }
                return Ok(resolved);
            }
            Err(_) => match existing.parent() {
                // Walked above the root without finding anything real, which cannot happen for a
                // path built from it — treat it as an escape rather than reason about why.
                Some(parent) if parent.starts_with(root) || parent == root => existing = parent,
                _ => return Err(PathError::Escapes),
            },
        }
    }
}

/// What is in a folder, directories first and then names, both case-insensitively — the order a
/// file manager uses, so the list does not reshuffle as things are added.
pub fn list(root: &Path, relative: &str) -> Result<Vec<Entry>, PathError> {
    let target = resolve_within(root, relative)?;
    if !target.exists() {
        return Err(PathError::NotFound);
    }
    if !target.is_dir() {
        return Err(PathError::NotADirectory);
    }

    let mut entries = Vec::new();
    for item in std::fs::read_dir(&target).map_err(|e| PathError::Io(e.to_string()))? {
        let item = item.map_err(|e| PathError::Io(e.to_string()))?;
        let metadata = match item.metadata() {
            Ok(metadata) => metadata,
            // A file that vanished between the listing and the stat is not an error worth failing
            // the whole folder over.
            Err(_) => continue,
        };
        let Some(name) = item.file_name().to_str().map(str::to_string) else {
            continue;
        };
        entries.push(Entry {
            name,
            is_dir: metadata.is_dir(),
            size_bytes: if metadata.is_dir() {
                0
            } else {
                metadata.len() as i64
            },
            modified: metadata
                .modified()
                .ok()
                .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339()),
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Creates a folder, and every folder above it that is missing.
pub fn create_folder(root: &Path, relative: &str) -> Result<(), PathError> {
    let target = resolve_within(root, relative)?;
    if target == root {
        // Creating the root is not a request anyone makes on purpose, and answering "done" would
        // hide a caller that thinks it named something.
        return Err(PathError::Unsafe);
    }
    std::fs::create_dir_all(&target).map_err(|e| PathError::Io(e.to_string()))
}

/// Writes a file into a folder under the root, returning the name it was actually stored under.
///
/// The name is returned rather than assumed because it can differ twice over: `safe_filename`
/// rewrites what a filesystem cannot carry, and a collision adds a suffix. Overwriting silently
/// would be the wrong default here — two attachments called `relatorio.docx` from two different
/// senders are two files, not one.
pub fn write_file(
    root: &Path,
    folder: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<String, PathError> {
    let directory = resolve_within(root, folder)?;
    if !directory.is_dir() {
        return Err(PathError::NotADirectory);
    }

    let safe = crate::email::safe_filename(filename);
    let name = available_name(&directory, &safe);
    std::fs::write(directory.join(&name), bytes).map_err(|e| PathError::Io(e.to_string()))?;
    Ok(name)
}

/// Finds a name that is not taken, by numbering rather than replacing.
fn available_name(directory: &Path, wanted: &str) -> String {
    if !directory.join(wanted).exists() {
        return wanted.to_string();
    }
    let (stem, extension) = match wanted.rsplit_once('.') {
        // A leading dot is the whole name, not an extension: `.gitignore` has no stem to number.
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (wanted, String::new()),
    };
    for attempt in 2..1000 {
        let candidate = format!("{stem} ({attempt}){extension}");
        if !directory.join(&candidate).exists() {
            return candidate;
        }
    }
    // A thousand files of the same name is not a case worth more code; the last one wins the race
    // rather than the function failing on something nobody will meet.
    format!("{stem} (1000){extension}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = ensure_root(dir.path()).unwrap();
        (dir, root)
    }

    /// The whole module rests on this one function, so it is tested against every shape of "name
    /// somewhere else" rather than the few that came to mind first.
    #[test]
    fn nothing_outside_the_root_can_be_named() {
        let (_guard, root) = temp_root();
        for escape in [
            "..",
            "../outside",
            "../../outside",
            "sub/../../outside",
            r"..\outside",
            r"sub\..\..\outside",
            "/etc/passwd",
            r"C:\Windows\System32",
            r"\\server\share",
            // A drive-relative path on Windows resolves against that drive's current directory.
            "C:outside",
        ] {
            assert_eq!(
                resolve_within(&root, escape),
                Err(PathError::Escapes),
                "accepted {escape:?}"
            );
        }
    }

    /// A component that the filename rule would rewrite is refused, not renamed: storing something
    /// under a different name than the caller asked for makes the two disagree about what exists.
    ///
    /// `....//outside` belongs here rather than with the escapes, and the reason is the point of
    /// the whitelist: `....` is not `..`, so a blacklist looking for parent references waves it
    /// through — one pass of naive `..`-stripping turns it INTO `../`. It is caught anyway, by the
    /// name rule, because a component of nothing but dots is not a name a filesystem will keep.
    #[test]
    fn an_unstorable_name_is_refused_rather_than_rewritten() {
        let (_guard, root) = temp_root();
        for bad in [
            "trailing.",
            "con",
            "with\u{7f}control",
            "  ",
            "....//outside",
        ] {
            assert!(
                matches!(
                    resolve_within(&root, bad),
                    Err(PathError::Unsafe) | Err(PathError::Escapes)
                ),
                "accepted {bad:?}"
            );
        }
    }

    #[test]
    fn ordinary_paths_resolve_under_the_root() {
        let (_guard, root) = temp_root();
        assert_eq!(resolve_within(&root, "").unwrap(), root);
        assert_eq!(resolve_within(&root, ".").unwrap(), root);
        assert_eq!(
            resolve_within(&root, "BACMAT/2026").unwrap(),
            root.join("BACMAT").join("2026")
        );
        // Accents and spaces are ordinary in a filename and must survive.
        assert_eq!(
            resolve_within(&root, "Relatórios de julho").unwrap(),
            root.join("Relatórios de julho")
        );
    }

    #[test]
    fn a_folder_can_be_created_and_listed() {
        let (_guard, root) = temp_root();
        create_folder(&root, "BACMAT/2026").unwrap();
        write_file(&root, "BACMAT/2026", "guia.docx", b"conteudo").unwrap();

        let top = list(&root, "").unwrap();
        assert_eq!(top.len(), 1);
        assert!(top[0].is_dir && top[0].name == "BACMAT");

        let inner = list(&root, "BACMAT/2026").unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].name, "guia.docx");
        assert_eq!(inner[0].size_bytes, 8);
        assert!(!inner[0].is_dir);
    }

    /// Two senders attaching `relatorio.docx` sent two files, and the second must not erase the
    /// first just because they agreed on a name.
    #[test]
    fn a_colliding_name_is_numbered_rather_than_overwritten() {
        let (_guard, root) = temp_root();
        assert_eq!(
            write_file(&root, "", "relatorio.docx", b"primeiro").unwrap(),
            "relatorio.docx"
        );
        assert_eq!(
            write_file(&root, "", "relatorio.docx", b"segundo").unwrap(),
            "relatorio (2).docx"
        );
        assert_eq!(
            write_file(&root, "", "relatorio.docx", b"terceiro").unwrap(),
            "relatorio (3).docx"
        );
        assert_eq!(
            std::fs::read(root.join("relatorio.docx")).unwrap(),
            b"primeiro"
        );
    }

    /// The name a sender chose is made safe on the way to disk, and the caller is told what it
    /// became rather than left assuming.
    #[test]
    fn a_written_filename_is_made_safe_and_reported() {
        let (_guard, root) = temp_root();
        let stored = write_file(&root, "", "../../.ssh/authorized_keys", b"x").unwrap();
        assert_eq!(stored, "authorized_keys");
        assert!(root.join("authorized_keys").exists());
    }

    #[test]
    fn listing_something_that_is_not_a_folder_says_so() {
        let (_guard, root) = temp_root();
        write_file(&root, "", "guia.docx", b"x").unwrap();
        assert_eq!(list(&root, "guia.docx"), Err(PathError::NotADirectory));
        assert_eq!(list(&root, "nao-existe"), Err(PathError::NotFound));
    }
}
