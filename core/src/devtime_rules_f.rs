//! Family F, across sessions (F1, F2): command sequences and failures that recur in several sessions
//! of one project, which no single session can show. PURE over the rows `devtime_store::cross_session_rows`
//! returns; the engine stores what it finds with `scope = 'cross'` and never projects it onto spans.
//!
//! Stub: finds nothing until the rules are written.

use crate::config::DevtimeRulesConfig;
use crate::devtime_store::CrossAttempt;

/// A recurring pattern, with the sessions it spans. Times are epoch milliseconds.
#[derive(Debug, Clone)]
pub struct CrossFinding {
    pub rule_id: &'static str,
    pub sessions: Vec<String>,
    pub latest_session: String,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub cost_ms: i64,
    pub count: i64,
    pub attempt_ids: Vec<String>,
    /// The sequence, or the `(class, program)` key, the finding is anchored on.
    pub anchor: String,
}

pub fn run_cross(_rows: &[CrossAttempt], _cfg: &DevtimeRulesConfig) -> Vec<CrossFinding> {
    Vec::new()
}
