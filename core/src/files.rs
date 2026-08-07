//! The files folder: one directory a person can arrange, and nothing outside it.
//!
//! This module exists because of a single question — given a path that came from somewhere else,
//! does it name a place inside the root? Every route here is a thin wrapper around
//! [`resolve_within`], and the wrappers are uninteresting on purpose: the whole design is that
//! there is exactly ONE function deciding what is reachable, so a mistake has one place to be.
//!
//! It matters more than an ordinary file browser would, because some of the names arriving here
//! were chosen by whoever sent the mail an attachment was filed from, and are later read by an
//! agent. That a person now also uploads their own files here changes who picked the name, never
//! the rule: this folder is arranged through this module or not at all.

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
    /// A directory with something still in it, asked to go without that being said out loud.
    NotEmpty,
    /// A destination already taken. A rename never overwrites — see [`move_entry`].
    Exists,
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
/// already keeps their own files — nothing here should ever land in a synced folder by default,
/// and an upload is a copy INTO this root rather than a window onto the disk around it.
pub fn root_for(data_local_dir: &Path) -> PathBuf {
    data_local_dir.join("files")
}

/// The largest upload the daemon will take in one request.
///
/// It is a memory ceiling as much as a policy one: the body arrives whole before anything is
/// written, so this number is also how much RAM one upload can hold at once. Chosen well above what
/// mail carries (a 25 MB attachment is already an unusual one) and well below the size at which a
/// desktop daemon buffering a file becomes the problem. A download has no matching limit because it
/// is streamed — reading back never needed the file to fit in memory.
pub const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

/// What this directory was called while mail attachments were the only thing in it.
///
/// Kept as a name rather than deleted with the old behaviour, because the directory it names is on
/// somebody's disk with their filed mail in it.
const FILED_MAIL_ROOT: &str = "mail";

/// Creates the root if it is missing, and returns it canonicalised.
///
/// Canonicalised ONCE, at startup, because every later containment check compares against it: a
/// root that is still a symlink or a short path (`C:\PROGRA~1`) would compare unequal to the
/// resolved children it actually contains, and the check would reject everything or — worse, if
/// written the other way round — accept everything.
///
/// The rename from `mail/` runs here, once, and only into a name nothing is using yet: mail filed
/// before this folder grew past mail has to still be in it afterwards, or the feature arrives by
/// hiding the user's files. A failed rename leaves the old directory alone and makes an empty new
/// one — recoverable by hand, which "helpfully" merging two trees would not be.
pub fn ensure_root(data_local_dir: &Path) -> std::io::Result<PathBuf> {
    let root = root_for(data_local_dir);
    let filed_mail = data_local_dir.join(FILED_MAIL_ROOT);
    if !root.exists()
        && filed_mail.is_dir()
        && let Err(error) = std::fs::rename(&filed_mail, &root)
    {
        tracing::warn!(
            %error,
            from = %filed_mail.display(),
            to = %root.display(),
            "could not move the old mail folder into the files folder — filed mail stays where it is"
        );
    }
    std::fs::create_dir_all(&root)?;
    std::fs::canonicalize(&root)
}

/// PURE-ish: resolves a relative path against the root, or refuses.
///
/// The rule is whitelist, not blacklist. Only ordinary named components are allowed through —
/// `..`, absolute roots, Windows drive prefixes and UNC shares are all ways of naming somewhere
/// else, and a folder inside this root never needs one. Stripping `..` instead of refusing it
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

/// One search hit: an entry, plus where it is relative to the ROOT so a click can go straight there.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Hit {
    pub path: String,
    #[serde(flatten)]
    pub entry: Entry,
}

/// What a search found, and whether it stopped early.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// True when a ceiling cut the walk short. Said out loud rather than left to look like an
    /// exhaustive answer — a search that quietly returns half the matches is worse than one that
    /// admits it.
    pub truncated: bool,
}

/// The most hits one search returns, and the most entries it will look at to find them.
///
/// Two ceilings rather than one because they fail differently: a folder with 400 matching files
/// answers instantly and needs the first, and a deep tree with two matches walks everything and
/// needs the second.
const SEARCH_HITS: usize = 500;
const SEARCH_VISITS: usize = 20_000;

