//! Setting remote access up from the app, with no terminal and no hand-written file.
//!
//! The owner used to build the remote page, write `remote.yaml` and run `tailscale serve` by hand.
//! This module does the same three things on request: it finds the Tailscale CLI wherever the
//! platform installs it, reads what Tailscale says about this machine, publishes the gateway on the
//! tailnet, writes `remote.yaml`, and starts the gateway.
//!
//! Tailscale is never driven by free-form arguments. [`TailscaleCall`] is a closed list, each with a
//! fixed argument vector, and `tailscale funnel` is not on it: Funnel would publish the page to the
//! public internet, and no code path here can ask for it. The parsing and the argument lists are
//! pure so tests feed them fixed JSON and never touch a real Tailscale.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use sqlx::SqlitePool;

use super::{REMOTE_FILE, RemoteConfig};

/// How long one Tailscale CLI call may take before it is abandoned.
const CLI_TIMEOUT: Duration = Duration::from_secs(15);

/// Every Tailscale call this daemon makes. A closed set on purpose: see the module comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailscaleCall {
    /// What Tailscale knows about this machine.
    Status,
    /// What `tailscale serve` is publishing now.
    ServeStatus,
    /// Publish the gateway on the tailnet over HTTPS, in the background.
    ServeOn,
    /// Stop publishing what `ServeOn` published.
    ServeOff,
}

impl TailscaleCall {
    /// The fixed argument vector. Always `serve` or `status`; never `funnel`.
    pub fn args(self) -> Vec<String> {
        let words: Vec<&str> = match self {
            Self::Status => vec!["status", "--json"],
            Self::ServeStatus => vec!["serve", "status", "--json"],
            Self::ServeOn => {
                return vec![
                    "serve".to_string(),
                    "--bg".to_string(),
                    "--https=443".to_string(),
                    format!("http://{}", crate::sidecar::REMOTE_ADDR),
                ];
            }
            Self::ServeOff => vec!["serve", "--https=443", "off"],
        };
        words.into_iter().map(str::to_string).collect()
    }
}

/// What the wizard needs to know about Tailscale on this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TailscaleView {
    /// The CLI was found. False is a normal answer, not an error: the owner may use their own HTTPS.
    pub installed: bool,
    /// Tailscale is running and signed in to a tailnet.
    pub signed_in: bool,
    /// `machine.tailnet.ts.net`, lower case, without the trailing dot.
    pub dns_name: Option<String>,
    /// The tailnet has HTTPS certificates enabled (an admin-console switch), so `serve` can work.
    pub https_ok: bool,
}

/// PURE: reads `tailscale status --json`. Unreadable output means "found but not usable".
pub fn parse_status(json: &str) -> TailscaleView {
    let mut view = TailscaleView {
        installed: true,
        ..TailscaleView::default()
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return view;
    };
    view.signed_in = value["BackendState"].as_str() == Some("Running");
    view.dns_name = value["Self"]["DNSName"]
        .as_str()
        .map(|name| name.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|name| !name.is_empty());
    // `CertDomains` is empty until the tailnet turns HTTPS certificates on.
    view.https_ok = value["CertDomains"]
        .as_array()
        .is_some_and(|domains| !domains.is_empty());
    view
}

/// What is published on HTTPS port 443 of this machine's tailnet name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServePort {
    /// Nothing.
    Free,
    /// Only the gateway: every handler proxies to [`crate::sidecar::REMOTE_ADDR`].
    Ours,
    /// Something else (another proxy, a file path, a text, a TCP forward). Not ours to replace.
    Foreign,
}

/// What `tailscale serve status --json` says, reduced to what setup decides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeReading {
    pub port: ServePort,
    /// `AllowFunnel` is true for this host's :443, i.e. it is open to the whole internet.
    pub funnel: bool,
}

/// PURE: reads `tailscale serve status --json` (Tailscale's `ipn.ServeConfig`): top-level `TCP` keyed
/// by port, `Web` keyed by `host:443` with `Handlers` keyed by path, `AllowFunnel` keyed by
/// `host:443`. Nothing configured prints `{}` or nothing at all.
pub fn parse_serve(json: &str) -> Result<ServeReading, String> {
    let text = if json.trim().is_empty() { "{}" } else { json };
    let value: serde_json::Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    if !value.is_object() {
        return Err("tailscale serve status is not a JSON object".to_string());
    }
    let on_443 = |key: &str| key.rsplit_once(':').is_some_and(|(_, port)| port == "443");
    let gateway = format!("http://{}", crate::sidecar::REMOTE_ADDR);

    let mut sites = 0usize;
    let mut all_ours = true;
    if let Some(web) = value["Web"].as_object() {
        for (_, site) in web.iter().filter(|(key, _)| on_443(key.as_str())) {
            sites += 1;
            let ours = site["Handlers"].as_object().is_some_and(|handlers| {
                !handlers.is_empty()
                    && handlers.values().all(|handler| {
                        handler["Proxy"]
                            .as_str()
                            .is_some_and(|proxy| proxy.trim().trim_end_matches('/') == gateway)
                    })
            });
            all_ours &= ours;
        }
    }
    // A TCP entry on 443 with no web site behind it is a raw forward or a leftover: not ours either.
    let port = match (sites, value["TCP"]["443"].is_null()) {
        (0, true) => ServePort::Free,
        (0, false) => ServePort::Foreign,
        _ if all_ours => ServePort::Ours,
        _ => ServePort::Foreign,
    };
    let funnel = value["AllowFunnel"].as_object().is_some_and(|hosts| {
        hosts
            .iter()
            .any(|(key, on)| on_443(key.as_str()) && on.as_bool() == Some(true))
    });
    Ok(ServeReading { port, funnel })
}

