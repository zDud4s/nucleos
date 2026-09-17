//! §spec pilar-de-browser

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::Notify;

/// The supervisor's key for each sidecar, named once.
///
/// `main.rs` writes these when it spawns a supervisor and `health.rs` reads them when it reports a
/// row, and the two files agreeing is load-bearing: a drifted literal would leave a sidecar row
/// reading `not running` forever with nothing actually wrong. A shared constant is a cheaper
/// guarantee than a test that watches for the drift.
pub const ECHO: &str = "echo";
pub const TELEGRAM: &str = "telegram";
pub const EMAIL: &str = "email";
pub const WEB: &str = "web";
pub const BROWSER: &str = "browser";

/// The two values [`SidecarState::state`] takes, written once because it is serialized to the shell.
const RUNNING: &str = "running";
const DOWN: &str = "down";

/// First delay after a sidecar dies. A single crash should cost about as much as a restart.
const RESTART_BASE: Duration = Duration::from_secs(2);
/// Ceiling for the backoff: long enough that a binary which was never built is nearly free, short
/// enough that a sidecar which starts working again is back within a minute.
const RESTART_MAX: Duration = Duration::from_secs(60);

/// How long a sidecar has to stay up before the supervisor treats it as having genuinely started.
///
/// Anything shorter is a process that failed during startup — a bad config, a port already taken, a
/// missing credential — and those do not fix themselves by being tried again immediately.
const HEALTHY_UPTIME: Duration = Duration::from_secs(60);

/// PURE: how long to wait before the next attempt, given how long the process that just ended lived.
///
/// A spawn failure passes `Duration::ZERO`: it lived for no time at all, which is the same thing
/// this says about a process that died during startup. That is deliberate — the two failures differ
/// in how they are reported, not in how often it is worth retrying them.
fn next_delay(lived: Duration, current: Duration) -> Duration {
    if lived >= HEALTHY_UPTIME {
        RESTART_BASE
    } else {
        (current * 2).min(RESTART_MAX)
    }
}

/// What each supervised sidecar is doing, as its own supervisor last saw it.
///
/// Process-wide rather than a field on `AppState`, the way `assistant::BUSY_CHATS` is: there is one
/// set of sidecars per daemon, not per handler state, and the supervisors are spawned as free tasks
/// that outlive every request.
///
/// It exists because a dead sidecar was previously visible nowhere. The supervisor restarts it and
/// writes one line to a log; if the email poller was failing to start, the Mail tab simply looked
/// like a quiet mailbox — the same thing an empty inbox looks like.
static SIDECARS: LazyLock<Mutex<BTreeMap<String, SidecarState>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Every sidecar this daemon has spawned, held by the kernel so that they die when it does.
///
/// `kill_on_drop` below covers the orderly shutdown and covers nothing else: `TerminateProcess` —
/// which is what Stop-Process, Task Manager and a crash all are — runs no destructor. MEASURED,
/// 2026-08-20: two days of daemon restarts had left 31 orphaned sidecars alive, and because an
/// orphan keeps its loopback port, every freshly spawned replacement died at `bind` and was
/// "restarted" forever. The browser sidecar the app was actually talking to was two days old and
/// would have stayed that way through any number of restarts, with `/sidecars` reporting `running`
/// — which was true, and was about the wrong process.
///
/// Process-wide for the same reason `SIDECARS` is: there is one set of sidecars per daemon. See
/// [`crate::process_tree::Litter`] for the primitive and for what it does not promise off Windows.
static LITTER: LazyLock<crate::process_tree::Litter> =
    LazyLock::new(crate::process_tree::Litter::new);

/// What the supervisor last observed, reduced to what a readiness row needs to grade it.
///
/// Carries `io::ErrorKind` rather than a `health::FailureCategory` deliberately: this module reports
/// what the operating system said, and the vocabulary a user reads is `health.rs`'s to choose.
/// Keeping that translation in one place is what stops one spawn failure being described two ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// `spawn` returned `Ok`, and the child has not been observed to exit.
    Running,
    /// Ran and died. The supervisor is waiting out its backoff before trying again.
    Restarting,
    /// Never started at all — usually a binary that was never built.
    FailedToSpawn(io::ErrorKind),
}

/// One sidecar, and what has happened to it since the daemon started.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SidecarState {
    pub name: String,
    /// `running`, or `down` while it is backing off before the next attempt.
    pub state: &'static str,
    /// When the current process started. `None` means it is not running right now.
    pub started_at: Option<String>,
    /// What went wrong last: an exit status, or the error that stopped it starting at all.
    ///
    /// Kept even while the sidecar is running again, because "up, but it has crashed nine times"
    /// is a different situation from "up", and only one of them is fine.
    pub last_failure: Option<String>,
    pub last_failure_at: Option<String>,
    /// How many times this sidecar has been restarted since the daemon started.
    pub restarts: u32,
    /// The last line this sidecar printed, and when it printed it.
    ///
    /// The field that closes the gap this module was already half-way through: `last_failure` says
    /// how a child *ended*, and a sidecar whose every poll fails while the process stays up never
    /// ends. The email poller is the case that made it matter — it logs `email: poll failed: …` and
    /// keeps running, so the panel read `running` for eight days while no mail arrived.
    ///
    /// One line rather than a buffer. What a reader needs here is "is it still saying something
    /// wrong"; the history is in the log file, which now has these lines too. A ring buffer in
    /// process-wide state would be a second, worse log.
    pub last_line: Option<String>,
    pub last_line_at: Option<String>,
    /// The kind of the error that stopped it STARTING, when that is what went wrong.
    ///
    /// `last_failure` above is for a person and carries the message; this is for `health.rs`, which
    /// has to map a failure to a category without parsing prose. Not serialized, because the panel
    /// already shows the sentence and an `ErrorKind` is not one.
    ///
    /// Cleared on every successful spawn and on every exit, so it only ever describes the failure
    /// the sidecar is sitting in right now. Left uncleared, a sidecar that once could not start,
    /// then ran, then crashed would be reported as a missing binary forever after.
    #[serde(skip)]
    pub spawn_error: Option<io::ErrorKind>,
}

