//! Restarting the daemon after running work settled (`POST /system/restart`), and swapping in a
//! staged executable on the way.
//!
//! A restart is two exits. A daemon under `--supervise` leaves with `EXIT_RESTART_REQUESTED` and
//! the supervisor relaunches it; the supervisor kills its job when the daemon exits, so a
//! relauncher spawned by a supervised daemon would die with it. Any other daemon (macOS, Linux,
//! `run-daemon.ps1`, started by hand) spawns `<exe> --relaunch-after <pid> <its own args>`, detached
//! from its job and session, BEFORE it stops ([`prepare_restart`]; a failed spawn cancels the stop),
//! and then exits. That process waits for the pid to be gone, swaps a staged
//! executable in, starts the daemon again and leaves. Both paths call [`swap_staged`].
//!
//! Under a systemd unit, the relauncher enters a transient user scope when `systemd-run` is
//! available. That escapes the service cgroup while the unit keeps its default behavior of
//! stopping the daemon's sidecars with it.
//!
//! Waiting for the pid, pausing and spawning are injected, so the tests need no real process.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The flag of the relauncher process. It must be the first argument.
pub const RELAUNCH_FLAG: &str = "--relaunch-after";

/// Set by `--supervise` on the daemon it launches. A daemon that sees it leaves with
/// `supervise::EXIT_RESTART_REQUESTED` and spawns nothing. The daemon removes it from its
/// environment at start (see [`take_supervised_marker`]).
pub const SUPERVISED_VAR: &str = "NUCLEOS_SUPERVISED";

/// How often the relauncher looks for the old daemon.
const WAIT_POLL: Duration = Duration::from_millis(500);
/// How many looks before it gives up: two minutes, well past the daemon's 10 s exit watchdog.
const MAX_POLLS: u32 = 240;

/// Whether this daemon was launched by `--supervise`. Set once by [`take_supervised_marker`].
static SUPERVISED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Reads `NUCLEOS_SUPERVISED` and removes it from the process environment, so no child (agents,
/// sidecars, a relauncher) inherits it and mistakes itself for a supervised daemon. Called once at
/// the very start of the daemon's `main`; later calls change nothing.
pub fn take_supervised_marker() {
    SUPERVISED.get_or_init(|| {
        let marked = std::env::var(SUPERVISED_VAR).is_ok_and(|value| value == "1");
        // SAFETY: this is the first thing `main` does. The tokio runtime has its worker threads, but
        // they are idle: no task has been spawned yet and nothing else reads or writes the
        // environment at this point.
        unsafe { std::env::remove_var(SUPERVISED_VAR) };
        marked
    });
}

fn supervised() -> bool {
    SUPERVISED.get().copied().unwrap_or(false)
}

/// What the daemon does once it has stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitPlan {
    /// Exit 0: an ordinary stop, or a restart whose relauncher was spawned before the stop.
    Plain,
    /// Exit `EXIT_RESTART_REQUESTED` and let the supervisor relaunch.
    ForSupervisor,
}

/// PURE: how a stopping daemon leaves.
pub fn exit_plan(restart_requested: bool, supervised: bool) -> ExitPlan {
    if restart_requested && supervised {
        ExitPlan::ForSupervisor
    } else {
        ExitPlan::Plain
    }
}

/// The exit code of a daemon that has stopped. Spawns nothing: [`prepare_restart`] already did,
/// before the stop. Called from both the graceful path and the exit watchdog.
pub fn exit_code_for_stop() -> i32 {
    match exit_plan(crate::shutdown::restart_requested(), supervised()) {
        ExitPlan::Plain => 0,
        ExitPlan::ForSupervisor => crate::supervise::EXIT_RESTART_REQUESTED,
    }
}

/// Call right before marking the restart and stopping. A supervised daemon needs nothing: the
/// supervisor relaunches it. Any other spawns its relauncher now, which is safe early because the
/// relauncher waits for this pid to be gone. On `Err` the caller must not stop: the daemon keeps
/// serving rather than leaving with nobody to start it again.
pub fn prepare_restart() -> Result<(), String> {
    prepare_restart_with(supervised(), spawn_relauncher)
}

