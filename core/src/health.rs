//! §spec pilar-de-browser
//!
//! Bounded, credential-safe readiness readout for the protected HTTP API.
//!
//! Sidecar entries report liveness: the supervisor publishes whether it started each child and
//! whether it has since seen it exit, and these rows read that. One limit remains, and it is worth
//! stating precisely because the previous one was overstated in the other direction — a child that
//! is running but wedged reads `ok`, because the supervisor watches processes, not progress.
//!
//! `cli_binary` and `voice_transcriber` name no resident process, so for them "alive" can only mean
//! the program runs on this machine. They execute it, behind a cache: the shell polls this endpoint
//! every three seconds and the whole readout is budgeted at one second, so running a CLI on the
//! request path would flap `down` on an install that works. See [`exec_probe`].
//!
//! A disabled subsystem never drags the aggregate down. An unconfigured optional pillar is not a
//! fault, and treating it as one teaches readers to ignore the readout when it matters.

use serde::Serialize;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::sidecar::Liveness;
use crate::state::AppState;
use crate::worktree;

const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const SUBSYSTEM_TIMEOUT: Duration = Duration::from_millis(500);
const AGGREGATE_TIMEOUT: Duration = Duration::from_secs(1);
const LOW_DISK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DAEMON_TOKEN_KEY: &str = "daemon-token";
const TELEGRAM_TOKEN_KEY: &str = "telegram-token";
/// How long an exec verdict is trusted before a refresh is kicked off behind the readout.
const EXEC_CACHE_TTL: Duration = Duration::from_secs(60);
/// A generous ceiling for the background exec. Nobody waits on it, so it can afford to be patient
/// with a CLI that takes its time starting — which is exactly what the readout itself cannot do.
const EXEC_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Configured, and the process is not up. Distinct from `Missing`, which is about a file that
    /// is not there, and from `NotConfigured`, which is about a pillar nobody asked for.
    NotRunning,
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
    let web_enabled = state.web.enabled;
    let browser_enabled = state.browser.enabled;
    let github_asked_for = state.github.enabled && state.github.configured;
    let github_binary = state.github.binary.clone();
    let voice_armed = state.voice.armed;
    let stt_command = state.voice.stt_command.clone();
    // `speaker.is_some()` and not `!tts_command.is_empty()`: `speaker_for` is the one place that
    // decides whether a command becomes a capability, and a probe that re-derives that condition is a
    // second opinion about it. The transcriber probe learned this the hard way with `split_command`.
    // A COMMAND speaker only. A resident one has no program to look for, and the núcleo does not
    // probe Ollama either — see `speaker_probe`.
    let voice_speaks = state.voice.speaker.is_some() && !state.voice.tts_command.trim().is_empty();
    let tts_command = state.voice.tts_command.clone();
    let (pool, cli, credentials, disk, echo, telegram, email, web, browser, voice, speaker, github) = tokio::join!(
        run_subsystem("sqlite_pool", pool_probe(state.pool.clone())),
        run_subsystem("cli_binary", cli_probe()),
        run_subsystem("credential_manager", credential_manager_probe()),
        run_subsystem("worktree_disk", disk_probe()),
        run_subsystem(
            "echo_sidecar",
            sidecar_probe("echo_sidecar", crate::sidecar::ECHO, true),
        ),
        run_subsystem("telegram_sidecar", telegram_sidecar_probe()),
        run_subsystem(
            "email_sidecar",
            sidecar_probe("email_sidecar", crate::sidecar::EMAIL, email_enabled),
        ),
        run_subsystem(
            "web_sidecar",
            sidecar_probe("web_sidecar", crate::sidecar::WEB, web_enabled),
        ),
        // Spec §9.4 asks for three states rather than one — Chromium downloaded, sidecar running,
        // browser reachable — and this is the second of the three. The first belongs to the sidecar,
        // which is the only process that knows where the binary is, and it reports it by refusing to
        // start with a message naming the path. Collapsing them here would make a fresh installation
        // that has not downloaded 300MB yet look like a fault, which is the readout teaching people
        // to ignore it.
        run_subsystem(
            "browser_sidecar",
            sidecar_probe("browser_sidecar", crate::sidecar::BROWSER, browser_enabled),
        ),
        run_subsystem("voice_transcriber", voice_probe(voice_armed, stt_command)),
        run_subsystem("voice_speaker", speaker_probe(voice_speaks, tts_command)),
        run_subsystem("github", github_probe(github_asked_for, github_binary)),
    );
    let subsystems = vec![
        pool,
        cli,
        credentials,
        disk,
        echo,
        telegram,
        email,
        web,
        browser,
        voice,
        speaker,
        github,
    ];

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
        let resolved = tokio::task::spawn_blocking(resolve_cli_binary)
            .await
            .map_err(classify_error)?
            .ok_or(FailureCategory::Missing)?;
        // The resolved path, not the configured name: the lookup has already decided which file on
        // PATH this is, and re-deriving it in another process would be a second chance to disagree.
        exec_probe(resolved.to_string_lossy().into_owned(), "--version").await
    })
    .await
}

