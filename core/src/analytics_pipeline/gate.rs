//! Metrics 4, 5 and 6: the gate over `verify_runs` windowed by `started_at`, the flaky/suspect
//! snapshot from `verify::flaky`, the cache hit rate and the bypass ratio from
//! `verify::observe::deviation` (spec §6.1).

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use sqlx::SqlitePool;

use super::stats::{self, ratio};
use crate::analytics::Window;
use crate::verify::observe::{Deviation, deviation};
use crate::verify::runs::{
    REQUESTED_BY_BISECT, REQUESTED_BY_FLAKE_CHECK, STATUS_ERRORED, STATUS_FAILED, STATUS_PASSED,
    STATUS_SKIPPED_CACHED,
};

/// Both windows in one read: `[previous.from, window.to)`. A queued row has no `started_at`, so
/// the range test leaves it out; `output_tail` is never loaded.
pub(crate) const GATE_ROWS_SQL: &str = "SELECT project_id, sha, group_name, status, requested_by,
        duration_ms, started_at
   FROM verify_runs
  WHERE started_at >= ?1 AND started_at < ?2 AND (?3 IS NULL OR project_id = ?3)
  ORDER BY started_at, id";

/// At most this many groups in `most_red`.
const MOST_RED: usize = 5;
/// How many of a project's latest terminal grouped runs the flaky snapshot reads.
const FLAKY_HISTORY: i64 = 200;
/// At most this many flaky groups in the payload.
const FLAKY_SHOWN: usize = 10;

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct Row {
    pub project_id: Option<String>,
    pub sha: Option<String>,
    pub group_name: Option<String>,
    pub status: String,
    pub requested_by: String,
    pub duration_ms: Option<i64>,
    pub started_at: String,
}

/// Probes run on a red already known; counting them would bias the rate down (spec §6.1, 4).
fn is_probe(row: &Row) -> bool {
    row.requested_by == REQUESTED_BY_FLAKE_CHECK || row.requested_by == REQUESTED_BY_BISECT
}

/// Cached, queued and running rows are outside the denominator.
fn is_terminal(row: &Row) -> bool {
    [STATUS_PASSED, STATUS_FAILED, STATUS_ERRORED].contains(&row.status.as_str())
}

