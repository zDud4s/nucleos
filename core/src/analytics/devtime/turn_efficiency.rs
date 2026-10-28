//! Metric 9 of the devtime overview (spec 2026-10-17-analytics-devtime §3 row 9, §6.1 and §8): the
//! current window's per-turn stats folded per call class, and the slow turns no rule explains.
//!
//! No SQL: a pure fold over `store::turn_stat_window_rows`. The slow turns come from
//! `devtime::unexplained::pick_unexplained`, the same picker `list_unexplained` uses, so the
//! overview and that listing never disagree on what "unexplained" means.
//!
//! The medians basis is wider than the window: a class median over a 7-day view alone would move
//! with every few turns, so medians span `min(window start, now - unexplained.window_days)` (the
//! basis `list_unexplained` already uses), while only the current window's turns are counted and
//! listed. No item carries a session id, a turn number or a worktree (§8): the panel shows when a
//! slow turn happened and in which project, never which session it was.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

use super::{ratio, stamp};
use crate::analytics::Window;
use crate::config::DevtimeRulesConfig;
use crate::devtime::store::{self as store, TurnStatRow};
use crate::devtime::unexplained::{
    MAX_WINDOW_DAYS, UnexplainedTurn, median_of_sorted, pick_unexplained,
};

/// At most this many slow turns are listed; `Unexplained::turns` still counts all of them.
const ITEM_CAP: usize = 20;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TurnClasses {
    /// The sum of `classes[].turns`.
    pub turns: i64,
    pub classes: Vec<TurnClassRow>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TurnClassRow {
    pub turn_class: String,
    /// The class's call range under the current `class_bounds`; `None` for a token outside them.
    pub min_calls: Option<i64>,
    /// `None` for the open last class, or a token outside the bounds.
    pub max_calls: Option<i64>,
    pub turns: i64,
    pub calls: i64,
    pub active_ms: i64,
    pub explained_ms: i64,
    /// `explained_ms / active_ms`; `None` without active time.
    pub explained_ratio: Option<f64>,
    pub median_active_ms: i64,
    /// `round(active_ms / calls)`; `None` without calls.
    pub ms_per_call: Option<i64>,
    /// The class's listed slow turns, counted before the item cap.
    pub unexplained_turns: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Unexplained {
    /// Every listed slow turn, before the cap.
    pub turns: i64,
    pub active_ms: i64,
    pub criteria: UnexplainedCriteria,
    /// Largest first, at most [`ITEM_CAP`].
    pub items: Vec<UnexplainedItem>,
}

/// The request's `rules.unexplained`, as the slow turns were picked.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UnexplainedCriteria {
    pub median_multiple: f64,
    pub min_turn_ms: u64,
    pub max_explained_fraction: f64,
    pub min_class_turns: u32,
    /// `YYYY-MM-DD` of the medians basis start.
    pub medians_from: String,
}

/// One slow turn, with no session id, turn number or worktree (spec §8).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UnexplainedItem {
    pub project_id: String,
    pub day: String,
    pub started_at: String,
    pub turn_class: String,
    pub calls: i64,
    pub active_ms: i64,
    pub explained_ms: i64,
    pub class_median_ms: i64,
}

/// What metric 9 read: the current window's turn stats, and the slow turns picked over the wider
/// medians basis.
#[derive(Debug, Default)]
pub(crate) struct TurnReads {
    current: Vec<TurnStatRow>,
    found: Vec<UnexplainedTurn>,
    bounds: Vec<u32>,
    criteria: UnexplainedCriteria,
}

/// The start of the medians basis: the window's start, or `window_days` before `now` when that
/// reaches further back.
pub(crate) fn medians_since(window: Window, now: DateTime<Utc>, window_days: u32) -> DateTime<Utc> {
    let days = i64::from(window_days).min(MAX_WINDOW_DAYS);
    window.start().min(now - Duration::days(days))
}

pub(crate) async fn read(
    pool: &SqlitePool,
    rules: &DevtimeRulesConfig,
    project: Option<&str>,
    window: Window,
    now: DateTime<Utc>,
) -> sqlx::Result<TurnReads> {
    let current = stamp(window.start());
    let until = stamp(now.min(window.end()));
    let basis = stamp(medians_since(window, now, rules.unexplained.window_days));
    let rows = store::turn_stat_window_rows(pool, project, &basis, &until).await?;
    Ok(prepare(rows, &current, &basis, rules))
}