/// PURE: whether `tailscale serve status --json` shows the gateway being published.
pub fn parse_serving(json: &str) -> bool {
    parse_serve(json).is_ok_and(|reading| reading.port == ServePort::Ours)
}

/// Why a `serve` call could not turn the gateway on, or could not be allowed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeError {
    /// 443 already serves something that is not the gateway; it is left alone.
    Taken,
    /// 443 is open to the public internet (Funnel). Not what the owner agreed to; setup never
    /// turns it on, off or around.
    Funnel,
    /// The CLI failed, timed out, or printed a page the person has to approve first.
    Failed(CliFailure),
}

/// PURE: whether it is safe to publish the gateway. `Ok(true)`: it already is (nothing to do).
pub fn serve_gate(reading: &ServeReading) -> Result<bool, ServeError> {
    if reading.funnel {
        return Err(ServeError::Funnel);
    }
    match reading.port {
        ServePort::Foreign => Err(ServeError::Taken),
        ServePort::Ours => Ok(true),
        ServePort::Free => Ok(false),
    }
}

/// PURE: the first Tailscale `https://` link in CLI output, which is where `tailscale serve` sends a
/// person whose tailnet never enabled Serve. Whitespace, quotes and trailing punctuation end it.
/// Any other link is not an approval page, and the app must not offer to open it as one.
pub fn approval_url(text: &str) -> Option<String> {
    text.match_indices("https://").find_map(|(start, _)| {
        let rest = &text[start..];
        let end = rest
            .find(|c: char| {
                c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>')
            })
            .unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ',', ';', ')']);
        (url.len() <= 512 && is_tailscale_link(url)).then(|| url.to_string())
    })
}

/// PURE: the host of this `https://` link is Tailscale's. Only plain host characters count: with a
/// userinfo, a port or a backslash in the authority, a browser may not read the host we read.
fn is_tailscale_link(url: &str) -> bool {
    let after_scheme = url.strip_prefix("https://").unwrap_or_default();
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
        && (host == "tailscale.com" || host.ends_with(".tailscale.com"))
}

/// A failed CLI call: what to tell the person, and the approval link if the CLI printed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliFailure {
    pub why: String,
    pub approval_url: Option<String>,
}

/// PURE: a failure that keeps whatever link the CLI had printed on either stream.
fn failure(why: impl Into<String>, stdout: &str, stderr: &str) -> CliFailure {
    CliFailure {
        why: why.into(),
        approval_url: approval_url(&format!("{stdout}\n{stderr}")),
    }
}

fn plain_failure(why: impl Into<String>) -> CliFailure {
    CliFailure {
        why: why.into(),
        approval_url: None,
    }
}

/// PURE: the places the Tailscale CLI lives on `os` (`std::env::consts::OS`), most likely first.
/// `program_files` is the Windows `ProgramFiles` and `ProgramFiles(x86)` values.
pub fn cli_candidates(os: &str, program_files: &[String]) -> Vec<PathBuf> {
    match os {
        "windows" => {
            let roots: Vec<PathBuf> = if program_files.is_empty() {
                vec![PathBuf::from(r"C:\Program Files")]
            } else {
                program_files.iter().map(PathBuf::from).collect()
            };
            roots
                .into_iter()
                .map(|root| root.join("Tailscale").join("tailscale.exe"))
                .collect()
        }
        // The app bundle's binary is also the CLI; the Homebrew and standalone installs sit on PATH.
        "macos" => [
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "/opt/homebrew/bin/tailscale",
            "/usr/local/bin/tailscale",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect(),
        _ => [
            "/usr/bin/tailscale",
            "/usr/sbin/tailscale",
            "/usr/local/bin/tailscale",
            "/snap/bin/tailscale",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect(),
    }
}

/// PURE (the filesystem comes in as `exists`): the first candidate that exists, else a `tailscale`
/// found on `path_var`.
pub fn find_cli(
    os: &str,
    program_files: &[String],
    path_var: Option<&OsStr>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(found) = cli_candidates(os, program_files)
        .into_iter()
        .find(|path| exists(path))
    {
        return Some(found);
    }
    let name = if os == "windows" {
        "tailscale.exe"
    } else {
        "tailscale"
    };
    std::env::split_paths(path_var?)
        .map(|dir| dir.join(name))
        .find(|path| exists(path))
}

/// The CLI on this machine, if any.
pub fn locate_cli() -> Option<PathBuf> {
    let program_files: Vec<String> = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect();
    find_cli(
        std::env::consts::OS,
        &program_files,
        std::env::var_os("PATH").as_deref(),
        |path| path.is_file(),
    )
}

/// The most a CLI call's output is kept at, per stream.
const CLI_OUTPUT_MAX: usize = 64 * 1024;

/// Copies a child's pipe into `into` as it arrives, so a call that is abandoned on the timeout still
/// leaves what it had printed (`output()` would drop it with the future).
fn capture<R>(pipe: Option<R>, into: Arc<Mutex<Vec<u8>>>) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let Some(mut pipe) = pipe else {
            return;
        };
        let mut chunk = [0u8; 4096];
        loop {
            match tokio::io::AsyncReadExt::read(&mut pipe, &mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let mut kept = into.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if kept.len() < CLI_OUTPUT_MAX {
                        kept.extend_from_slice(&chunk[..read]);
                    }
                }
            }
        }
    })
}

