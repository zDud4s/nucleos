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

/// In-flight runs' abort handles, keyed by `runs.id`.
pub type RunHandles = Arc<Mutex<HashMap<i64, AbortHandle>>>;

/// The email pillar's process-wide settings, resolved once at startup.
///
/// Grouped into one struct rather than spread across `AppState` because they are read together and
/// change together: spec §3.4 makes this config startup-time on purpose, so editing `.ai/email.yaml`
/// means restarting the daemon, never recompiling it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailRuntime {
    /// False keeps every part of the pillar dormant — no polling, no triage, no digest.
    pub enabled: bool,
    /// Which triage classes are worth a notification. Empty means none, and that is a real setting.
    pub notify_classes: Vec<String>,
    pub digest_hour_utc: u8,
    pub retain_bodies_days: u8,
    /// The directory a triage run works in, so it never inherits the daemon's (spec §5.5).
    pub sandbox: std::path::PathBuf,
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
        }
    }
}

impl EmailRuntime {
    pub fn from_config(config: &crate::config::EmailConfig, sandbox: std::path::PathBuf) -> Self {
        Self {
            enabled: config.enabled,
            notify_classes: config.notify_classes.clone(),
            digest_hour_utc: config.digest_hour_utc,
            retain_bodies_days: config.retain_bodies_days,
            sandbox,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub token: Token,
    pub pool: SqlitePool,
    pub runner: Arc<dyn CommandRunner>,
    /// Read-only after startup, so it is shared rather than copied per clone of the state.
    pub email: Arc<EmailRuntime>,
    /// In-flight runs' abort handles, keyed by `runs.id`. Inserted when a run's task spawns
    /// (`runs::create_run`), removed when it completes/times out/is cancelled.
    pub run_handles: RunHandles,
    pub run_timeout: Duration,
}