fn prepare_restart_with(
    supervised: bool,
    spawn: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), String> {
    if supervised {
        return Ok(());
    }
    spawn().map_err(|error| format!("could not spawn the relauncher: {error}"))
}

/// `nucleos-core.next(.exe)` beside the executable: where a build is staged.
pub fn next_path(exe: &Path) -> PathBuf {
    exe.with_file_name(format!("nucleos-core.next{}", std::env::consts::EXE_SUFFIX))
}

/// `nucleos-core.old(.exe)` beside the executable: where the replaced one waits to be deleted.
pub fn old_path(exe: &Path) -> PathBuf {
    exe.with_file_name(format!("nucleos-core.old{}", std::env::consts::EXE_SUFFIX))
}

/// What [`swap_staged`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum SwapOutcome {
    /// No staged executable; nothing to do.
    NothingStaged,
    /// The staged executable is now the executable and the old one is `.old`.
    Swapped,
    /// The swap did not happen and the executable is as it was. The reason is for the journal.
    Failed(String),
}

/// Swaps a staged executable in: `exe` becomes `.old`, `.next` becomes `exe`. A failed second
/// rename puts the old one back. A running executable can be renamed on every platform here.
pub fn swap_staged(exe: &Path) -> SwapOutcome {
    swap_staged_with(exe, &|from, to| std::fs::rename(from, to))
}

/// [`swap_staged`] over an injected rename.
fn swap_staged_with(
    exe: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> SwapOutcome {
    let next = next_path(exe);
    if !next.is_file() {
        return SwapOutcome::NothingStaged;
    }
    let old = free_old_path(exe);
    if let Err(error) = rename(exe, &old) {
        return SwapOutcome::Failed(format!("could not move {} aside: {error}", exe.display()));
    }
    if let Err(error) = rename(&next, exe) {
        let put_back = match rename(&old, exe) {
            Ok(()) => "the old executable is back".to_owned(),
            Err(back) => format!("and putting the old executable back failed too: {back}"),
        };
        return SwapOutcome::Failed(format!(
            "could not move {} into place: {error}; {put_back}",
            next.display()
        ));
    }
    SwapOutcome::Swapped
}

/// Where the running executable can be moved aside. Normally `.old`; a `--supervise` that has
/// already swapped once still runs from that file, which cannot be deleted, so the next swap takes
/// a name of its own rather than wait for the supervisor to end.
fn free_old_path(exe: &Path) -> PathBuf {
    let old = old_path(exe);
    if !old.exists() || std::fs::remove_file(&old).is_ok() {
        return old;
    }
    (1..)
        .map(|n| {
            exe.with_file_name(format!(
                "nucleos-core.old.{n}{}",
                std::env::consts::EXE_SUFFIX
            ))
        })
        .find(|path| !path.exists() || std::fs::remove_file(path).is_ok())
        .unwrap_or(old)
}

/// Removes the `.old` executables swaps left. `Ok(false)` when there were none. The caller logs a
/// failure: a file is in use by a supervisor still running from it, and goes next time.
pub fn clean_old(exe: &Path) -> std::io::Result<bool> {
    let Some(dir) = exe.parent() else {
        return Ok(false);
    };
    let mut removed = false;
    let mut failure = None;
    for entry in std::fs::read_dir(dir)?.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("nucleos-core.old")
        {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed = true,
            Err(error) => failure = Some(error),
        }
    }
    failure.map_or(Ok(removed), Err)
}

/// Why a relaunch did not start the daemon.
#[derive(Debug)]
pub enum RelaunchError {
    /// The old daemon was still there after the wait.
    PidStillAlive,
    Spawn(std::io::Error),
}