/// Runs one fixed Tailscale call and returns its output. `Err` carries what the CLI said, and the
/// link it asked the person to open, if it printed one (also when it was abandoned on the timeout).
pub async fn run(cli: &Path, call: TailscaleCall) -> Result<String, CliFailure> {
    let mut command = tokio::process::Command::new(cli);
    command
        .args(call.args())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes over the app.
    let mut child = command
        .spawn()
        .map_err(|error| plain_failure(format!("could not run tailscale: {error}")))?;
    let out = Arc::new(Mutex::new(Vec::new()));
    let err = Arc::new(Mutex::new(Vec::new()));
    let readers = [
        capture(child.stdout.take(), out.clone()),
        capture(child.stderr.take(), err.clone()),
    ];
    let waited = tokio::time::timeout(CLI_TIMEOUT, child.wait()).await;
    if waited.is_err() {
        let _ = child.start_kill();
    }
    // The pipes close when the child exits or is killed; a grandchild holding one open must not
    // hold this call.
    for reader in readers {
        let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
    }
    let text = |buffer: &Arc<Mutex<Vec<u8>>>| {
        let bytes = buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let (stdout, stderr) = (text(&out), text(&err));
    let status = match waited {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            return Err(failure(
                format!("could not wait for tailscale: {error}"),
                &stdout,
                &stderr,
            ));
        }
        Err(_) => {
            return Err(failure(
                "tailscale did not answer in time",
                &stdout,
                &stderr,
            ));
        }
    };
    // `status` prints its JSON even when Tailscale is stopped or logged out and exits non-zero.
    if status.success() || (call == TailscaleCall::Status && !stdout.trim().is_empty()) {
        return Ok(stdout);
    }
    let said = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    let why = if said.is_empty() {
        format!("tailscale exited with {status}")
    } else {
        said.to_string()
    };
    Err(failure(why, &stdout, &stderr))
}

/// What Tailscale says about this machine. No CLI is reported as `installed: false`, not an error.
pub async fn probe(cli: Option<&Path>) -> TailscaleView {
    let Some(cli) = cli else {
        return TailscaleView::default();
    };
    match run(cli, TailscaleCall::Status).await {
        Ok(json) => parse_status(&json),
        Err(_) => TailscaleView {
            installed: true,
            ..TailscaleView::default()
        },
    }
}

/// What `tailscale serve` is publishing on 443, or why that could not be read.
pub async fn read_serve(cli: &Path) -> Result<ServeReading, CliFailure> {
    let json = run(cli, TailscaleCall::ServeStatus).await?;
    parse_serve(&json)
        .map_err(|why| plain_failure(format!("could not read tailscale serve status: {why}")))
}

/// [`read_serve`] for display: no CLI or an unreadable answer is `None`.
pub async fn serve_reading(cli: Option<&Path>) -> Option<ServeReading> {
    read_serve(cli?).await.ok()
}

/// Whether the gateway is being published right now.
pub async fn is_serving(cli: Option<&Path>) -> bool {
    serve_reading(cli)
        .await
        .is_some_and(|reading| reading.port == ServePort::Ours)
}

/// Publishes the gateway on the tailnet, unless that would touch something that is not ours.
/// `Ok(true)` when this call turned it on (and so is the one to roll it back), `Ok(false)` when it
/// was already published. The port is checked first: `serve --https=443` acts on the whole port.
pub async fn serve_on(cli: &Path) -> Result<bool, ServeError> {
    let reading = read_serve(cli).await.map_err(ServeError::Failed)?;
    if serve_gate(&reading)? {
        return Ok(false);
    }
    run(cli, TailscaleCall::ServeOn)
        .await
        .map_err(ServeError::Failed)?;
    Ok(true)
}

/// Stops publishing, but only when 443 points at our gateway. `Ok(false)`: it did not, so nothing
/// was touched.
pub async fn serve_off(cli: &Path) -> Result<bool, CliFailure> {
    if read_serve(cli).await?.port != ServePort::Ours {
        return Ok(false);
    }
    run(cli, TailscaleCall::ServeOff).await?;
    Ok(true)
}

/// `https://<dns name>`, the origin a browser on the tailnet sees.
pub fn origin_from_dns(dns_name: &str) -> String {
    format!("https://{}", dns_name.trim().trim_end_matches('.'))
}

/* ------------------------------------------------------------- the page -- */

/// PURE: where an installation can keep `dist-remote`, given the directory of the daemon's own
/// executable. The sidecars sit beside that executable; the bundle's resources do not follow them on
/// every platform: Windows keeps them in the install directory, a macOS app in `Contents/Resources`
/// (the executable is in `Contents/MacOS`), a Linux package under `/usr/lib/<product>`.
pub fn dist_candidates(exe_dir: &Path) -> Vec<PathBuf> {
    let mut found = vec![exe_dir.join("dist-remote")];
    if let Some(parent) = exe_dir.parent() {
        found.push(parent.join("Resources").join("dist-remote"));
        found.push(parent.join("lib").join("NucleOS").join("dist-remote"));
        found.push(parent.join("lib").join("nucleos").join("dist-remote"));
    }
    found
}

