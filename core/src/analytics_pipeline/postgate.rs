//! Metric 7, the post-merge gate (spec analytics-pipeline §6.1 "7. Pós-merge"). "Now" comes from
//! `postgate_state` (D7: a stopped recorder must not make the screen lie about the present);
//! "over the window" comes from `postgate_transitions`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

use super::{rfc3339, stats};
use crate::analytics::Window;
use crate::execution::vcs::REVERT_KIND;

const STATE_SQL: &str = "SELECT project_id, red_groups, red_phase, culprit_sha FROM postgate_state
  WHERE (?1 IS NULL OR project_id = ?1) ORDER BY project_id";
/// Every change before the window's end: the colour at the window's start needs the last one
/// before it. The bound is a `YYYY-MM-DD`, which sorts against the RFC 3339 `at` as a day does.
const TRANSITIONS_SQL: &str = "SELECT project_id, at, to_state FROM postgate_transitions
  WHERE at < ?1 AND (?2 IS NULL OR project_id = ?2) ORDER BY project_id, at, id";
/// The post-merge gate files its revert as `origin = 'daemon'` (`Origin::Daemon`, whose spelling
/// is private to the queue, so the column's value is written out here as `landing.rs` does).
const REVERTS_SQL: &str = "SELECT COUNT(*) FROM vcs_requests
  WHERE origin = 'daemon' AND op = ?1 AND created_at >= ?2 AND created_at < ?3
    AND (?4 IS NULL OR project_id = ?4)";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectRed {
    pub project_id: String,
    pub red_now: bool,
    pub red_s: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RedDay {
    pub day: String,
    pub partial: bool,
    pub red_s: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Postgate {
    pub red_now: bool,
    pub red_since: Option<String>,
    pub red_phase: Option<String>,
    pub red_groups: Vec<String>,
    pub culprit_sha: Option<String>,
    pub red_s_in_window: i64,
    pub prev_red_s: i64,
    pub red_events: i64,
    pub reverts: i64,
    pub per_project: Vec<ProjectRed>,
    pub red_s_daily: Vec<RedDay>,
}

/// PURE: seconds a project was red inside `[from, to)`, from its ordered changes `(at, to_red)`.
///
/// Colour at `from`: the last change at or before it; before the first change, the opposite of
/// that change; with no change at all, today's colour. A red with no recorded start counts only
/// from `since` (when recording began), never from a guess.
pub(crate) fn red_seconds(
    changes: &[(DateTime<Utc>, bool)],
    red_now: bool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    since: Option<DateTime<Utc>>,
) -> i64 {
    if to <= from {
        return 0;
    }
    let (mut red, mut cursor) = match changes.iter().rev().find(|(at, _)| *at <= from) {
        Some((_, red)) => (*red, from),
        None => (
            changes.first().map_or(red_now, |(_, first)| !first),
            since.map_or(from, |since| since.max(from)),
        ),
    };
    let mut total = 0;
    for (at, to_red) in changes.iter().filter(|(at, _)| *at > from && *at < to) {
        if red && *at > cursor {
            total += (*at - cursor).num_seconds();
        }
        red = *to_red;
        cursor = cursor.max(*at);
    }
    if red && to > cursor {
        total += (to - cursor).num_seconds();
    }
    total
}

/// One project as `postgate_state` and its transitions describe it.
struct Project {
    red_now: bool,
    phase: Option<String>,
    groups: Vec<String>,
    culprit_sha: Option<String>,
    changes: Vec<(DateTime<Utc>, bool)>,
}

impl Project {
    /// The `→ red` that is still open: the last change, when it is a red and the project is red.
    fn red_since(&self) -> Option<DateTime<Utc>> {
        match self.changes.last() {
            Some((at, true)) if self.red_now => Some(*at),
            _ => None,
        }
    }

    fn red_s(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        since: Option<DateTime<Utc>>,
    ) -> i64 {
        red_seconds(&self.changes, self.red_now, from, to, since)
    }
}

pub(crate) async fn read(
    pool: &SqlitePool,
    project: Option<&str>,
    window: Window,
    now: DateTime<Utc>,
    since: Option<DateTime<Utc>>,
) -> sqlx::Result<Postgate> {
    type StateRow = (String, String, Option<String>, Option<String>);
    let states: Vec<StateRow> = sqlx::query_as(STATE_SQL)
        .bind(project)
        .fetch_all(pool)
        .await?;
    let transitions: Vec<(String, String, String)> = sqlx::query_as(TRANSITIONS_SQL)
        .bind(window.to_bound())
        .bind(project)
        .fetch_all(pool)
        .await?;
    let reverts: i64 = sqlx::query_scalar(REVERTS_SQL)
        .bind(REVERT_KIND)
        .bind(window.from_bound())
        .bind(window.to_bound())
        .bind(project)
        .fetch_one(pool)
        .await?;

    // A project is any one with a state row or a recorded change; one with only changes is not
    // red now (its state row is gone).
    let mut projects: BTreeMap<String, Project> = BTreeMap::new();
    for (id, groups, phase, culprit_sha) in states {
        let groups: Vec<String> = serde_json::from_str(&groups).unwrap_or_default();
        projects.insert(
            id,
            Project {
                red_now: !groups.is_empty(),
                phase,
                groups,
                culprit_sha,
                changes: Vec::new(),
            },
        );
    }
    for (id, at, to_state) in &transitions {
        // A row whose moment cannot be read is skipped, never a reason to fail the block.
        let Some(at) = stats::parse_at(at) else {
            continue;
        };
        projects
            .entry(id.clone())
            .or_insert_with(|| Project {
                red_now: false,
                phase: None,
                groups: Vec::new(),
                culprit_sha: None,
                changes: Vec::new(),
            })
            .changes
            .push((at, to_state == "red"));
    }

    let (start, end) = (window.start(), window.end().min(now));
    let previous = window.previous();
    let in_window = |moment: &DateTime<Utc>| *moment >= window.start() && *moment < window.end();
    let red_events = projects
        .values()
        .flat_map(|p| &p.changes)
        .filter(|(at, to_red)| *to_red && in_window(at))
        .count() as i64;

    let per_project: Vec<ProjectRed> = projects
        .iter()
        .map(|(id, p)| ProjectRed {
            project_id: id.clone(),
            red_now: p.red_now,
            red_s: p.red_s(start, end, since),
        })
        .collect();
    let prev_red_s = projects
        .values()
        .map(|p| p.red_s(previous.start(), previous.end().min(now), since))
        .sum();

    let last_day = window.days - 1;
    let red_s_daily = window
        .day_list()
        .into_iter()
        .enumerate()
        .map(|(offset, day)| {
            let from = start + Duration::days(offset as i64);
            let to = (from + Duration::days(1)).min(now);
            RedDay {
                day,
                partial: offset as i64 == last_day,
                red_s: projects.values().map(|p| p.red_s(from, to, since)).sum(),
            }
        })
        .collect();

    // The top-level fields describe the project red the longest: no recorded start first (it is
    // older than the record), then the earliest start, then the id.
    let reds: BTreeSet<_> = projects
        .iter()
        .filter(|(_, p)| p.red_now)
        .map(|(id, p)| (p.red_since(), id.as_str()))
        .collect();
    let headline = reds.first().and_then(|(_, id)| projects.get(*id));

    Ok(Postgate {
        red_now: !reds.is_empty(),
        red_since: headline.and_then(Project::red_since).map(rfc3339),
        red_phase: headline.and_then(|p| p.phase.clone()),
        red_groups: headline.map(|p| p.groups.clone()).unwrap_or_default(),
        culprit_sha: headline.and_then(|p| p.culprit_sha.clone()),
        red_s_in_window: per_project.iter().map(|p| p.red_s).sum(),
        prev_red_s,
        red_events,
        reverts,
        per_project,
        red_s_daily,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::tests::{at, now};
    use crate::analytics_pipeline::landing::tests::{merged, request, Req};

    /// Spec §11: a red is clipped to the window, an open one counts until `to`, one that began
    /// before the window counts from its start.
    #[test]
    fn red_time_clips_to_the_window_and_counts_an_open_red_until_now() {
        let from = at("2026-10-11T00:00:00Z");
        let to = at("2026-10-17T14:03:11Z");
        let changes = [
            (at("2026-10-10T23:00:00Z"), true),  // red before the window...
            (at("2026-10-11T01:00:00Z"), false), // ...green an hour into it: 3600 s
            (at("2026-10-17T14:00:00Z"), true),  // still red at `to`: 191 s
        ];
        assert_eq!(red_seconds(&changes, true, from, to, None), 3600 + 191);
        assert_eq!(red_seconds(&[], false, from, to, None), 0);
        assert_eq!(
            red_seconds(&[], true, from, to, Some(at("2026-10-17T14:00:00Z"))),
            191,
            "a red older than the record counts only from when recording began"
        );
    }

    /// Spec §11: a red with no transition began before the table: `red_since` is null.
    #[tokio::test]
    async fn a_red_that_began_before_the_table_has_no_since_and_says_so() {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO postgate_state (project_id, target, red_groups, red_phase, culprit_sha)
             VALUES ('p', 'master', '[\"core\"]', 'bisect', 'ab12')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let window = Window::new(7, now()).unwrap();
        let block = read(&pool, None, window, now(), None).await.unwrap();
        assert!(block.red_now);
        assert_eq!(block.red_since, None);
        assert_eq!(block.red_phase.as_deref(), Some("bisect"));
        assert_eq!(block.red_groups, ["core"]);
        assert_eq!(block.culprit_sha.as_deref(), Some("ab12"));
        assert_eq!(block.per_project.len(), 1);
    }

    /// `red_since` is the last `→ red` with no later green; events and reverts are window counts.
    #[tokio::test]
    async fn red_events_reverts_and_since_come_from_the_window() {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO postgate_state (project_id, target, red_groups)
             VALUES ('p', 'master', '[\"core\"]')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let changes = [
            ("2026-10-12T10:00:00Z", "red"),
            ("2026-10-12T11:00:00Z", "green"),
            ("2026-10-17T13:00:00Z", "red"),
        ];
        for (moment, to) in changes {
            sqlx::query(
                "INSERT INTO postgate_transitions (project_id, at, to_state) VALUES ('p', ?, ?)",
            )
            .bind(moment)
            .bind(to)
            .execute(&pool)
            .await
            .unwrap();
        }
        let revert = Req {
            op: REVERT_KIND,
            origin: "daemon",
            ..merged()
        };
        request(&pool, revert).await;
        let window = Window::new(7, now()).unwrap();
        let block = read(&pool, None, window, now(), None).await.unwrap();
        assert_eq!(block.red_since.as_deref(), Some("2026-10-17T13:00:00Z"));
        assert_eq!(block.red_events, 2);
        assert_eq!(block.reverts, 1);
        assert_eq!(block.red_s_in_window, 3600 + 3791);
        let today = block.red_s_daily.last().unwrap();
        assert_eq!((today.red_s, today.partial), (3791, true));
        assert_eq!(block.red_s_daily.len(), 7);
    }
}
