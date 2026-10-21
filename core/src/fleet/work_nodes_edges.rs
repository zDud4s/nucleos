//! `work_nodes`: an `EdgeSource` over the `work_nodes` table (migration 0216; spec 2026-10-20 §4,
//! item 4). A work node's `parent_id` says "this unit of delegated work belongs under that one";
//! the fleet tree gets an edge when both ends resolve to nodes some source sights, even when they
//! are two processes of two harnesses with nothing in common on disk.
//!
//! **How a work node becomes a `NodeKey`.** The table has no harness column, and `POST /nodes`
//! only records `session_id`/`chat_id` (the route sets `run_id` to `None`; the column and
//! `OpenArgs.run_id` exist). So a bare session id could belong to Claude Code or to Codex. The
//! rule, kept free of heuristics:
//!
//! - `session_id` s: candidates `claude-code:s` and `codex-cli:s` (a uuid is sighted under at most one).
//! - `run_id` n: the run's `runs.session_id` gives the same two candidates, plus `daemon:run-n`
//!   (the key `daemon_runs` uses for a run without a native adapter or session id).
//! - neither (both blank), but `chat_id` c: the chat's latest `mode='assistant'` turn at or before
//!   the node's creation, resolved as a run.
//!
//! Every child candidate is paired with every parent candidate; fusion drops the pairs whose ends
//! no source sights (contract rule 3). Pairs where both ends are the same node are not emitted
//! (an orchestrator's children usually run in its own session).
//!
//! **One query per pass** (contract amendment 12): the run sessions and the chat turns are read by
//! `LEFT JOIN`s in the query that reads the node pairs, never by a query per row.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Duration};
use sqlx::SqlitePool;

use super::model::{NodeKey, Timestamp};
use super::source::{Edge, EdgeSource, SourceResult};

/// The native harnesses a bare session id may belong to (contract §2 labels).
const SESSION_HARNESSES: [&str; 2] = ["claude-code", "codex-cli"];
/// The label `daemon_runs` gives runs that have no native adapter or no session id.
const DAEMON_HARNESS: &str = "daemon";
/// The most rows one edge query reads. The table is a bounded tree per task; this is a ceiling
/// against a runaway table, not a page size.
pub(super) const MAX_EDGE_ROWS: i64 = 2000;

pub(super) fn session_keys(session_id: &str) -> Vec<NodeKey> {
    SESSION_HARNESSES.iter().map(|harness| NodeKey::new(harness, session_id)).collect()
}

/// Every key `daemon_runs` could have sighted run `run_id` under, given its `runs.session_id`.
pub(super) fn run_keys(run_id: i64, session_id: Option<&str>) -> Vec<NodeKey> {
    let mut keys = session_id
        .map(str::trim)
        .filter(|session| !session.is_empty())
        .map(session_keys)
        .unwrap_or_default();
    keys.push(NodeKey::new(DAEMON_HARNESS, &format!("run-{run_id}")));
    keys
}

/// Whether a stored RFC 3339 stamp (the house text format; `Z` and `+00:00` both occur) is at or
/// after `since`. An unparseable stamp counts as inside: dropping an edge over a bad date would
/// hide a real parent.
pub(super) fn in_window(stamp: &str, since: Timestamp) -> bool {
    DateTime::parse_from_rfc3339(stamp).map(|at| at >= since).unwrap_or(true)
}

/// A lower bound for SQL text comparison, one hour before `since`: text order is exact only up to
/// the sub-second/offset suffix, so SQL over-selects and `in_window` decides.
pub(super) fn coarse_bound(since: Timestamp) -> String {
    (since - Duration::hours(1)).to_rfc3339()
}

fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

fn dedup_keys(keys: &mut Vec<NodeKey>) {
    let mut seen = HashSet::new();
    keys.retain(|key| seen.insert(key.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(keys: &[NodeKey]) -> Vec<String> {
        keys.iter().map(NodeKey::id).collect()
    }

    #[test]
    fn a_bare_session_id_is_a_candidate_for_every_native_harness() {
        assert_eq!(labels(&session_keys("s1")), ["claude-code:s1", "codex-cli:s1"]);
    }

    #[test]
    fn a_run_is_its_session_candidates_plus_the_daemon_run_key() {
        assert_eq!(
            labels(&run_keys(7, Some("s1"))),
            ["claude-code:s1", "codex-cli:s1", "daemon:run-7"]
        );
        assert_eq!(labels(&run_keys(7, None)), ["daemon:run-7"]);
        assert_eq!(labels(&run_keys(7, Some(""))), ["daemon:run-7"]);
    }

    #[test]
    fn the_window_keeps_unparseable_timestamps_and_drops_old_ones() {
        let since: Timestamp = "2026-10-20T10:00:00Z".parse().unwrap();
        assert!(in_window("2026-10-20T10:00:00.5+00:00", since));
        assert!(in_window("2026-10-20T11:00:00Z", since));
        assert!(!in_window("2026-10-20T09:59:59Z", since));
        assert!(in_window("garbage", since), "a bad stamp degrades to kept, never to lost edges");
    }

    #[test]
    fn the_coarse_sql_bound_is_an_hour_before_since() {
        let since: Timestamp = "2026-10-20T10:00:00Z".parse().unwrap();
        assert!(coarse_bound(since).starts_with("2026-10-20T09:00:00"));
    }
}
