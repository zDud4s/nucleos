//! A red that is not the branch's although the base is green: a test that also failed in other
//! worktrees in the last 24 hours, whose diffs do not touch the test's file (autonomy-trees spec,
//! S5). It extends `inherit::inherited_red`, the one place a tree branch's gate asks "whose red is
//! this?": the answer is the same `Inherited` verdict, quarantined for that gate only. A global
//! ticket is opened per test so the flake is not forgotten. Only a branch's own gate asks: the
//! integrated tip's final verify never does, so there the test counts again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::SqlitePool;

use crate::task_claims::{keys, machine};
use crate::verify::failures::Failures;
use crate::verify::plan::Ticket;
use crate::verify::resolve;

/// How far back another worktree's red still counts.
pub const WINDOW_HOURS: i64 = 24;
/// How many other worktrees must have seen the same test fail.
pub const MIN_WORKTREES: usize = 2;
/// The newest failed rows read; older ones are outside any 24 h window on a busy machine anyway.
const ROWS_READ: i64 = 500;
/// Every git read of one decision shares this budget.
const GIT_BUDGET: Duration = Duration::from_secs(20);

/// What one other worktree reported failing inside the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elsewhere {
    pub worktree: String,
    pub tests: BTreeSet<String>,
    /// The files the failures pointed at (`Failures::files`).
    pub files: BTreeSet<String>,
}

/// The failed rows of the project's other worktrees finished inside the window, merged per worktree.
pub async fn recent_reds(
    pool: &SqlitePool,
    project_id: &str,
    own_worktree: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<Elsewhere>> {
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT worktree, failed_tests, finished_at FROM verify_runs \
         WHERE status = 'failed' AND failed_tests IS NOT NULL AND project_id = ? AND worktree != ? \
         ORDER BY id DESC LIMIT ?",
    )
    .bind(project_id)
    .bind(own_worktree)
    .bind(ROWS_READ)
    .fetch_all(pool)
    .await?;
    let since = now - chrono::Duration::hours(WINDOW_HOURS);
    let mut by_worktree: BTreeMap<String, Elsewhere> = BTreeMap::new();
    for (worktree, json, finished_at) in rows {
        let recent = finished_at
            .and_then(|at| DateTime::parse_from_rfc3339(&at).ok())
            .is_some_and(|at| at >= since);
        let Ok(failures) = serde_json::from_str::<Failures>(&json) else {
            continue;
        };
        if !recent {
            continue;
        }
        let entry = by_worktree
            .entry(worktree.clone())
            .or_insert_with(|| Elsewhere {
                worktree,
                tests: BTreeSet::new(),
                files: BTreeSet::new(),
            });
        entry.tests.extend(failures.tests);
        entry.files.extend(failures.files);
    }
    Ok(by_worktree.into_values().collect())
}

/// Whether every one of `failed` failed in at least [`MIN_WORKTREES`] of `others` (distinct
/// worktrees, already filtered to those whose diff leaves the test's file alone).
pub fn failed_elsewhere(failed: &BTreeSet<String>, others: &[Elsewhere]) -> bool {
    !failed.is_empty()
        && failed.iter().all(|id| {
            others
                .iter()
                .filter(|other| other.tests.contains(id))
                .count()
                >= MIN_WORKTREES
        })
}

/// Whether a diff touches any of the failure files. A runner may print a path relative to a crate,
/// so a suffix match on a path boundary counts too: when in doubt the diff touches.
fn touches(changed: &[String], files: &BTreeSet<String>) -> bool {
    changed.iter().any(|path| {
        files.iter().any(|file| {
            path == file
                || path.ends_with(&format!("/{file}"))
                || file.ends_with(&format!("/{path}"))
        })
    })
}

