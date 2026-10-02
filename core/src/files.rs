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
//!
//! Deleting moves into a trash that lives BESIDE the root, never in it (see [`trash_for`]), so no
//! route over the root can reach what was deleted and the only way back in is [`restore`] — which
//! puts the recorded path through [`resolve_within`] like any other.

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

/// Where deleted things wait, beside the root and never inside it.
///
/// A sibling rather than a hidden folder under the root, and that choice is the whole security
/// argument for the trash: every route over the root reaches whatever [`resolve_within`] lets it,
/// so a trash inside the root would be listed, searched, downloaded and moved like anything else —
/// and a `move` OUT of it would be a restore that skipped every check [`restore`] makes. Outside the
/// root, the only doors to it are the functions below that take it by name.
pub fn trash_for(data_local_dir: &Path) -> PathBuf {
    data_local_dir.join("files-trash")
}

/// Creates the trash if it is missing, and returns it canonicalised — once, at startup, for the
/// reason [`ensure_root`] gives: the root was canonicalised the same way, and a rename between a
/// resolved path and an unresolved one is how the two stop agreeing about where they are.
pub fn ensure_trash(data_local_dir: &Path) -> std::io::Result<PathBuf> {
    let trash = trash_for(data_local_dir);
    std::fs::create_dir_all(&trash)?;
    std::fs::canonicalize(&trash)
}

/// How long something deleted can still be brought back.
///
/// Thirty days because it is the window a person already knows: the mail this folder files
/// attachments from expires on the same clock, and every mainstream "recently deleted" uses it, so
/// nobody has to learn a second number. Past it an entry is removed for good — at startup and on
/// every delete, which bounds the trash by the rate things are deleted rather than by a timer the
/// daemon would have to keep alive.
pub const TRASH_RETENTION: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);

/// What a delete put in the trash — the answer to "what can I undo", and one row of the "Recently
/// deleted" list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Trashed {
    /// The handle a restore names. Opaque to the caller, and checked character by character before
    /// it goes anywhere near a path — see [`valid_trash_id`].
    pub id: String,
    /// Where it was, relative to the root with forward slashes: the path a restore puts it back at.
    pub path: String,
    pub is_dir: bool,
    /// For a folder, everything under it — the one number a person weighing "do I want this back"
    /// cannot see any other way. A lower bound past [`SEARCH_VISITS`] entries, where the walk stops.
    pub size_bytes: i64,
    /// RFC 3339 UTC, to the millisecond.
    pub deleted_at: String,
}

/// The record kept beside each trashed item, as `origin.json`. [`Trashed`] minus the id, because
/// the id is the entry's own directory name and a second copy of it could only disagree with it.
#[derive(serde::Serialize, serde::Deserialize)]
struct Origin {
    path: String,
    deleted_at: String,
    is_dir: bool,
    size_bytes: i64,
}

/// The record's name inside an entry.
const ORIGIN_FILE: &str = "origin.json";

/// The folder the item itself goes into, inside its entry.
///
/// A subfolder and not the entry itself, because a person can delete a file called `origin.json`:
/// placed beside the record under its own name it would overwrite it, or be overwritten by it. Kept
/// under its ORIGINAL name inside this folder rather than renamed to something opaque, so that a
/// person looking at the trash by hand — the recovery of last resort — sees what each entry is.
const ITEM_DIR: &str = "item";

/// `yyyymmddThhmmssmmm`, then `-` and eight hex digits: 27 characters, all of them ASCII, all of
/// them legal in a filename everywhere, and fixed-width so that sorting ids sorts time.
const TRASH_ID_STAMP: usize = 18;
const TRASH_ID_LEN: usize = TRASH_ID_STAMP + 1 + 8;