/// Waits for the old daemon to be gone, swaps a staged executable in, and starts `exe` with `args`.
/// `alive` asks whether the pid is still running, `pause` sleeps one poll, `spawn` starts the
/// daemon. `Ok` carries what the swap did, for the journal; a failed swap still starts the
/// executable that is there.
pub fn relaunch_after(
    exe: &Path,
    args: &[OsString],
    mut alive: impl FnMut() -> bool,
    mut pause: impl FnMut(),
    max_polls: u32,
    spawn: impl FnOnce(&Path, &[OsString]) -> std::io::Result<()>,
) -> Result<SwapOutcome, RelaunchError> {
    let mut polls = 0;
    while alive() {
        polls += 1;
        if polls > max_polls {
            return Err(RelaunchError::PidStillAlive);
        }
        pause();
    }
    let swapped = swap_staged(exe);
    spawn(exe, args).map_err(RelaunchError::Spawn)?;
    Ok(swapped)
}

/// PURE: the pid and the daemon arguments of `<exe> --relaunch-after <pid> <args...>`, or `None`
/// when this is not a relauncher. The flag is only read in the first position, so a daemon whose
/// own arguments mention it is not mistaken for one.
pub fn parse_relaunch(args: &[OsString]) -> Option<(u32, Vec<OsString>)> {
    if args.get(1)? != RELAUNCH_FLAG {
        return None;
    }
    let pid = args.get(2)?.to_str()?.parse().ok()?;
    Some((pid, args[3..].to_vec()))
}

/// Runs the relauncher when the arguments ask for it, and returns its exit code. `None` means this
/// is not a relauncher and `main` goes on.
pub fn dispatch(args: &[OsString]) -> Option<i32> {
    let (pid, daemon_args) = parse_relaunch(args)?;
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            note(&format!("cannot resolve the executable: {error}"));
            return Some(1);
        }
    };
    let result = relaunch_after(
        &exe,
        &daemon_args,
        || pid_alive(pid),
        || std::thread::sleep(WAIT_POLL),
        MAX_POLLS,
        spawn_detached,
    );
    Some(match result {
        Ok(SwapOutcome::Swapped) => {
            note("swapped in the staged executable and started the daemon");
            0
        }
        Ok(SwapOutcome::NothingStaged) => {
            note("started the daemon");
            0
        }
        Ok(SwapOutcome::Failed(reason)) => {
            note(&format!("started the daemon without the swap: {reason}"));
            0
        }
        Err(RelaunchError::PidStillAlive) => {
            note(&format!(
                "ERROR: daemon {pid} did not exit within 120 seconds; not swapping or starting another"
            ));
            1
        }
        Err(RelaunchError::Spawn(error)) => {
            note(&format!("could not start the daemon: {error}"));
            1
        }
    })
}

/// A line in `logs/nucleos-relaunch.log`: the relauncher has no console and no tracing subscriber.
fn note(line: &str) {
    let Ok(default_dir) = crate::data_dir::default_data_dir() else {
        return;
    };
    let dir = crate::data_dir::data_dir_from(
        std::env::var(crate::daemon_client::DATA_DIR_VAR)
            .ok()
            .as_deref(),
        &default_dir,
    )
    .join("logs");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("nucleos-relaunch.log"))
    {
        let _ = writeln!(
            file,
            "{} nucleos-relaunch: {line}",
            chrono::Local::now().to_rfc3339()
        );
    }
}

/// Spawns the relauncher for this daemon: the same executable and the same arguments after the
/// flag and this pid. The working directory and environment are inherited.
fn relaunch_command(
    exe: &Path,
    args: &[OsString],
    under_systemd: bool,
    systemd_run_available: bool,
) -> (PathBuf, Vec<OsString>) {
    if under_systemd && systemd_run_available {
        let mut command_args = vec![
            OsString::from("--user"),
            OsString::from("--scope"),
            OsString::from("--collect"),
            OsString::from("--"),
            exe.as_os_str().to_owned(),
        ];
        command_args.extend_from_slice(args);
        (PathBuf::from("systemd-run"), command_args)
    } else {
        (exe.to_path_buf(), args.to_vec())
    }
}

fn systemd_run_on_path() -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|path| path.join("systemd-run").is_file())
    })
}

fn spawn_relauncher() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut args = vec![
        OsString::from(RELAUNCH_FLAG),
        OsString::from(std::process::id().to_string()),
    ];
    args.extend(std::env::args_os().skip(1));
    let (program, command_args) = relaunch_command(
        &exe,
        &args,
        std::env::var_os("INVOCATION_ID").is_some(),
        systemd_run_on_path(),
    );
    spawn_detached(&program, &command_args)
}

