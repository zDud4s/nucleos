//! Production-only preparation of a no-TTY login command: which executable runs, and how a
//! missing CLI is reported. Kept out of `login.rs` (size ceiling) and out of the spawner seam, so
//! the state-machine tests keep using scripted processes.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use crate::health::resolve_program;

/// The env var that overrides the executable of a login program: `NUCLEOS_<PROGRAM>_BIN`
/// (`NUCLEOS_CLAUDE_BIN` already names the Claude CLI elsewhere in the daemon).
fn override_var(program: &str) -> String {
    format!("NUCLEOS_{}_BIN", program.to_ascii_uppercase())
}

/// PURE: the executable to run for `program`, given the override value, if any. A configured
/// override wins; else `program` is looked up the way a shell would. Unlike a bare spawn, this
/// resolves a Windows `.exe` on `PATH` and refuses a `.cmd` shim up front.
pub(super) fn resolve_with(program: &str, configured: Option<OsString>) -> io::Result<PathBuf> {
    let wanted = configured.unwrap_or_else(|| program.into());
    resolve_program(&wanted).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the `{program}` CLI was not found; install it or set {}",
                override_var(program)
            ),
        )
    })
}

/// The executable for a login command, reading the override from the environment.
pub(super) fn resolve_executable(program: &str) -> io::Result<PathBuf> {
    resolve_with(program, std::env::var_os(override_var(program)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_var_is_named_after_the_program() {
        assert_eq!(override_var("claude"), "NUCLEOS_CLAUDE_BIN");
        assert_eq!(override_var("codex"), "NUCLEOS_CODEX_BIN");
    }

    #[test]
    fn a_configured_executable_that_exists_is_the_one_to_run() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("my-claude.exe");
        std::fs::write(&bin, b"").unwrap();
        let found = resolve_with("claude", Some(bin.clone().into_os_string())).unwrap();
        assert_eq!(found, bin);
    }

    #[test]
    fn a_missing_cli_is_reported_as_not_found_and_names_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let ghost = dir.path().join("no-such-cli");
        let error = resolve_with("claude", Some(ghost.into_os_string())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let text = error.to_string();
        assert!(text.contains("`claude` CLI was not found"), "{text}");
        assert!(text.contains("NUCLEOS_CLAUDE_BIN"), "{text}");
    }
}