impl SidecarState {
    /// What `health.rs` needs in order to grade this row.
    ///
    /// `state` and `spawn_error` are the fields the supervisor writes; this is the one reading of
    /// them, so the two never drift into disagreeing about the same child.
    pub fn liveness(&self) -> Liveness {
        match (self.state, self.spawn_error) {
            (RUNNING, _) => Liveness::Running,
            (_, Some(kind)) => Liveness::FailedToSpawn(kind),
            _ => Liveness::Restarting,
        }
    }
}

/// Every supervised sidecar, in name order.
pub fn states() -> Vec<SidecarState> {
    SIDECARS.lock().unwrap().values().cloned().collect()
}

/// What the supervisor last saw of `name`, or `None` when nothing ever supervised it.
///
/// `None` is load-bearing and must not be collapsed into a state here. A configured sidecar with no
/// entry is one that nobody ever started — the email pillar whose hook barrier failed, say — and
/// surfacing that is half the reason this registry exists.
///
/// A poisoned lock answers `None` rather than panicking: the readout degrades to "nobody started
/// this", which is the same conservative answer it gives before the first spawn.
pub fn liveness_of(name: &str) -> Option<Liveness> {
    Some(SIDECARS.lock().ok()?.get(name)?.liveness())
}

fn record(name: &str, update: impl FnOnce(&mut SidecarState)) {
    let mut sidecars = SIDECARS.lock().unwrap();
    let entry = sidecars
        .entry(name.to_owned())
        .or_insert_with(|| SidecarState {
            name: name.to_owned(),
            state: DOWN,
            started_at: None,
            last_failure: None,
            last_failure_at: None,
            restarts: 0,
            last_line: None,
            last_line_at: None,
            spawn_error: None,
        });
    update(entry);
}

/// The longest line kept from a sidecar's output.
///
/// A ceiling rather than a guess about line length: this text is now written to a log file that has
/// retention but no per-line limit, and one child printing a megabyte on a loop would fill the disk
/// through a path nothing else guards. Generous enough that a Go stack trace's first line survives.
const MAX_LINE_BYTES: usize = 2_000;

/// PURE: one line of a sidecar's output, made safe to keep.
///
/// Two jobs, both of which have to happen before the line reaches a log file or the shell:
///
/// - **Credentials come out.** A sidecar's environment holds the daemon token and, for email, the
///   IMAP password; the ordinary way those escape is a connection error quoting the URL it failed on
///   (`imaps://user:hunter2@host`). Until now that output only ever reached a console nobody kept.
///   Writing it to a file that rotates daily and is read later is a different proposition, so
///   `redact_url` is applied on the way in.
/// - **Length is bounded**, on a character boundary so the result is still a `String`.
///
/// `redact_url` is applied per whitespace-separated token, not to the line. It was written for a
/// string that IS a URL: given `dial imaps://u:p@host failed` it finds `://`, reads the scheme as
/// `dial imaps`, rejects it for the space, and hands the line back untouched — password included.
/// Splitting first is what puts a real URL in front of it.
///
/// The consequence is that the redactor also eats a bare `someone@example.com` down to its domain,
/// because that is what `redact_url` does with anything carrying userinfo. That is the right
/// direction to err in for this particular text: it is a mail sidecar's output, it now persists in
/// a file, and `redact.rs` already forbids logging what a message says.
///
/// Returns `None` for a line that is only whitespace: Go's `log` and a flushing writer both emit
/// those, and a panel that shows the sidecar's "last line" as an empty string reads as a bug.
fn keepable_line(raw: &str) -> Option<String> {
    let trimmed = raw.trim_end_matches(['\r', '\n']).trim();
    if trimmed.is_empty() {
        return None;
    }
    let redacted = trimmed
        .split_whitespace()
        .map(crate::redact::redact_url)
        .collect::<Vec<_>>()
        .join(" ");
    if redacted.len() <= MAX_LINE_BYTES {
        return Some(redacted);
    }
    // `floor_char_boundary` is unstable, so the cut is found by walking back to one.
    let mut cut = MAX_LINE_BYTES;
    while cut > 0 && !redacted.is_char_boundary(cut) {
        cut -= 1;
    }
    Some(format!("{}…", &redacted[..cut]))
}

