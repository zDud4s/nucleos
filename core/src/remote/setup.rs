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
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// PURE: whether `tailscale serve status --json` shows the gateway being published.
pub fn parse_serving(json: &str) -> bool {
    fn mentions(value: &serde_json::Value, needle: &str) -> bool {
        match value {
            serde_json::Value::String(text) => text.contains(needle),
            serde_json::Value::Array(items) => items.iter().any(|item| mentions(item, needle)),
            serde_json::Value::Object(map) => map.values().any(|item| mentions(item, needle)),
            _ => false,
        }
    }
    serde_json::from_str::<serde_json::Value>(json)
        .is_ok_and(|value| mentions(&value, crate::sidecar::REMOTE_ADDR))
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

/// Runs one fixed Tailscale call and returns its output. `Err` carries what the CLI said.
pub async fn run(cli: &Path, call: TailscaleCall) -> Result<String, String> {
    let mut command = tokio::process::Command::new(cli);
    command
        .args(call.args())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes over the app.
    let output = tokio::time::timeout(CLI_TIMEOUT, command.output())
        .await
        .map_err(|_| "tailscale did not answer in time".to_string())?
        .map_err(|error| format!("could not run tailscale: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // `status` prints its JSON even when Tailscale is stopped or logged out and exits non-zero.
    if output.status.success() || (call == TailscaleCall::Status && !stdout.trim().is_empty()) {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr.trim().to_string())
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

/// Whether the gateway is being published right now.
pub async fn is_serving(cli: Option<&Path>) -> bool {
    let Some(cli) = cli else {
        return false;
    };
    run(cli, TailscaleCall::ServeStatus)
        .await
        .is_ok_and(|json| parse_serving(&json))
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

/// Starts the gateway supervisor, replacing a running one so a changed origin takes effect.
/// Returns whether one was started. Must be called inside the tokio runtime.
pub fn launch_gateway(env: Vec<(String, String)>) -> bool {
    if !GATEWAY_ALLOWED.load(Ordering::SeqCst) {
        return false;
    }
    let mut slot = GATEWAY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(old) = slot.take() {
        // Dropping the supervisor drops its child, which is spawned with `kill_on_drop`.
        old.abort();
    }
    *slot = Some(tokio::spawn(crate::sidecar::supervise(
        crate::sidecar::REMOTE.to_string(),
        crate::sidecar::binary(crate::sidecar::REMOTE),
        env,
    )));
    true
}

/// Stops the gateway this process started, if it did.
pub fn stop_gateway() {
    let mut slot = GATEWAY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(old) = slot.take() {
        old.abort();
    }
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
    Storage(sqlx::Error),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOrigin(why) => write!(f, "invalid_origin: {why}"),
            Self::PageMissing => f.write_str("remote_page_missing"),
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

/// Checks everything that can be checked without changing anything: the origin and the page.
/// Keeps the owner's other settings (`dist_dir`, `stream_max_seconds`) from an existing file.
pub fn plan(root: &Path, origin: &str, exe_dir: &Path) -> Result<RemoteConfig, SetupError> {
    let mut config = RemoteConfig::load(&root.join(REMOTE_FILE));
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
pub async fn disable(
    pool: &SqlitePool,
    root: &Path,
    stop: impl FnOnce(),
) -> Result<(), SetupError> {
    let mut config = RemoteConfig::load(&root.join(REMOTE_FILE));
    config.enabled = false;
    write_config(root, &config)?;
    super::disarm(pool).await?;
    stop();
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
        disable(&pool, &root, || stopped = true).await.unwrap();
        assert!(stopped);
        assert!(!super::super::is_armed(&pool).await.unwrap());
        let off = RemoteConfig::load(&root.join(REMOTE_FILE));
        assert!(!off.enabled);
        assert_eq!(off.public_origin, "https://mybox.tail1234.ts.net");

        // Nothing is left half-written.
        assert!(!root.join(format!("{REMOTE_FILE}.tmp")).exists());
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