/// Finds entries whose name contains `query`, in this folder and every folder under it.
///
/// Case-insensitive and substring, because that is what a person typing three letters into a search
/// box means — not a glob they have to get right, and not a regex a filename would fight with.
///
/// Symlinked directories are matched but never descended into. `resolve_within` already refuses a
/// link that points outside the root, so this is about the other half: a link pointing back INSIDE
/// it would be a cycle, and a walk that follows one never ends.
pub fn search(root: &Path, relative: &str, query: &str) -> Result<Found, PathError> {
    let start = resolve_within(root, relative)?;
    if !start.is_dir() {
        return Err(PathError::NotADirectory);
    }
    let needle = query.to_lowercase();
    if needle.is_empty() {
        // An empty query matches everything, which is the listing — and answering it here would
        // walk the whole tree to say what `list` says about one folder.
        return Ok(Found {
            hits: Vec::new(),
            truncated: false,
        });
    }

    let mut hits = Vec::new();
    let mut visits = 0usize;
    let mut truncated = false;
    let mut pending = vec![start];

    while let Some(directory) = pending.pop() {
        let reader = match std::fs::read_dir(&directory) {
            Ok(reader) => reader,
            // A folder that vanished or refuses to open mid-walk is skipped, not fatal: the rest of
            // the tree is still a useful answer.
            Err(_) => continue,
        };
        for item in reader.flatten() {
            visits += 1;
            if visits > SEARCH_VISITS || hits.len() >= SEARCH_HITS {
                truncated = true;
                return Ok(Found { hits, truncated });
            }
            let Some(name) = item.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(metadata) = item.metadata() else {
                continue;
            };
            let full = item.path();
            let is_link = std::fs::symlink_metadata(&full)
                .map(|link| link.is_symlink())
                .unwrap_or(false);

            if metadata.is_dir() && !is_link {
                pending.push(full.clone());
            }
            if !name.to_lowercase().contains(&needle) {
                continue;
            }
            // Relative to the root, which is the only path the shell can send back to any route.
            let Ok(under_root) = full.strip_prefix(root) else {
                continue;
            };
            hits.push(Hit {
                path: under_root.to_string_lossy().replace('\\', "/"),
                entry: Entry {
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
                },
            });
        }
    }

    hits.sort_by(|a, b| {
        b.entry
            .is_dir
            .cmp(&a.entry.is_dir)
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
    Ok(Found { hits, truncated })
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

/// Resolves a path that must name a file that is there, for handing its bytes back.
///
/// Reading is the half this folder was missing: it accepted attachments from the day it existed and
/// had no way to give one back, so the only route to a filed file was the file manager of the OS.
/// The bytes are not read here — the caller streams them — because a folder that will carry
/// whatever a person uploads must not have its ceiling set by how much of one file fits in memory.
pub fn resolve_file(root: &Path, relative: &str) -> Result<PathBuf, PathError> {
    let target = resolve_within(root, relative)?;
    let metadata = std::fs::metadata(&target).map_err(|_| PathError::NotFound)?;
    if metadata.is_dir() {
        // Not `NotADirectory` inverted into a new variant: the caller asked for the bytes of a
        // thing that has none, and the shell's answer is to open it as a folder instead.
        return Err(PathError::NotADirectory);
    }
    Ok(target)
}

/// Removes a file, or a directory the caller has said out loud it wants gone with its contents.
///
/// The two-step for a full directory is the whole point: `remove_dir_all` on a mistyped path is the
/// one action here nobody can undo, and this folder holds attachments that exist nowhere else once
/// the mail body they came from has expired. An empty directory goes without ceremony — there is
/// nothing to lose — and a full one is refused until the caller repeats itself with `recursive`.
pub fn delete(root: &Path, relative: &str, recursive: bool) -> Result<(), PathError> {
    let target = resolve_within(root, relative)?;
    if target == root {
        // Emptying the root is not something a wrong path should be able to ask for by accident.
        return Err(PathError::Unsafe);
    }
    let metadata = std::fs::symlink_metadata(&target).map_err(|_| PathError::NotFound)?;
    if !metadata.is_dir() {
        // A symlink is removed as the link it is, never followed — which is also why the metadata
        // read above does not follow it. Windows needs `remove_dir` for a link that points at a
        // directory and `remove_file` for every other link, and nothing in the entry says which,
        // so the second is tried when the first refuses rather than reported as an I/O failure.
        return std::fs::remove_file(&target)
            .or_else(|error| {
                if metadata.is_symlink() {
                    std::fs::remove_dir(&target)
                } else {
                    Err(error)
                }
            })
            .map_err(|e| PathError::Io(e.to_string()));
    }

    let empty = std::fs::read_dir(&target)
        .map_err(|e| PathError::Io(e.to_string()))?
        .next()
        .is_none();
    if !empty && !recursive {
        return Err(PathError::NotEmpty);
    }
    if empty {
        std::fs::remove_dir(&target).map_err(|e| PathError::Io(e.to_string()))
    } else {
        std::fs::remove_dir_all(&target).map_err(|e| PathError::Io(e.to_string()))
    }
}

/// Renames or moves something inside the root. Both are the same operation with a different parent.
///
/// A taken destination is refused rather than numbered, which is the opposite of [`write_file`] and
/// deliberately so: filing an attachment asks for somewhere to put a file, and a rename asks for
/// one exact name. Numbering the second would answer a question nobody asked, and the caller and
/// the disk would disagree about what exists.
pub fn move_entry(root: &Path, from: &str, to: &str) -> Result<(), PathError> {
    let source = resolve_within(root, from)?;
    let destination = resolve_within(root, to)?;
    if source == root || destination == root {
        return Err(PathError::Unsafe);
    }
    if !source.exists() {
        return Err(PathError::NotFound);
    }
    if destination.exists() {
        return Err(PathError::Exists);
    }
    // A directory moved inside itself is the loop a file manager has to refuse: the rename would
    // either fail with an OS error nobody can read or, on a platform that allows it, detach the
    // subtree from the root that anchors every containment check made here.
    if destination.starts_with(&source) {
        return Err(PathError::Unsafe);
    }
    match destination.parent() {
        Some(parent) if parent.is_dir() => {}
        // Moving into a folder that is not there is a typo, not an instruction to create it: a
        // silently created parent is how a file ends up somewhere nobody looks.
        _ => return Err(PathError::NotFound),
    }
    std::fs::rename(&source, &destination).map_err(|e| PathError::Io(e.to_string()))
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

    /// Mail filed before this folder grew past mail has to still be in it afterwards. A rename that
    /// silently did nothing would look identical to a working one until someone went looking for a
    /// document they filed last month.
    #[test]
    fn mail_filed_under_the_old_name_arrives_in_the_new_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("mail").join("BACMAT")).unwrap();
        std::fs::write(
            dir.path().join("mail").join("BACMAT").join("guia.docx"),
            b"x",
        )
        .unwrap();

        let root = ensure_root(dir.path()).unwrap();

        assert!(root.ends_with("files"));
        assert_eq!(list(&root, "BACMAT").unwrap()[0].name, "guia.docx");
        assert!(
            !dir.path().join("mail").exists(),
            "the old folder was left behind as well"
        );
    }

    /// The second startup must not move anything: by then `files/` is the folder in use, and a
    /// `mail/` beside it would be something else — a folder the user made, most likely.
    #[test]
    fn the_move_happens_once_and_never_merges() {
        let dir = tempfile::tempdir().unwrap();
        let root = ensure_root(dir.path()).unwrap();
        write_file(&root, "", "carregado.txt", b"upload").unwrap();
        std::fs::create_dir_all(dir.path().join("mail")).unwrap();
        std::fs::write(dir.path().join("mail").join("outro.txt"), b"y").unwrap();

        let again = ensure_root(dir.path()).unwrap();

        assert_eq!(again, root);
        assert_eq!(
            list(&root, "").unwrap().len(),
            1,
            "the second root swallowed a sibling"
        );
        assert!(dir.path().join("mail").join("outro.txt").exists());
    }

    /// Search descends, which is the whole difference between it and filtering a listing.
    #[test]
    fn a_search_finds_matches_below_the_folder_it_starts_in() {
        let (_guard, root) = temp_root();
        create_folder(&root, "BACMAT/2026").unwrap();
        create_folder(&root, "Recibos").unwrap();
        write_file(&root, "BACMAT/2026", "guia de transporte.docx", b"x").unwrap();
        write_file(&root, "Recibos", "guia.pdf", b"x").unwrap();
        write_file(&root, "", "outro.txt", b"x").unwrap();

        let found = search(&root, "", "GUIA").unwrap();

        assert!(!found.truncated);
        let paths: Vec<_> = found.hits.iter().map(|hit| hit.path.as_str()).collect();
        // Case-insensitive, and the path is relative to the ROOT so the shell can act on it.
        assert_eq!(
            paths,
            vec!["BACMAT/2026/guia de transporte.docx", "Recibos/guia.pdf"]
        );

        // Starting deeper searches only that subtree.
        let narrower = search(&root, "Recibos", "guia").unwrap();
        assert_eq!(narrower.hits.len(), 1);
        assert_eq!(narrower.hits[0].path, "Recibos/guia.pdf");
    }

    #[test]
    fn a_search_matches_folders_too_and_refuses_to_leave_the_root() {
        let (_guard, root) = temp_root();
        create_folder(&root, "Faturas").unwrap();

        let found = search(&root, "", "fatur").unwrap();
        assert_eq!(found.hits.len(), 1);
        assert!(found.hits[0].entry.is_dir);

        assert_eq!(search(&root, "../outside", "x"), Err(PathError::Escapes));
        // An empty query is the listing's job, and walking the tree to answer it would be waste.
        assert!(search(&root, "", "").unwrap().hits.is_empty());
    }

    #[test]
    fn a_file_can_be_resolved_for_reading_and_a_folder_cannot() {
        let (_guard, root) = temp_root();
        create_folder(&root, "BACMAT").unwrap();
        write_file(&root, "BACMAT", "guia.docx", b"conteudo").unwrap();

        assert_eq!(
            resolve_file(&root, "BACMAT/guia.docx").unwrap(),
            root.join("BACMAT").join("guia.docx")
        );
        assert_eq!(resolve_file(&root, "BACMAT"), Err(PathError::NotADirectory));
        assert_eq!(
            resolve_file(&root, "BACMAT/nao-existe"),
            Err(PathError::NotFound)
        );
        assert_eq!(resolve_file(&root, "../outside"), Err(PathError::Escapes));
    }

    /// Deleting a full folder takes two words, not one. The file inside is the reason: the mail it
    /// was filed from is gone after thirty days, so this copy is the only one.
    #[test]
    fn a_full_folder_is_refused_until_the_caller_says_recursive() {
        let (_guard, root) = temp_root();
        create_folder(&root, "BACMAT").unwrap();
        write_file(&root, "BACMAT", "guia.docx", b"x").unwrap();

        assert_eq!(delete(&root, "BACMAT", false), Err(PathError::NotEmpty));
        assert!(root.join("BACMAT").join("guia.docx").exists());

        delete(&root, "BACMAT", true).unwrap();
        assert!(!root.join("BACMAT").exists());
    }

    #[test]
    fn a_file_and_an_empty_folder_go_without_ceremony() {
        let (_guard, root) = temp_root();
        write_file(&root, "", "guia.docx", b"x").unwrap();
        create_folder(&root, "vazia").unwrap();

        delete(&root, "guia.docx", false).unwrap();
        delete(&root, "vazia", false).unwrap();

        assert!(list(&root, "").unwrap().is_empty());
    }

    /// The two deletions nothing should be able to ask for by accident.
    #[test]
    fn the_root_itself_is_not_deletable_and_neither_is_anything_outside_it() {
        let (_guard, root) = temp_root();
        write_file(&root, "", "guia.docx", b"x").unwrap();

        assert_eq!(delete(&root, "", true), Err(PathError::Unsafe));
        assert_eq!(delete(&root, ".", true), Err(PathError::Unsafe));
        assert_eq!(delete(&root, "../outside", true), Err(PathError::Escapes));
        assert_eq!(delete(&root, "nao-existe", false), Err(PathError::NotFound));
        assert!(root.join("guia.docx").exists());
    }

    #[test]
    fn something_can_be_renamed_and_moved_into_another_folder() {
        let (_guard, root) = temp_root();
        create_folder(&root, "BACMAT/2026").unwrap();
        write_file(&root, "", "guia.docx", b"conteudo").unwrap();

        move_entry(&root, "guia.docx", "BACMAT/2026/guia final.docx").unwrap();

        assert!(!root.join("guia.docx").exists());
        assert_eq!(
            std::fs::read(root.join("BACMAT").join("2026").join("guia final.docx")).unwrap(),
            b"conteudo"
        );
    }

    /// Where a rename parts company with filing: `write_file` numbers a collision because it was
    /// asked for somewhere to put a file, and this was asked for one exact name.
    #[test]
    fn a_rename_onto_a_taken_name_is_refused_rather_than_numbered() {
        let (_guard, root) = temp_root();
        write_file(&root, "", "primeiro.docx", b"um").unwrap();
        write_file(&root, "", "segundo.docx", b"dois").unwrap();

        assert_eq!(
            move_entry(&root, "primeiro.docx", "segundo.docx"),
            Err(PathError::Exists)
        );
        assert_eq!(std::fs::read(root.join("segundo.docx")).unwrap(), b"dois");
    }

    #[test]
    fn a_move_cannot_escape_the_root_or_swallow_itself() {
        let (_guard, root) = temp_root();
        create_folder(&root, "BACMAT").unwrap();
        write_file(&root, "", "guia.docx", b"x").unwrap();

        assert_eq!(
            move_entry(&root, "guia.docx", "../outside.docx"),
            Err(PathError::Escapes)
        );
        assert_eq!(
            move_entry(&root, "../outside.docx", "guia2.docx"),
            Err(PathError::Escapes)
        );
        // The loop: a folder cannot be moved inside itself.
        assert_eq!(
            move_entry(&root, "BACMAT", "BACMAT/2026"),
            Err(PathError::Unsafe)
        );
        // Nor can the root be either end of a move.
        assert_eq!(move_entry(&root, "", "BACMAT"), Err(PathError::Unsafe));
        assert_eq!(move_entry(&root, "BACMAT", ""), Err(PathError::Unsafe));
        // A destination whose folder does not exist is a typo, not a request to create it.
        assert_eq!(
            move_entry(&root, "guia.docx", "nao-existe/guia.docx"),
            Err(PathError::NotFound)
        );
        assert!(root.join("guia.docx").exists());
    }
}
