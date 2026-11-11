//! The crossing (spec D12): the only way the owner's account reads or writes a path the sandbox
//! can also write.
//!
//! A sandboxed account can plant junctions (and, with Developer Mode, symlinks) and hard links
//! anywhere it has been granted. So the owner's side never trusts a path it is handed there:
//!
//! - [`read_crossing`] opens without following a reparse point, refuses a file with more than one
//!   hard link, confirms the final path is inside the grant, caps the size and hands back
//!   [`Untrusted`] bytes;
//! - [`write_crossing`] writes through a [`CrossingWriter`]. Today (no backend) that is
//!   [`InProcessWriter`]: a same-process safe write that refuses a reparse point and replaces the
//!   directory entry rather than writing through it. The contained writer (the daemon's own exe
//!   with `--sandbox-write`, running as the unit's account) plugs in at [`current_writer`].

use std::fmt;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Why a crossing was refused. Every variant but `Io` is a refusal about the path, not the disk.
#[derive(Debug)]
pub enum CrossingError {
    /// The path (or its final component) is a reparse point: a junction or a symlink.
    ReparsePoint(PathBuf),
    /// The file has more than one hard link, so its name proves nothing about where it lives.
    HardLinked(PathBuf),
    /// The path, once every link in it is resolved, is not inside any granted directory.
    Outside(PathBuf),
    /// The file is bigger than the ceiling the caller asked for.
    TooLarge { path: PathBuf, limit: u64 },
    /// Not a regular file (a directory, or a path with no file name).
    NotAFile(PathBuf),
    Io(io::Error),
}

impl fmt::Display for CrossingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReparsePoint(p) => write!(f, "{} is a reparse point", p.display()),
            Self::HardLinked(p) => write!(f, "{} has more than one hard link", p.display()),
            Self::Outside(p) => write!(f, "{} is outside the grant", p.display()),
            Self::TooLarge { path, limit } => {
                write!(f, "{} is larger than {limit} bytes", path.display())
            }
            Self::NotAFile(p) => write!(f, "{} is not a regular file", p.display()),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CrossingError {}

impl From<io::Error> for CrossingError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Bytes that came from a path the sandbox can write. Data, never an instruction: the type has no
/// `Deref` to `[u8]`, so a caller has to say `as_bytes()` and see what it is holding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untrusted(Vec<u8>);

impl Untrusted {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

/// Whether `meta` describes a junction or symlink (on Windows, any reparse point).
fn is_reparse(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// Hard links to the open file, asked of the handle and not of the name.
fn link_count(file: &File, meta: &Metadata) -> io::Result<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // Declared here instead of through windows-sys, whose `Win32_Storage_FileSystem` feature
        // this crate does not enable. `BY_HANDLE_FILE_INFORMATION` is 13 consecutive `u32`s:
        // attributes, three FILETIMEs (two each), volume serial, size high/low, then the link
        // count at index 10, then the file index high/low.
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                handle: *mut std::ffi::c_void,
                info: *mut [u32; 13],
            ) -> i32;
        }
        let _ = meta;
        let mut info = [0u32; 13];
        // SAFETY: `info` is a plain-old-data out parameter of the documented size and the handle
        // stays open for the whole call.
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(u64::from(info[10]))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = file;
        Ok(meta.nlink())
    }
}

/// Opens `path` for reading without following a reparse point or symlink in its last component.
fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Whether `real` (already canonical) lies under one of the granted directories.
fn inside(real: &Path, grant: &[PathBuf]) -> bool {
    grant
        .iter()
        .filter_map(|g| g.canonicalize().ok())
        .any(|g| real.starts_with(&g))
}