/// PURE: picks the slow turns over every row (the medians basis), listing only those at or after
/// `current`, and keeps the rows at or after `current` for the class table.
pub(crate) fn prepare(
    rows: Vec<TurnStatRow>,
    current: &str,
    medians_from: &str,
    rules: &DevtimeRulesConfig,
) -> TurnReads {
    let settings = &rules.unexplained;
    let found = pick_unexplained(&rows, Some(current), rules);
    let current_rows = rows
        .into_iter()
        .filter(|row| row.started_at.as_str() >= current)
        .collect();
    TurnReads {
        current: current_rows,
        found,
        bounds: settings.class_bounds.clone(),
        criteria: UnexplainedCriteria {
            median_multiple: settings.median_multiple,
            min_turn_ms: settings.min_turn_seconds.saturating_mul(1000),
            max_explained_fraction: settings.max_explained_fraction,
            min_class_turns: settings.min_class_turns,
            medians_from: medians_from.get(..10).unwrap_or(medians_from).to_owned(),
        },
    }
}

/// The `n` of a `c<n>` token; `None` for anything else.
fn class_index(token: &str) -> Option<usize> {
    let digits = token.strip_prefix('c')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The calls range of class `index` under `bounds` (see `unexplained::turn_class`): `c0` is
/// `0..=b0`, `ci` is `b(i-1)+1..=bi`, and `c<len>` is open above. Both `None` past `len`.
fn call_range(index: Option<usize>, bounds: &[u32]) -> (Option<i64>, Option<i64>) {
    let Some(index) = index else {
        return (None, None);
    };
    if index > bounds.len() {
        return (None, None);
    }
    let min = match index {
        0 => 0,
        _ => i64::from(bounds[index - 1]) + 1,
    };
    (Some(min), bounds.get(index).map(|bound| i64::from(*bound)))
}

pub(crate) fn classes(reads: &TurnReads) -> TurnClasses {
    let mut listed: BTreeMap<&str, i64> = BTreeMap::new();
    for turn in &reads.found {
        *listed.entry(turn.stat.turn_class.as_str()).or_default() += 1;
    }
    let mut by_class: BTreeMap<&str, Vec<&TurnStatRow>> = BTreeMap::new();
    for row in &reads.current {
        by_class
            .entry(row.turn_class.as_str())
            .or_default()
            .push(row);
    }
    let mut rows: Vec<TurnClassRow> = by_class
        .into_iter()
        .map(|(token, turns)| {
            let (min_calls, max_calls) = call_range(class_index(token), &reads.bounds);
            let calls: i64 = turns.iter().map(|row| row.calls).sum();
            let active_ms: i64 = turns.iter().map(|row| row.active_ms).sum();
            let explained_ms: i64 = turns.iter().map(|row| row.explained_ms).sum();
            let mut actives: Vec<i64> = turns.iter().map(|row| row.active_ms).collect();
            actives.sort_unstable();
            TurnClassRow {
                turn_class: token.to_owned(),
                min_calls,
                max_calls,
                turns: i64::try_from(turns.len()).unwrap_or(i64::MAX),
                calls,
                active_ms,
                explained_ms,
                explained_ratio: ratio(explained_ms, active_ms),
                median_active_ms: median_of_sorted(&actives),
                ms_per_call: (calls > 0).then(|| (active_ms as f64 / calls as f64).round() as i64),
                unexplained_turns: listed.get(token).copied().unwrap_or(0),
            }
        })
        .collect();
    // By the numeric index of `c<n>`; tokens that do not parse go last, by token.
    rows.sort_by(|a, b| {
        let key = |row: &TurnClassRow| {
            let index = class_index(&row.turn_class);
            (index.is_none(), index.unwrap_or(0))
        };
        key(a)
            .cmp(&key(b))
            .then_with(|| a.turn_class.cmp(&b.turn_class))
    });
    TurnClasses {
        turns: rows.iter().map(|row| row.turns).sum(),
        classes: rows,
    }
}

pub(crate) fn unexplained(reads: &TurnReads) -> Unexplained {
    // `found` is already largest first (the picker's order).
    let items = reads
        .found
        .iter()
        .take(ITEM_CAP)
        .map(|turn| {
            let stat = &turn.stat;
            UnexplainedItem {
                project_id: stat.project_id.clone(),
                day: stat
                    .started_at
                    .get(..10)
                    .unwrap_or(&stat.started_at)
                    .to_owned(),
                started_at: stat.started_at.clone(),
                turn_class: stat.turn_class.clone(),
                calls: stat.calls,
                active_ms: stat.active_ms,
                explained_ms: stat.explained_ms,
                class_median_ms: turn.class_median_ms,
            }
        })
        .collect();
    Unexplained {
        turns: i64::try_from(reads.found.len()).unwrap_or(i64::MAX),
        active_ms: reads.found.iter().map(|turn| turn.stat.active_ms).sum(),
        criteria: reads.criteria.clone(),
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::{Scope, Window};
    use crate::config::ModelsConfig;
    use crate::devtime::store::fixture::session;
    use crate::devtime::store::turn_stat_fixture::{insert_stat, stat};
    use chrono::{DateTime, Utc};

    /// The medians basis start for `now` below with the default 30 rules days.
    const BASIS: &str = "2026-09-17T12:00:00.000Z";
    /// The 7-day window's start.
    const CURRENT: &str = "2026-10-11T00:00:00.000Z";
    /// Inside the medians basis, before the window.
    const BEFORE: &str = "2026-10-01T10:00:00.000Z";

    /// A stat inside the current window, at minute `seq` of 2026-10-12 10:00 (seq < 60).
    fn row(seq: i64, class: &str, calls: i64, active_ms: i64, explained_ms: i64) -> TurnStatRow {
        TurnStatRow {
            calls,
            explained_ms,
            ..stat(
                "s1",
                seq,
                "alpha",
                class,
                &format!("2026-10-12T10:{seq:02}:00.000Z"),
                active_ms,
            )
        }
    }

    /// `n` ordinary 100 s turns of `class` before the window, seqs from `first`.
    fn ordinary_before(first: i64, n: i64, class: &str) -> Vec<TurnStatRow> {
        (first..first + n)
            .map(|seq| stat("s0", seq, "alpha", class, BEFORE, 100_000))
            .collect()
    }

    fn prepared(rows: Vec<TurnStatRow>) -> TurnReads {
        prepare(rows, CURRENT, BASIS, &DevtimeRulesConfig::default())
    }

    #[test]
    fn classes_sort_by_index_with_call_ranges() {
        let tokens = ["c3", "c0", "c10", "c1", "zz", "c2"];
        let rows = tokens
            .iter()
            .enumerate()
            .map(|(seq, token)| row(seq as i64, token, 1, 1_000, 0))
            .collect();
        let block = classes(&prepared(rows));
        let order: Vec<&str> = block
            .classes
            .iter()
            .map(|class| class.turn_class.as_str())
            .collect();
        assert_eq!(order, ["c0", "c1", "c2", "c3", "c10", "zz"]);
        let ranges: Vec<(Option<i64>, Option<i64>)> = block
            .classes
            .iter()
            .map(|class| (class.min_calls, class.max_calls))
            .collect();
        assert_eq!(
            ranges,
            [
                (Some(0), Some(0)),
                (Some(1), Some(5)),
                (Some(6), Some(20)),
                (Some(21), None),
                (None, None),
                (None, None),
            ]
        );

        let mut rules = DevtimeRulesConfig::default();
        rules.unexplained.class_bounds = Vec::new();
        let open = classes(&prepare(
            vec![row(1, "c0", 7, 1_000, 0)],
            CURRENT,
            BASIS,
            &rules,
        ));
        assert_eq!(
            (open.classes[0].min_calls, open.classes[0].max_calls),
            (Some(0), None)
        );
    }

    #[test]
    fn class_figures_are_sums_medians_and_ratios() {
        let rows = vec![
            row(1, "c1", 2, 60_000, 0),
            row(2, "c1", 3, 100_000, 50_000),
            row(3, "c1", 5, 200_000, 0),
            row(4, "c2", 7, 1, 0),
            row(5, "c2", 8, 2, 0),
            row(6, "c0", 0, 30_000, 0),
            row(7, "c3", 25, 0, 0),
        ];
        let block = classes(&prepared(rows));
        assert_eq!(block.turns, 7);
        let by = |token: &str| {
            block
                .classes
                .iter()
                .find(|class| class.turn_class == token)
                .unwrap()
                .clone()
        };
        let c1 = by("c1");
        assert_eq!(
            (c1.turns, c1.calls, c1.active_ms, c1.explained_ms),
            (3, 10, 360_000, 50_000)
        );
        assert_eq!(c1.explained_ratio, Some(50_000.0 / 360_000.0));
        assert_eq!(c1.median_active_ms, 100_000);
        assert_eq!(c1.ms_per_call, Some(36_000));
        assert_eq!(by("c2").median_active_ms, 1);
        let c0 = by("c0");
        assert_eq!(c0.ms_per_call, None);
        assert_eq!(c0.explained_ratio, Some(0.0));
        let c3 = by("c3");
        assert_eq!(c3.explained_ratio, None);
        assert_eq!(c3.ms_per_call, Some(0));
    }

    #[test]
    fn classes_count_the_current_window_only() {
        let mut rows = ordinary_before(100, 3, "c1");
        rows.push(row(1, "c1", 3, 50_000, 0));
        let block = classes(&prepared(rows));
        assert_eq!(block.turns, 1);
        assert_eq!(block.classes.len(), 1);
        assert_eq!(block.classes[0].active_ms, 50_000);
    }

    #[test]
    fn slow_turns_use_basis_medians_but_list_the_window_only() {
        let mut rows = ordinary_before(100, 12, "c1");
        // Slow, but before the window: counts toward the median, is not listed.
        rows.push(stat("s0", 200, "alpha", "c1", BEFORE, 600_000));
        rows.push(row(1, "c1", 3, 600_000, 0));
        let reads = prepared(rows);
        let block = unexplained(&reads);
        assert_eq!(block.turns, 1, "{block:?}");
        assert_eq!(block.active_ms, 600_000);
        let item = &block.items[0];
        assert_eq!(item.class_median_ms, 100_000);
        assert_eq!(item.started_at, "2026-10-12T10:01:00.000Z");
        assert_eq!(item.day, "2026-10-12");
        assert_eq!(item.project_id, "alpha");
        assert_eq!(classes(&reads).classes[0].unexplained_turns, 1);
    }

    #[test]
    fn unexplained_is_largest_first_and_capped_at_twenty() {
        let mut rows = ordinary_before(100, 60, "c1");
        let slow: Vec<i64> = (0..25).map(|i| 400_000 + i * 1_000).collect();
        for (seq, active) in slow.iter().enumerate() {
            rows.push(row(seq as i64, "c1", 3, *active, 0));
        }
        let block = unexplained(&prepared(rows));
        assert_eq!(block.items.len(), 20);
        assert_eq!(block.turns, 25);
        assert_eq!(block.active_ms, slow.iter().sum::<i64>());
        assert!(
            block
                .items
                .windows(2)
                .all(|pair| pair[0].active_ms >= pair[1].active_ms),
            "{:?}",
            block.items
        );
        assert_eq!(block.items[0].active_ms, 424_000);
    }

    #[test]
    fn unexplained_turns_per_class_counts_listed_turns() {
        let mut rows = ordinary_before(100, 60, "c1");
        rows.extend(ordinary_before(200, 12, "c2"));
        for seq in 0..22 {
            rows.push(row(seq, "c1", 3, 400_000, 0));
        }
        let reads = prepared(rows);
        let block = classes(&reads);
        let count = |token: &str| {
            block
                .classes
                .iter()
                .find(|class| class.turn_class == token)
                .map(|class| class.unexplained_turns)
        };
        assert_eq!(count("c1"), Some(22));
        // c2 has no turn in the current window, so no row at all.
        assert_eq!(count("c2"), None);
        assert_eq!(unexplained(&reads).items.len(), 20);

        let mut rows = ordinary_before(100, 12, "c2");
        rows.push(row(1, "c2", 3, 100_000, 0));
        rows.extend(ordinary_before(200, 12, "c1"));
        rows.push(row(2, "c1", 3, 600_000, 0));
        let block = classes(&prepared(rows));
        let turns: Vec<(&str, i64)> = block
            .classes
            .iter()
            .map(|class| (class.turn_class.as_str(), class.unexplained_turns))
            .collect();
        assert_eq!(turns, [("c1", 1), ("c2", 0)]);
    }

    #[test]
    fn criteria_mirror_the_rules_and_the_basis_day() {
        let reads = prepared(Vec::new());
        let block = unexplained(&reads);
        let criteria = &block.criteria;
        assert_eq!(criteria.min_turn_ms, 300_000);
        assert_eq!(criteria.median_multiple, 3.0);
        assert_eq!(criteria.max_explained_fraction, 0.2);
        assert_eq!(criteria.min_class_turns, 10);
        assert_eq!(criteria.medians_from, "2026-09-17");
        assert_eq!((block.turns, block.active_ms, block.items.len()), (0, 0, 0));
        assert_eq!(classes(&reads), TurnClasses::default());
    }

    fn at(moment: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(moment)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn medians_since_is_the_wider_of_window_and_rules_basis() {
        let now = at("2026-10-17T12:00:00Z");
        let week = Window::new(7, now).unwrap();
        assert_eq!(medians_since(week, now, 30), now - Duration::days(30));
        assert_eq!(stamp(medians_since(week, now, 30)), BASIS);
        let quarter = Window::new(90, now).unwrap();
        assert_eq!(medians_since(quarter, now, 30), quarter.start());
        assert_eq!(
            medians_since(week, now, u32::MAX),
            now - Duration::days(36_500)
        );
    }

    /// Noon on 2026-10-17: the 7-day window is 2026-10-11 ..= 17, the previous 10-04 ..= 10.
    async fn overview(pool: &sqlx::SqlitePool, scope: Scope<'_>) -> super::super::DevtimeOverview {
        let now = at("2026-10-17T12:00:00Z");
        super::super::devtime_overview(
            pool,
            &ModelsConfig::default(),
            &DevtimeRulesConfig::default(),
            scope,
            Window::new(7, now).unwrap(),
            now,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn overview_scopes_turn_stats_by_project_and_window() {
        let pool = crate::testdb::fresh_pool().await;
        let inside = "2026-10-12T10:00:00.000Z";
        insert_stat(&pool, &stat("a1", 1, "alpha", "c1", inside, 10_000)).await;
        insert_stat(&pool, &stat("b1", 1, "beta", "c1", inside, 20_000)).await;
        // After now, and in the previous window: neither counts.
        insert_stat(
            &pool,
            &stat("a1", 2, "alpha", "c1", "2026-10-17T13:00:00.000Z", 30_000),
        )
        .await;
        insert_stat(
            &pool,
            &stat("a1", 0, "alpha", "c1", "2026-10-08T10:00:00.000Z", 40_000),
        )
        .await;

        let global = overview(&pool, Scope::Global).await.turn_classes;
        assert_eq!(global.turns, 2, "{global:?}");
        assert_eq!(global.classes[0].active_ms, 30_000);
        let alpha = overview(&pool, Scope::Project("alpha")).await.turn_classes;
        assert_eq!(alpha.turns, 1, "{alpha:?}");
        assert_eq!(alpha.classes[0].active_ms, 10_000);
    }

    #[tokio::test]
    async fn turn_blocks_json_carry_no_session_or_worktree() {
        let pool = crate::testdb::fresh_pool().await;
        session(
            &pool,
            "sess-secret-9",
            "alpha",
            "2026-10-12T09:00:00.000Z",
            None,
        )
        .await;
        sqlx::query("UPDATE devtime_sessions SET worktree = 'C:/wt/secret'")
            .execute(&pool)
            .await
            .unwrap();
        for seq in 1..=13 {
            let active = if seq == 13 { 600_000 } else { 100_000 };
            let moment = format!("2026-10-12T10:{seq:02}:00.000Z");
            insert_stat(
                &pool,
                &stat("sess-secret-9", seq, "alpha", "c1", &moment, active),
            )
            .await;
        }

        let report = serde_json::to_value(overview(&pool, Scope::Global).await).unwrap();
        assert_eq!(report["turn_classes"]["turns"], 13, "{report}");
        assert_eq!(report["unexplained"]["turns"], 1, "{report}");
        let item = &report["unexplained"]["items"][0];
        assert!(item.is_object(), "{report}");
        for key in ["session_id", "turn_seq", "worktree"] {
            assert!(item.get(key).is_none(), "{key} in {item}");
        }
        for block in ["turn_classes", "unexplained"] {
            let text = report[block].to_string();
            for needle in ["sess-secret", "C:/", "/wt"] {
                assert!(!text.contains(needle), "{needle} in {text}");
            }
        }
        assert!(report["agents"].is_object(), "{report}");
    }
}