/// The first candidate that holds a built page (`index.html`).
pub fn find_dist_dir(exe_dir: &Path) -> Option<PathBuf> {
    dist_candidates(exe_dir)
        .into_iter()
        .find(|dir| dir.join("index.html").is_file())
}

/// Where the gateway serves the page from: the configured directory, else the installed layout,
/// else the first candidate (so a missing page reads as the gateway's 404, as it always has).
pub fn resolve_dist_dir(config: &RemoteConfig, exe_dir: &Path) -> PathBuf {
    if let Some(dir) = &config.dist_dir {
        return PathBuf::from(dir);
    }
    find_dist_dir(exe_dir).unwrap_or_else(|| exe_dir.join("dist-remote"))
}

/* ----------------------------------------------------------- the gateway -- */

static GATEWAY: Mutex<Option<tokio::task::JoinHandle<()>>> = Mutex::new(None);
static GATEWAY_ALLOWED: AtomicBool = AtomicBool::new(false);

/// Lets this process run the gateway. Only the primary daemon does (like every sidecar): a second
/// daemon sharing the machine would fight it for the port. Until then [`launch_gateway`] is a no-op.
pub fn allow_gateway() {
    GATEWAY_ALLOWED.store(true, Ordering::SeqCst);
}

/// Whether this process may run the gateway, i.e. [`allow_gateway`] was called. Ask before changing
/// anything: a refused launch after a write would leave "on" with no gateway behind it.
pub fn gateway_allowed() -> bool {
    GATEWAY_ALLOWED.load(Ordering::SeqCst)
}

/// How long a killed gateway gets to release 127.0.0.1:8797. On Windows the port can stay bound a
/// moment after the process is gone, and a replacement that binds early dies and backs off.
const PORT_RELEASE: Duration = Duration::from_millis(300);

/// Stops a gateway supervisor and waits until it is really gone: aborting alone only schedules the
/// drop that kills the child (`kill_on_drop`). The registry row goes too, since the aborted
/// supervisor can no longer write `down` itself.
async fn retire(old: tokio::task::JoinHandle<()>) {
    old.abort();
    let _ = old.await;
    crate::sidecar::forget(crate::sidecar::REMOTE);
    tokio::time::sleep(PORT_RELEASE).await;
}

/// Starts the gateway supervisor, replacing a running one so a changed origin takes effect. The new
/// supervisor first waits for the old one to be gone, so two children never race for the port.
/// Returns whether one was started. Must be called inside the tokio runtime.
pub fn launch_gateway(env: Vec<(String, String)>) -> bool {
    if !gateway_allowed() {
        return false;
    }
    let mut slot = GATEWAY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let old = slot.take();
    *slot = Some(tokio::spawn(async move {
        if let Some(old) = old {
            retire(old).await;
        }
        crate::sidecar::supervise(
            crate::sidecar::REMOTE.to_string(),
            crate::sidecar::binary(crate::sidecar::REMOTE),
            env,
        )
        .await;
    }));
    true
}

/// Stops the gateway this process started, if it did, and returns once its child is gone.
pub async fn stop_gateway() {
    let old = GATEWAY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(old) = old {
        retire(old).await;
    }
}

/// One setup operation at a time (turn on, turn off): they share `remote.yaml`, its staging file,
/// the gateway key and the Tailscale port, and two interleaved ones could leave any of them torn.
static SETUP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Held by the caller for the whole of a turn-on or turn-off.
pub async fn lock_setup() -> tokio::sync::MutexGuard<'static, ()> {
    SETUP_LOCK.lock().await
}

/* ----------------------------------------------------------------- setup -- */

#[derive(Debug)]
pub enum SetupError {
    /// The origin is not one the gateway would accept; the text says why.
    InvalidOrigin(String),
    /// No built remote page was found, so the gateway would answer 404 to everything.
    PageMissing,
    /// `remote.yaml` could not be written.
    Write(String),
    /// `remote.yaml` exists but cannot be read or parsed. Writing over it would throw away the
    /// owner's settings, so nothing is done until they fix or remove it.
    ConfigUnreadable(String),
    Storage(sqlx::Error),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOrigin(why) => write!(f, "invalid_origin: {why}"),
            Self::PageMissing => f.write_str("remote_page_missing"),
            Self::ConfigUnreadable(why) => write!(f, "config_unreadable: {why}"),
            Self::Write(why) => write!(f, "could not write {REMOTE_FILE}: {why}"),
            Self::Storage(error) => write!(f, "storage: {error}"),
        }
    }
}

impl std::error::Error for SetupError {}

impl From<sqlx::Error> for SetupError {
    fn from(error: sqlx::Error) -> Self {
        Self::Storage(error)
    }
}

/// Writes `remote.yaml` whole or not at all.
fn write_config(root: &Path, config: &RemoteConfig) -> Result<(), SetupError> {
    let text =
        serde_yaml::to_string(config).map_err(|error| SetupError::Write(error.to_string()))?;
    let body = format!("# Written by NucleOS (Settings, System, Remote devices).\n{text}");
    std::fs::create_dir_all(root).map_err(|error| SetupError::Write(error.to_string()))?;
    let target = root.join(REMOTE_FILE);
    let staging = root.join(format!("{REMOTE_FILE}.tmp"));
    std::fs::write(&staging, body).map_err(|error| SetupError::Write(error.to_string()))?;
    std::fs::rename(&staging, &target).map_err(|error| {
        let _ = std::fs::remove_file(&staging);
        SetupError::Write(error.to_string())
    })
}

