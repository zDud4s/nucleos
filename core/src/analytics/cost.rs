//! The cost block: what a window cost, per day, by project (global) or mode (project), with the
//! judge as a key of its own (spec §4.4, §4.9). Measured and time-estimated cost are never folded
//! into one number.

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::SqlitePool;

use super::sessions::{Priced, RunRow, day_of};
use super::{Scope, Window};

/// The judge's cost, a key of its own in both scopes, always last (spec §4.9).
pub const JUDGE_KEY: &str = "judge";
/// Every project past the eighth, on the global page.
pub const OTHER_KEY: &str = "__other__";
/// Runs with no project (`runs.project_id` is nullable), on the global page.
pub const NO_PROJECT_KEY: &str = "__none__";
const MAX_PROJECT_KEYS: usize = 8;

/// What a window cost. Measured, estimated and judge are three numbers, never one (spec §4.4, §4.9).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct CostTotal {
    pub measured_usd: f64,
    pub estimated_usd: f64,
    pub judge_usd: f64,
    /// Finished runs in the window.
    pub runs: i64,
    /// Of those, the ones whose cost is a time estimate.
    pub estimated_runs: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostPart {
    pub key: String,
    pub measured_usd: f64,
    pub estimated_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostPoint {
    pub day: String,
    pub partial: bool,
    /// One part per entry of `keys`, in that order, zeros included.
    pub by: Vec<CostPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CostKey {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostBlock {
    pub total: CostTotal,
    pub previous: CostTotal,
    pub series: Vec<CostPoint>,
    /// What `key` means: `project` (global) or `mode` (project).
    pub by_label: &'static str,
    /// Current-window cost desc, then key; global keeps 8 and folds the rest into `__other__`;
    /// `judge` always present and last.
    pub keys: Vec<CostKey>,
}

/// One judge call's cost on the UTC day of its own `created_at`.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeLine {
    pub day: String,
    pub usd: f64,
}

/// Judge calls in `[from, to)` by their own `created_at`, scoped like `JudgeSpendScope`: every
/// call for the global page (the LEFT JOIN keeps calls whose run is gone, as `All` does), and
/// those whose run is the project's for a project. One query per request, not one per day.
pub async fn judge_lines(
    pool: &SqlitePool,
    project: Option<&str>,
    from: &str,
    to: &str,
) -> sqlx::Result<Vec<JudgeLine>> {
    let raw: Vec<(String, f64)> = sqlx::query_as(
        "SELECT j.created_at, j.cost_usd FROM judge_verdicts j
           LEFT JOIN runs r ON r.id = j.run_id
          WHERE j.created_at >= ?1 AND j.created_at < ?2 AND (?3 IS NULL OR r.project_id = ?3)
         UNION ALL
         SELECT j.created_at, j.cost_usd FROM judge_resolutions j
           LEFT JOIN runs r ON r.id = j.run_id
          WHERE j.created_at >= ?1 AND j.created_at < ?2 AND (?3 IS NULL OR r.project_id = ?3)",
    )
    .bind(from)
    .bind(to)
    .bind(project)
    .fetch_all(pool)
    .await?;
    Ok(raw
        .into_iter()
        .map(|(at, usd)| JudgeLine {
            day: day_of(&at).to_owned(),
            usd,
        })
        .collect())
}

fn cost_key(scope: Scope<'_>, row: &RunRow) -> String {
    match scope {
        Scope::Global => match row.project_id.as_deref() {
            None => NO_PROJECT_KEY.to_owned(),
            // A project named like a reserved key would share its cell and repeat its key.
            Some(id) if is_reserved(id) => format!("{PROJECT_PREFIX}{id}"),
            Some(id) => id.to_owned(),
        },
        Scope::Project(_) => row.mode.clone(),
    }
}

const PROJECT_PREFIX: &str = "project:";

/// Ids that would collide with a synthetic key, or with an escaped one.
fn is_reserved(id: &str) -> bool {
    matches!(id, JUDGE_KEY | OTHER_KEY | NO_PROJECT_KEY) || id.starts_with(PROJECT_PREFIX)
}

fn key_label(key: &str) -> String {
    match key {
        JUDGE_KEY => "Judge".to_owned(),
        OTHER_KEY => "Other projects".to_owned(),
        NO_PROJECT_KEY => "No project".to_owned(),
        other => other
            .strip_prefix(PROJECT_PREFIX)
            .unwrap_or(other)
            .to_owned(),
    }
}

fn cost_total(window: Window, priced: &[Priced<'_>], judge: &[JudgeLine]) -> CostTotal {
    let mut total = CostTotal::default();
    for run in priced.iter().filter(|run| window.holds(run.day())) {
        total.runs += 1;
        if run.estimated {
            total.estimated_usd += run.usd;
            total.estimated_runs += 1;
        } else {
            total.measured_usd += run.usd;
        }
    }
    total.judge_usd = judge
        .iter()
        .filter(|line| window.holds(&line.day))
        .map(|line| line.usd)
        .sum();
    total
}

pub fn cost_block(
    scope: Scope<'_>,
    window: Window,
    priced: &[Priced<'_>],
    judge: &[JudgeLine],
) -> CostBlock {
    let current: Vec<&Priced<'_>> = priced
        .iter()
        .filter(|run| window.holds(run.day()))
        .collect();

    // Rank the keys by what they cost in this window.
    let mut spent: BTreeMap<String, f64> = BTreeMap::new();
    for run in &current {
        *spent.entry(cost_key(scope, run.row)).or_default() += run.usd;
    }
    let mut ranked: Vec<(String, f64)> = spent.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let limit = match scope {
        Scope::Global => MAX_PROJECT_KEYS,
        Scope::Project(_) => usize::MAX,
    };
    let folded = ranked.len() > limit;
    let mut keys: Vec<String> = ranked.into_iter().take(limit).map(|(key, _)| key).collect();

    // (day, key) -> (measured, estimated)
    let mut cells: BTreeMap<(String, String), (f64, f64)> = BTreeMap::new();
    for run in &current {
        let key = cost_key(scope, run.row);
        let key = if keys.contains(&key) {
            key
        } else {
            OTHER_KEY.to_owned()
        };
        let cell = cells.entry((run.day().to_owned(), key)).or_default();
        if run.estimated {
            cell.1 += run.usd;
        } else {
            cell.0 += run.usd;
        }
    }
    for line in judge.iter().filter(|line| window.holds(&line.day)) {
        cells
            .entry((line.day.clone(), JUDGE_KEY.to_owned()))
            .or_default()
            .0 += line.usd;
    }
    if folded {
        keys.push(OTHER_KEY.to_owned());
    }
    keys.push(JUDGE_KEY.to_owned());

    let days = window.day_list();
    let last = days.len() - 1;
    let series = days
        .iter()
        .enumerate()
        .map(|(index, day)| CostPoint {
            day: day.clone(),
            partial: index == last,
            by: keys
                .iter()
                .map(|key| {
                    let (measured_usd, estimated_usd) = cells
                        .get(&(day.clone(), key.clone()))
                        .copied()
                        .unwrap_or_default();
                    CostPart {
                        key: key.clone(),
                        measured_usd,
                        estimated_usd,
                    }
                })
                .collect(),
        })
        .collect();

    CostBlock {
        total: cost_total(window, priced, judge),
        previous: cost_total(window.previous(), priced, judge),
        series,
        by_label: match scope {
            Scope::Global => "project",
            Scope::Project(_) => "mode",
        },
        keys: keys
            .iter()
            .map(|key| CostKey {
                key: key.clone(),
                label: key_label(key),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::tests::{Run, close, done, insert, now, report_of};
    use crate::analytics::{Analytics, report};

    /// `(measured_usd, estimated_usd)` of one key on one day.
    fn cost_on(report: &Analytics, day: &str, key: &str) -> (f64, f64) {
        let point = report
            .cost
            .series
            .iter()
            .find(|point| point.day == day)
            .unwrap_or_else(|| panic!("no cost point for {day}"));
        point
            .by
            .iter()
            .find(|part| part.key == key)
            .map(|part| (part.measured_usd, part.estimated_usd))
            .unwrap_or_else(|| panic!("no key {key} on {day}"))
    }

    fn keys_of(report: &Analytics) -> Vec<&str> {
        report
            .cost
            .keys
            .iter()
            .map(|key| key.key.as_str())
            .collect()
    }

    async fn judge_verdict(pool: &SqlitePool, run_id: i64, at: &str, usd: f64) {
        sqlx::query(
            "INSERT INTO judge_verdicts
             (run_id, tool_name, tool_input_digest, action_class, classifier_decision, judge,
              model, questions_version, final_decision, cost_usd, created_at)
             VALUES (?, 'Bash', 'd', 'unrecognized', 'pending_approval', 'observe', 'jev-latest',
                     1, 'pending_approval', ?, ?)",
        )
        .bind(run_id)
        .bind(usd)
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn judge_resolution(pool: &SqlitePool, run_id: i64, at: &str, usd: f64) {
        sqlx::query(
            "INSERT INTO judge_resolutions (run_id, lineage_root_id, event, tool_input_digest,
                                            default_outcome, final_outcome, cost_usd, created_at)
             VALUES (?, ?, 'park', 'd', 'park', 'park', ?, ?)",
        )
        .bind(run_id)
        .bind(run_id)
        .bind(usd)
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Spec test 3 (cost): two runs of one session each count their own cost on their own day —
    /// `compute_spend`'s rule, not the token rule.
    #[tokio::test]
    async fn the_cost_of_a_session_is_summed_run_by_run_each_on_its_day() {
        let pool = crate::testdb::fresh_pool().await;
        insert(
            &pool,
            Run {
                session: Some("s1"),
                cost: Some(1.0),
                ..done("alpha", "2026-10-01T23:50:00Z")
            },
        )
        .await;
        insert(
            &pool,
            Run {
                session: Some("s1"),
                cost: Some(2.0),
                ..done("alpha", "2026-10-02T00:10:00Z")
            },
        )
        .await;

        let report = report_of(&pool, Scope::Project("alpha"), 30).await;

        assert_eq!(report.cost.series.len(), 30);
        assert_eq!(report.cost.by_label, "mode");
        assert!(close(cost_on(&report, "2026-10-01", "worktree").0, 1.0));
        assert!(close(cost_on(&report, "2026-10-02", "worktree").0, 2.0));
        assert!(close(report.cost.total.measured_usd, 3.0));
        assert_eq!(report.cost.total.runs, 2);
    }

    /// Spec test 4 (cost): an unknown cost is estimated from time and shown apart, never folded in.
    #[tokio::test]
    async fn an_unknown_cost_is_estimated_and_kept_apart_from_the_measured_one() {
        let pool = crate::testdb::fresh_pool().await;
        insert(
            &pool,
            Run {
                created: "2026-10-02T09:00:00Z",
                cost: None,
                ..done("alpha", "2026-10-02T10:00:00Z")
            },
        )
        .await;
        insert(
            &pool,
            Run {
                cost: Some(1.25),
                ..done("alpha", "2026-10-02T11:00:00Z")
            },
        )
        .await;

        let report = report_of(&pool, Scope::Project("alpha"), 30).await;

        let total = &report.cost.total;
        assert!(close(total.measured_usd, 1.25));
        // One hour at the seeded default of $3/h (0012_budget.sql).
        assert!(close(total.estimated_usd, 3.0));
        assert_eq!((total.runs, total.estimated_runs), (2, 1));
        let (measured, estimated) = cost_on(&report, "2026-10-02", "worktree");
        assert!(close(measured, 1.25) && close(estimated, 3.0));
    }

    /// Spec test 9: a run started one day and finished another is that other day's cost.
    #[tokio::test]
    async fn a_run_lands_on_the_day_it_completed_not_the_day_it_started() {
        let pool = crate::testdb::fresh_pool().await;
        insert(
            &pool,
            Run {
                created: "2026-10-01T22:00:00Z",
                cost: Some(2.0),
                ..done("alpha", "2026-10-03T01:00:00Z")
            },
        )
        .await;

        let report = report_of(&pool, Scope::Global, 30).await;

        assert!(close(cost_on(&report, "2026-10-01", "alpha").0, 0.0));
        assert!(close(cost_on(&report, "2026-10-03", "alpha").0, 2.0));
    }

    /// Spec test 10: global stacks by project, eight at most plus `__other__`, the judge last;
    /// a project stacks by mode.
    #[tokio::test]
    async fn global_stacks_eight_projects_and_the_rest_project_stacks_modes() {
        let pool = crate::testdb::fresh_pool().await;
        const PROJECTS: [&str; 10] = [
            "p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08", "p09", "p10",
        ];
        for (rank, project) in PROJECTS.iter().enumerate() {
            insert(
                &pool,
                Run {
                    cost: Some(10.0 - rank as f64),
                    ..done(project, "2026-10-10T12:00:00Z")
                },
            )
            .await;
        }

        let global = report_of(&pool, Scope::Global, 30).await;
        assert_eq!(global.cost.by_label, "project");
        assert_eq!(
            keys_of(&global),
            vec![
                "p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08", OTHER_KEY, JUDGE_KEY
            ]
        );
        assert!(close(
            cost_on(&global, "2026-10-10", OTHER_KEY).0,
            2.0 + 1.0
        ));
        assert!(close(global.cost.total.measured_usd, 55.0));

        let project = report_of(&pool, Scope::Project("p01"), 30).await;
        assert_eq!(keys_of(&project), vec!["worktree", JUDGE_KEY]);
        assert!(close(project.cost.total.measured_usd, 10.0));
    }

    /// A run with no project is the `__none__` key in global scope, labelled, and not in a
    /// project's report.
    #[tokio::test]
    async fn a_run_with_no_project_is_the_none_key_in_global_scope_only() {
        let pool = crate::testdb::fresh_pool().await;
        insert(
            &pool,
            Run {
                project: None,
                cost: Some(0.5),
                ..done("alpha", "2026-10-10T12:00:00Z")
            },
        )
        .await;
        insert(&pool, done("alpha", "2026-10-10T13:00:00Z")).await;

        let global = report_of(&pool, Scope::Global, 30).await;
        assert!(keys_of(&global).contains(&NO_PROJECT_KEY));
        let key = global
            .cost
            .keys
            .iter()
            .find(|key| key.key == NO_PROJECT_KEY)
            .unwrap();
        assert_eq!(key.label, "No project");
        assert!(close(cost_on(&global, "2026-10-10", NO_PROJECT_KEY).0, 0.5));

        let project = report_of(&pool, Scope::Project("alpha"), 30).await;
        assert!(!keys_of(&project).contains(&NO_PROJECT_KEY));
        assert!(close(project.cost.total.measured_usd, 1.0));
    }

    /// A project whose id equals a reserved key must not share the key's cell or repeat it.
    #[tokio::test]
    async fn a_project_named_like_a_reserved_key_keeps_its_own_key() {
        let pool = crate::testdb::fresh_pool().await;
        let run = insert(
            &pool,
            Run {
                cost: Some(2.0),
                ..done("judge", "2026-10-10T12:00:00Z")
            },
        )
        .await;
        judge_verdict(&pool, run, "2026-10-10T13:00:00Z", 0.25).await;

        let global = report_of(&pool, Scope::Global, 30).await;
        let keys = keys_of(&global);
        assert_eq!(keys, vec!["project:judge", JUDGE_KEY]);
        let label = &global.cost.keys[0].label;
        assert_eq!(label, "judge");
        assert!(close(
            cost_on(&global, "2026-10-10", "project:judge").0,
            2.0
        ));
        assert!(close(cost_on(&global, "2026-10-10", JUDGE_KEY).0, 0.25));
    }

    /// An empty window still names the judge key, and answers zeros rather than nothing.
    #[tokio::test]
    async fn an_empty_window_has_only_the_judge_key_and_zeros() {
        let pool = crate::testdb::fresh_pool().await;
        let report = report_of(&pool, Scope::Global, 7).await;
        assert_eq!(keys_of(&report), vec![JUDGE_KEY]);
        assert_eq!(report.cost.series.len(), 7);
        assert_eq!(report.cost.total, CostTotal::default());
    }

    /// Spec test 12: the judge is its own key, bucketed by the VERDICT's day; and a project's
    /// analytics cost equals `project_spend` for the same window when every run and verdict sits
    /// inside it by both clocks.
    #[tokio::test]
    async fn the_judge_is_its_own_key_and_the_project_total_matches_project_spend() {
        let pool = crate::testdb::fresh_pool().await;
        let measured = insert(
            &pool,
            Run {
                created: "2026-10-01T10:00:00Z",
                cost: Some(1.5),
                ..done("alpha", "2026-10-01T10:30:00Z")
            },
        )
        .await;
        let estimated = insert(
            &pool,
            Run {
                created: "2026-10-02T09:00:00Z",
                cost: None,
                ..done("alpha", "2026-10-02T10:00:00Z")
            },
        )
        .await;
        let other = insert(
            &pool,
            Run {
                cost: Some(7.0),
                ..done("beta", "2026-10-03T10:00:00Z")
            },
        )
        .await;
        judge_verdict(&pool, measured, "2026-10-05T08:00:00Z", 0.25).await;
        judge_resolution(&pool, estimated, "2026-10-06T08:00:00Z", 0.5).await;
        judge_verdict(&pool, other, "2026-10-05T09:00:00Z", 0.75).await;

        let window = Window::new(30, now()).unwrap();
        let alpha = report(&pool, Scope::Project("alpha"), window, now())
            .await
            .unwrap();

        assert!(
            close(cost_on(&alpha, "2026-10-05", JUDGE_KEY).0, 0.25),
            "the verdict's day"
        );
        assert!(
            close(cost_on(&alpha, "2026-10-01", JUDGE_KEY).0, 0.0),
            "not the run's day"
        );
        assert!(
            close(cost_on(&alpha, "2026-10-06", JUDGE_KEY).1, 0.0),
            "never estimated"
        );
        assert!(close(alpha.cost.total.judge_usd, 0.75));
        assert!(close(alpha.cost.total.measured_usd, 1.5));
        assert!(close(alpha.cost.total.estimated_usd, 3.0));
        let spend =
            crate::governance::budget::project_spend(&pool, "alpha", window.start(), window.end())
                .await
                .unwrap();
        let total = &alpha.cost.total;
        assert!(
            close(
                total.measured_usd + total.estimated_usd + total.judge_usd,
                spend.usd
            ),
            "analytics {total:?} against project_spend {}",
            spend.usd
        );
        // 1.5 measured + 3.0 estimated + 0.75 judge, worked out by hand.
        assert!(close(spend.usd, 5.25));

        let global = report(&pool, Scope::Global, window, now()).await.unwrap();
        let all = crate::governance::budget::judge_spend(
            &pool,
            Some(window.start()),
            Some(window.end()),
            crate::governance::budget::JudgeSpendScope::All,
        )
        .await
        .unwrap();
        assert!(close(global.cost.total.judge_usd, all));
        assert!(close(all, 1.5));
    }
}
