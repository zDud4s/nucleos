//! Bounded, credential-safe readiness readout for the protected HTTP API.
//!
//! Sidecar entries describe configuration and binary presence only. The supervisor is a
//! fire-and-forget restart loop with no process-status surface, so this module must not imply that
//! a configured binary is currently running. The `*_sidecar_binary` names make that limit explicit.
//!
//! A disabled subsystem never drags the aggregate down. An unconfigured optional pillar is not a
//! fault, and treating it as one teaches readers to ignore the readout when it matters.

use serde::Serialize;
use sqlx::SqlitePool;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::state::AppState;
use crate::worktree;

const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const SUBSYSTEM_TIMEOUT: Duration = Duration::from_millis(500);
const AGGREGATE_TIMEOUT: Duration = Duration::from_secs(1);
const LOW_DISK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DAEMON_TOKEN_KEY: &str = "daemon-token";
const TELEGRAM_TOKEN_KEY: &str = "telegram-token";

/// A small, stable verdict vocabulary shared by the aggregate and every subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Ok,
    Degraded,
    Down,
    Disabled,
}

/// A closed diagnostic vocabulary. It intentionally carries no error text: network and IMAP
/// errors can embed credential-bearing URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureCategory {
    Timeout,
    NotConfigured,
    Unreachable,
    PermissionDenied,
    Missing,
    LowDiskSpace,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubsystemReadout {
    pub name: &'static str,
    pub status: HealthState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<FailureCategory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthReadout {
    pub status: HealthState,
    pub subsystems: Vec<SubsystemReadout>,
}

impl HealthReadout {
    fn aggregate_timeout() -> Self {
        Self {
            status: HealthState::Down,
            subsystems: vec![SubsystemReadout::down(
                "aggregate",
                FailureCategory::Timeout,
            )],
        }
    }
}

impl SubsystemReadout {
    fn ok(name: &'static str) -> Self {
        Self {
            name,
            status: HealthState::Ok,
            reason: None,
        }
    }

    fn degraded(name: &'static str, reason: FailureCategory) -> Self {
        Self {
            name,
            status: HealthState::Degraded,
            reason: Some(reason),
        }
    }

    fn down(name: &'static str, reason: FailureCategory) -> Self {
        Self {
            name,
            status: HealthState::Down,
            reason: Some(reason),
        }
    }

    fn disabled(name: &'static str, reason: FailureCategory) -> Self {
        Self {
            name,
            status: HealthState::Disabled,
            reason: Some(reason),
        }
    }
}

/// Collects independent probes under per-probe, per-subsystem, and aggregate budgets.
pub async fn readout(state: AppState) -> HealthReadout {
    match tokio::time::timeout(AGGREGATE_TIMEOUT, collect_readout(state)).await {
        Ok(readout) => readout,
        Err(_) => HealthReadout::aggregate_timeout(),
    }
}

async fn collect_readout(state: AppState) -> HealthReadout {
    let email_enabled = state.email.enabled;
    let voice_armed = state.voice.armed;
    let stt_command = state.voice.stt_command.clone();
    let (pool, cli, credentials, disk, echo, telegram, email, voice) = tokio::join!(
        run_subsystem("sqlite_pool", pool_probe(state.pool.clone())),
        run_subsystem("cli_binary", cli_probe()),
        run_subsystem("credential_manager", credential_manager_probe()),
        run_subsystem("worktree_disk", disk_probe()),
        run_subsystem(
            "echo_sidecar_binary",
            sidecar_binary_probe("echo_sidecar_binary", true),
        ),
        run_subsystem("telegram_sidecar_binary", telegram_sidecar_probe()),
        run_subsystem(
            "email_sidecar_binary",
            sidecar_binary_probe("email_sidecar_binary", email_enabled),
        ),
        run_subsystem("voice_transcriber", voice_probe(voice_armed, stt_command)),
    );
    let subsystems = vec![pool, cli, credentials, disk, echo, telegram, email, voice];

    HealthReadout {
        status: aggregate_state(&subsystems),
        subsystems,
    }
}

async fn run_subsystem<F>(name: &'static str, probe: F) -> SubsystemReadout
where
    F: Future<Output = SubsystemReadout>,
{
    match tokio::time::timeout(SUBSYSTEM_TIMEOUT, probe).await {
        Ok(readout) => readout,
        Err(_) => SubsystemReadout::down(name, FailureCategory::Timeout),
    }
}

async fn run_probe<F>(name: &'static str, probe: F) -> SubsystemReadout
where
    F: Future<Output = Result<HealthState, FailureCategory>>,
{
    run_probe_with_timeout(name, PROBE_TIMEOUT, probe).await
}

async fn run_probe_with_timeout<F>(
    name: &'static str,
    timeout: Duration,
    probe: F,
) -> SubsystemReadout
where
    F: Future<Output = Result<HealthState, FailureCategory>>,
{
    match tokio::time::timeout(timeout, probe).await {
        Ok(Ok(HealthState::Ok)) => SubsystemReadout::ok(name),
        Ok(Ok(HealthState::Degraded)) => SubsystemReadout::degraded(name, FailureCategory::Unknown),
        Ok(Ok(HealthState::Disabled)) => {
            SubsystemReadout::disabled(name, FailureCategory::NotConfigured)
        }
        Ok(Ok(HealthState::Down)) => SubsystemReadout::down(name, FailureCategory::Unknown),
        Ok(Err(category)) => SubsystemReadout::down(name, category),
        Err(_) => SubsystemReadout::down(name, FailureCategory::Timeout),
    }
}

async fn pool_probe(pool: SqlitePool) -> SubsystemReadout {
    run_probe("sqlite_pool", async move {
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map(|_| HealthState::Ok)
            .map_err(classify_error)
    })
    .await
}

async fn cli_probe() -> SubsystemReadout {
    run_probe("cli_binary", async {
        tokio::task::spawn_blocking(resolve_cli_binary)
            .await
            .map_err(classify_error)?
            .map(|_| HealthState::Ok)
            .ok_or(FailureCategory::Missing)
    })
    .await
}

/// Whether the configured transcriber is a program that exists.
///
/// Reports readiness of configuration only, like every other probe here — it resolves the binary and
/// does not run it. The whitespace split is load-bearing: `stt_command` is a whole command line, so
/// handing the string to a path lookup unsplit would look for a program whose name contains its own
/// arguments and report every configured transcriber as missing.
///
/// Absent config is `Ok`, not a failure. Voice being off is a state, not a fault.
async fn voice_probe(armed: bool, command: String) -> SubsystemReadout {
    run_probe("voice_transcriber", async move {
        if !armed {
            return Ok(HealthState::Ok);
        }
        let program = command
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        tokio::task::spawn_blocking(move || resolve_program(std::ffi::OsStr::new(&program)))
            .await
            .map_err(classify_error)?
            .map(|_| HealthState::Ok)
            .ok_or(FailureCategory::Missing)
    })
    .await
}

async fn credential_manager_probe() -> SubsystemReadout {
    run_probe("credential_manager", async {
        tokio::task::spawn_blocking(|| crate::secrets::load_secret(DAEMON_TOKEN_KEY))
            .await
            .map_err(classify_error)?
            .map(|_| HealthState::Ok)
            .map_err(classify_error)
    })
    .await
}

async fn disk_probe() -> SubsystemReadout {
    let mut readout = run_probe("worktree_disk", async {
        tokio::task::spawn_blocking(worktree_available_space)
            .await
            .map_err(classify_error)?
            .map(|available| {
                if available < LOW_DISK_BYTES {
                    HealthState::Degraded
                } else {
                    HealthState::Ok
                }
            })
            .map_err(category_from_io)
    })
    .await;

    if readout.status == HealthState::Degraded {
        readout.reason = Some(FailureCategory::LowDiskSpace);
    } else if readout.reason == Some(FailureCategory::NotConfigured) {
        readout.status = HealthState::Disabled;
    }
    readout
}

async fn telegram_sidecar_probe() -> SubsystemReadout {
    let configured = run_probe("telegram_sidecar_binary", async {
        tokio::task::spawn_blocking(|| crate::secrets::load_secret(TELEGRAM_TOKEN_KEY))
            .await
            .map_err(classify_error)?
            .map_err(classify_error)
            .map(|secret| {
                if secret.is_some() {
                    HealthState::Ok
                } else {
                    HealthState::Disabled
                }
            })
    })
    .await;

    if configured.status == HealthState::Disabled || configured.status == HealthState::Down {
        return configured;
    }
    sidecar_binary_probe("telegram_sidecar_binary", true).await
}

async fn sidecar_binary_probe(name: &'static str, configured: bool) -> SubsystemReadout {
    if !configured {
        return SubsystemReadout::disabled(name, FailureCategory::NotConfigured);
    }

    run_probe(name, async move {
        tokio::task::spawn_blocking(move || sidecar_binary_path(name).is_some())
            .await
            .map_err(classify_error)
            .and_then(|present| {
                present
                    .then_some(HealthState::Ok)
                    .ok_or(FailureCategory::Missing)
            })
    })
    .await
}

fn aggregate_state(subsystems: &[SubsystemReadout]) -> HealthState {
    if subsystems
        .iter()
        .any(|entry| entry.status == HealthState::Down)
    {
        HealthState::Down
    } else if subsystems
        .iter()
        .any(|entry| entry.status == HealthState::Degraded)
    {
        HealthState::Degraded
    } else if subsystems
        .iter()
        .any(|entry| entry.status == HealthState::Ok)
    {
        HealthState::Ok
    } else {
        HealthState::Disabled
    }
}

fn resolve_cli_binary() -> Option<PathBuf> {
    let configured = std::env::var_os("NUCLEOS_CLAUDE_BIN").unwrap_or_else(|| "claude".into());
    resolve_program(&configured)
}

/// Finds `program` the way a shell would: as a path when it looks like one, else along `PATH`.
///
/// Extracted from `resolve_cli_binary` when the voice transcriber became a second configured program
/// needing the same lookup. Both probes report configuration readiness only — neither runs anything.
fn resolve_program(program: &std::ffi::OsStr) -> Option<PathBuf> {
    let path = PathBuf::from(program);

    if path.components().count() > 1 || path.is_absolute() {
        return binary_candidate(&path);
    }

    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|directory| binary_candidate(&directory.join(program)))
}

fn binary_candidate(path: &Path) -> Option<PathBuf> {
    let mut candidates = vec![path.to_path_buf()];
    if path.extension().is_none() {
        candidates.push(path.with_extension("exe"));
    }
    candidates.into_iter().find(|candidate| {
        candidate.is_file()
            && !candidate
                .extension()
                .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("cmd"))
    })
}

fn sidecar_binary_path(name: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    let stem = name.strip_suffix("_binary")?.replace('_', "-");
    let path = directory.join(format!("{stem}.exe"));
    path.is_file().then_some(path)
}

fn worktree_available_space() -> io::Result<u64> {
    let project_root = std::env::current_dir().map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "a worktree root needs a current project directory",
        )
    })?;
    let root = worktree::worktree_root(&project_root);
    let existing_root = root
        .ancestors()
        .find(|candidate| candidate.exists())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no existing worktree root ancestor",
            )
        })?;
    disk_free_space(existing_root)
}

