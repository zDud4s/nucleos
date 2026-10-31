//! Where this process's database lives: the data directory and the file in it.
//!
//! Shared by startup (`main.rs`) and the settings recovery CLI (`settings_cli`), so that a command that edits
//! the configuration of "this daemon" cannot resolve a different database than the daemon opens. It is a module
//! of its own, and not part of `daemon_client` (which owns `NUCLEOS_PORT` / `NUCLEOS_DATA_DIR` and `port()`),
//! because `daemon_client.rs` is at its file-size ceiling.

use crate::daemon_client::DATA_DIR_VAR;
use std::path::{Path, PathBuf};

/// The database file inside a data directory. Its WAL and shared-memory siblings are `<file>-wal`
/// and `<file>-shm`.
pub const DB_FILE: &str = "nucleos.db";

/// PURE: the data directory, given what `NUCLEOS_DATA_DIR` said and the platform default.
///
/// An empty value is not an override, for the reason `daemon_client::is_primary` gives: it is what a shell
/// leaves behind when a variable is exported and never given one, and honouring it would put the database in
/// the working directory.
pub fn data_dir_from(override_dir: Option<&str>, default_dir: &Path) -> PathBuf {
    match override_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => default_dir.to_path_buf(),
    }
}

/// The platform's per-user data directory for NucleOS (`ProjectDirs::from("dev", "nucleos", "NucleOS")`).
pub fn default_data_dir() -> Result<PathBuf, String> {
    directories::ProjectDirs::from("dev", "nucleos", "NucleOS")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .ok_or_else(|| "could not resolve local app data directory".to_owned())
}

/// The data directory THIS process would use: the environment override, else the platform default.
pub fn data_dir() -> Result<PathBuf, String> {
    Ok(data_dir_from(
        std::env::var(DATA_DIR_VAR).ok().as_deref(),
        &default_data_dir()?,
    ))
}

/// The database file of a data directory.
pub fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DB_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `main.rs` used to match on the variable inline. The rule is the same one, now shared with the
    /// settings recovery CLI, so the two can never look at different databases.
    #[test]
    fn an_empty_data_dir_override_is_not_an_override() {
        let default = Path::new("default-dir");
        assert_eq!(
            data_dir_from(Some("elsewhere"), default),
            PathBuf::from("elsewhere")
        );
        assert_eq!(
            data_dir_from(Some(""), default),
            PathBuf::from("default-dir")
        );
        assert_eq!(data_dir_from(None, default), PathBuf::from("default-dir"));
    }

    #[test]
    fn the_database_is_a_file_directly_under_the_data_dir() {
        assert_eq!(DB_FILE, "nucleos.db");
        assert_eq!(
            db_path(Path::new("some-dir")),
            Path::new("some-dir").join("nucleos.db")
        );
    }

    /// Startup and the CLI must resolve the platform default from the SAME `ProjectDirs` triple.
    #[test]
    fn the_default_data_dir_is_the_project_dirs_local_data_dir() {
        let expected = directories::ProjectDirs::from("dev", "nucleos", "NucleOS")
            .expect("this machine has a home directory")
            .data_local_dir()
            .to_path_buf();
        assert_eq!(default_data_dir(), Ok(expected));
    }
}
