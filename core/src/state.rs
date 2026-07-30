use crate::auth::Token;
use crate::runner::CommandRunner;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::AbortHandle;

/// Production default for how long a single run may take before it's marked `"timed_out"` and its
/// process killed. Tests override `AppState.run_timeout` to something much shorter.
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(600);

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
    /// Maximum silence between streamed events; independent of the total wall-clock run timeout.
    pub progress_timeout: Duration,
    pub run_timeout: Duration,
}