/// Forwards one of a child's output streams to the log and to its `SidecarState`.
///
/// Runs as its own task so it cannot delay `child.wait()`, and ends by itself: the read hits EOF
/// when the child closes the pipe, which is exactly when there is nothing left to say.
///
/// `stderr` is logged at WARN and `stdout` at INFO, because that is what the Go sidecars mean by
/// them — the standard library's `log` writes to stderr, so every `email: poll failed: …` arrives
/// on that stream. Both are recorded as the last line: what a reader wants is the last thing the
/// process said, not the last thing it said on one particular pipe.
async fn pump<R>(name: String, stream: &'static str, reader: R)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;

    let mut lines = tokio::io::BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(raw)) => {
                let Some(line) = keepable_line(&raw) else {
                    continue;
                };
                if stream == "stderr" {
                    tracing::warn!(sidecar = %name, stream, output = %line, "sidecar output");
                } else {
                    tracing::info!(sidecar = %name, stream, output = %line, "sidecar output");
                }
                let at = chrono::Utc::now().to_rfc3339();
                record(&name, |entry| {
                    entry.last_line = Some(line);
                    entry.last_line_at = Some(at);
                });
            }
            // EOF: the child closed this pipe, which for a dying process is the ordinary ending.
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(sidecar = %name, stream, %error, "could not read sidecar output");
                break;
            }
        }
    }
}

/// The variable that says where the sidecar binaries are, for a daemon that does not sit beside them.
pub const SIDECAR_DIR_VAR: &str = "NUCLEOS_SIDECAR_DIR";

/// Where the sidecar whose executable is called `file` is, for this daemon.
///
/// Beside the daemon's own executable unless [`SIDECAR_DIR_VAR`] says otherwise. The default is the
/// layout `scripts/build-sidecars.sh` produces and an installation ships; the variable is for a
/// daemon built anywhere else. That is not rare on this machine: a build that must not overwrite the
/// running daemon's binary goes to a target directory of its own, which has no sidecars in it, and
/// on 2026-09-09 a daemon swapped in from one came up with all five down.
pub fn binary(file: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("the daemon can name its own executable");
    binary_in(std::env::var_os(SIDECAR_DIR_VAR).as_deref(), &exe, file)
}

/// PURE: [`binary`], given what the environment and the executable's own path said. An empty
/// variable is an unset one, the way a shell that exported `NUCLEOS_SIDECAR_DIR=` meant it.
fn binary_in(configured: Option<&std::ffi::OsStr>, exe: &Path, file: &str) -> PathBuf {
    match configured.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(file),
        None => exe.parent().unwrap_or(Path::new(".")).join(file),
    }
}

/// PURE: what a sidecar that could not start records. It names the path, because "the system cannot
/// find the file specified" does not say which file, and which file is the whole diagnosis.
fn spawn_failure(binary_path: &Path, error: &io::Error) -> String {
    let remedy = if error.kind() == io::ErrorKind::NotFound {
        format!(
            " (build it with scripts/build-sidecars.sh, or set {SIDECAR_DIR_VAR} to where it is)"
        )
    } else {
        String::new()
    };
    format!("could not start {}: {error}{remedy}", binary_path.display())
}

pub async fn supervise(name: String, binary_path: PathBuf, env: Vec<(String, String)>) {
    let mut delay = RESTART_BASE;
    let mut attempts: u32 = 0;
    loop {
        let mut cmd = Command::new(&binary_path);
        for (k, v) in &env {
            cmd.env(k, v);
        }
        // A sidecar's environment holds the daemon token, and for email the IMAP password too.
        // Without this, shutting the daemon down left the process running with both. It is the
        // orderly half only — see LITTER for the half that survives being terminated.
        cmd.kill_on_drop(true);
        // Piped rather than inherited, which is what it was. Inheriting sent every sidecar's output
        // to the daemon's own console and nowhere else: not the log file, not `/sidecars`, not the
        // health readout. A poller that logged a failure on every cycle and stayed up was therefore
        // reported as `running` with no failure at all, which is the one shape this registry exists
        // to make impossible.
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        match cmd.spawn() {
            Ok(mut child) => {
                // Adopted before anything else is done with it, and while the `Child` is still held
                // — which is what stops Windows reusing the pid between the spawn and the adoption.
                if let Some(pid) = child.id() {
                    LITTER.adopt(pid);
                }
                // Taken before `wait()`, which needs the child mutably and would otherwise hold the
                // handles for as long as the process lives.
                if let Some(stdout) = child.stdout.take() {
                    tokio::spawn(pump(name.clone(), "stdout", stdout));
                }
                if let Some(stderr) = child.stderr.take() {
                    tokio::spawn(pump(name.clone(), "stderr", stderr));
                }
                let started_at = chrono::Utc::now().to_rfc3339();
                let launched = std::time::Instant::now();
                let restarts = attempts;
                record(&name, |entry| {
                    entry.state = RUNNING;
                    entry.started_at = Some(started_at);
                    entry.restarts = restarts;
                    entry.spawn_error = None;
                });
                let status = child.wait().await;
                tracing::warn!(sidecar = %name, ?status, "sidecar exited — restarting");
                let failed_at = chrono::Utc::now().to_rfc3339();
                let detail = match &status {
                    Ok(status) => format!("exited: {status}"),
                    Err(error) => format!("could not be waited on: {error}"),
                };
                record(&name, |entry| {
                    entry.state = DOWN;
                    entry.started_at = None;
                    entry.last_failure = Some(detail);
                    entry.last_failure_at = Some(failed_at);
                    entry.spawn_error = None;
                });
                // Ran and then died is a different event from cannot start: a rare crash should
                // restart promptly rather than inherit a backoff earned by something else. But
                // only a sidecar that actually RAN has earned that — resetting on every exit
                // whatsoever turned a binary that dies during startup into a process launch and a
                // log line every two seconds, for as long as the machine stayed on. How long it
                // lived is what tells the two apart.
                delay = next_delay(launched.elapsed(), delay);
            }
            Err(error) => {
                // Exponential, because the usual cause is a binary that was never built — `main.rs`
                // supervises the echo sidecar unconditionally, built or not. At a flat two seconds
                // that wrote a warning every two seconds forever, roughly 43k lines a day, into a
                // log directory that had no retention either.
                tracing::warn!(
                    sidecar = %name,
                    %error,
                    ?delay,
                    "sidecar failed to spawn — backing off"
                );
                let failed_at = chrono::Utc::now().to_rfc3339();
                let detail = spawn_failure(&binary_path, &error);
                let kind = error.kind();
                record(&name, |entry| {
                    entry.state = DOWN;
                    entry.started_at = None;
                    entry.last_failure = Some(detail);
                    entry.last_failure_at = Some(failed_at);
                    entry.spawn_error = Some(kind);
                });
                delay = next_delay(Duration::ZERO, delay);
            }
        }
        attempts = attempts.saturating_add(1);
        delay = wait_out(&name, delay).await;
    }
}

