//! What a person drags onto the window: which files it means, and permission to read them.
//!
//! This module exists because a webview cannot open a file it was dropped. The browser's own
//! drag-and-drop never fires here — Tauri intercepts the OS drop so it can hand over real paths —
//! and a path is not something JavaScript can read. So the split is: this side resolves the drop
//! into a list of files and hands the page a manifest; the page asks for one file's bytes at a time
//! and uploads them the same way the file picker does.
//!
//! **The allowed set is the point of the module.** These commands read absolute paths chosen by the
//! frontend, and a page that could name any path could read anything this user can. So a path is
//! readable only after the OS told US it was dropped on our window: the set is written by the drag
//! event and consulted by the command, and nothing the page says can add to it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One file a drop resolved to.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DroppedFile {
    /// Absolute, on this machine. Handed back to [`read_dropped`] and to nothing else.
    pub path: String,
    /// Where it goes UNDER the folder being viewed, so a dropped tree keeps its shape. Empty for a
    /// file dropped on its own.
    pub folder: String,
    pub name: String,
    pub size: u64,
}

/// What one drop amounts to.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Dropped {
    pub files: Vec<DroppedFile>,
    /// True when a ceiling cut the walk short — said out loud, because a drop that silently
    /// uploads 500 of 900 files looks exactly like one that uploaded everything.
    pub truncated: bool,
}

/// The paths this process will read, because the OS said they were dropped on our window.
#[derive(Debug, Default)]
pub struct Allowed(Mutex<HashSet<PathBuf>>);

impl Allowed {
    /// Replaces the set: only the most recent drop is readable.
    ///
    /// Replacing rather than accumulating keeps the window of what this process would open as short
    /// as the gesture that opened it, and stops an hour of dropping from leaving a list of
    /// everything the user ever dragged here.
    fn remember(&self, files: &[DroppedFile]) {
        let mut allowed = self
            .0
            .lock()
            .expect("the dropped-path set is never poisoned");
        allowed.clear();
        allowed.extend(files.iter().map(|file| PathBuf::from(&file.path)));
    }

    fn permits(&self, path: &Path) -> bool {
        self.0
            .lock()
            .expect("the dropped-path set is never poisoned")
            .contains(path)
    }
}

/// The most files one drop can carry, and the most entries the walk will look at to find them.
const MAX_FILES: usize = 500;
const MAX_VISITS: usize = 20_000;

/// The largest file this will read into memory.
///
/// The counterpart of `files::MAX_UPLOAD_BYTES` in the daemon, repeated here rather than shared
/// because these are two crates that do not depend on each other. The daemon would refuse a bigger
/// upload anyway; refusing it before reading means not spending 200 MB of memory to be told so.
pub const MAX_READ_BYTES: u64 = 100 * 1024 * 1024;

/// Turns the paths an OS drop carried into the files they mean.
///
/// PURE apart from reading the filesystem: no state, no window, no upload. A dropped directory is
/// walked so its shape survives the trip — `Relatórios\2026\guia.docx` arrives wanting the folder
/// `Relatórios/2026`, which is what makes dropping a folder do what dropping a folder looks like it
/// should do.
pub fn resolve(paths: &[PathBuf]) -> Dropped {
    let mut files = Vec::new();
    let mut visits = 0usize;

    for path in paths {
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            continue;
        };
        // A dropped symlink is followed no further than its own metadata: what it points at was not
        // what the person dragged.
        if metadata.is_file() {
            if let Some(file) = describe(path, "", metadata.len()) {
                files.push(file);
            }
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }

        let base = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut pending = vec![(path.clone(), base)];
        while let Some((directory, under)) = pending.pop() {
            let Ok(reader) = std::fs::read_dir(&directory) else {
                continue;
            };
            for item in reader.flatten() {
                visits += 1;
                if visits > MAX_VISITS || files.len() >= MAX_FILES {
                    return Dropped {
                        files,
                        truncated: true,
                    };
                }
                let Ok(entry_meta) = item.metadata() else {
                    continue;
                };
                let name = item.file_name().to_string_lossy().into_owned();
                if entry_meta.is_dir() {
                    let deeper = if under.is_empty() {
                        name
                    } else {
                        format!("{under}/{name}")
                    };
                    pending.push((item.path(), deeper));
                } else if entry_meta.is_file() {
                    if let Some(file) = describe(&item.path(), &under, entry_meta.len()) {
                        files.push(file);
                    }
                }
            }
        }
    }

    Dropped {
        files,
        truncated: false,
    }
}

