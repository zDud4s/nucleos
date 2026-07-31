use crate::auth::Token;
use crate::runner::CommandRunner;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::AbortHandle;

/// Production default for how long a single run may take before it's marked `"timed_out"` and its
/// process killed. Tests override `AppState.run_timeout` to something much shorter.
///
/// This is the clock for a run somebody is waiting on. Autonomous runs get a longer one — see
/// [`AUTONOMOUS_RUN_TIMEOUT_MULTIPLIER`].
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(600);

/// How much longer an autonomous run may take than an interactive one.
///
/// A wall clock and a progress deadline answer different questions. The progress deadline
/// ([`DEFAULT_PROGRESS_TIMEOUT`]) is the one that catches a run that has stopped getting anywhere;
/// the wall clock is a backstop against a run that keeps making progress and never finishes. Set
/// below the length of an ordinary task, a backstop stops being a backstop and becomes the usual
/// way runs end.
///
/// **Measured, 2026-07-30.** Three `worktree` runs of one real task were killed at the 600-second
/// wall clock with 400+ streamed events each and sub-tasks still open, while the same task in
/// `real` mode finished in 20 turns. The failure was the deadline, not the runs: nothing about
/// them was stuck, and each one spent money the daemon then had to write off. A worktree run does
/// setup, edits, a build and a gate; 10 minutes does not buy that.
///
/// 30 minutes is a bound, not a measurement — nobody has yet run one to completion to find out
/// what it needs. It is three times the observed floor and still finite, which is the property
/// that matters: an autonomous run must not be able to run all day.
pub const AUTONOMOUS_RUN_TIMEOUT_MULTIPLIER: u32 = 3;

/// Production default for how long a run may stay silent between streamed events.
///
/// This measures silence, not total run duration. The independent `DEFAULT_RUN_TIMEOUT` remains
/// unchanged at 600 seconds.
pub const DEFAULT_PROGRESS_TIMEOUT: Duration = Duration::from_secs(300);

/// Production default for how long a repository verification gate may run.
///
/// A gate is a subprocess over a repository the daemon does not control, so its deadline answers a
/// different question from how long the agent itself may run and must remain independently chosen.
pub const DEFAULT_GATE_TIMEOUT: Duration = Duration::from_secs(900);

/// In-flight runs' abort handles, keyed by `runs.id`.
pub type RunHandles = Arc<Mutex<HashMap<i64, AbortHandle>>>;

/// In-flight steerable runs' turn channels, keyed by `runs.id`.
pub type RunMessages = Arc<Mutex<HashMap<i64, tokio::sync::mpsc::UnboundedSender<String>>>>;

/// The email pillar's process-wide settings, resolved once at startup.
///
/// Grouped into one struct rather than spread across `AppState` because they are read together and
/// change together: spec §3.4 makes this config startup-time on purpose, so editing `.ai/email.yaml`
/// means restarting the daemon, never recompiling it.
#[derive(Debug, Clone)]
pub struct EmailRuntime {
    /// False keeps every part of the pillar dormant — no polling, no triage, no digest.
    pub enabled: bool,
    /// Which triage classes are worth a notification. Empty means none, and that is a real setting.
    pub notify_classes: Vec<String>,
    pub digest_hour_utc: u8,
    pub retain_bodies_days: u8,
    /// The mailbox the sidecar collects from.
    ///
    /// Kept here so something can be ASKED which one it is. The shell reads the collection cursor
    /// per mailbox and had no route that reported the configured name, so it hard-coded `INBOX` —
    /// which reads empty, not wrong, for anyone collecting from anywhere else. That is the worst
    /// shape a wrong answer can take: it looks like nothing has arrived.
    pub mailbox: String,
    /// The mailbox the user's own sent mail is read from, when one is configured.
    pub sent_mailbox: Option<String>,
    /// The IMAP host and account, for saying WHICH mailbox this is. Never the password: that comes
    /// from Credential Manager, is handed to the sidecar process, and is not in this struct to leak.
    pub host: String,
    pub username: String,
    pub poll_interval_secs: u64,
    /// The directory a triage run works in, so it never inherits the daemon's (spec §5.5).
    pub sandbox: std::path::PathBuf,
    /// The mail organization folder, canonicalised once so every containment check compares
    /// against a path the filesystem has already resolved.
    pub files_root: std::path::PathBuf,
    /// Set once the hook barrier has been PROVEN at startup, and read by the triage loop before
    /// every batch.
    ///
    /// `enabled` alone is not enough to authorise reading mail into a prompt: the pillar's premise
    /// is that untrusted content never meets a tool, and an unverified barrier is not a barrier. The
    /// loop itself runs regardless, because it also owns retention — bodies already stored do not
    /// stop needing to expire because the barrier failed.
    pub armed: Arc<std::sync::atomic::AtomicBool>,
}

