//! Starting the daemon that ships beside the shell in a release bundle.
//!
//! The shell may START it, but never owns it: the child is detached, never waited on and never
//! killed, so the daemon's lifecycle stays independent of the window (Part A).

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the bundled daemon sits: beside the shell executable.
pub fn bundled_daemon(shell_exe: &Path) -> PathBuf {
    shell_exe.with_file_name(format!("nucleos-core{}", std::env::consts::EXE_SUFFIX))
}

/// A daemon is started only from a release bundle, when nothing answers on the port and the
/// bundled binary is actually there.
pub fn should_spawn(release_bundle: bool, answered: bool, present: bool) -> bool {
    release_bundle && !answered && present
}

fn answers() -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], 8791));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// Starts the bundled daemon when this is a release bundle and none answers. Never blocks the
/// caller: the probing happens on a std thread.
pub fn ensure_running() {
    if !cfg!(feature = "release-bundle") {
        return;
    }
    std::thread::spawn(|| {
        // A daemon started at logon may still be binding; a second primary would reconcile runs
        // before failing to bind, so give the port up to 10 x 500 ms to answer first.
        let mut answered = answers();
        for _ in 0..10 {
            if answered {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
            answered = answers();
        }
        let Ok(shell_exe) = std::env::current_exe() else {
            return;
        };
        let exe = bundled_daemon(&shell_exe);
        if !should_spawn(true, answered, exe.is_file()) {
            return;
        }
        let mut cmd = std::process::Command::new(&exe);
        if let Some(dir) = exe.parent() {
            cmd.current_dir(dir);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
            cmd.creation_flags(0x0800_0000 | 0x0000_0200);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        // Deliberately dropped: never waited on, never killed.
        let _ = cmd.spawn();
    });
}

/// Whether this build offers updates and runs the bundled daemon: release bundles only.
#[tauri::command]
pub fn updates_enabled() -> bool {
    cfg!(feature = "release-bundle")
}

/// How this shell was installed, which decides whether it may update itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    AppImage,
    SystemPackage,
    Other,
}

/// Classifies the install. STUB (RED): always `Other`.
pub fn install_kind_of(_exe: &Path, _appimage_env: bool) -> InstallKind {
    InstallKind::Other
}

#[cfg(test)]
mod tests {
    use super::InstallKind;
    use std::path::{Path, PathBuf};

    #[test]
    fn install_kind_appimage_env_wins_even_under_usr_bin() {
        assert_eq!(
            super::install_kind_of(Path::new("/tmp/.mount_x/usr/bin/nucleos-shell"), true),
            InstallKind::AppImage
        );
        assert_eq!(
            super::install_kind_of(Path::new("/usr/bin/nucleos-shell"), true),
            InstallKind::AppImage,
            "APPIMAGE takes priority over /usr/bin"
        );
    }

    #[test]
    fn install_kind_usr_bin_is_a_system_package() {
        assert_eq!(
            super::install_kind_of(Path::new("/usr/bin/nucleos-shell"), false),
            InstallKind::SystemPackage
        );
    }

    #[test]
    fn install_kind_usr_lib_is_a_system_package() {
        assert_eq!(
            super::install_kind_of(Path::new("/usr/lib/nucleos/x"), false),
            InstallKind::SystemPackage
        );
    }

    #[test]
    fn install_kind_other_locations_are_other() {
        for p in ["/opt/nucleos/nucleos-shell", "C:/apps/nucleos/shell.exe", "/usrx/bin/nucleos-shell"] {
            assert_eq!(
                super::install_kind_of(Path::new(p), false),
                InstallKind::Other,
                "{p}"
            );
        }
    }

    /// The shell starts a daemon only from a release bundle, when nothing answers on the port
    /// and the bundled binary is actually there.
    #[test]
    fn the_bundled_daemon_starts_only_from_a_release_bundle() {
        assert!(super::should_spawn(true, false, true));
        assert!(
            !super::should_spawn(false, false, true),
            "dev builds never spawn"
        );
        assert!(
            !super::should_spawn(true, true, true),
            "a daemon already answers"
        );
        assert!(
            !super::should_spawn(true, false, false),
            "no binary, nothing to spawn"
        );
    }

    #[test]
    fn the_bundled_daemon_sits_beside_the_shell() {
        let shell = Path::new("C:/apps/nucleos/shell.exe");
        let expected: PathBuf = Path::new("C:/apps/nucleos")
            .join(format!("nucleos-core{}", std::env::consts::EXE_SUFFIX));
        assert_eq!(super::bundled_daemon(shell), expected);
    }
}