fn describe(path: &Path, folder: &str, size: u64) -> Option<DroppedFile> {
    Some(DroppedFile {
        path: path.to_str()?.to_owned(),
        folder: folder.to_owned(),
        name: path.file_name()?.to_str()?.to_owned(),
        size,
    })
}

/// Records what the OS says was dropped, and answers with the manifest for the page.
pub fn accept(allowed: &Allowed, paths: &[PathBuf]) -> Dropped {
    let dropped = resolve(paths);
    allowed.remember(&dropped.files);
    dropped
}

/// One dropped file's bytes, for the page to upload.
///
/// Raw bytes rather than a JSON array or base64: `tauri::ipc::Response` travels as binary, and the
/// alternatives would turn a 40 MB document into a 55 MB string on the way to a `fetch` that wants
/// bytes back anyway.
#[tauri::command]
pub fn read_dropped(
    path: String,
    allowed: tauri::State<'_, Allowed>,
) -> Result<tauri::ipc::Response, String> {
    let path = PathBuf::from(path);
    if !allowed.permits(&path) {
        // Deliberately the same answer as a file that is not there: which of the two it is would
        // tell a caller whether a path exists, and this command is the wrong place to ask.
        return Err("that file was not dropped on this window".to_owned());
    }
    let metadata = std::fs::metadata(&path).map_err(|_| "could not read that file".to_owned())?;
    if metadata.len() > MAX_READ_BYTES {
        return Err(format!(
            "that file is larger than the {} MB the daemon will take in one upload",
            MAX_READ_BYTES / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(&path).map_err(|_| "could not read that file".to_owned())?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_dropped_file_arrives_with_no_folder_and_a_dropped_tree_keeps_its_shape() {
        let temp = tempfile::tempdir().unwrap();
        write(&temp.path().join("solto.txt"), b"x");
        write(&temp.path().join("Relatorios/2026/guia.docx"), b"xy");

        let dropped = resolve(&[
            temp.path().join("solto.txt"),
            temp.path().join("Relatorios"),
        ]);

        assert!(!dropped.truncated);
        let mut shape: Vec<_> = dropped
            .files
            .iter()
            .map(|file| (file.folder.as_str(), file.name.as_str(), file.size))
            .collect();
        shape.sort_unstable();
        assert_eq!(
            shape,
            vec![("", "solto.txt", 1), ("Relatorios/2026", "guia.docx", 2),]
        );
    }

    /// The set is what stands between "the page asks for a file it was given" and "the page asks
    /// for anything on this disk".
    #[test]
    fn only_a_path_the_os_dropped_is_readable_and_only_the_last_drop() {
        let temp = tempfile::tempdir().unwrap();
        write(&temp.path().join("primeiro.txt"), b"um");
        write(&temp.path().join("segundo.txt"), b"dois");
        let allowed = Allowed::default();

        accept(&allowed, &[temp.path().join("primeiro.txt")]);
        assert!(allowed.permits(&temp.path().join("primeiro.txt")));
        assert!(!allowed.permits(&temp.path().join("segundo.txt")));
        // Nothing on this machine is readable by asking nicely.
        assert!(!allowed.permits(Path::new("C:/Windows/System32/config/SAM")));

        accept(&allowed, &[temp.path().join("segundo.txt")]);
        assert!(
            !allowed.permits(&temp.path().join("primeiro.txt")),
            "a new drop must retire the previous one rather than pile on"
        );
    }

    #[test]
    fn a_drop_that_hits_the_ceiling_says_so() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..(MAX_FILES + 10) {
            write(
                &temp.path().join("muitos").join(format!("{index}.txt")),
                b"x",
            );
        }

        let dropped = resolve(&[temp.path().join("muitos")]);

        assert!(dropped.truncated, "a cut-short walk must not look complete");
        assert!(dropped.files.len() <= MAX_FILES);
    }
}