/// Whether the configured transcriber is a program that runs here.
///
/// Splitting is load-bearing: `stt_command` is a whole command line, so handing the string to a path
/// lookup unsplit would look for a program whose name contains its own arguments and report every
/// configured transcriber as missing.
///
/// It splits through `transcribe::split_command`, the same function that spawns the child, and that
/// sharing is the point rather than a tidiness. This probe used its own `split_whitespace` until a
/// quoted program path became supported — at which point it started looking for a program whose name
/// began with a quote character, and reported a transcriber that worked perfectly as missing. A probe
/// that parses its subject differently from the code under test is worse than no probe: it raises the
/// alarm on exactly the configuration it was added to bless.
///
/// Absent config is `Ok`, not a failure. Voice being off is a state, not a fault.
async fn voice_probe(armed: bool, command: String) -> SubsystemReadout {
    run_probe("voice_transcriber", async move {
        if !armed {
            return Ok(HealthState::Ok);
        }
        let program = crate::transcribe::split_command(&command)
            .into_iter()
            .next()
            .unwrap_or_default();
        let resolved =
            tokio::task::spawn_blocking(move || resolve_program(std::ffi::OsStr::new(&program)))
                .await
                .map_err(classify_error)?
                .ok_or(FailureCategory::Missing)?;
        exec_probe(resolved.to_string_lossy().into_owned(), "--help").await
    })
    .await
}

/// Whether the configured speaker is a program that runs here.
///
/// The transcriber probe's twin, down to splitting through the same `split_command` for the same
/// reason — a probe that parses its subject differently from the code that spawns it raises the alarm
/// on exactly the configuration it was added to bless.
///
/// One difference, and it is the whole reason this is a separate row rather than a second check
/// inside `voice_probe`: a missing SPEAKER is not a missing pillar. Voice with no TTS still hears the
/// question and still answers it, in writing. Folding the two together would paint a working
/// conversation red for lacking a voice, and this module's header says what that costs — it teaches
/// people to ignore the readout when it matters.
///
/// Not configured is `Ok`. A núcleo that does not speak is a choice, not a fault.
///
/// **A resident speaker (`tts_url`) is `Ok` here too, and is not probed.** There is no program to
/// resolve, and the alternative — an HTTP probe — would be a new pattern in this module for one
/// loopback service while Ollama, the other one, has none. A row that graded the operator's Piper
/// server and said nothing about their Ollama would be describing their setup rather than this
/// daemon's. The failure still surfaces: `HttpSpeaker` reports a refused connection, `voice.rs` turns
/// it into a 502, and the window says the speaker failed.
async fn speaker_probe(configured: bool, command: String) -> SubsystemReadout {
    run_probe("voice_speaker", async move {
        if !configured {
            return Ok(HealthState::Ok);
        }
        let program = crate::transcribe::split_command(&command)
            .into_iter()
            .next()
            .unwrap_or_default();
        let resolved =
            tokio::task::spawn_blocking(move || resolve_program(std::ffi::OsStr::new(&program)))
                .await
                .map_err(classify_error)?
                .ok_or(FailureCategory::Missing)?;
        exec_probe(resolved.to_string_lossy().into_owned(), "--help").await
    })
    .await
}

