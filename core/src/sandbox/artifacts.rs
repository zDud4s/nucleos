//! The owner's side of the D12 crossing for the files an agent's job directory holds.
//!
//! Every reader of `.nucleos/*.json`, `spec.md`, `council.md`, a session transcript or a document
//! in the files root goes through [`read_in`] / [`write_in`] instead of `std::fs`, so a link the
//! unit planted in its own tree cannot make the owner's account read or overwrite something else.
//! Nothing here arms the sandbox: a legitimate plain file reads exactly as it did before.

use std::path::{Path, PathBuf};

use super::{CrossingError, Untrusted, read_crossing, write_crossing};

/// Ceiling for one artifact (a plan, a spec, a council synthesis, a map file).
pub const ARTIFACT_CAP: u64 = 8 * 1024 * 1024;

/// Ceiling for one session transcript. Transcripts are the largest thing the core reads; a file
/// past this is refused instead of pulled into memory whole.
pub const TRANSCRIPT_CAP: u64 = 256 * 1024 * 1024;

/// Reads `path` through the crossing with `root` as the grant. A file that is not there is
/// `Ok(None)`; every other refusal is an error the caller must not turn into "absent".
pub fn read_in(path: &Path, root: &Path, max: u64) -> Result<Option<Untrusted>, CrossingError> {
    match read_crossing(path, &[root.to_path_buf()], max) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(CrossingError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Writes `bytes` to `path` through the crossing, `root` being the only place it may land.
pub fn write_in(path: &Path, root: &Path, bytes: &[u8]) -> Result<(), CrossingError> {
    write_crossing(path, &[PathBuf::from(root)], bytes)
}

/// The first script a command names that the worktree's copy cannot be trusted to equal the
/// project root's, with the reason; `None` when every one is byte-identical.
///
/// The worktree copy is read through the crossing (a link or a hard link planted in its place is a
/// refusal); the project root's copy is the operator's own and is read directly. This sits beside
/// `gate::tampered_command_scripts`, which compares bytes but follows links.
pub fn tampered_scripts<'a>(
    project_root: &Path,
    worktree: &Path,
    commands: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    for command in commands {
        let Ok(words) = crate::gate::split_command(command) else {
            return Some(format!("command `{command}` cannot be read"));
        };
        for word in words {
            if Path::new(&word).is_absolute() || !worktree.join(&word).is_file() {
                continue;
            }
            let used = match read_in(&worktree.join(&word), worktree, ARTIFACT_CAP) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    return Some(format!("gate script `{word}` vanished from the worktree"));
                }
                Err(error) => {
                    return Some(format!("gate script `{word}` is not trustworthy: {error}"));
                }
            };
            match std::fs::read(project_root.join(&word)) {
                Ok(configured) if configured == used.as_bytes() => {}
                Ok(_) => {
                    return Some(format!(
                        "gate script `{word}` differs from the project root's"
                    ));
                }
                Err(error) => {
                    return Some(format!(
                        "gate script `{word}` is unreadable in the project root: {error}"
                    ));
                }
            }
        }
    }
    None
}

/// Copies `from` over `to` (both inside `root`) as a crossing read followed by a crossing write.
pub fn copy_in(from: &Path, to: &Path, root: &Path) -> Result<(), CrossingError> {
    let Some(bytes) = read_in(from, root, ARTIFACT_CAP)? else {
        return Err(CrossingError::Io(std::io::Error::from(
            std::io::ErrorKind::NotFound,
        )));
    };
    write_in(to, root, bytes.as_bytes())
}