/// Starts `exe` outside this process's session and job, with no console and no inherited stdio, so
/// it outlives its parent.
pub fn spawn_detached(exe: &Path, args: &[OsString]) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` is async-signal-safe and touches no memory of the forked child.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Breakaway is refused when the job does not allow it; then the plain detach has to do.
        command.creation_flags(
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
        );
        if command.spawn().is_ok() {
            return Ok(());
        }
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().map(|_| ())
}

/// Whether a process with this pid is running.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether a process with this pid is running.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: the handle is checked before use and closed on the one path that opened it.
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if process.is_null() {
            return false;
        }
        let waited = WaitForSingleObject(process, 0);
        CloseHandle(process);
        waited == WAIT_TIMEOUT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn fake_exe(dir: &Path) -> PathBuf {
        let exe = dir.join(format!("nucleos-core{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&exe, "old build").unwrap();
        exe
    }

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_relauncher_starts_the_same_exe_after_the_pid_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path());
        let args = os(&["--some-flag", "value"]);
        let events = RefCell::new(Vec::<String>::new());
        let mut looks = 0;
        let result = relaunch_after(
            &exe,
            &args,
            || {
                looks += 1;
                events.borrow_mut().push(format!("look {looks}"));
                looks < 3
            },
            || events.borrow_mut().push("pause".to_owned()),
            10,
            |started, started_args| {
                events
                    .borrow_mut()
                    .push(format!("spawn {} {started_args:?}", started.display()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result, SwapOutcome::NothingStaged);
        assert_eq!(
            *events.borrow(),
            vec![
                "look 1".to_owned(),
                "pause".to_owned(),
                "look 2".to_owned(),
                "pause".to_owned(),
                "look 3".to_owned(),
                format!("spawn {} {args:?}", exe.display()),
            ]
        );
    }

    #[test]
    fn relauncher_uses_a_systemd_scope_when_available() {
        let exe = Path::new("/opt/nucleos/nucleos-core");
        let args = os(&["--relaunch-after", "42", "--serve", "localhost"]);

        let (program, command_args) = relaunch_command(exe, &args, true, true);

        assert_eq!(program, PathBuf::from("systemd-run"));
        assert_eq!(
            command_args,
            os(&[
                "--user",
                "--scope",
                "--collect",
                "--",
                "/opt/nucleos/nucleos-core",
                "--relaunch-after",
                "42",
                "--serve",
                "localhost",
            ])
        );
    }

    #[test]
    fn relauncher_stays_detached_when_systemd_scope_is_unavailable() {
        let exe = Path::new("/opt/nucleos/nucleos-core");
        let args = os(&["--relaunch-after", "42"]);

        let (program, command_args) = relaunch_command(exe, &args, true, false);

        assert_eq!(program, exe);
        assert_eq!(command_args, args);
    }

    #[test]
    fn a_daemon_that_never_exits_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path());
        std::fs::write(next_path(&exe), "new build").unwrap();
        let spawned = RefCell::new(false);
        let result = relaunch_after(
            &exe,
            &[],
            || true,
            || {},
            3,
            |_, _| {
                *spawned.borrow_mut() = true;
                Ok(())
            },
        );
        assert!(matches!(result, Err(RelaunchError::PidStillAlive)));
        assert!(!*spawned.borrow());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old build");
        assert!(
            next_path(&exe).exists(),
            "the staged binary must not be swapped"
        );
    }

    #[test]
    fn the_relauncher_wait_is_bounded_at_120_seconds() {
        assert_eq!(WAIT_POLL * MAX_POLLS, Duration::from_secs(120));
    }

    #[test]
    fn a_staged_exe_replaces_the_running_one_before_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path());
        std::fs::write(next_path(&exe), "new build").unwrap();
        let seen_at_spawn = RefCell::new(None::<(String, String)>);
        let result = relaunch_after(
            &exe,
            &[],
            || false,
            || {},
            1,
            |started, _| {
                *seen_at_spawn.borrow_mut() = Some((
                    std::fs::read_to_string(started).unwrap(),
                    std::fs::read_to_string(old_path(started)).unwrap(),
                ));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result, SwapOutcome::Swapped);
        assert_eq!(
            seen_at_spawn.borrow().clone(),
            Some(("new build".to_owned(), "old build".to_owned())),
            "the new build is in place when the daemon starts"
        );
        assert!(!next_path(&exe).exists(), "the staged file was consumed");
    }

    #[test]
    fn a_held_old_exe_does_not_block_the_next_swap() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir
            .path()
            .join(format!("nucleos-core{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&exe, "running").unwrap();
        std::fs::write(next_path(&exe), "staged").unwrap();
        // A directory with a file in it: `remove_file` refuses it on every platform, as Windows
        // refuses an executable a supervisor is running from.
        std::fs::create_dir(old_path(&exe)).unwrap();
        std::fs::write(old_path(&exe).join("held"), "").unwrap();

        assert_eq!(swap_staged(&exe), SwapOutcome::Swapped);

        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "staged");
        let aside = dir.path().join(format!(
            "nucleos-core.old.1{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert_eq!(std::fs::read_to_string(aside).unwrap(), "running");
        assert!(clean_old(&exe).is_err(), "the held one is reported");
    }

    #[test]
    fn a_failed_second_rename_puts_the_old_exe_back() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path());
        std::fs::write(next_path(&exe), "new build").unwrap();
        let calls = std::cell::Cell::new(0);
        let outcome = swap_staged_with(&exe, &|from, to| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                return Err(std::io::Error::other("refused"));
            }
            std::fs::rename(from, to)
        });
        assert!(matches!(outcome, SwapOutcome::Failed(_)), "{outcome:?}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old build");
        assert!(
            next_path(&exe).exists(),
            "the staged file stays for a retry"
        );
    }

    #[test]
    fn nothing_staged_swaps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path());
        assert_eq!(swap_staged(&exe), SwapOutcome::NothingStaged);
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old build");
    }

    #[test]
    fn the_old_exe_is_cleaned_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_exe(dir.path());
        assert!(!clean_old(&exe).unwrap(), "nothing to clean");
        std::fs::write(old_path(&exe), "old build").unwrap();
        assert!(clean_old(&exe).unwrap());
        assert!(!old_path(&exe).exists());
    }

    #[test]
    fn the_relaunch_flag_is_read_only_in_first_position() {
        let (pid, rest) =
            parse_relaunch(&os(&["nucleos-core", "--relaunch-after", "42", "--a", "b"])).unwrap();
        assert_eq!(pid, 42);
        assert_eq!(rest, os(&["--a", "b"]));
        assert!(parse_relaunch(&os(&["nucleos-core", "--a", "--relaunch-after", "42"])).is_none());
        assert!(parse_relaunch(&os(&["nucleos-core", "--relaunch-after", "x"])).is_none());
        assert!(parse_relaunch(&os(&["nucleos-core"])).is_none());
    }

    #[test]
    fn a_stopping_daemon_picks_its_exit_from_the_restart_and_the_supervisor() {
        assert_eq!(exit_plan(false, true), ExitPlan::Plain);
        assert_eq!(exit_plan(false, false), ExitPlan::Plain);
        assert_eq!(exit_plan(true, true), ExitPlan::ForSupervisor);
        assert_eq!(exit_plan(true, false), ExitPlan::Plain);
    }

    #[test]
    fn the_relauncher_is_spawned_before_the_stop_unless_supervised() {
        let spawned = std::cell::Cell::new(0);
        let ok = || {
            spawned.set(spawned.get() + 1);
            Ok(())
        };
        assert_eq!(prepare_restart_with(true, ok), Ok(()));
        assert_eq!(spawned.get(), 0, "a supervised daemon spawns nothing");
        assert_eq!(prepare_restart_with(false, ok), Ok(()));
        assert_eq!(spawned.get(), 1);
        let refused = prepare_restart_with(false, || Err(std::io::Error::other("refused")));
        assert!(refused.unwrap_err().contains("refused"));
    }

    #[test]
    fn this_process_is_alive_and_an_absurd_pid_is_not() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(u32::MAX - 1));
    }
}