/// Whether the GitHub pillar could act if it were asked to.
///
/// **`asked_for` is `enabled` AND the file existing, and both halves are load-bearing.**
/// `GithubConfig::enabled` defaults to TRUE so that a machine with no `.ai/github.yaml` is capable
/// of everything and autonomous in nothing — which means `enabled` alone can no longer distinguish
/// "the owner wants this" from "the owner has never heard of it". Grading on `enabled` alone would
/// put a red row on every installation that has never touched GitHub, and this module's own header
/// says what that costs: it teaches readers to ignore the readout when it matters.
///
/// Once it HAS been asked for, the two failures are kept apart, because they send a person to two
/// different places:
///
/// - no `gh` on PATH is `Missing` — a thing this computer cannot do;
/// - no token is `PermissionDenied` — a thing it could do if somebody pasted a credential. Reported
///   any other way it reads like a broken repository, and somebody loses an hour.
///
/// It probes the TOKEN and never `gh auth status`, and that is the whole point of asking this
/// question here: the CLI's own login lives in the interactive session's keyring, so a green
/// `gh auth status` would say healthy while the daemon — a scheduled task — could not act.
async fn github_probe(asked_for: bool, binary: String) -> SubsystemReadout {
    run_probe("github", async move {
        if !asked_for {
            return Ok(HealthState::Disabled);
        }
        // The two facts `execute` needs, in the order it needs them, and the CATEGORY comes from
        // `github::Failure` rather than being chosen again here. One vocabulary, defined where the
        // failures are, so the readout and the refusal a caller gets cannot come to disagree.
        // The name off the runtime rather than the literal `gh`, so this probe and
        // `github::execute` cannot be looking for two different files — the second chance to
        // disagree that `cli_probe` refuses to take.
        let failure =
            if tokio::task::spawn_blocking(move || resolve_program(std::ffi::OsStr::new(&binary)))
                .await
                .map_err(classify_error)?
                .is_none()
            {
                Some(crate::github::Failure::MissingCli)
            } else if crate::github::load_token().await.is_none() {
                Some(crate::github::Failure::MissingToken)
            } else {
                None
            };
        match failure {
            None => Ok(HealthState::Ok),
            Some(failure) => Err(failure.category()),
        }
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
    let configured = run_probe("telegram_sidecar", async {
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
    sidecar_probe("telegram_sidecar", crate::sidecar::TELEGRAM, true).await
}

/// PURE: what the supervisor's last observation of a sidecar means for its row.
///
/// The `configured` gate runs first and is never reached past: a pillar nobody turned on is a state,
/// not a fault, and must not be dragged into `down` by having no registry entry.
///
/// `None` shares an arm with `Restarting` on purpose. Nothing ever supervised this name — either the
/// daemon is still starting, which is honest for the seconds it lasts, or something refused to start
/// it. The email sidecar is the case that matters: `main.rs` returns before `supervise` when the
/// hook barrier cannot be proven, and this row used to stay green on the strength of an `.exe`
/// sitting on disk while the pillar was silently off.
fn sidecar_row(
    name: &'static str,
    configured: bool,
    liveness: Option<Liveness>,
) -> SubsystemReadout {
    if !configured {
        return SubsystemReadout::disabled(name, FailureCategory::NotConfigured);
    }
    match liveness {
        Some(Liveness::Running) => SubsystemReadout::ok(name),
        Some(Liveness::Restarting) | None => {
            SubsystemReadout::down(name, FailureCategory::NotRunning)
        }
        Some(Liveness::FailedToSpawn(kind)) => {
            SubsystemReadout::down(name, category_from_spawn(kind))
        }
    }
}

/// The row name and the supervisor's key are different things, so the call site passes both.
async fn sidecar_probe(
    name: &'static str,
    supervised_as: &'static str,
    configured: bool,
) -> SubsystemReadout {
    sidecar_row(name, configured, crate::sidecar::liveness_of(supervised_as))
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

type ExecVerdict = Result<HealthState, FailureCategory>;

/// Keyed by the program itself, not by the row: the verdict is a fact about a specific binary, so
/// re-pointing `stt_command` at a different transcriber must not inherit the old one's answer.
static EXEC_CACHE: LazyLock<Mutex<HashMap<String, (Instant, ExecVerdict)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// What actually happened when the probe tried to run the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecOutcome {
    Exited { success: bool },
    TimedOut,
    SpawnFailed(io::ErrorKind),
}

/// PURE: what an attempt to run the program means for the row.
fn exec_verdict(outcome: ExecOutcome) -> ExecVerdict {
    match outcome {
        ExecOutcome::Exited { success: true } => Ok(HealthState::Ok),
        // Deliberately not `down`. `--help` and `--version` conventions differ between CLIs, and a
        // probe that raises the alarm on a working install is worse than no probe — the lesson this
        // module already learned from the quoted-path bug in its tests.
        ExecOutcome::Exited { success: false } | ExecOutcome::TimedOut => Ok(HealthState::Degraded),
        ExecOutcome::SpawnFailed(kind) => Err(category_from_spawn(kind)),
    }
}

/// PURE: a cached verdict, but only while it is fresh.
fn cached_verdict(entry: Option<(Instant, ExecVerdict)>, now: Instant) -> Option<ExecVerdict> {
    entry
        .filter(|(stamped, _)| now.duration_since(*stamped) < EXEC_CACHE_TTL)
        .map(|(_, verdict)| verdict)
}

/// A cached verdict on whether `program` runs here, refreshed behind the readout rather than during
/// it.
///
/// The readout never waits for the program. The shell polls `/health` every three seconds and
/// [`AGGREGATE_TIMEOUT`] is one second, so executing on the request path would spawn two processes
/// every three seconds and report `down: timeout` for any CLI that takes longer than a second to
/// answer — which a Node CLI routinely does.
///
/// A cold read answers `Ok`, because the caller only gets here once the path has resolved: that is
/// precisely the check this probe used to do on its own, and it is honest about being a config
/// check. The executed verdict replaces it one poll later.
async fn exec_probe(program: String, flag: &'static str) -> ExecVerdict {
    let entry = EXEC_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(&program).cloned());
    if let Some(verdict) = cached_verdict(entry, Instant::now()) {
        return verdict;
    }

    // Claim the slot before spawning, or every poll until the refresh lands starts another one.
    if let Ok(mut cache) = EXEC_CACHE.lock() {
        cache.insert(program.clone(), (Instant::now(), Ok(HealthState::Ok)));
    }
    tokio::spawn(refresh_exec_cache(program, flag));
    Ok(HealthState::Ok)
}

async fn refresh_exec_cache(program: String, flag: &'static str) {
    let verdict = exec_verdict(run_program_once(&program, flag).await);
    if let Ok(mut cache) = EXEC_CACHE.lock() {
        cache.insert(program, (Instant::now(), verdict));
    }
}

/// Runs the program with a harmless flag, purely to see whether it runs at all.
///
/// This is what execution buys over resolving a path: wrong architecture, a missing DLL, a corrupt
/// file, permission bits, an `.exe` that is really text — all of them resolve fine and fail here.
///
/// stdio is closed and `kill_on_drop` set so a transcriber that ignores `--help` and sits waiting
/// for audio is killed at the timeout instead of accumulating orphans.
async fn run_program_once(program: &str, flag: &'static str) -> ExecOutcome {
    let mut command = tokio::process::Command::new(program);
    command
        .arg(flag)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    match command.spawn() {
        Err(error) => ExecOutcome::SpawnFailed(error.kind()),
        Ok(mut child) => match tokio::time::timeout(EXEC_PROBE_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => ExecOutcome::Exited {
                success: status.success(),
            },
            Ok(Err(error)) => ExecOutcome::SpawnFailed(error.kind()),
            Err(_) => ExecOutcome::TimedOut,
        },
    }
}

/// A spawn failure's `NotFound` means the program is absent, so it is `Missing` — where
/// [`category_from_io`] maps the same `ErrorKind` to `NotConfigured` for the disk probe, which is
/// asking a different question of the same error.
fn category_from_spawn(kind: io::ErrorKind) -> FailureCategory {
    match kind {
        io::ErrorKind::NotFound => FailureCategory::Missing,
        io::ErrorKind::PermissionDenied => FailureCategory::PermissionDenied,
        _ => FailureCategory::Unknown,
    }
}

fn worktree_available_space() -> io::Result<u64> {
    let project_root = std::env::current_dir().map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "a worktree root needs a current project directory",
        )
    })?;
    free_space_for_worktrees(&project_root)
}