fn in_window(row: &Row, window: Window) -> bool {
    row.started_at
        .get(..10)
        .is_some_and(|day| window.holds(day))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DurationDay {
    pub day: String,
    pub partial: bool,
    pub p50: Option<i64>,
    pub n: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RedGroup {
    pub group: String,
    pub red: i64,
    pub terminal: i64,
    pub rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FlakyGroup {
    pub project_id: String,
    pub group: String,
    pub observed: i64,
    pub flaky: i64,
    pub rate: Option<f64>,
    pub suspect: bool,
}

/// Metric 6: cached units over the terminal ones plus the cached (non-probe), both windows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cache {
    pub hit_rate: Option<f64>,
    pub prev_hit_rate: Option<f64>,
}

/// Metric 6: verification commands runs executed themselves over the `verify` units, summed over
/// the projects in scope and then divided, never an average of per-project ratios.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bypass {
    pub observed: i64,
    pub verify_units: i64,
    pub ratio: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gate {
    pub pass_rate: Option<f64>,
    pub prev_pass_rate: Option<f64>,
    pub terminal: i64,
    pub first_pass_rate: Option<f64>,
    pub retry_share: Option<f64>,
    pub duration_ms_daily: Vec<DurationDay>,
    pub most_red: Vec<RedGroup>,
    pub legacy_runs: i64,
    pub flaky: Vec<FlakyGroup>,
    pub cache: Option<Cache>,
    /// Filled by `read`; the rows here cannot see it.
    pub bypass: Option<Bypass>,
}

fn pass_rate(rows: &[&Row]) -> Option<f64> {
    let passed = rows
        .iter()
        .filter(|row| row.status == STATUS_PASSED)
        .count() as i64;
    ratio(passed, rows.len() as i64)
}

/// Cached over terminal-or-cached, over the non-probe rows of `window`.
fn hit_rate(rows: &[Row], window: Window) -> Option<f64> {
    let counted: Vec<&Row> = rows
        .iter()
        .filter(|row| !is_probe(row) && in_window(row, window))
        .filter(|row| is_terminal(row) || row.status == STATUS_SKIPPED_CACHED)
        .collect();
    let cached = counted
        .iter()
        .filter(|row| row.status == STATUS_SKIPPED_CACHED)
        .count() as i64;
    ratio(cached, counted.len() as i64)
}

/// PURE: the projects' deviations, summed and then divided.
pub(crate) fn bypass_of(deviations: &[Deviation]) -> Bypass {
    let observed: i64 = deviations.iter().map(|d| d.observed).sum();
    let verify_units: i64 = deviations.iter().map(|d| d.verify_units).sum();
    Bypass {
        observed,
        verify_units,
        ratio: ratio(observed, verify_units),
    }
}

/// PURE: metrics 4 and 5's window half. The flaky snapshot has no window and is filled by `read`.
pub(crate) fn gate_block(rows: &[Row], window: Window) -> Gate {
    // Terminal runs that count, in both windows, in `started_at` order.
    let counted: Vec<&Row> = rows
        .iter()
        .filter(|row| !is_probe(row) && is_terminal(row))
        .collect();
    let now_rows: Vec<&Row> = counted
        .iter()
        .copied()
        .filter(|row| in_window(row, window))
        .collect();
    let prev_rows: Vec<&Row> = counted
        .iter()
        .copied()
        .filter(|row| in_window(row, window.previous()))
        .collect();

    // First try vs retry: the first sighting of a key is its first try. Seen across both
    // windows; a row without a sha or a group cannot be retried, so it is always a first.
    let mut seen: HashSet<(Option<&str>, &str, &str)> = HashSet::new();
    let (mut firsts, mut first_passed, mut retries) = (0, 0, 0);
    for row in &counted {
        let first = match (row.sha.as_deref(), row.group_name.as_deref()) {
            (Some(sha), Some(group)) => seen.insert((row.project_id.as_deref(), sha, group)),
            _ => true,
        };
        if !in_window(row, window) {
            continue;
        }
        if first {
            firsts += 1;
            if row.status == STATUS_PASSED {
                first_passed += 1;
            }
        } else {
            retries += 1;
        }
    }

    let today = window.span().to;
    let duration_ms_daily = window
        .day_list()
        .into_iter()
        .map(|day| {
            let values: Vec<i64> = now_rows
                .iter()
                .filter(|row| row.started_at.starts_with(&day))
                .filter_map(|row| row.duration_ms)
                .collect();
            let dist = stats::dist(values);
            DurationDay {
                partial: day == today,
                p50: dist.p50,
                n: dist.n,
                day,
            }
        })
        .collect();

    let legacy_runs = now_rows
        .iter()
        .filter(|row| row.group_name.is_none())
        .count() as i64;
    let mut by_group: BTreeMap<&str, (i64, i64)> = BTreeMap::new(); // group -> (red, terminal)
    for row in &now_rows {
        let Some(group) = row.group_name.as_deref() else {
            continue;
        };
        let entry = by_group.entry(group).or_default();
        entry.1 += 1;
        if row.status != STATUS_PASSED {
            entry.0 += 1;
        }
    }
    let mut most_red: Vec<RedGroup> = by_group
        .into_iter()
        .filter(|(_, (red, _))| *red > 0)
        .map(|(group, (red, terminal))| RedGroup {
            group: group.to_owned(),
            red,
            terminal,
            rate: ratio(red, terminal),
        })
        .collect();
    most_red.sort_by(|a, b| b.red.cmp(&a.red).then_with(|| a.group.cmp(&b.group)));
    most_red.truncate(MOST_RED);

    Gate {
        pass_rate: pass_rate(&now_rows),
        prev_pass_rate: pass_rate(&prev_rows),
        terminal: now_rows.len() as i64,
        first_pass_rate: ratio(first_passed, firsts),
        retry_share: ratio(retries, now_rows.len() as i64),
        duration_ms_daily,
        most_red,
        legacy_runs,
        flaky: Vec::new(),
        cache: Some(Cache {
            hit_rate: hit_rate(rows, window),
            prev_hit_rate: hit_rate(rows, window.previous()),
        }),
        bypass: None,
    }
}

/// PURE: one project's flaky groups from its history, suspect by `flaky.rs`'s own constants.
pub(crate) fn flaky_groups(
    project_id: &str,
    history: &[crate::verify::flaky::Run],
) -> Vec<FlakyGroup> {
    use crate::verify::flaky::{MIN_OBSERVED, SUSPECT_PER_MILLE, group_rates};
    group_rates(history)
        .into_iter()
        .filter(|(_, rate)| rate.flaky > 0)
        .map(|(group, rate)| FlakyGroup {
            project_id: project_id.to_owned(),
            group,
            observed: i64::from(rate.observed),
            flaky: i64::from(rate.flaky),
            rate: ratio(i64::from(rate.flaky), i64::from(rate.observed)),
            suspect: rate.observed >= MIN_OBSERVED
                && rate.flaky * 1000 >= rate.observed * SUSPECT_PER_MILLE,
        })
        .collect()
}

/// A snapshot with no window (spec §6.1, 5): each project's latest runs, worst rate first.
async fn flaky(pool: &SqlitePool, project: Option<&str>) -> sqlx::Result<Vec<FlakyGroup>> {
    let projects: Vec<String> = match project {
        Some(id) => vec![id.to_owned()],
        None => {
            sqlx::query_scalar(
                "SELECT DISTINCT project_id FROM verify_runs
                  WHERE project_id IS NOT NULL ORDER BY project_id",
            )
            .fetch_all(pool)
            .await?
        }
    };
    let mut groups = Vec::new();
    for id in &projects {
        let history = crate::verify::flaky::history(pool, id, FLAKY_HISTORY).await?;
        groups.extend(flaky_groups(id, &history));
    }
    groups.sort_by(|a, b| {
        b.rate
            .partial_cmp(&a.rate)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.project_id.cmp(&b.project_id))
            .then_with(|| a.group.cmp(&b.group))
    });
    groups.truncate(FLAKY_SHOWN);
    Ok(groups)
}

/// One `deviation` per project in scope over the window's whole days. Globally, every project that
/// has an observation or a `verify` ticket: a project with units and no observation lowers the
/// ratio and must be counted.
async fn bypass(pool: &SqlitePool, project: Option<&str>, window: Window) -> sqlx::Result<Bypass> {
    let projects: Vec<String> = match project {
        Some(id) => vec![id.to_owned()],
        None => {
            sqlx::query_scalar(
                "SELECT project_id FROM verification_observations
                 UNION SELECT project_id FROM verify_requests ORDER BY 1",
            )
            .fetch_all(pool)
            .await?
        }
    };
    let (from, to) = (window.from_bound(), window.to_bound());
    let mut deviations = Vec::with_capacity(projects.len());
    for id in &projects {
        deviations.push(deviation(pool, id, &from, Some(to.as_str())).await?);
    }
    Ok(bypass_of(&deviations))
}

pub(crate) async fn rows(
    pool: &SqlitePool,
    project: Option<&str>,
    window: Window,
) -> sqlx::Result<Vec<Row>> {
    sqlx::query_as(GATE_ROWS_SQL)
        .bind(window.previous().from_bound())
        .bind(window.to_bound())
        .bind(project)
        .fetch_all(pool)
        .await
}

pub(crate) async fn read(
    pool: &SqlitePool,
    project: Option<&str>,
    window: Window,
) -> sqlx::Result<Gate> {
    let rows = rows(pool, project, window).await?;
    let mut gate = gate_block(&rows, window);
    gate.flaky = flaky(pool, project).await?;
    gate.bypass = Some(bypass(pool, project, window).await?);
    Ok(gate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::tests::now;

    /// `(project, sha, group, status, requested_by, duration_ms, started_at)`
    type V = (
        &'static str,
        &'static str,
        Option<&'static str>,
        &'static str,
        &'static str,
        Option<i64>,
        &'static str,
    );

    async fn seed(pool: &SqlitePool, rows: &[V]) {
        for (project, sha, group, status, by, duration, started) in rows {
            sqlx::query(
                "INSERT INTO verify_runs (project_id, worktree, sha, scope, origin, requested_by,
                                          group_name, argv, status, duration_ms, started_at,
                                          finished_at)
                 VALUES (?, '/w', ?, 'full', 'gate', ?, ?, '[]', ?, ?, ?, ?)",
            )
            .bind(project)
            .bind(sha)
            .bind(by)
            .bind(group)
            .bind(status)
            .bind(duration)
            .bind(started)
            .bind(started)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn gate(pool: &SqlitePool) -> Gate {
        read(pool, None, Window::new(7, now()).unwrap())
            .await
            .unwrap()
    }

    /// Spec §11: cached, queued and running are outside the denominator; probes (flake-check,
    /// bisect) are outside the rate entirely.
    #[tokio::test]
    async fn gate_pass_rate_excludes_cached_queued_running_and_probe_runs() {
        let pool = crate::testdb::fresh_pool().await;
        let day = "2026-10-16T10:00:00+00:00";
        seed(
            &pool,
            &[
                ("p", "a", Some("core"), "passed", "gate", Some(1000), day),
                (
                    "p",
                    "b",
                    Some("core"),
                    "passed",
                    "postgate",
                    Some(3000),
                    day,
                ),
                ("p", "c", Some("core"), "failed", "gate", Some(2000), day),
                ("p", "d", Some("core"), "errored", "gate", None, day),
                (
                    "p",
                    "e",
                    Some("core"),
                    "skipped_cached",
                    "gate",
                    Some(0),
                    day,
                ),
                ("p", "f", Some("core"), "running", "gate", None, day),
                (
                    "p",
                    "c",
                    Some("core"),
                    "failed",
                    "flake-check",
                    Some(2000),
                    day,
                ),
                ("p", "c", Some("core"), "failed", "bisect", Some(2000), day),
            ],
        )
        .await;
        let block = gate(&pool).await;
        assert_eq!(block.terminal, 4);
        assert_eq!(block.pass_rate, Some(0.5));
        assert_eq!(block.prev_pass_rate, None);
        let point = block
            .duration_ms_daily
            .iter()
            .find(|p| p.day == "2026-10-16")
            .unwrap();
        assert_eq!(
            (point.p50, point.n),
            (Some(2000), 3),
            "terminal non-probe rows with a duration"
        );
        assert_eq!(block.duration_ms_daily.len(), 7);
        assert!(block.duration_ms_daily.last().unwrap().partial);
        let cache = Cache {
            hit_rate: Some(0.2),
            prev_hit_rate: None,
        };
        assert_eq!(
            block.cache,
            Some(cache),
            "one cached over four terminal plus the cached; running and probes outside"
        );
    }

    /// Spec §11: per (project, sha, group) the oldest row is the first try, by `started_at`;
    /// a first try in the previous window makes this window's row a retry.
    #[tokio::test]
    async fn gate_first_pass_vs_retry_orders_by_started_at_per_sha_and_group() {
        let pool = crate::testdb::fresh_pool().await;
        seed(
            &pool,
            &[
                // retry of the next row
                (
                    "p",
                    "a",
                    Some("core"),
                    "passed",
                    "gate",
                    Some(1),
                    "2026-10-16T11:00:00+00:00",
                ),
                // first, red
                (
                    "p",
                    "a",
                    Some("core"),
                    "failed",
                    "gate",
                    Some(1),
                    "2026-10-16T10:00:00+00:00",
                ),
                // first, green
                (
                    "p",
                    "a",
                    Some("shell"),
                    "passed",
                    "gate",
                    Some(1),
                    "2026-10-16T10:00:00+00:00",
                ),
                // previous window
                (
                    "p",
                    "z",
                    Some("core"),
                    "failed",
                    "gate",
                    Some(1),
                    "2026-10-08T10:00:00+00:00",
                ),
                // retry here
                (
                    "p",
                    "z",
                    Some("core"),
                    "passed",
                    "gate",
                    Some(1),
                    "2026-10-12T10:00:00+00:00",
                ),
            ],
        )
        .await;
        let block = gate(&pool).await;
        assert_eq!(block.terminal, 4);
        assert_eq!(
            block.first_pass_rate,
            Some(0.5),
            "two firsts in the window, one green"
        );
        assert_eq!(
            block.retry_share,
            Some(0.5),
            "two of four terminal rows are retries"
        );
    }

    /// Spec §11: a NULL group is counted as legacy and never ranked as a group.
    #[tokio::test]
    async fn legacy_group_name_is_bucketed_and_never_ranked() {
        let pool = crate::testdb::fresh_pool().await;
        let day = "2026-10-16T10:00:00+00:00";
        seed(
            &pool,
            &[
                ("p", "a", None, "failed", "gate", Some(1), day),
                ("p", "b", None, "failed", "gate", Some(1), day),
                ("p", "c", Some("core"), "failed", "gate", Some(1), day),
                ("q", "c", Some("core"), "passed", "gate", Some(1), day),
                ("p", "d", Some("shell"), "passed", "gate", Some(1), day),
            ],
        )
        .await;
        let block = gate(&pool).await;
        assert_eq!(block.legacy_runs, 2);
        assert_eq!(
            block.most_red,
            vec![RedGroup {
                group: "core".into(),
                red: 1,
                terminal: 2,
                rate: Some(0.5)
            }],
            "only groups that went red, across projects; never the legacy rows"
        );
    }

    /// Spec §6.1, 5: flaky comes from `flaky::group_rates` over each project's recent history;
    /// suspect is `observed >= MIN_OBSERVED` and `flaky*1000 >= observed*SUSPECT_PER_MILLE`.
    #[tokio::test]
    async fn flaky_groups_come_from_the_flaky_module_with_its_suspect_rule() {
        let pool = crate::testdb::fresh_pool().await;
        // Five keys of `shell` observed; one of them passed and failed on the same sha and
        // fingerprint.
        let insert = "INSERT INTO verify_runs (project_id, worktree, sha, scope, origin,
                                               requested_by, group_name, argv, fingerprint,
                                               status, duration_ms, started_at, finished_at)
                      VALUES ('p', '/w', ?, 'full', 'gate', 'gate', 'shell', '[]', 'fp', ?, 1,
                              ?, ?)";
        for sha in ["a", "b", "c", "d", "e"] {
            let at = "2026-10-16T10:00:00+00:00";
            sqlx::query(insert)
                .bind(sha)
                .bind("passed")
                .bind(at)
                .bind(at)
                .execute(&pool)
                .await
                .unwrap();
        }
        let at = "2026-10-16T11:00:00+00:00";
        sqlx::query(insert)
            .bind("a")
            .bind("failed")
            .bind(at)
            .bind(at)
            .execute(&pool)
            .await
            .unwrap();
        let block = gate(&pool).await;
        assert_eq!(
            block.flaky,
            vec![FlakyGroup {
                project_id: "p".into(),
                group: "shell".into(),
                observed: 5,
                flaky: 1,
                rate: Some(0.2),
                suspect: true,
            }]
        );
    }

    /// Spec §6.1, 6: the bypass ratio sums `observed` and `verify_units` over the projects and
    /// divides once. Two projects at 1/1 and 0/9 are 0.1, not the 0.5 an average would say.
    #[tokio::test]
    async fn bypass_ratio_is_summed_then_divided_not_averaged() {
        let pool = crate::testdb::fresh_pool().await;
        let at = "2026-10-16T10:00:00+00:00";
        for (project, observed, units) in [("p", 1, 1), ("q", 0, 9)] {
            for _ in 0..observed {
                sqlx::query(
                    "INSERT INTO verification_observations (project_id, run_id, tool_name,
                                                            command, kind, name, segment,
                                                            decision, created_at)
                     VALUES (?, 7, 'Bash', 'cargo test', 'tool', 'cargo', 'cargo test',
                             'allow', ?)",
                )
                .bind(project)
                .bind(at)
                .execute(&pool)
                .await
                .unwrap();
            }
            let ticket: i64 = sqlx::query_scalar(
                "INSERT INTO verify_requests (project_id, worktree, kind, scope, priority, caller,
                                              created_at)
                 VALUES (?, '/w', 'test', 'changed', 1, 'agent', ?) RETURNING id",
            )
            .bind(project)
            .bind(at)
            .fetch_one(&pool)
            .await
            .unwrap();
            for _ in 0..units {
                sqlx::query(
                    "INSERT INTO verify_runs (project_id, worktree, scope, origin, origin_id,
                                              requested_by, argv, status, started_at,
                                              finished_at)
                     VALUES (?, '/w', 'full', 'verify', ?, 'gate', '[]', 'passed', ?, ?)",
                )
                .bind(project)
                .bind(ticket)
                .bind(at)
                .bind(at)
                .execute(&pool)
                .await
                .unwrap();
            }
        }
        let summed = Bypass {
            observed: 1,
            verify_units: 10,
            ratio: Some(0.1),
        };
        assert_eq!(gate(&pool).await.bypass, Some(summed));
        let window = Window::new(7, now()).unwrap();
        let p = read(&pool, Some("p"), window).await.unwrap();
        assert_eq!(p.bypass.and_then(|b| b.ratio), Some(1.0), "p alone");
        // Outside the window counts for nothing.
        let earlier = Window::new(7, now() - chrono::Duration::days(30)).unwrap();
        let none = read(&pool, None, earlier).await.unwrap().bypass;
        let empty = Bypass {
            observed: 0,
            verify_units: 0,
            ratio: None,
        };
        assert_eq!(none, Some(empty));
    }
}