/// `daemon_token` is the control token here, deliberately, where the email sidecar gets a scoped
/// one. This sidecar is the user's remote control: it approves proposals, works the kill switch and
/// cancels runs — the shell's surface, reached from a phone. An allowlist for it would be all of
/// Control minus a few routes, which reads like a boundary without being one. Narrowing it means
/// first deciding what a chat message is allowed to do, which is a product decision.
pub fn telegram_env(
    daemon_url: &str,
    daemon_token: &str,
    bot_token: &str,
) -> Vec<(String, String)> {
    vec![
        ("NUCLEOS_DAEMON_URL".to_string(), daemon_url.to_string()),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), daemon_token.to_string()),
        ("TELEGRAM_BOT_TOKEN".to_string(), bot_token.to_string()),
    ]
}

/// Where the email sidecar serves attachments, and where the núcleo asks for them.
///
/// A constant rather than configuration because it is one fact shared by two processes, and a fact
/// with two homes eventually has two values. 8793 follows the daemon (8791) and the echo sidecar
/// (8792). Loopback is not a default here — the sidecar refuses to bind anything else.
pub const EMAIL_FETCH_ADDR: &str = "127.0.0.1:8793";

/// Where the web sidecar answers the núcleo.
///
/// A constant for the same reason `EMAIL_FETCH_ADDR` is: one fact shared by two processes, and a
/// fact with two homes eventually has two values. 8794 follows email's attachment listener (8793).
/// Loopback is not a default — `requireLoopback` on the Go side refuses to bind anything else,
/// because a process that fetches arbitrary URLs and listens off-machine is an open proxy with the
/// owner's address on it.
pub const WEB_ADDR: &str = "127.0.0.1:8794";

/// Where the browser sidecar answers the núcleo. 8795 follows the web sidecar (8794).
///
/// A constant for the same reason the two above are, with the stake raised: this process drives
/// browsers holding the owner's logged-in profiles, so `requireLoopback` on the Go side refuses to
/// bind anything else. A listener off this machine would hand those sessions to whoever asked.
pub const BROWSER_ADDR: &str = "127.0.0.1:8795";

/// The browser sidecar's environment (spec §8).
///
/// The site lists are deliberately ABSENT, exactly as the trust allowlist is absent from
/// [`web_env`] and for a sharper version of the same reason. The sidecar enforces a fence per
/// session, against the list the núcleo sends WITH that session — so there is one list, in one
/// place, read at the moment it is used. A copy in the environment would be a second allowlist that
/// only changes when the process restarts, and the one that drifts is always the one nobody reads.
pub fn browser_env(
    daemon_url: &str,
    daemon_token: &str,
    config: &crate::config::BrowserConfig,
) -> Vec<(String, String)> {
    vec![
        ("NUCLEOS_DAEMON_URL".to_string(), daemon_url.to_string()),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), daemon_token.to_string()),
        ("BROWSER_ADDR".to_string(), BROWSER_ADDR.to_string()),
        // The real driver. "fake" is what the sidecar defaults to, and a browser pillar that ran on
        // the fake would answer every question with an invented page — so the daemon names the one
        // it means rather than relying on a default it did not choose.
        ("BROWSER_DRIVER".to_string(), "chrome".to_string()),
        (
            "BROWSER_MAX_SESSIONS".to_string(),
            config.max_sessions.to_string(),
        ),
        (
            "BROWSER_CACHE_MB".to_string(),
            config.cache_size_mb.to_string(),
        ),
        (
            "BROWSER_MAX_PROFILES".to_string(),
            config.max_profiles.to_string(),
        ),
        (
            "BROWSER_DISK_BUDGET_MB".to_string(),
            config.disk_budget_mb.to_string(),
        ),
        (
            "BROWSER_OPEN_TIMEOUT_SECS".to_string(),
            config.load_timeout_seconds.to_string(),
        ),
    ]
}