#[cfg(windows)]
fn disk_free_space(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            available_to_caller: *mut u64,
            total_bytes: *mut u64,
            total_free_bytes: *mut u64,
        ) -> i32;
    }

    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    // `path` is NUL-terminated and remains alive for the whole Win32 call; the other pointers
    // point at writable stack values of the documented `ULARGE_INTEGER` width.
    let success = unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(available)
    }
}

#[cfg(not(windows))]
fn disk_free_space(_path: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "disk-space probe is implemented with the Windows filesystem API",
    ))
}

fn category_from_io(error: io::Error) -> FailureCategory {
    match error.kind() {
        io::ErrorKind::PermissionDenied => FailureCategory::PermissionDenied,
        io::ErrorKind::NotFound => FailureCategory::NotConfigured,
        io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::NotConnected
        | io::ErrorKind::AddrInUse
        | io::ErrorKind::AddrNotAvailable => FailureCategory::Unreachable,
        _ => FailureCategory::Unknown,
    }
}

/// Deliberately discards error text before it can cross the health-response boundary.
fn classify_error<E>(_error: E) -> FailureCategory {
    FailureCategory::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_subsystems_do_not_drag_the_aggregate_down() {
        let entries = [
            SubsystemReadout::ok("pool"),
            SubsystemReadout::disabled("email", FailureCategory::NotConfigured),
        ];
        assert_eq!(aggregate_state(&entries), HealthState::Ok);
    }

    #[test]
    fn aggregate_prioritises_down_then_degraded() {
        assert_eq!(
            aggregate_state(&[
                SubsystemReadout::ok("cli"),
                SubsystemReadout::down("sqlite_pool", FailureCategory::Unknown),
            ]),
            HealthState::Down
        );
        assert_eq!(
            aggregate_state(&[
                SubsystemReadout::ok("pool"),
                SubsystemReadout::degraded("disk", FailureCategory::LowDiskSpace),
            ]),
            HealthState::Degraded
        );
    }

    #[tokio::test]
    async fn a_timed_out_probe_is_down_without_hanging_the_readout() {
        let pending = std::future::pending::<Result<HealthState, FailureCategory>>();
        let readout = run_probe_with_timeout("slow", Duration::ZERO, pending).await;
        assert_eq!(readout.status, HealthState::Down);
        assert_eq!(readout.reason, Some(FailureCategory::Timeout));
    }

    #[tokio::test]
    async fn raw_probe_errors_are_reduced_to_categories_before_serialization() {
        let raw_error = "imap://user:password@host.example.test/INBOX";
        let readout = run_probe("mail", async {
            Err::<HealthState, _>(classify_error(raw_error))
        })
        .await;
        let encoded = serde_json::to_string(&readout).unwrap();
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains("user:password@host"));
    }
}