/// Reads `path`, a file the sandbox could have written, as the owner's account.
///
/// `grant` is every directory the unit may write; `max_bytes` is the ceiling the caller accepts.
pub fn read_crossing(
    path: &Path,
    grant: &[PathBuf],
    max_bytes: u64,
) -> Result<Untrusted, CrossingError> {
    // Cheap early refusal with a clear reason; the flag on the open below is what closes the race.
    let by_name = std::fs::symlink_metadata(path)?;
    if is_reparse(&by_name) {
        return Err(CrossingError::ReparsePoint(path.to_path_buf()));
    }
    let mut file = open_no_follow(path)?;
    let meta = file.metadata()?;
    if is_reparse(&meta) {
        return Err(CrossingError::ReparsePoint(path.to_path_buf()));
    }
    if !meta.is_file() {
        return Err(CrossingError::NotAFile(path.to_path_buf()));
    }
    if link_count(&file, &meta)? > 1 {
        return Err(CrossingError::HardLinked(path.to_path_buf()));
    }
    // Resolves every link in the middle of the path too: a junction above the file shows up here.
    let real = path.canonicalize()?;
    if !inside(&real, grant) {
        return Err(CrossingError::Outside(real));
    }
    if meta.len() > max_bytes {
        return Err(CrossingError::TooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
        });
    }
    // The file may have grown since `metadata`: read one byte past the ceiling to notice.
    let mut bytes = Vec::new();
    (&mut file).take(max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(CrossingError::TooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
        });
    }
    Ok(Untrusted(bytes))
}

/// How a write reaches a granted path. The seam for the contained writer (spec D12): when a
/// backend exists, `current_writer` returns one that runs the write as the unit's account.
pub trait CrossingWriter {
    fn write(&self, path: &Path, grant: &[PathBuf], bytes: &[u8]) -> Result<(), CrossingError>;
}

/// The v1 (no backend) writer: same process, safe against a link planted in the path.
pub struct InProcessWriter;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl CrossingWriter for InProcessWriter {
    fn write(&self, path: &Path, grant: &[PathBuf], bytes: &[u8]) -> Result<(), CrossingError> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(CrossingError::NotAFile(path.to_path_buf()));
        };
        // The parent is resolved first, so a junction in the directory part lands outside the grant.
        let real_parent = parent.canonicalize()?;
        if !inside(&real_parent, grant) {
            return Err(CrossingError::Outside(real_parent));
        }
        let target = real_parent.join(name);
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if is_reparse(&meta) => return Err(CrossingError::ReparsePoint(target)),
            Ok(meta) if meta.is_dir() => return Err(CrossingError::NotAFile(target)),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Write beside the target and rename over it: the rename replaces the directory entry, so a
        // hard link to a file outside the grant keeps its old content instead of being written.
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp = real_parent.join(format!(
            ".{}.sbx-{}-{sequence}.tmp",
            name.to_string_lossy(),
            std::process::id()
        ));
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        let renamed = written.and_then(|()| std::fs::rename(&temp, &target));
        if let Err(e) = renamed {
            let _ = std::fs::remove_file(&temp);
            return Err(e.into());
        }
        Ok(())
    }
}

/// The writer every `write_crossing` uses. Replaced, not branched around, when the backend lands.
pub fn current_writer() -> &'static dyn CrossingWriter {
    &InProcessWriter
}