impl Default for EmailRuntime {
    /// The pillar off, which is what every context that has not configured it should see —
    /// including every test that does not care about email.
    fn default() -> Self {
        Self {
            enabled: false,
            notify_classes: vec!["urgent".to_string()],
            digest_hour_utc: 7,
            retain_bodies_days: 14,
            mailbox: "INBOX".to_string(),
            sent_mailbox: None,
            host: String::new(),
            username: String::new(),
            poll_interval_secs: 300,
            sandbox: std::path::PathBuf::new(),
            files_root: std::path::PathBuf::new(),
            armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl EmailRuntime {
    pub fn from_config(
        config: &crate::config::EmailConfig,
        sandbox: std::path::PathBuf,
        files_root: std::path::PathBuf,
    ) -> Self {
        Self {
            enabled: config.enabled,
            notify_classes: config.notify_classes.clone(),
            digest_hour_utc: config.digest_hour_utc,
            retain_bodies_days: config.retain_bodies_days,
            mailbox: config.mailbox.clone(),
            sent_mailbox: config.sent_mailbox.clone(),
            host: config.host.clone(),
            username: config.username.clone(),
            poll_interval_secs: config.poll_interval_secs,
            sandbox,
            files_root,
            armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub token: Token,
    pub pool: SqlitePool,
    pub runner: Arc<dyn CommandRunner>,
    /// Present only when startup proved a configured loopback model can hold the full local triage
    /// prompt. `None` preserves the CLI path, which is the feature's ship-dark default.
    pub triage_runner: Option<Arc<dyn CommandRunner>>,
    /// Why local triage is unavailable, when a local model WAS configured but could not be trusted.
    ///
    /// This is deliberately separate from `triage_runner` being `None`. `None` alone means "no local
    /// model configured", and that correctly falls back to the CLI — the ship-dark guarantee.
    /// `Some(reason)` means the operator ASKED for local inference and it could not be provided, and
    /// the whole reason they asked is that message bodies must not leave this machine. Falling back
    /// to the remote CLI there would violate that at exactly the moment nobody is watching, so triage
    /// stops instead and mail queues, the same way it already does for the kill switch and the budget.
    pub local_triage_disabled: Option<String>,
    /// Read-only after startup, so it is shared rather than copied per clone of the state.
    pub email: Arc<EmailRuntime>,
    /// The voice pillar's settings, its transcriber and its HTTP client, resolved once at startup.
    ///
    /// One field rather than three because they are read together and switched on together, and
    /// because `VoiceRuntime::default()` means "off" — which is what keeps every test that does not
    /// care about voice from having to know it exists. Same reasoning as `email` above.
    pub voice: Arc<crate::voice::VoiceRuntime>,
    /// In-flight runs' abort handles, keyed by `runs.id`. Inserted when a run's task spawns
    /// (`runs::create_run`), removed when it completes/times out/is cancelled.
    pub run_handles: RunHandles,
    /// In-flight steerable runs' turn channels, keyed by `runs.id`. Inserted when a run that opted
    /// in spawns (`runs::spawn_run`), removed beside the abort handle when its task ends, so the two
    /// maps never disagree about which runs are still listening.
    ///
    /// Carries the caller's text, not the CLI's wire format: framing a turn is `runner.rs`'s job, so
    /// the receiving end wraps it. An entry here is plumbing and never permission —
    /// `http::post_run_message` decides on the run's own recorded facts and only then looks for the
    /// channel.
    pub run_messages: RunMessages,
    /// Maximum silence between streamed events; independent of the total wall-clock run timeout.
    pub progress_timeout: Duration,
    pub run_timeout: Duration,
}