/// The file as it is: absent or only comments is the defaults; a file that exists and does not parse
/// is an error. [`RemoteConfig::load`] answers the defaults for that too, which is right for a
/// daemon starting up (fail closed) and wrong for a writer: it would overwrite the file.
pub fn load_strict(path: &Path) -> Result<RemoteConfig, SetupError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RemoteConfig::default());
        }
        Err(error) => return Err(SetupError::ConfigUnreadable(error.to_string())),
    };
    if text.lines().all(|line| {
        let line = line.trim();
        line.is_empty() || line.starts_with('#')
    }) {
        return Ok(RemoteConfig::default());
    }
    RemoteConfig::parse(&text).map_err(SetupError::ConfigUnreadable)
}

/// Checks everything that can be checked without changing anything: the origin and the page.
/// Keeps the owner's other settings (`dist_dir`, `stream_max_seconds`) from an existing file, and
/// refuses one it cannot read rather than replace it.
pub fn plan(root: &Path, origin: &str, exe_dir: &Path) -> Result<RemoteConfig, SetupError> {
    let mut config = load_strict(&root.join(REMOTE_FILE))?;
    config.enabled = true;
    // The gateway compares bytes, so the origin is written the way a browser writes it.
    config.public_origin = origin.trim().trim_end_matches('/').to_ascii_lowercase();
    config.validate().map_err(SetupError::InvalidOrigin)?;
    if !resolve_dist_dir(&config, exe_dir)
        .join("index.html")
        .is_file()
    {
        return Err(SetupError::PageMissing);
    }
    Ok(config)
}

/// Writes `remote.yaml`, arms the gateway's key and hands the gateway's environment to `start`.
pub async fn commit(
    pool: &SqlitePool,
    root: &Path,
    config: &RemoteConfig,
    exe_dir: &Path,
    daemon_url: &str,
    start: impl FnOnce(Vec<(String, String)>),
) -> Result<(), SetupError> {
    write_config(root, config)?;
    let key = crate::auth::mint_service_token(pool, crate::auth::Service::Remote).await?;
    let dist = resolve_dist_dir(config, exe_dir);
    start(crate::sidecar::remote_env(
        daemon_url,
        &key,
        config,
        &dist.to_string_lossy(),
    ));
    Ok(())
}

/// [`plan`] then [`commit`]: the whole "turn it on" step except publishing on Tailscale.
pub async fn enable(
    pool: &SqlitePool,
    root: &Path,
    origin: &str,
    exe_dir: &Path,
    daemon_url: &str,
    start: impl FnOnce(Vec<(String, String)>),
) -> Result<RemoteConfig, SetupError> {
    let config = plan(root, origin, exe_dir)?;
    commit(pool, root, &config, exe_dir, daemon_url, start).await?;
    Ok(config)
}