/// Writes `bytes` to `path`, a path inside `grant` the sandbox may also write, as the owner.
pub fn write_crossing(path: &Path, grant: &[PathBuf], bytes: &[u8]) -> Result<(), CrossingError> {
    current_writer().write(path, grant, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(dir: &Path) -> Vec<PathBuf> {
        vec![dir.to_path_buf()]
    }

    /// A directory link `link` -> `target`: a junction on Windows (no privilege needed), a symlink
    /// elsewhere.
    fn link_dir(link: &Path, target: &Path) {
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .status()
                .expect("cmd runs");
            assert!(status.success(), "mklink /J failed");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).expect("symlink");
    }

    #[test]
    fn a_plain_file_inside_the_grant_is_read_as_untrusted_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plan.json");
        std::fs::write(&file, b"{\"a\":1}").unwrap();
        let got = read_crossing(&file, &grant(dir.path()), 1024).unwrap();
        assert_eq!(got.as_bytes(), b"{\"a\":1}");
        assert_eq!(got.to_string_lossy(), "{\"a\":1}");
    }

    #[test]
    fn a_file_outside_the_grant_is_refused() {
        let granted = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let file = elsewhere.path().join("secret.txt");
        std::fs::write(&file, b"owner data").unwrap();
        let err = read_crossing(&file, &grant(granted.path()), 1024).unwrap_err();
        assert!(matches!(err, CrossingError::Outside(_)), "{err}");
    }

    #[test]
    fn a_file_reached_through_a_link_in_the_directory_part_is_refused() {
        let granted = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("secret.txt"), b"owner data").unwrap();
        let link = granted.path().join("hop");
        link_dir(&link, elsewhere.path());
        let err = read_crossing(&link.join("secret.txt"), &grant(granted.path()), 1024).unwrap_err();
        assert!(matches!(err, CrossingError::Outside(_)), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_as_the_final_component_is_refused_even_when_it_points_inside() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, b"x").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = read_crossing(&link, &grant(dir.path()), 1024).unwrap_err();
        assert!(matches!(err, CrossingError::ReparsePoint(_)), "{err}");
    }

    #[test]
    fn a_file_with_a_second_hard_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"x").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        let err = read_crossing(&b, &grant(dir.path()), 1024).unwrap_err();
        assert!(matches!(err, CrossingError::HardLinked(_)), "{err}");
    }

    #[test]
    fn a_file_over_the_ceiling_is_refused_and_one_at_the_ceiling_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.bin");
        std::fs::write(&file, [7u8; 10]).unwrap();
        assert_eq!(read_crossing(&file, &grant(dir.path()), 10).unwrap().as_bytes().len(), 10);
        let err = read_crossing(&file, &grant(dir.path()), 9).unwrap_err();
        assert!(matches!(err, CrossingError::TooLarge { limit: 9, .. }), "{err}");
    }

    #[test]
    fn a_directory_is_not_a_file_to_read() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        // Opening a directory fails outright on Windows and is `NotAFile` on Unix; both refuse.
        assert!(read_crossing(&sub, &grant(dir.path()), 1024).is_err());
    }

    #[test]
    fn a_write_inside_the_grant_creates_and_then_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("council.md");
        write_crossing(&file, &grant(dir.path()), b"one").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"one");
        write_crossing(&file, &grant(dir.path()), b"two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
    }

    #[test]
    fn a_write_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        write_crossing(&dir.path().join("a.md"), &grant(dir.path()), b"x").unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.md".to_owned()]);
    }

    #[test]
    fn a_write_outside_the_grant_is_refused_and_creates_nothing() {
        let granted = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let file = elsewhere.path().join("a.md");
        let err = write_crossing(&file, &grant(granted.path()), b"x").unwrap_err();
        assert!(matches!(err, CrossingError::Outside(_)), "{err}");
        assert!(!file.exists());
    }

    #[test]
    fn a_write_through_a_link_in_the_directory_part_never_lands_outside() {
        let granted = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let link = granted.path().join("hop");
        link_dir(&link, elsewhere.path());
        let err = write_crossing(&link.join("a.md"), &grant(granted.path()), b"x").unwrap_err();
        assert!(matches!(err, CrossingError::Outside(_)), "{err}");
        assert!(!elsewhere.path().join("a.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_write_onto_a_symlink_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, b"keep").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = write_crossing(&link, &grant(dir.path()), b"x").unwrap_err();
        assert!(matches!(err, CrossingError::ReparsePoint(_)), "{err}");
        assert_eq!(std::fs::read(&real).unwrap(), b"keep");
    }

    #[test]
    fn a_write_onto_a_hard_link_replaces_the_name_and_leaves_the_other_link_alone() {
        let dir = tempfile::tempdir().unwrap();
        let owner = dir.path().join("owner.txt");
        let planted = dir.path().join("planted.txt");
        std::fs::write(&owner, b"owner data").unwrap();
        std::fs::hard_link(&owner, &planted).unwrap();
        write_crossing(&planted, &grant(dir.path()), b"new").unwrap();
        assert_eq!(std::fs::read(&planted).unwrap(), b"new");
        assert_eq!(std::fs::read(&owner).unwrap(), b"owner data");
    }

    #[test]
    fn a_write_onto_a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let err = write_crossing(&sub, &grant(dir.path()), b"x").unwrap_err();
        assert!(matches!(err, CrossingError::NotAFile(_)), "{err}");
    }
}