/// Makes the suffix of an id unique within a millisecond in this process. The directory is created
/// with `create_dir`, which fails rather than reuses, so this counter is what makes a collision
/// rare and the filesystem is what makes one harmless — two daemons, or a counter that wrapped,
/// simply try the next number.
static TRASH_SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Whether `id` is one this module could have made. Strict on purpose: it is the only thing a
/// restore joins onto the trash path, and a whitelist of the exact shape is the rule
/// [`resolve_within`] applies to paths — anything that is not certainly an id is refused, so `..`,
/// a separator or a drive letter never reaches a `join`.
fn valid_trash_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == TRASH_ID_LEN
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..TRASH_ID_STAMP].iter().all(u8::is_ascii_digit)
        && bytes[TRASH_ID_STAMP] == b'-'
        && bytes[TRASH_ID_STAMP + 1..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

/// When an entry was made, read off its NAME rather than its record: the name is the one thing a
/// half-written entry is guaranteed to have, so dating by it is what lets a purge clear an entry
/// whose record never landed instead of keeping it forever.
fn trash_id_time(id: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if !valid_trash_id(id) {
        return None;
    }
    chrono::NaiveDateTime::parse_from_str(&id[..TRASH_ID_STAMP], "%Y%m%dT%H%M%S%3f")
        .ok()
        .map(|naive| naive.and_utc())
}

/// Creates a fresh, empty entry in the trash and returns its id and path.
fn new_trash_entry(
    trash: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(String, PathBuf), PathError> {
    let stamp = now.format("%Y%m%dT%H%M%S%3f").to_string();
    // Bounded, because the only way to exhaust it is a trash where every attempt collides, and
    // that should surface as the error it is rather than as a loop.
    let mut last_error = None;
    for _ in 0..16 {
        let sequence = TRASH_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("{stamp}-{sequence:08x}");
        let entry = trash.join(&id);
        match std::fs::create_dir(&entry) {
            Ok(()) => return Ok((id, entry)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_error = Some(error);
            }
            Err(error) => return Err(PathError::Io(error.to_string())),
        }
    }
    Err(PathError::Io(last_error.map_or_else(
        || "no free trash id".to_string(),
        |e| e.to_string(),
    )))
}

/// How many bytes a trashed item holds. A folder is walked — never through a symlink, for the
/// cycle [`search`] names — and the walk is capped like a search's, because it runs on the delete
/// path and a folder of a million files should not make "delete" slow to answer.
fn size_of(target: &Path, metadata: &std::fs::Metadata) -> i64 {
    if metadata.is_symlink() {
        return 0;
    }
    if !metadata.is_dir() {
        return metadata.len() as i64;
    }
    let mut total = 0i64;
    let mut visits = 0usize;
    let mut pending = vec![target.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(reader) = std::fs::read_dir(&directory) else {
            continue;
        };
        for item in reader.flatten() {
            visits += 1;
            if visits > SEARCH_VISITS {
                return total;
            }
            let Ok(metadata) = std::fs::symlink_metadata(item.path()) else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(item.path());
            } else if !metadata.is_symlink() {
                total = total.saturating_add(metadata.len() as i64);
            }
        }
    }
    total
}

/// Moves a file, or a directory the caller has said out loud it wants gone with its contents, into
/// the trash — and says what it moved, so the caller can offer to undo it.
///
/// It used to remove, and nothing could bring a mistake back: this folder holds attachments that
/// exist nowhere else once the mail body they came from has expired. A rename into a sibling
/// directory costs the same as a removal and buys [`TRASH_RETENTION`] of second chances.
///
/// The two-step for a full directory stays exactly as it was, although the step it guarded is no
/// longer irreversible. It was never only for a person: an agent that names the wrong folder should
/// still have to repeat itself before a whole subtree leaves the place every other agent looks.
///
/// **A failed move is an error, never a removal.** `rename` fails across volumes, on a file another
/// process holds open, on a trash that went missing — and in every one of those the thing asked for
/// stays exactly where it was. Falling back to deleting would turn "the trash is unavailable" into
/// the irreversible act this function exists to replace, and it would do so precisely when nobody
/// is looking. A symlink is moved as the link it is: `rename` never follows one.
///
/// The record is written BEFORE the item moves, so there is never a trashed item without a way
/// back. The opposite half-state — a record with no item, when the move fails — is cleaned up here,
/// and if even that fails the entry is invisible to [`list_trash`] and dated by its name for
/// [`purge`].
pub fn delete(
    root: &Path,
    trash: &Path,
    relative: &str,
    recursive: bool,
) -> Result<Trashed, PathError> {
    let target = resolve_within(root, relative)?;
    if target == root {
        // Emptying the root is not something a wrong path should be able to ask for by accident.
        return Err(PathError::Unsafe);
    }
    // Not followed: a symlink is trashed as the link, and its size is not its target's.
    let metadata = std::fs::symlink_metadata(&target).map_err(|_| PathError::NotFound)?;
    if metadata.is_dir() {
        let empty = std::fs::read_dir(&target)
            .map_err(|e| PathError::Io(e.to_string()))?
            .next()
            .is_none();
        if !empty && !recursive {
            return Err(PathError::NotEmpty);
        }
    }
    let Some(name) = target.file_name().map(std::ffi::OsStr::to_os_string) else {
        return Err(PathError::Unsafe);
    };
    // Recorded as the root-relative path the routes speak, with forward slashes on every platform,
    // so the record means the same thing to a restore on this machine and to a person reading it.
    let path = target
        .strip_prefix(root)
        .map_err(|_| PathError::Escapes)?
        .to_string_lossy()
        .replace('\\', "/");

    let now = chrono::Utc::now();
    let origin = Origin {
        path,
        deleted_at: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        is_dir: metadata.is_dir(),
        size_bytes: size_of(&target, &metadata),
    };

    let (id, entry) = new_trash_entry(trash, now)?;
    let moved = (|| {
        let record = serde_json::to_vec_pretty(&origin).map_err(std::io::Error::other)?;
        std::fs::write(entry.join(ORIGIN_FILE), record)?;
        std::fs::create_dir(entry.join(ITEM_DIR))?;
        std::fs::rename(&target, entry.join(ITEM_DIR).join(&name))
    })();
    if let Err(error) = moved {
        // Only what this call created is removed — a record and two empty directories — and each
        // with the call that refuses anything else, so a mistake here can never reach the item.
        let _ = std::fs::remove_dir(entry.join(ITEM_DIR));
        let _ = std::fs::remove_file(entry.join(ORIGIN_FILE));
        let _ = std::fs::remove_dir(&entry);
        return Err(PathError::Io(error.to_string()));
    }

    // After the move and never instead of it: the delete that was asked for has already happened,
    // and an old entry that would not go is a disk-space question, not this caller's failure.
    if let Err(error) = purge(trash, TRASH_RETENTION) {
        tracing::warn!(%error, trash = %trash.display(), "could not clear expired entries from the files trash");
    }

    Ok(Trashed {
        id,
        path: origin.path,
        is_dir: origin.is_dir,
        size_bytes: origin.size_bytes,
        deleted_at: origin.deleted_at,
    })
}

/// Reads one entry back, or `None` for anything that is not a whole one: a name this module did
/// not make, a record that will not parse, or a record whose item is gone (a move that failed and
/// could not be cleaned up, or a restore that could not clear its leftovers).
fn read_trash_entry(trash: &Path, id: &str) -> Option<(Origin, PathBuf)> {
    if !valid_trash_id(id) {
        return None;
    }
    let entry = trash.join(id);
    let record = std::fs::read(entry.join(ORIGIN_FILE)).ok()?;
    let origin: Origin = serde_json::from_slice(&record).ok()?;
    let name = Path::new(&origin.path).file_name()?.to_os_string();
    let item = entry.join(ITEM_DIR).join(name);
    std::fs::symlink_metadata(&item).ok()?;
    Some((origin, item))
}

/// What is in the trash, newest first.
///
/// An entry that is not whole is skipped rather than failing the list: one bad record must not hide
/// every good one, which is the whole list a person came here to use. Newest first by id, which
/// sorts as time because its stamp is fixed-width and leads.
pub fn list_trash(trash: &Path) -> Result<Vec<Trashed>, PathError> {
    let mut items = Vec::new();
    for item in std::fs::read_dir(trash).map_err(|e| PathError::Io(e.to_string()))? {
        let Ok(item) = item else { continue };
        let Some(id) = item.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some((origin, _)) = read_trash_entry(trash, &id) else {
            continue;
        };
        items.push(Trashed {
            id,
            path: origin.path,
            is_dir: origin.is_dir,
            size_bytes: origin.size_bytes,
            deleted_at: origin.deleted_at,
        });
    }
    items.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(items)
}

/// Puts a trashed item back where it was, and returns that path relative to the root.
///
/// The recorded path is NOT trusted because this module wrote it: it goes through
/// [`resolve_within`] like every other path, because the trash is a directory on disk that anything
/// with the user's permissions can edit, and a restore is a write into the root. One gate, still.
///
/// A taken destination is refused rather than numbered, for the reason [`move_entry`] gives — the
/// person asked for THIS thing back at THIS path, and a `guia (2).docx` they did not ask for is how
/// the caller and the disk come to disagree. A missing parent folder is refused rather than
/// created, also for `move_entry`'s reason: a silently recreated parent is how a file ends up
/// somewhere nobody looks. Either way the item stays in the trash, and the caller can make room or
/// recreate the folder and try again.
pub fn restore(root: &Path, trash: &Path, id: &str) -> Result<String, PathError> {
    if !valid_trash_id(id) {
        return Err(PathError::Unsafe);
    }
    let (origin, item) = read_trash_entry(trash, id).ok_or(PathError::NotFound)?;
    let destination = resolve_within(root, &origin.path)?;
    if destination == root {
        return Err(PathError::Unsafe);
    }
    // `symlink_metadata` and not `exists`: a dangling link at the destination is still a name that
    // is taken, and `exists` would follow it and say there is nothing there.
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(PathError::Exists);
    }
    match destination.parent() {
        Some(parent) if parent.is_dir() => {}
        _ => return Err(PathError::NotFound),
    }
    std::fs::rename(&item, &destination).map_err(|e| PathError::Io(e.to_string()))?;

    // The item is home; what is left is a record and an empty folder. Removed with the calls that
    // refuse anything non-empty, and a failure is logged rather than returned — the restore that
    // was asked for has happened, and the leftover is invisible to `list_trash` and dated for
    // `purge`.
    let entry = trash.join(id);
    let cleaned = std::fs::remove_dir(entry.join(ITEM_DIR))
        .and_then(|()| std::fs::remove_file(entry.join(ORIGIN_FILE)))
        .and_then(|()| std::fs::remove_dir(&entry));
    if let Err(error) = cleaned {
        tracing::warn!(%error, entry = %entry.display(), "restored, but could not clear the trash entry");
    }
    Ok(origin.path)
}