/// The web sidecar's environment (spec §3.4).
///
/// `search_key` is the provider's API key, from Credential Manager, passed to the child process and
/// never written to a file — the same handling the mail password gets, for the same reason.
///
/// The trust allowlist is deliberately ABSENT from this list. The sidecar fetches; it never decides
/// what may be believed. Sending it the allowlist would put one security decision in two processes,
/// and the copy that drifts is always the one nobody is reading.
pub fn web_env(
    daemon_url: &str,
    daemon_token: &str,
    config: &crate::config::WebConfig,
    search_key: &str,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("NUCLEOS_DAEMON_URL".to_string(), daemon_url.to_string()),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), daemon_token.to_string()),
        ("WEB_ADDR".to_string(), WEB_ADDR.to_string()),
        ("WEB_SEARCH_PROVIDER".to_string(), config.provider.clone()),
        (
            "WEB_FETCH_TIMEOUT_SECS".to_string(),
            config.fetch_timeout_seconds.to_string(),
        ),
        (
            "WEB_MAX_PAGE_BYTES".to_string(),
            config.max_page_bytes.to_string(),
        ),
    ];
    // Emitted only when there is one, like `smtp_env`: an empty key would have the sidecar build a
    // Brave provider that answers 401 to every query, which presents as a broken search rather than
    // an unconfigured one.
    if !search_key.trim().is_empty() {
        env.push(("WEB_BRAVE_KEY".to_string(), search_key.to_string()));
    }
    if !config.searxng_url.trim().is_empty() {
        env.push((
            "WEB_SEARXNG_URL".to_string(),
            config.searxng_url.trim().to_string(),
        ));
    }
    env
}

fn sent_mailbox_env(config: &crate::config::EmailConfig) -> Option<(String, String)> {
    config
        .sent_mailbox
        .as_ref()
        .map(|mailbox| ("EMAIL_SENT_MAILBOX".to_string(), mailbox.clone()))
}

/// The submission server, passed only once somebody has named one.
///
/// Conditional for the same reason `sent_mailbox_env` is, and with a sharper edge: the sidecar
/// decides whether it can send at all by whether this host reached it. Emitting the pair
/// unconditionally would hand it an empty host and a port, which reads like a configured server
/// right up to the point a person is told their message went out.
fn smtp_env(config: &crate::config::EmailConfig) -> Vec<(String, String)> {
    if config.smtp_host.trim().is_empty() {
        return Vec::new();
    }
    vec![
        ("EMAIL_SMTP_HOST".to_string(), config.smtp_host.clone()),
        ("EMAIL_SMTP_PORT".to_string(), config.smtp_port.to_string()),
    ]
}

/// The email sidecar's environment (spec §3.4). This list is the contract between the núcleo and
/// the Go sidecar: it reads nothing from disk and holds no config of its own, so anything it needs
/// is here or it does not exist. The password comes from Credential Manager and never touches a
/// file — it is passed to the child process and nowhere else.
///
/// `daemon_token` is this sidecar's own key (`auth::Service::Email`), not the control token. It
/// opens `/email/cursor` and `/email/incoming` and nothing else, which is the complete set
/// `daemon/client.go` builds a URL for. The process on the other end parses MIME written by
/// strangers; a bug in that parser should cost the mailbox, not the daemon.
pub fn email_env(
    daemon_url: &str,
    daemon_token: &str,
    config: &crate::config::EmailConfig,
    password: &str,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("NUCLEOS_DAEMON_URL".to_string(), daemon_url.to_string()),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), daemon_token.to_string()),
        ("EMAIL_FETCH_ADDR".to_string(), EMAIL_FETCH_ADDR.to_string()),
        ("EMAIL_IMAP_HOST".to_string(), config.host.clone()),
        ("EMAIL_IMAP_PORT".to_string(), config.port.to_string()),
        ("EMAIL_IMAP_USERNAME".to_string(), config.username.clone()),
        ("EMAIL_IMAP_PASSWORD".to_string(), password.to_string()),
        ("EMAIL_MAILBOX".to_string(), config.mailbox.clone()),
        (
            "EMAIL_POLL_INTERVAL_SECS".to_string(),
            config.poll_interval_secs.to_string(),
        ),
    ];
    env.extend(sent_mailbox_env(config));
    env.extend(smtp_env(config));
    env
}

/// A "try now" bell for one sidecar: rung by the restart route, heard by [`supervise`].
///
/// A [`Notify`] and not a channel, for one property. `notify_one` STORES a permit when nobody is
/// waiting, so a bell rung in the gap between the supervisor recording `DOWN` and reaching its sleep
/// is heard AT the sleep rather than lost. A `watch` or a `broadcast` needs its receiver to exist
/// first, and the moment this is rung in is exactly the moment it might not.
///
/// The same property has a cost worth naming: a bell rung just after the supervisor spawned is kept
/// and spent on the NEXT backoff, which skips one wait. That is an early retry after a real death,
/// not a wrong state, and the route refuses a running child anyway — so the window is a race between
/// a person's press and a process coming up, not an ordinary path. Closing it would mean holding a
/// lock across the spawn, which is worse than the thing it fixes.
///
/// Process-wide beside [`SIDECARS`], and for its reason: one set of sidecars per daemon, supervised
/// by free tasks that outlive every request.
static BELLS: LazyLock<Mutex<BTreeMap<String, Arc<Notify>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn bell_for(name: &str) -> Arc<Notify> {
    BELLS
        .lock()
        .unwrap()
        .entry(name.to_owned())
        .or_insert_with(|| Arc::new(Notify::new()))
        .clone()
}

