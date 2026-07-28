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

#[derive(Clone)]
pub struct AppState {
    pub token: Token,
    pub pool: SqlitePool,
    pub runner: Arc<dyn CommandRunner>,
    /// In-flight runs' abort handles, keyed by `runs.id`. Inserted when a run's task spawns
    /// (`runs::create_run`), removed when it completes/times out/is cancelled.
    pub run_handles: RunHandles,
    pub run_timeout: Duration,
}