/// Removes, for good, every entry older than `max_age`, and says how many went.
///
/// Only names this module could have made are touched — anything else in the directory was put
/// there by somebody else, and this function has no business guessing what. One entry that will
/// not go (a file held open, say) is logged and skipped rather than stopping the rest; the error
/// returned is only for a trash that cannot be read at all. `remove_dir_all` does not follow
/// symlinks, so a trashed link to a folder takes the link and never the folder it points at.
pub fn purge(trash: &Path, max_age: std::time::Duration) -> std::io::Result<usize> {
    let Ok(max_age) = chrono::Duration::from_std(max_age) else {
        // An age too large to represent keeps everything, which is the safe way to be wrong.
        return Ok(0);
    };
    let cutoff = chrono::Utc::now() - max_age;
    let mut removed = 0;
    for item in std::fs::read_dir(trash)? {
        let Ok(item) = item else { continue };
        let Some(id) = item.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(made) = trash_id_time(&id) else {
            continue;
        };
        if made >= cutoff {
            continue;
        }
        match std::fs::remove_dir_all(item.path()) {
            Ok(()) => removed += 1,
            Err(error) => {
                tracing::warn!(%error, %id, "could not remove an expired files trash entry");
            }
        }
    }
    Ok(removed)
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

    /// A root and its trash, made the way startup makes them: siblings under one data directory.
    fn temp_root_and_trash() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = ensure_root(dir.path()).unwrap();
        let trash = ensure_trash(dir.path()).unwrap();
        (dir, root, trash)
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
            "/etc/passwd",
        ] {
            assert_eq!(
                resolve_within(&root, escape),
                Err(PathError::Escapes),
                "accepted {escape:?}"
            );
        }

        #[cfg(windows)]
        for escape in [
            r"..\outside",
            r"sub\..\..\outside",
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

    #[cfg(unix)]
    #[test]
    fn windows_spellings_are_unsafe_names_on_unix() {
        let (_guard, root) = temp_root();

        // On Unix `\\` and `:` are filename characters, so these name a file inside the root,
        // and `email::safe_filename` refuses them as unsafe names.
        for unsafe_name in [
            r"..\outside",
            r"sub\..\..\outside",
            r"C:\Windows\System32",
            r"\\server\share",
            "C:outside",
        ] {
            assert_eq!(
                resolve_within(&root, unsafe_name),
                Err(PathError::Unsafe),
                "accepted {unsafe_name:?}"
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
        let (_guard, root, trash) = temp_root_and_trash();
        create_folder(&root, "BACMAT").unwrap();
        write_file(&root, "BACMAT", "guia.docx", b"x").unwrap();

        assert_eq!(
            delete(&root, &trash, "BACMAT", false),
            Err(PathError::NotEmpty)
        );
        assert!(root.join("BACMAT").join("guia.docx").exists());
        assert!(
            list_trash(&trash).unwrap().is_empty(),
            "a refused delete still made an entry"
        );

        delete(&root, &trash, "BACMAT", true).unwrap();
        assert!(!root.join("BACMAT").exists());
    }

    #[test]
    fn a_file_and_an_empty_folder_go_without_ceremony() {
        let (_guard, root, trash) = temp_root_and_trash();
        write_file(&root, "", "guia.docx", b"x").unwrap();
        create_folder(&root, "vazia").unwrap();

        delete(&root, &trash, "guia.docx", false).unwrap();
        delete(&root, &trash, "vazia", false).unwrap();

        assert!(list(&root, "").unwrap().is_empty());
    }

    /// The two deletions nothing should be able to ask for by accident.
    #[test]
    fn the_root_itself_is_not_deletable_and_neither_is_anything_outside_it() {
        let (_guard, root, trash) = temp_root_and_trash();
        write_file(&root, "", "guia.docx", b"x").unwrap();

        assert_eq!(delete(&root, &trash, "", true), Err(PathError::Unsafe));
        assert_eq!(delete(&root, &trash, ".", true), Err(PathError::Unsafe));
        assert_eq!(
            delete(&root, &trash, "../outside", true),
            Err(PathError::Escapes)
        );
        assert_eq!(
            delete(&root, &trash, "nao-existe", false),
            Err(PathError::NotFound)
        );
        assert!(root.join("guia.docx").exists());
    }

    /// Deleting is a move, not a removal: the root no longer has it, the trash does, and the answer
    /// says enough to undo it.
    #[test]
    fn a_delete_moves_the_item_into_the_trash_and_leaves_nothing_in_the_root() {
        let (_guard, root, trash) = temp_root_and_trash();
        create_folder(&root, "BACMAT").unwrap();
        write_file(&root, "BACMAT", "guia.docx", b"conteudo").unwrap();

        let trashed = delete(&root, &trash, "BACMAT/guia.docx", false).unwrap();

        assert!(!root.join("BACMAT").join("guia.docx").exists());
        assert_eq!(trashed.path, "BACMAT/guia.docx");
        assert!(!trashed.is_dir);
        assert_eq!(trashed.size_bytes, 8);
        assert!(valid_trash_id(&trashed.id), "{:?}", trashed.id);
        assert!(trashed.deleted_at.ends_with('Z'), "{}", trashed.deleted_at);
        assert_eq!(
            std::fs::read(trash.join(&trashed.id).join(ITEM_DIR).join("guia.docx")).unwrap(),
            b"conteudo"
        );
        // Nothing of the trash is reachable through the root: it is a sibling, not a child.
        assert!(!trash.starts_with(&root));
        assert!(search(&root, "", "guia").unwrap().hits.is_empty());
        assert_eq!(list_trash(&trash).unwrap(), vec![trashed]);
    }

    /// A deleted file that happens to be called `origin.json` must not collide with the record kept
    /// beside it — which is why the item lives one folder down.
    #[test]
    fn a_file_named_like_the_record_survives_the_round_trip() {
        let (_guard, root, trash) = temp_root_and_trash();
        write_file(&root, "", "origin.json", b"meu").unwrap();

        let trashed = delete(&root, &trash, "origin.json", false).unwrap();
        assert_eq!(restore(&root, &trash, &trashed.id).unwrap(), "origin.json");

        assert_eq!(std::fs::read(root.join("origin.json")).unwrap(), b"meu");
    }

    /// Two deletes in the same millisecond are two entries, never one overwriting the other.
    #[test]
    fn deletes_in_quick_succession_get_distinct_ids() {
        let (_guard, root, trash) = temp_root_and_trash();
        let mut ids = std::collections::HashSet::new();
        for index in 0..20 {
            let name = format!("f{index}.txt");
            write_file(&root, "", &name, b"x").unwrap();
            assert!(ids.insert(delete(&root, &trash, &name, false).unwrap().id));
        }
        assert_eq!(list_trash(&trash).unwrap().len(), 20);
    }

    #[test]
    fn the_trash_lists_newest_first_and_skips_what_is_not_whole() {
        let (_guard, root, trash) = temp_root_and_trash();
        for name in ["primeiro.txt", "segundo.txt", "terceiro.txt"] {
            write_file(&root, "", name, b"x").unwrap();
            delete(&root, &trash, name, false).unwrap();
            // Past a millisecond, so the order under test is time and not the counter.
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        // Three kinds of debris a real trash can hold, none of which may break the list: a name
        // this module did not make, an entry with no record, and a record that will not parse.
        std::fs::create_dir(trash.join("nao-e-um-id")).unwrap();
        std::fs::create_dir(trash.join("20990101T000000000-0000abcd")).unwrap();
        let garbled = trash.join("20990102T000000000-0000abce");
        std::fs::create_dir(&garbled).unwrap();
        std::fs::write(garbled.join(ORIGIN_FILE), b"{not json").unwrap();

        let paths: Vec<_> = list_trash(&trash)
            .unwrap()
            .into_iter()
            .map(|item| item.path)
            .collect();
        assert_eq!(paths, vec!["terceiro.txt", "segundo.txt", "primeiro.txt"]);
    }

    #[test]
    fn a_file_comes_back_from_the_trash_to_where_it_was() {
        let (_guard, root, trash) = temp_root_and_trash();
        create_folder(&root, "BACMAT").unwrap();
        write_file(&root, "BACMAT", "guia.docx", b"conteudo").unwrap();
        let trashed = delete(&root, &trash, "BACMAT/guia.docx", false).unwrap();

        assert_eq!(
            restore(&root, &trash, &trashed.id).unwrap(),
            "BACMAT/guia.docx"
        );

        assert_eq!(
            std::fs::read(root.join("BACMAT").join("guia.docx")).unwrap(),
            b"conteudo"
        );
        assert!(list_trash(&trash).unwrap().is_empty());
        assert!(
            !trash.join(&trashed.id).exists(),
            "the emptied entry was left behind"
        );
    }

    #[test]
    fn a_full_folder_comes_back_whole() {
        let (_guard, root, trash) = temp_root_and_trash();
        create_folder(&root, "BACMAT/2026").unwrap();
        write_file(&root, "BACMAT", "guia.docx", b"abc").unwrap();
        write_file(&root, "BACMAT/2026", "recibo.pdf", b"defgh").unwrap();

        let trashed = delete(&root, &trash, "BACMAT", true).unwrap();
        assert!(trashed.is_dir);
        assert_eq!(trashed.size_bytes, 8, "a folder's size is what is under it");
        assert!(!root.join("BACMAT").exists());

        assert_eq!(restore(&root, &trash, &trashed.id).unwrap(), "BACMAT");
        assert_eq!(
            std::fs::read(root.join("BACMAT").join("2026").join("recibo.pdf")).unwrap(),
            b"defgh"
        );
        assert_eq!(
            std::fs::read(root.join("BACMAT").join("guia.docx")).unwrap(),
            b"abc"
        );
    }

    /// A restore asks for one exact place, as a rename does: a taken name is refused, not numbered
    /// and not overwritten, and the item stays in the trash to be asked for again.
    #[test]
    fn a_restore_refuses_a_taken_name() {
        let (_guard, root, trash) = temp_root_and_trash();
        write_file(&root, "", "guia.docx", b"antigo").unwrap();
        let trashed = delete(&root, &trash, "guia.docx", false).unwrap();
        write_file(&root, "", "guia.docx", b"novo").unwrap();

        assert_eq!(restore(&root, &trash, &trashed.id), Err(PathError::Exists));

        assert_eq!(std::fs::read(root.join("guia.docx")).unwrap(), b"novo");
        assert_eq!(list_trash(&trash).unwrap(), vec![trashed]);
    }

    /// The folder it came from is gone, and is not silently recreated — the reason `move_entry`
    /// gives: a recreated parent is how a file ends up somewhere nobody looks.
    #[test]
    fn a_restore_refuses_a_folder_that_is_no_longer_there() {
        let (_guard, root, trash) = temp_root_and_trash();
        create_folder(&root, "Pasta").unwrap();
        write_file(&root, "Pasta", "dentro.txt", b"y").unwrap();
        let inner = delete(&root, &trash, "Pasta/dentro.txt", false).unwrap();
        delete(&root, &trash, "Pasta", false).unwrap();

        assert_eq!(restore(&root, &trash, &inner.id), Err(PathError::NotFound));

        assert!(!root.join("Pasta").exists());
        assert!(list_trash(&trash).unwrap().contains(&inner));
    }

    /// The id is the only thing from the request that is joined onto the trash path, so everything
    /// that is not certainly an id is refused before it reaches one.
    #[test]
    fn a_restore_refuses_anything_that_is_not_an_id() {
        let (_guard, root, trash) = temp_root_and_trash();
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            r"C:\x",
            "../files",
            "20260101T000000000-0000abcd/../..",
            "20260101T000000000-0000ABCD",
            "20260101T000000000-0000abc",
            "20260101T000000000-0000abcd ",
        ] {
            assert_eq!(
                restore(&root, &trash, bad),
                Err(PathError::Unsafe),
                "accepted {bad:?}"
            );
        }
        // Well formed but not there.
        assert_eq!(
            restore(&root, &trash, "20260101T000000000-0000abcd"),
            Err(PathError::NotFound)
        );
    }

    /// The record is on disk where anything can edit it, so the path it names goes through the same
    /// gate as a path from a request.
    #[test]
    fn a_tampered_record_cannot_restore_outside_the_root() {
        let (_guard, root, trash) = temp_root_and_trash();
        write_file(&root, "", "guia.docx", b"x").unwrap();
        let trashed = delete(&root, &trash, "guia.docx", false).unwrap();
        let record = trash.join(&trashed.id).join(ORIGIN_FILE);
        let text = std::fs::read_to_string(&record)
            .unwrap()
            .replace("\"guia.docx\"", "\"../guia.docx\"");
        std::fs::write(&record, text).unwrap();

        assert_eq!(restore(&root, &trash, &trashed.id), Err(PathError::Escapes));
        assert!(!root.parent().unwrap().join("guia.docx").exists());
    }

    #[test]
    fn a_purge_removes_only_what_has_expired() {
        let (_guard, root, trash) = temp_root_and_trash();
        write_file(&root, "", "recente.txt", b"x").unwrap();
        let recent = delete(&root, &trash, "recente.txt", false).unwrap();
        // An entry from years ago with a tree in it, and one with no record at all — dated by its
        // name, which is what lets a half-written entry expire instead of staying forever.
        let old = trash.join("20200101T000000000-00000001");
        std::fs::create_dir_all(old.join(ITEM_DIR).join("velho")).unwrap();
        std::fs::write(old.join(ITEM_DIR).join("velho").join("a.txt"), b"x").unwrap();
        let bare = trash.join("20200102T000000000-00000002");
        std::fs::create_dir(&bare).unwrap();
        // Something this module did not make is never touched, however old it looks.
        std::fs::create_dir(trash.join("de-outra-pessoa")).unwrap();

        assert_eq!(purge(&trash, TRASH_RETENTION).unwrap(), 2);

        assert!(!old.exists());
        assert!(!bare.exists());
        assert!(trash.join(&recent.id).exists());
        assert!(trash.join("de-outra-pessoa").exists());
    }

    /// Every delete sweeps the trash as it goes, so nothing past retention outlives the next one.
    #[test]
    fn a_delete_sweeps_expired_entries() {
        let (_guard, root, trash) = temp_root_and_trash();
        let old = trash.join("20200101T000000000-00000001");
        std::fs::create_dir(&old).unwrap();
        write_file(&root, "", "novo.txt", b"x").unwrap();

        delete(&root, &trash, "novo.txt", false).unwrap();

        assert!(!old.exists());
    }

    /// When the trash cannot take the item, the item stays exactly where it was. The fallback this
    /// refuses — "could not move it, so remove it" — is the irreversible act the trash replaced.
    #[test]
    fn a_trash_that_cannot_take_the_item_leaves_it_where_it_was() {
        let (_guard, root, trash) = temp_root_and_trash();
        write_file(&root, "", "guia.docx", b"conteudo").unwrap();
        let missing = trash.join("apagada");

        assert!(matches!(
            delete(&root, &missing, "guia.docx", false),
            Err(PathError::Io(_))
        ));
        assert_eq!(std::fs::read(root.join("guia.docx")).unwrap(), b"conteudo");
    }

    /// The same at the rename itself: a file another process holds open without sharing delete
    /// cannot be renamed on Windows, and the half-made entry is cleaned up behind the refusal.
    #[cfg(windows)]
    #[test]
    fn a_failed_rename_leaves_the_item_and_no_entry() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_guard, root, trash) = temp_root_and_trash();
        write_file(&root, "", "aberto.docx", b"conteudo").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(root.join("aberto.docx"))
            .unwrap();

        let refused = delete(&root, &trash, "aberto.docx", false);
        drop(held);

        assert!(matches!(refused, Err(PathError::Io(_))), "{refused:?}");
        assert_eq!(
            std::fs::read(root.join("aberto.docx")).unwrap(),
            b"conteudo"
        );
        assert_eq!(
            std::fs::read_dir(&trash).unwrap().count(),
            0,
            "the half-made entry was left in the trash"
        );
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
