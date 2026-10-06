//! PURE per-turn active and explained time, and the listing of slow turns that no rule explains.
//! `turn_stats` runs at ingestion over one session's facts; `list_unexplained` reads the stored rows
//! and decides at listing time, because class medians move as data arrives.
//!
//! Stub: the signatures are fixed and the bodies answer "nothing" until the logic is written.

use sqlx::SqlitePool;

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::SessionFacts;
use crate::devtime_store::{SpanMark, TurnStatRow};

/// A turn that took far longer than its class's median and that no rule explains.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub struct UnexplainedTurn {
    pub stat: TurnStatRow,
    /// The median `active_ms` of the turn's class.
    pub class_median_ms: i64,
}

/// One row per main-lane turn of the session. Stub: none.
pub fn turn_stats(
    _facts: &SessionFacts,
    _marks: &[SpanMark],
    _cfg: &DevtimeRulesConfig,
) -> Vec<TurnStatRow> {
    Vec::new()
}

/// The unexplained turns, largest first. Stub: none.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn list_unexplained(
    _pool: &SqlitePool,
    _cfg: &DevtimeRulesConfig,
    _project_id: Option<&str>,
    _since: Option<&str>,
    _limit: usize,
) -> sqlx::Result<Vec<UnexplainedTurn>> {
    Ok(Vec::new())
}