/// Turns remote access off: the file says so, the gateway's key is deleted, `stop` runs.
/// The origin stays in the file, so turning it back on needs no typing.
pub async fn disable<Stopped>(
    pool: &SqlitePool,
    root: &Path,
    stop: impl FnOnce() -> Stopped,
) -> Result<(), SetupError>
where
    Stopped: std::future::Future<Output = ()>,
{
    let mut config = load_strict(&root.join(REMOTE_FILE))?;
    config.enabled = false;
    write_config(root, &config)?;
    super::disarm(pool).await?;
    stop().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `tailscale status --json` prints for a signed-in machine whose tailnet has HTTPS on.
    const READY: &str = r#"{
        "Version": "1.80.0",
        "BackendState": "Running",
        "Self": { "DNSName": "Mybox.tail1234.ts.net.", "Online": true },
        "MagicDNSSuffix": "tail1234.ts.net",
        "CertDomains": ["mybox.tail1234.ts.net"]
    }"#;

    fn page(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
    }

    #[test]
    fn setup_reads_a_tailnet_name_from_tailscale_status() {
        let ready = parse_status(READY);
        assert!(ready.installed && ready.signed_in && ready.https_ok);
        assert_eq!(ready.dns_name.as_deref(), Some("mybox.tail1234.ts.net"));
        assert_eq!(
            origin_from_dns(ready.dns_name.as_deref().unwrap()),
            "https://mybox.tail1234.ts.net"
        );

        // Signed in, but the tailnet has not switched HTTPS certificates on.
        let no_https = parse_status(
            r#"{"BackendState":"Running","Self":{"DNSName":"b.t.ts.net."},"CertDomains":[]}"#,
        );
        assert!(no_https.signed_in && !no_https.https_ok);

        let logged_out = parse_status(r#"{"BackendState":"NeedsLogin","Self":{"DNSName":""}}"#);
        assert!(logged_out.installed && !logged_out.signed_in);
        assert_eq!(logged_out.dns_name, None);

        // Garbage from the CLI is "found but unusable", never a panic.
        let garbage = parse_status("not json");
        assert!(garbage.installed && !garbage.signed_in && garbage.dns_name.is_none());

        let serving = r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"mybox.tail1234.ts.net:443":
            {"Handlers":{"/":{"Proxy":"http://127.0.0.1:8797"}}}}}"#;
        assert!(parse_serving(serving));
        assert!(!parse_serving("{}"));
        assert!(!parse_serving(
            r#"{"Web":{"x:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:3000"}}}}}"#
        ));
    }

    #[tokio::test]
    async fn no_tailscale_is_reported_not_an_error() {
        let view = probe(None).await;
        assert_eq!(view, TailscaleView::default());
        assert!(!view.installed);
        assert!(!is_serving(None).await);

        // Nothing at any candidate and nothing on PATH.
        assert_eq!(find_cli("windows", &[], None, |_| false), None);
        assert_eq!(find_cli("linux", &[], None, |_| false), None);
    }

    #[test]
    fn the_cli_is_looked_for_where_each_platform_installs_it() {
        let windows = cli_candidates("windows", &[r"D:\Apps".to_string()]);
        assert_eq!(
            windows,
            vec![
                PathBuf::from(r"D:\Apps")
                    .join("Tailscale")
                    .join("tailscale.exe")
            ]
        );
        assert_eq!(
            cli_candidates("windows", &[]),
            vec![
                PathBuf::from(r"C:\Program Files")
                    .join("Tailscale")
                    .join("tailscale.exe")
            ]
        );
        assert!(cli_candidates("macos", &[]).contains(&PathBuf::from(
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale"
        )));
        assert!(cli_candidates("linux", &[]).contains(&PathBuf::from("/usr/bin/tailscale")));

        // The first existing candidate wins over PATH; PATH is the fallback.
        let path = std::env::join_paths(["/opt/ts/bin"]).unwrap();
        let on_path = PathBuf::from("/opt/ts/bin").join("tailscale");
        assert_eq!(
            find_cli("linux", &[], Some(path.as_os_str()), |p| p == on_path),
            Some(on_path.clone())
        );
        assert_eq!(
            find_cli("linux", &[], Some(path.as_os_str()), |_| true),
            Some(PathBuf::from("/usr/bin/tailscale"))
        );
    }

    #[test]
    fn setup_never_runs_funnel() {
        let calls = [
            TailscaleCall::Status,
            TailscaleCall::ServeStatus,
            TailscaleCall::ServeOn,
            TailscaleCall::ServeOff,
        ];
        for call in calls {
            let args = call.args();
            assert!(
                args.iter()
                    .all(|arg| !arg.to_ascii_lowercase().contains("funnel")),
                "{call:?}: {args:?}"
            );
            assert!(matches!(args[0].as_str(), "serve" | "status"), "{call:?}");
        }
        assert_eq!(
            TailscaleCall::ServeOn.args(),
            ["serve", "--bg", "--https=443", "http://127.0.0.1:8797"]
        );
    }

    #[test]
    fn setup_finds_the_page_in_the_installed_layout() {
        let tmp = tempfile::tempdir().unwrap();

        // Windows: the resource sits in the install directory, beside the daemon.
        let windows = tmp.path().join("win");
        page(&windows.join("dist-remote"));
        assert_eq!(find_dist_dir(&windows), Some(windows.join("dist-remote")));

        // macOS: the daemon is in Contents/MacOS, the page in Contents/Resources.
        let mac_exe = tmp
            .path()
            .join("NucleOS.app")
            .join("Contents")
            .join("MacOS");
        std::fs::create_dir_all(&mac_exe).unwrap();
        page(
            &mac_exe
                .parent()
                .unwrap()
                .join("Resources")
                .join("dist-remote"),
        );
        assert_eq!(
            find_dist_dir(&mac_exe),
            Some(
                mac_exe
                    .parent()
                    .unwrap()
                    .join("Resources")
                    .join("dist-remote")
            )
        );

        // Linux package: /usr/bin and /usr/lib/NucleOS.
        let usr = tmp.path().join("usr");
        std::fs::create_dir_all(usr.join("bin")).unwrap();
        page(&usr.join("lib").join("NucleOS").join("dist-remote"));
        assert_eq!(
            find_dist_dir(&usr.join("bin")),
            Some(usr.join("lib").join("NucleOS").join("dist-remote"))
        );

        // A directory with no index.html is not a page; and a configured directory always wins.
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(empty.join("dist-remote")).unwrap();
        assert_eq!(find_dist_dir(&empty), None);
        assert_eq!(
            resolve_dist_dir(&RemoteConfig::default(), &empty),
            empty.join("dist-remote")
        );
        let configured = RemoteConfig {
            dist_dir: Some("/elsewhere".to_string()),
            ..RemoteConfig::default()
        };
        assert_eq!(
            resolve_dist_dir(&configured, &windows),
            PathBuf::from("/elsewhere")
        );
    }

    #[tokio::test]
    async fn setup_writes_remote_yaml_and_starts_the_gateway() {
        let pool = crate::testdb::fresh_pool().await;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("home");
        let exe_dir = tmp.path().join("app");
        page(&exe_dir.join("dist-remote"));
        let mut started: Option<Vec<(String, String)>> = None;

        let config = enable(
            &pool,
            &root,
            "https://MyBox.tail1234.ts.net/",
            &exe_dir,
            "http://127.0.0.1:8791",
            |env| started = Some(env),
        )
        .await
        .unwrap();

        assert_eq!(config.public_origin, "https://mybox.tail1234.ts.net");
        let written = RemoteConfig::load(&root.join(REMOTE_FILE));
        assert!(written.enabled);
        assert_eq!(written.public_origin, "https://mybox.tail1234.ts.net");
        assert!(written.validate().is_ok());
        assert!(super::super::is_armed(&pool).await.unwrap());

        let env = started.expect("the gateway was started");
        let get = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        assert_eq!(
            get("REMOTE_PUBLIC_ORIGIN").as_deref(),
            Some("https://mybox.tail1234.ts.net")
        );
        assert_eq!(
            get("REMOTE_DIST_DIR").map(PathBuf::from),
            Some(exe_dir.join("dist-remote"))
        );
        // The gateway holds its own key, never anything else.
        assert!(get("NUCLEOS_DAEMON_TOKEN").unwrap().starts_with("remote."));

        // Turning it off keeps the origin, drops the key and stops the gateway.
        let mut stopped = false;
        disable(&pool, &root, || async { stopped = true })
            .await
            .unwrap();
        assert!(stopped);
        assert!(!super::super::is_armed(&pool).await.unwrap());
        let off = RemoteConfig::load(&root.join(REMOTE_FILE));
        assert!(!off.enabled);
        assert_eq!(off.public_origin, "https://mybox.tail1234.ts.net");

        // Nothing is left half-written.
        assert!(!root.join(format!("{REMOTE_FILE}.tmp")).exists());
    }

    /// What `tailscale serve status --json` printed here with the gateway published.
    const SERVE_OURS: &str = r#"{
        "TCP": { "443": { "HTTPS": true } },
        "Web": { "mybox.tail1234.ts.net:443": { "Handlers": { "/": { "Proxy": "http://127.0.0.1:8797" } } } }
    }"#;

    #[test]
    fn serve_status_tells_ours_from_foreign_and_free() {
        let ours = parse_serve(SERVE_OURS).unwrap();
        assert_eq!(
            ours,
            ServeReading {
                port: ServePort::Ours,
                funnel: false
            }
        );
        assert!(parse_serving(SERVE_OURS));

        // Nothing configured: `{}`, or no output at all.
        for empty in ["{}", "", "  \n"] {
            assert_eq!(
                parse_serve(empty).unwrap().port,
                ServePort::Free,
                "{empty:?}"
            );
        }

        // Another proxy, a different port, a path, a text, a second path next to ours.
        for foreign in [
            r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"x.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:3000"}}}}}"#,
            r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"x.ts.net:443":{"Handlers":{"/":{"Path":"/srv/www"}}}}}"#,
            r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"x.ts.net:443":{"Handlers":{"/":{"Text":"hi"}}}}}"#,
            r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"x.ts.net:443":{"Handlers":{
                "/":{"Proxy":"http://127.0.0.1:8797"},"/other":{"Proxy":"http://127.0.0.1:3000"}}}}}"#,
            r#"{"TCP":{"443":{"TCPForward":"127.0.0.1:22"}}}"#,
            r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"x.ts.net:443":{"Handlers":{}}}}"#,
        ] {
            assert_eq!(
                parse_serve(foreign).unwrap().port,
                ServePort::Foreign,
                "{foreign}"
            );
            assert!(!parse_serving(foreign), "{foreign}");
        }

        // Another port is not our business: only 443 is acted on.
        let other_port = r#"{"TCP":{"8443":{"HTTPS":true}},"Web":{"x.ts.net:8443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:3000"}}}}}"#;
        assert_eq!(parse_serve(other_port).unwrap().port, ServePort::Free);

        assert!(parse_serve("not json").is_err());
        assert!(parse_serve("[1]").is_err());
    }

    #[test]
    fn serve_status_reports_a_publicly_exposed_port() {
        let funnel = r#"{
            "TCP": { "443": { "HTTPS": true } },
            "Web": { "mybox.tail1234.ts.net:443": { "Handlers": { "/": { "Proxy": "http://127.0.0.1:8797" } } } },
            "AllowFunnel": { "mybox.tail1234.ts.net:443": true }
        }"#;
        let reading = parse_serve(funnel).unwrap();
        assert_eq!(reading.port, ServePort::Ours);
        assert!(reading.funnel);
        assert_eq!(serve_gate(&reading), Err(ServeError::Funnel));

        // Explicitly false, or another port, is not exposed.
        let off = SERVE_OURS.replace(
            "\"Web\"",
            r#""AllowFunnel": {"mybox.tail1234.ts.net:443": false, "x:8443": true}, "Web""#,
        );
        assert!(!parse_serve(&off).unwrap().funnel);
    }

    #[test]
    fn serve_is_only_turned_on_over_a_free_port_or_our_own() {
        let reading = |port, funnel| ServeReading { port, funnel };
        assert_eq!(serve_gate(&reading(ServePort::Free, false)), Ok(false));
        assert_eq!(serve_gate(&reading(ServePort::Ours, false)), Ok(true));
        assert_eq!(
            serve_gate(&reading(ServePort::Foreign, false)),
            Err(ServeError::Taken)
        );
        // Funnel wins over everything else.
        assert_eq!(
            serve_gate(&reading(ServePort::Free, true)),
            Err(ServeError::Funnel)
        );
        assert_eq!(
            serve_gate(&reading(ServePort::Foreign, true)),
            Err(ServeError::Funnel)
        );
    }

    #[test]
    fn an_approval_link_is_pulled_out_of_cli_output() {
        let printed = "\nServe is not enabled on your tailnet.\nTo enable, visit:\n\n         https://login.tailscale.com/f/serve?node=n123abc\n\n";
        assert_eq!(
            approval_url(printed).as_deref(),
            Some("https://login.tailscale.com/f/serve?node=n123abc")
        );
        assert_eq!(
            approval_url("go to (https://login.tailscale.com/x), then retry.").as_deref(),
            Some("https://login.tailscale.com/x")
        );
        // A docs link before it is skipped, and no other host is ever offered as an approval.
        assert_eq!(
            approval_url("see https://example.com/help or https://login.tailscale.com/y")
                .as_deref(),
            Some("https://login.tailscale.com/y")
        );
        assert_eq!(approval_url("see https://example.com/x"), None);
        assert_eq!(approval_url("https://tailscale.com.evil.example/x"), None);
        assert_eq!(
            approval_url("https://login.tailscale.com@evil.example/x"),
            None
        );
        assert_eq!(approval_url("https://eviltailscale.com/x"), None);
        assert_eq!(approval_url("https://evil.example\\.tailscale.com/x"), None);
        assert_eq!(
            approval_url("https://evil.example:x@login.tailscale.com/"),
            None
        );
        assert_eq!(approval_url("no link here, http://insecure.example"), None);
        assert_eq!(approval_url("https://"), None);
        assert_eq!(approval_url(""), None);

        // Either stream can carry it, and a failure keeps it.
        let from_stdout = failure("did not answer", printed, "");
        assert_eq!(
            from_stdout.approval_url.as_deref(),
            Some("https://login.tailscale.com/f/serve?node=n123abc")
        );
        let from_stderr = failure("exit 1", "", printed);
        assert_eq!(from_stderr.approval_url, from_stdout.approval_url);
        assert_eq!(failure("exit 1", "", "boom").approval_url, None);
    }

    #[test]
    fn a_malformed_remote_yaml_is_refused_not_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("home");
        let exe_dir = tmp.path().join("app");
        page(&exe_dir.join("dist-remote"));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join(REMOTE_FILE);

        let broken = "enabled: [oops\npublic_origin: \"https://mine.example.com\"\n";
        std::fs::write(&file, broken).unwrap();
        let result = plan(&root, "https://mybox.tail1234.ts.net", &exe_dir);
        assert!(
            matches!(result, Err(SetupError::ConfigUnreadable(_))),
            "{result:?}"
        );
        assert!(matches!(
            load_strict(&file),
            Err(SetupError::ConfigUnreadable(_))
        ));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);

        // Absent, empty and comment-only files are the defaults, not errors.
        std::fs::write(&file, "# only a note\n\n").unwrap();
        assert!(!load_strict(&file).unwrap().enabled);
        std::fs::remove_file(&file).unwrap();
        assert!(!load_strict(&file).unwrap().enabled);
    }

    #[tokio::test]
    async fn turning_off_refuses_a_malformed_remote_yaml_and_leaves_it() {
        let pool = crate::testdb::fresh_pool().await;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(REMOTE_FILE), "enabled: [oops").unwrap();
        let mut stopped = false;
        let result = disable(&pool, tmp.path(), || async { stopped = true }).await;
        assert!(
            matches!(result, Err(SetupError::ConfigUnreadable(_))),
            "{result:?}"
        );
        assert!(!stopped);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(REMOTE_FILE)).unwrap(),
            "enabled: [oops"
        );
    }

    #[tokio::test]
    async fn setup_operations_are_serialized() {
        let first = lock_setup().await;
        assert!(SETUP_LOCK.try_lock().is_err(), "a second operation got in");
        drop(first);
        assert!(SETUP_LOCK.try_lock().is_ok());
    }

    #[tokio::test]
    async fn stopping_the_gateway_waits_for_it_and_clears_its_row() {
        allow_gateway();
        assert!(gateway_allowed());
        assert!(launch_gateway(Vec::new()));
        // The supervisor records a row as soon as it first tries to spawn (running, or down when
        // the binary is not built).
        let mut seen = false;
        for _ in 0..40 {
            if crate::sidecar::liveness_of(crate::sidecar::REMOTE).is_some() {
                seen = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(seen, "the supervisor never recorded the gateway");

        stop_gateway().await;
        assert_eq!(crate::sidecar::liveness_of(crate::sidecar::REMOTE), None);
        assert!(GATEWAY.lock().unwrap().is_none());

        // A relaunch over a running one retires the old supervisor before the new one starts.
        assert!(launch_gateway(Vec::new()));
        assert!(launch_gateway(Vec::new()));
        stop_gateway().await;
        assert_eq!(crate::sidecar::liveness_of(crate::sidecar::REMOTE), None);
    }

    #[tokio::test]
    async fn setup_refuses_a_non_https_origin() {
        let pool = crate::testdb::fresh_pool().await;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("home");
        let exe_dir = tmp.path().join("app");
        page(&exe_dir.join("dist-remote"));

        for origin in [
            "http://mybox.tail1234.ts.net",
            "https://mybox.ts.net/app",
            "mybox.ts.net",
            "",
        ] {
            let mut started = false;
            let result = enable(
                &pool,
                &root,
                origin,
                &exe_dir,
                "http://127.0.0.1:8791",
                |_| started = true,
            )
            .await;
            assert!(
                matches!(result, Err(SetupError::InvalidOrigin(_))),
                "{origin:?}: {result:?}"
            );
            assert!(!started, "{origin:?}");
        }
        assert!(!root.join(REMOTE_FILE).exists());
        assert!(!super::super::is_armed(&pool).await.unwrap());

        // A valid origin with no built page is refused as well: the gateway would 404 everything.
        let bare = tmp.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        let result = enable(
            &pool,
            &root,
            "https://mybox.tail1234.ts.net",
            &bare,
            "http://127.0.0.1:8791",
            |_| {},
        )
        .await;
        assert!(matches!(result, Err(SetupError::PageMissing)), "{result:?}");
        assert!(!root.join(REMOTE_FILE).exists());
    }
}