/// What asking a sidecar to restart can answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartOutcome {
    /// The supervisor was told to stop waiting and spawn now.
    Asked,
    /// A child is up, so there was nothing to hurry.
    AlreadyRunning,
    /// Nobody ever supervised this name, so there is no task to ask.
    NotSupervised,
}

/// Ask the supervisor of `name` to stop waiting out its backoff and spawn now.
///
/// **A running child is REFUSED rather than killed, and that is the decision this function exists to
/// hold.** Killing one would need a second registry holding the live [`tokio::process::Child`] —
/// `supervise` owns it on its own stack — and for the browser sidecar it costs every open session
/// (spec §9.1). That is a different feature: *stop this sidecar* rather than *stop waiting*, and a
/// destructive one. The control this serves is drawn only beside a row that reads `down`, so this
/// refusal is the backstop for a sidecar that came up between the render and the press.
///
/// `None` is refused for the sharper reason. `health.rs` reports both a supervisor backing off and a
/// name nothing ever supervised as `not-running`, and only the first has a task listening: ringing
/// the second's bell would answer "asked" and do nothing at all, which is the one answer a restart
/// button must never give.
///
/// [`Liveness::FailedToSpawn`] is allowed, and is the case worth having — a binary that was never
/// built is backing off at the 60 s ceiling, and the person pressing this has usually just built it.
pub fn ask_to_restart(name: &str) -> RestartOutcome {
    match liveness_of(name) {
        None => RestartOutcome::NotSupervised,
        Some(Liveness::Running) => RestartOutcome::AlreadyRunning,
        Some(Liveness::Restarting | Liveness::FailedToSpawn(_)) => {
            bell_for(name).notify_one();
            RestartOutcome::Asked
        }
    }
}