/// How much room is left on the volume that would hold this project's worktrees.
///
/// Takes the project root rather than reading the daemon's own directory, and that is the whole
/// reason it is a function of its own. `worktree_root` lives INSIDE each project root by default —
/// `<project>/.nucleos/worktrees`, not a sibling of it — so two projects can still sit on two
/// volumes; a reading taken from wherever the daemon happens to be running answers about a third.
/// The probe above keeps the old behaviour because a health readout is about the machine, not about
/// one project.
///
/// Blocking, and deliberately not wrapped in `spawn_blocking` here: `GetDiskFreeSpaceExW` is a
/// metadata read against an already-mounted volume, and every caller is either already on a
/// blocking thread or paying microseconds.
pub fn free_space_for_worktrees(project_root: &Path) -> io::Result<u64> {
    let root = worktree::worktree_root(project_root);
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

    /// A working transcriber behind a quoted path must not be reported as missing.
    ///
    /// The regression this pins: the probe used to split on whitespace while `transcribe.rs` split on
    /// quotes, so a `stt_command` naming `"C:\Program Files\...\whisper-cli.exe"` made the probe
    /// look for a program whose name STARTS WITH a quote character. It reported `down` for the exact
    /// configuration that quoting was added to support — a probe raising the alarm on a working setup,
    /// which is worse than no probe. Both now go through one function.
    ///
    /// The assertion is `not Missing` rather than `== Ok` since the probe started executing the
    /// program: a resolvable program can now answer `ok` or `degraded` depending on what IT does
    /// with `--help`, which is the program's business and not this regression's. Pinning `== Ok`
    /// would make a test about SPLITTING fail over an exit code — the same category of misdirected
    /// alarm the bug itself was.
    #[tokio::test]
    async fn a_quoted_transcriber_path_probes_the_program_and_not_the_quote() {
        // `cmd` exists on every Windows host and needs no arguments to resolve.
        let readout = voice_probe(true, "\"cmd\" -m model.bin".to_string()).await;

        assert_ne!(
            readout.reason,
            Some(FailureCategory::Missing),
            "a quoted path that resolves must not be reported missing, got {:?}",
            readout.status
        );
    }

    /// And the unquoted form, which every config written before quoting existed uses.
    #[tokio::test]
    async fn an_unquoted_transcriber_path_still_probes_its_first_token() {
        let readout = voice_probe(true, "cmd -m model.bin".to_string()).await;
        assert_ne!(readout.reason, Some(FailureCategory::Missing));

        let missing = voice_probe(true, "definitely-not-a-program-anywhere -x".to_string()).await;
        assert_eq!(
            missing.status,
            HealthState::Down,
            "a transcriber that does not exist has to be reported"
        );
        assert_eq!(missing.reason, Some(FailureCategory::Missing));
    }

    /// A row now reports what the supervisor saw, not what is sitting on disk.
    #[test]
    fn a_sidecar_row_reports_what_the_supervisor_saw() {
        let running = sidecar_row("echo_sidecar", true, Some(Liveness::Running));
        assert_eq!(running.status, HealthState::Ok);
        assert_eq!(running.reason, None);

        let restarting = sidecar_row("echo_sidecar", true, Some(Liveness::Restarting));
        assert_eq!(restarting.status, HealthState::Down);
        assert_eq!(restarting.reason, Some(FailureCategory::NotRunning));

        // A failed spawn proves the binary's absence better than an `is_file()` ever did: it proves
        // it at the moment it mattered, by the mechanism that mattered.
        let never_built = sidecar_row(
            "echo_sidecar",
            true,
            Some(Liveness::FailedToSpawn(io::ErrorKind::NotFound)),
        );
        assert_eq!(never_built.status, HealthState::Down);
        assert_eq!(never_built.reason, Some(FailureCategory::Missing));

        let refused = sidecar_row(
            "echo_sidecar",
            true,
            Some(Liveness::FailedToSpawn(io::ErrorKind::PermissionDenied)),
        );
        assert_eq!(refused.reason, Some(FailureCategory::PermissionDenied));
    }

    /// The case that motivated the work. When the email hook barrier fails, `main.rs` returns before
    /// `supervise` is ever called — so there is no registry entry at all. The row used to stay green
    /// because the `.exe` was on disk, which is the readout being green precisely when it should
    /// not be.
    #[test]
    fn a_configured_sidecar_nobody_started_is_down_rather_than_green() {
        let row = sidecar_row("email_sidecar", true, None);
        assert_eq!(row.status, HealthState::Down);
        assert_eq!(row.reason, Some(FailureCategory::NotRunning));
    }

    /// The counterpart that must NOT change: a pillar nobody turned on is a state, not a fault.
    #[test]
    fn a_pillar_nobody_turned_on_stays_disabled() {
        let row = sidecar_row("email_sidecar", false, None);
        assert_eq!(row.status, HealthState::Disabled);
        assert_eq!(row.reason, Some(FailureCategory::NotConfigured));
    }

    /// The supervisor writes two fields; `liveness` is the single reading of them. This pins the
    /// pairing that is easy to get wrong: `down` means two different things depending on whether a
    /// spawn error is sitting beside it.
    #[test]
    fn the_supervisors_two_fields_read_as_one_verdict() {
        let mut entry = crate::sidecar::SidecarState {
            name: "echo".to_string(),
            state: "running",
            started_at: Some("now".to_string()),
            last_failure: None,
            last_failure_at: None,
            restarts: 0,
            last_line: None,
            last_line_at: None,
            spawn_error: None,
        };
        assert_eq!(entry.liveness(), Liveness::Running);

        entry.state = "down";
        entry.last_failure = Some("exited: exit code: 1".to_string());
        assert_eq!(entry.liveness(), Liveness::Restarting);

        entry.spawn_error = Some(io::ErrorKind::NotFound);
        assert_eq!(
            entry.liveness(),
            Liveness::FailedToSpawn(io::ErrorKind::NotFound)
        );

        // And the clearing matters as much as the setting: a sidecar that could not start, then
        // started, must stop being reported as a missing binary.
        entry.state = "running";
        entry.spawn_error = None;
        assert_eq!(entry.liveness(), Liveness::Running);
    }

    /// Non-zero exit is `degraded`, never `down`. `--help` and `--version` conventions differ across
    /// CLIs, and the one unacceptable outcome for this probe is crying wolf over a working install.
    #[test]
    fn an_exec_probe_is_only_down_when_the_program_will_not_start() {
        assert_eq!(
            exec_verdict(ExecOutcome::Exited { success: true }),
            Ok(HealthState::Ok)
        );
        assert_eq!(
            exec_verdict(ExecOutcome::Exited { success: false }),
            Ok(HealthState::Degraded)
        );
        assert_eq!(
            exec_verdict(ExecOutcome::TimedOut),
            Ok(HealthState::Degraded)
        );
        assert_eq!(
            exec_verdict(ExecOutcome::SpawnFailed(io::ErrorKind::NotFound)),
            Err(FailureCategory::Missing)
        );
    }

    /// The cache is what keeps a five-second program off a one-second readout, so its expiry is
    /// worth a test of its own rather than being trusted to a comparison written once.
    #[test]
    fn a_stale_exec_verdict_is_not_reused() {
        let now = Instant::now();
        let fresh = now.checked_sub(EXEC_CACHE_TTL / 2).unwrap();
        let stale = now.checked_sub(EXEC_CACHE_TTL * 2).unwrap();

        assert_eq!(
            cached_verdict(Some((fresh, Ok(HealthState::Degraded))), now),
            Some(Ok(HealthState::Degraded))
        );
        assert_eq!(
            cached_verdict(Some((stale, Ok(HealthState::Ok))), now),
            None
        );
        assert_eq!(cached_verdict(None, now), None);
    }

    /// A spawn failure and a disk error carry the same `ErrorKind` and mean different things, which
    /// is exactly the kind of shared vocabulary that gets collapsed by a later tidy-up.
    #[test]
    fn a_missing_program_and_a_missing_directory_are_not_the_same_answer() {
        assert_eq!(
            category_from_spawn(io::ErrorKind::NotFound),
            FailureCategory::Missing
        );
        assert_eq!(
            category_from_io(io::Error::from(io::ErrorKind::NotFound)),
            FailureCategory::NotConfigured
        );
    }
}