/// The note of a quarantine, when every failed test of the branch's red also failed in at least two
/// other worktrees (diffs clear of the test's file) in the last 24 h. Opens one global ticket per
/// test first; a ticket that cannot be opened leaves the red the branch's, so nothing is quarantined
/// unseen. Nothing is answered when the base or a diff is unreadable.
pub async fn quarantine_note(
    pool: &SqlitePool,
    ticket: &Ticket,
    failed: &BTreeSet<String>,
    now: DateTime<Utc>,
) -> Option<String> {
    let base = ticket.base.clone()?;
    let mut others = recent_reds(pool, &ticket.project_id, &ticket.worktree, now)
        .await
        .inspect_err(|error| tracing::warn!(%error, "verify: cannot read other worktrees' reds"))
        .ok()?;
    others.retain(|other| !other.tests.is_disjoint(failed));
    if others.len() < MIN_WORKTREES {
        return None;
    }
    let deadline = Instant::now() + GIT_BUDGET;
    let own_changed = resolve::changed_paths(Path::new(&ticket.worktree), &base, deadline)
        .await
        .ok()?;
    let mut clear = Vec::new();
    for other in others {
        // No file named: nothing proves the diffs stay clear of the test, so it is no evidence.
        if other.files.is_empty() || touches(&own_changed, &other.files) {
            continue;
        }
        let Ok(changed) = resolve::changed_paths(Path::new(&other.worktree), &base, deadline).await
        else {
            continue;
        };
        if !touches(&changed, &other.files) {
            clear.push(other);
        }
    }
    if !failed_elsewhere(failed, &clear) {
        return None;
    }
    let mut opened = Vec::new();
    for id in failed {
        let key = keys::normalize(&format!("topic:flaky-{id}"), Path::new("")).ok()?;
        let files: Vec<String> = clear
            .iter()
            .filter(|other| other.tests.contains(id))
            .flat_map(|other| other.files.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let worktrees: Vec<&str> = clear
            .iter()
            .filter(|other| other.tests.contains(id))
            .map(|other| other.worktree.as_str())
            .collect();
        let detail = json!({
            "test": id,
            "worktrees": worktrees,
            "window_hours": WINDOW_HOURS,
            "quarantined_for": "branch gates; counts again at the integrated tip",
        });
        let title = format!(
            "test {id} fails in {} worktrees whose diffs do not touch it",
            worktrees.len()
        );
        let task =
            machine::open_machine_ticket(pool, &ticket.project_id, key, title, detail, files, now)
                .await
                .inspect_err(
                    |error| tracing::warn!(%error, "verify: cannot open the flaky-test ticket"),
                )
                .ok()?;
        opened.push(format!("#{}", task.id));
    }
    Some(format!(
        "quarantined for this gate: every failed test also failed in at least {MIN_WORKTREES} other \
         worktrees in the last {WINDOW_HOURS} h whose diffs do not touch it (global ticket {}); it \
         counts again at the integrated tip",
        opened.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn elsewhere(worktree: &str, tests: &[&str]) -> Elsewhere {
        Elsewhere {
            worktree: worktree.to_owned(),
            tests: set(tests),
            files: set(&["core/src/a.rs"]),
        }
    }

    #[test]
    fn two_other_worktrees_with_the_test_make_it_a_flake() {
        let others = [
            elsewhere("w1", &["a::t"]),
            elsewhere("w2", &["a::t", "b::u"]),
        ];
        assert!(failed_elsewhere(&set(&["a::t"]), &others));
    }

    #[test]
    fn one_other_worktree_is_not_enough() {
        let others = [elsewhere("w1", &["a::t"]), elsewhere("w2", &["b::u"])];
        assert!(!failed_elsewhere(&set(&["a::t"]), &others));
    }

    #[test]
    fn every_failed_test_needs_its_two_worktrees() {
        let others = [
            elsewhere("w1", &["a::t", "a::mine"]),
            elsewhere("w2", &["a::t"]),
        ];
        assert!(!failed_elsewhere(&set(&["a::t", "a::mine"]), &others));
        assert!(!failed_elsewhere(&BTreeSet::new(), &others));
    }

    #[test]
    fn a_diff_touches_a_file_by_path_or_by_suffix() {
        let files = set(&["src/a.rs"]);
        assert!(touches(&["src/a.rs".to_owned()], &files));
        assert!(touches(&["core/src/a.rs".to_owned()], &files));
        assert!(!touches(&["core/src/b.rs".to_owned()], &files));
        assert!(!touches(&[], &files));
    }

    async fn seed(pool: &SqlitePool, worktree: &str, tests: &[&str], finished_at: DateTime<Utc>) {
        let failures = Failures {
            tests: set(tests),
            files: set(&["core/src/a.rs"]),
        };
        let id = sqlx::query(
            "INSERT INTO verify_runs (project_id, worktree, scope, origin, requested_by, argv, \
             status, finished_at) VALUES ('p', ?, 'scope', 'verify', 'job', 'x', 'failed', ?)",
        )
        .bind(worktree)
        .bind(finished_at.to_rfc3339())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        crate::verify::runs::set_failed_tests(pool, id, &failures)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn recent_reds_merges_per_other_worktree_inside_the_window() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now();
        seed(&pool, "w1", &["a::t"], now - chrono::Duration::hours(1)).await;
        seed(&pool, "w1", &["a::u"], now - chrono::Duration::hours(2)).await;
        seed(&pool, "w2", &["a::t"], now - chrono::Duration::hours(23)).await;
        seed(&pool, "old", &["a::t"], now - chrono::Duration::hours(25)).await;
        seed(&pool, "own", &["a::t"], now).await;

        let reds = recent_reds(&pool, "p", "own", now).await.unwrap();

        let names: Vec<&str> = reds.iter().map(|red| red.worktree.as_str()).collect();
        assert_eq!(names, ["w1", "w2"]);
        assert_eq!(reds[0].tests, set(&["a::t", "a::u"]));
        assert!(failed_elsewhere(&set(&["a::t"]), &reds));
    }

    #[tokio::test]
    async fn another_project_s_reds_do_not_count() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now();
        seed(&pool, "w1", &["a::t"], now).await;
        assert!(
            recent_reds(&pool, "q", "own", now)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