/// Wait out `delay` unless somebody rings the bell first, and answer the delay the next attempt earned.
///
/// A bell heard starts the ladder over at [`RESTART_BASE`] rather than keeping a ceiling earned by a
/// binary that did not exist yet. It is the same judgement [`next_delay`] makes about a process that
/// ran before dying — this is a fresh incident, and the last one's backoff is not evidence about it.
async fn wait_out(name: &str, delay: Duration) -> Duration {
    let bell = bell_for(name);
    tokio::select! {
        _ = tokio::time::sleep(delay) => delay,
        _ = bell.notified() => {
            tracing::info!(sidecar = %name, "a restart was asked for — trying now");
            RESTART_BASE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A sidecar that starts and dies immediately, over and over, must cost less each time.
    ///
    /// The supervisor used to reset the delay on every exit, on the reasoning that a rare crash
    /// should restart promptly rather than inherit a backoff earned by something else. That is true
    /// of a rare crash and false of a permanent one: a binary that exits during startup — bad
    /// config, port taken, missing credential — then respawned every two seconds forever, which is
    /// a process launch and a log line every two seconds for as long as the machine is on. The
    /// spawn-failure path had already learned this; the exit path had not.
    #[test]
    fn a_sidecar_that_dies_during_startup_is_retried_more_slowly_each_time() {
        let mut delay = RESTART_BASE;
        let mut seen = vec![delay];
        for _ in 0..8 {
            delay = next_delay(Duration::from_secs(1), delay);
            seen.push(delay);
        }
        assert!(
            seen.windows(2)
                .all(|pair| pair[1] > pair[0] || pair[1] == RESTART_MAX),
            "a startup crash loop must back off, not hold at one delay: {seen:?}"
        );
        assert_eq!(
            delay, RESTART_MAX,
            "and it must settle at the ceiling rather than growing without bound"
        );
    }

    /// The property the reset existed to protect, kept: a sidecar that worked for a while and then
    /// crashed is back almost at once, and does not inherit a backoff from some earlier trouble.
    #[test]
    fn a_sidecar_that_ran_before_dying_restarts_promptly() {
        assert_eq!(next_delay(HEALTHY_UPTIME, RESTART_MAX), RESTART_BASE);
        assert_eq!(
            next_delay(HEALTHY_UPTIME * 10, RESTART_MAX),
            RESTART_BASE,
            "a long-lived sidecar's first crash is a fresh incident, whatever came before it"
        );
    }

    #[test]
    fn email_env_carries_exactly_the_contract() {
        let config = crate::config::EmailConfig {
            enabled: true,
            host: "imap.gmail.com".into(),
            port: 993,
            smtp_host: "smtp.gmail.com".into(),
            smtp_port: 465,
            username: "me@x.com".into(),
            mailbox: "INBOX".into(),
            poll_interval_secs: 300,
            ..Default::default()
        };
        let env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &config, "app-password")
                .into_iter()
                .collect();

        assert_eq!(env.len(), 11);
        assert_eq!(env["NUCLEOS_DAEMON_URL"], "http://127.0.0.1:8791");
        assert_eq!(env["NUCLEOS_DAEMON_TOKEN"], "tok");
        // The one address both processes have to agree on. The sidecar defaults to the same value,
        // so a mismatch would only ever come from this line drifting.
        assert_eq!(env["EMAIL_FETCH_ADDR"], EMAIL_FETCH_ADDR);
        assert_eq!(env["EMAIL_IMAP_HOST"], "imap.gmail.com");
        assert_eq!(env["EMAIL_IMAP_PORT"], "993");
        assert_eq!(env["EMAIL_IMAP_USERNAME"], "me@x.com");
        assert_eq!(env["EMAIL_IMAP_PASSWORD"], "app-password");
        assert_eq!(env["EMAIL_MAILBOX"], "INBOX");
        assert_eq!(env["EMAIL_POLL_INTERVAL_SECS"], "300");
        // Reading and sending are two servers as often as they are one, so the submission host
        // travels separately from the IMAP one rather than being derived from it.
        assert_eq!(env["EMAIL_SMTP_HOST"], "smtp.gmail.com");
        assert_eq!(env["EMAIL_SMTP_PORT"], "465");
    }

    /// An unconfigured submission host must reach the sidecar as an ABSENCE, not as an empty string.
    ///
    /// The sidecar decides whether it can send at all from whether this variable arrived. A blank
    /// `EMAIL_SMTP_HOST` alongside a perfectly ordinary `EMAIL_SMTP_PORT` looks like a configured
    /// server to anything reading the pair, and the failure surfaces only after somebody has been
    /// told their message went out — which for this pillar is the one failure that cannot be undone.
    #[test]
    fn the_submission_host_reaches_the_sidecar_only_once_somebody_has_named_one() {
        let unconfigured: HashMap<String, String> = email_env(
            "http://127.0.0.1:8791",
            "tok",
            &crate::config::EmailConfig::default(),
            "password",
        )
        .into_iter()
        .collect();
        assert!(
            !unconfigured.contains_key("EMAIL_SMTP_HOST")
                && !unconfigured.contains_key("EMAIL_SMTP_PORT"),
            "an unnamed submission host must leave BOTH variables absent, port included"
        );

        // Whitespace is the same absence wearing a hat: a host of spaces would connect to nothing.
        let blank = crate::config::EmailConfig {
            smtp_host: "   ".into(),
            ..Default::default()
        };
        let blank_env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &blank, "password")
                .into_iter()
                .collect();
        assert!(!blank_env.contains_key("EMAIL_SMTP_HOST"));

        let configured = crate::config::EmailConfig {
            smtp_host: "smtp.example.com".into(),
            smtp_port: 587,
            ..Default::default()
        };
        let configured_env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &configured, "password")
                .into_iter()
                .collect();
        assert_eq!(
            configured_env.get("EMAIL_SMTP_HOST").map(String::as_str),
            Some("smtp.example.com")
        );
        // The port travels with the host and is not assumed on the far side: an operator who moved
        // off 465 did so because their provider left them no choice.
        assert_eq!(
            configured_env.get("EMAIL_SMTP_PORT").map(String::as_str),
            Some("587")
        );
    }

    #[test]
    fn o_sidecar_recebe_a_pasta_de_enviados() {
        let without_sent = crate::config::EmailConfig {
            sent_mailbox: None,
            ..Default::default()
        };
        let without_sent_env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &without_sent, "password")
                .into_iter()
                .collect();
        assert!(
            !without_sent_env.contains_key("EMAIL_SENT_MAILBOX"),
            "an unconfigured sent mailbox must leave EMAIL_SENT_MAILBOX absent"
        );

        let with_sent = crate::config::EmailConfig {
            sent_mailbox: Some("[Gmail]/Sent Mail".into()),
            ..Default::default()
        };
        let with_sent_env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &with_sent, "password")
                .into_iter()
                .collect();
        assert_eq!(
            with_sent_env.get("EMAIL_SENT_MAILBOX").map(String::as_str),
            Some("[Gmail]/Sent Mail")
        );
    }

    #[test]
    fn telegram_env_has_the_three_expected_vars() {
        let env: HashMap<String, String> = telegram_env("http://127.0.0.1:8791", "tok", "bot")
            .into_iter()
            .collect();

        assert_eq!(env.len(), 3);
        assert_eq!(
            env.get("NUCLEOS_DAEMON_URL").map(String::as_str),
            Some("http://127.0.0.1:8791")
        );
        assert_eq!(
            env.get("NUCLEOS_DAEMON_TOKEN").map(String::as_str),
            Some("tok")
        );
        assert_eq!(
            env.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
            Some("bot")
        );
    }

    /// The reason this line is kept at all: a poller that fails on every cycle and stays up.
    ///
    /// Its output used to go to the daemon's console and nowhere a person or the shell would look,
    /// so `state` read `running`, `last_failure` read `None`, and the Mail tab looked like a quiet
    /// mailbox — which is what an empty inbox looks like too.
    #[test]
    fn a_failing_poller_line_survives_intact() {
        assert_eq!(
            keepable_line("email: poll failed: inbound mailbox \"INBOX\": dial tcp: timeout\n"),
            Some("email: poll failed: inbound mailbox \"INBOX\": dial tcp: timeout".to_string()),
        );
    }

    /// Inherited output reached a console nobody kept. A log file that rotates daily and is read
    /// afterwards is a different proposition, so credentials come out on the way in.
    #[test]
    fn credentials_in_a_connection_error_do_not_reach_the_log() {
        let line = keepable_line("dial imaps://duarte:hunter2@imap.gmail.com:993 failed")
            .expect("a line with a URL is still a line");
        assert!(
            !line.contains("hunter2"),
            "the IMAP password must not survive into the log: {line}"
        );
        assert!(
            line.contains("imap.gmail.com"),
            "the host is what makes the error readable and must survive: {line}"
        );
    }

    /// Whitespace-only output is not a thing the sidecar said. A panel showing an empty "last line"
    /// reads as a rendering bug rather than as silence.
    #[test]
    fn blank_output_is_not_recorded_as_something_said() {
        assert_eq!(keepable_line(""), None);
        assert_eq!(keepable_line("   \r\n"), None);
        assert_eq!(keepable_line("\n"), None);
    }

    /// One child printing without bound must not fill a disk through the one path with no limit.
    #[test]
    fn a_runaway_line_is_cut_to_the_ceiling() {
        let kept = keepable_line(&"x".repeat(MAX_LINE_BYTES * 3)).expect("a long line is a line");
        assert!(
            kept.len() <= MAX_LINE_BYTES + '…'.len_utf8(),
            "kept {} bytes, ceiling is {MAX_LINE_BYTES}",
            kept.len()
        );
        assert!(
            kept.ends_with('…'),
            "a cut line must say it was cut: {kept}"
        );
    }

    /// Cutting mid-character would panic on the slice. Multi-byte output is ordinary here — the
    /// sidecars log in whatever language the OS answers in, which on this machine is Portuguese.
    #[test]
    fn a_runaway_line_of_multibyte_characters_is_cut_on_a_boundary() {
        let kept = keepable_line(&"ç".repeat(MAX_LINE_BYTES)).expect("a long line is a line");
        assert!(kept.ends_with('…'));
        assert!(kept.len() <= MAX_LINE_BYTES + '…'.len_utf8());
    }

    /// The property that makes [`Notify`] the right primitive: a permit rung before anybody waits is
    /// kept, so a press landing in the gap between an exit and the sleep is not silently dropped.
    #[tokio::test]
    async fn a_bell_rung_before_anybody_waits_still_ends_the_backoff_and_starts_the_delay_over() {
        bell_for("test-bell").notify_one();
        let next = tokio::time::timeout(
            Duration::from_millis(200),
            wait_out("test-bell", RESTART_MAX),
        )
        .await
        .expect("a bell already rung must not leave the supervisor waiting out a minute");
        assert_eq!(
            next, RESTART_BASE,
            "a restart somebody asked for is a fresh incident, not an inherited ceiling"
        );
    }

    /// And the other half, without which the `select!` could be a no-op nobody would notice.
    #[tokio::test]
    async fn without_a_bell_the_supervisor_waits_out_its_backoff() {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                wait_out("test-silent", RESTART_MAX),
            )
            .await
            .is_err(),
            "an unrung bell must not shorten the wait"
        );
    }

    /// The three answers, and the two that are refusals.
    ///
    /// `not-running` is two situations wearing one word — a supervisor backing off, and a name no
    /// supervisor task exists for — and only one of them has anybody listening for the bell.
    #[test]
    fn a_restart_is_refused_for_a_running_child_and_asked_for_while_it_backs_off() {
        assert_eq!(
            ask_to_restart("test-never-supervised"),
            RestartOutcome::NotSupervised,
        );

        record("test-running", |entry| entry.state = RUNNING);
        assert_eq!(
            ask_to_restart("test-running"),
            RestartOutcome::AlreadyRunning
        );

        record("test-backing-off", |entry| {
            entry.state = DOWN;
            entry.spawn_error = None;
        });
        assert_eq!(ask_to_restart("test-backing-off"), RestartOutcome::Asked);

        // A binary that was never built is the case this is most useful for: it is sitting at the
        // 60 s ceiling, and whoever is pressing the button has just built it.
        record("test-never-built", |entry| {
            entry.state = DOWN;
            entry.spawn_error = Some(io::ErrorKind::NotFound);
        });
        assert_eq!(ask_to_restart("test-never-built"), RestartOutcome::Asked);
    }

    /// Beside the daemon by default, and wherever the variable says when it says anything.
    #[test]
    fn a_sidecar_is_found_beside_the_daemon_unless_told_otherwise() {
        let exe = Path::new("C:/Projects/.cargo-target-branch/debug/nucleos-core.exe");
        let beside = Path::new("C:/Projects/.cargo-target-branch/debug/echo-sidecar.exe");

        assert_eq!(binary_in(None, exe, "echo-sidecar.exe"), beside);
        assert_eq!(
            binary_in(Some(std::ffi::OsStr::new("")), exe, "echo-sidecar.exe"),
            beside
        );
        assert_eq!(
            binary_in(
                Some(std::ffi::OsStr::new("C:/Projects/.cargo-target/debug")),
                exe,
                "echo-sidecar.exe"
            ),
            Path::new("C:/Projects/.cargo-target/debug/echo-sidecar.exe")
        );
    }

    /// The failure a missing sidecar records names the file it looked for, and the two ways out.
    #[test]
    fn a_sidecar_that_cannot_start_says_where_it_looked() {
        let missing = io::Error::from(io::ErrorKind::NotFound);

        let said = spawn_failure(Path::new("C:/x/debug/echo-sidecar.exe"), &missing);

        assert!(said.contains("C:/x/debug/echo-sidecar.exe"), "{said}");
        assert!(said.contains(SIDECAR_DIR_VAR), "{said}");
        let refused = spawn_failure(
            Path::new("C:/x/debug/echo-sidecar.exe"),
            &io::Error::from(io::ErrorKind::PermissionDenied),
        );
        assert!(!refused.contains(SIDECAR_DIR_VAR), "{refused}");
    }
}
