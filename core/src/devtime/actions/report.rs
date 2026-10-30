//! The shadow-to-live promotion report of one `(action, rule)` pair (actions design spec, section 5;
//! plan chunk 3.3). One read-only query over `devtime_shadow`, shown to the owner with its numbers.
//! The report only signals `ready`; promoting is the owner's act (D2).
//!
//! Contract of [`action_promotion_report`] (the GREEN step implements exactly this; where the plan is
//! ambiguous the choice below is the one the tests pin):
//! - The precision of the rule is an INPUT (`precision`, from `precision::rule_report`), not queried
//!   here, so the report stays testable with a hand-made `RulePrecision`. `action_eligible` =
//!   `precision::action_eligible(precision, cfg)`; `wilson_lower` = `precision::wilson_lower(n_cases -
//!   n_not_rework, n_cases)`, reported so the owner sees the bound next to the threshold.
//! - Counted rows ("the sample") come from `store::decisions_for(action, rule_id, "shadow",
//!   [scope.action_version])` (so `mode = 'shadow'` only, the current action version only, never
//!   `void`), further filtered to `rule_version == scope.rule_version`, `project_id` when the scope
//!   names one (`None` = every project), `decision == 'fire'` (a suppressed row says nothing about
//!   whether firing would have helped) and `outcome_state == 'resolved'`.
//! - Effective outcome of a counted row: the human verdict PREVAILS over the automatic outcome
//!   (`useful` = hit, `noise` = miss); without a verdict the stored `outcome` (`hit` / `miss`) counts.
//!   Rows whose effective outcome is neither (a stored `ignored` and so on, no verdict) are not part of
//!   the sample: they only feed `ignored`.
//! - `n_resolved` = counted rows with an effective hit or miss. `n_sessions` = distinct `session_id`
//!   among them (`subject_key` stands in when a row has none, as a cross-session `propose` row does).
//!   `days_covered` = whole days (floor) from the earliest `decided_at` of those rows to `scope.now_ms`.
//!   `sample_ok` = `n_resolved >= effect_min_resolved && n_sessions >= effect_min_sessions &&
//!   days_covered >= effect_min_days`.
//! - `hit_rate` = effective hits / `n_resolved` (`None` when `n_resolved == 0`). `mean_hit_cost_ms` =
//!   mean of `outcome_cost_ms` (a missing one counts as 0) over the effective hits (`None` without
//!   hits); `median_hit_cost_ms` the same sample's median (mean of the two middle values when even).
//!   `shadow_approved` = `hit_rate >= cfg.shadow_hit_rate_min.<action>` && `mean_hit_cost_ms >=
//!   cfg.min_mean_hit_cost_ms`.
//! - Avoidable fraction (spec 6.1) = sum of `cost_ms` of the rule's non-cross findings at
//!   `scope.rule_version` / sum of `active_ms` of `devtime_turn_stats` (same project filter), by
//!   `started_at` window. `avoidable_now` = window `(now - W, now]`, `avoidable_prev` = window
//!   `(now - 2W, now - W]`, with `W = cfg.effect_min_days` days. A window without `active_ms` gives
//!   `None`. `regression_ok` = `avoidable_prev` is `None` or zero (no baseline) or `avoidable_now` is
//!   `None` or `avoidable_now <= avoidable_prev * 1.10` (a rise of exactly 10% relative passes).
//! - `human_marks` = counted rows (before the outcome filter above: any counted row with a verdict)
//!   carrying `human_verdict`. `human_rate` = `useful / (useful + noise)` (`None` without marks).
//!   `human_ok` = `human_marks >= cfg.human_min_marks` && `human_rate >= cfg.shadow_hit_rate_min.<action>`.
//! - `ready` = `action_eligible && sample_ok && shadow_approved && regression_ok && human_ok`.
//! - Aside, not part of `ready`: `suppress_rate` = suppressed / all rows of `decisions_for` at the
//!   current action and rule version (fire and suppress alike, any outcome state but `void`; `None`
//!   without rows); `ignored` = counted-scope rows (shadow, current versions, fire) with stored
//!   `outcome == 'ignored'` and no human verdict.

#![allow(dead_code)] // the routes (3.4) are the caller

use sqlx::SqlitePool;

use crate::config::DevtimeActionsConfig;
use crate::devtime::precision::{RulePrecision, action_eligible, wilson_lower};

/// Which pair the report is for, and "now" for its windows.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionScope<'a> {
    /// `inject` or `propose`.
    pub action: &'a str,
    pub rule_id: &'a str,
    /// `None` aggregates every project; `Some` filters shadow rows, findings and turn stats.
    pub project_id: Option<&'a str>,
    /// Current `rule_version` of the rule: rows and findings of other versions do not count.
    pub rule_version: i64,
    /// Current `ACTION_VERSION`: rows of other versions do not count.
    pub action_version: i64,
    /// Unix milliseconds.
    pub now_ms: i64,
}

/// The five promotion criteria with their numbers, plus the aside figures.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionReport {
    // (1) rule eligible
    pub action_eligible: bool,
    pub wilson_lower: f64,
    // (2) sample of the effect
    pub n_resolved: i64,
    pub n_sessions: i64,
    pub days_covered: i64,
    pub sample_ok: bool,
    // (3) shadow approved
    pub hit_rate: Option<f64>,
    pub mean_hit_cost_ms: Option<f64>,
    pub shadow_approved: bool,
    // (4) no regression of the avoidable fraction
    pub avoidable_now: Option<f64>,
    pub avoidable_prev: Option<f64>,
    pub regression_ok: bool,
    // (5) minimum human review
    pub human_marks: i64,
    pub human_rate: Option<f64>,
    pub human_ok: bool,
    /// All five hold. Only a signal: the owner promotes.
    pub ready: bool,
    // aside
    pub median_hit_cost_ms: Option<f64>,
    pub suppress_rate: Option<f64>,
    pub ignored: i64,
}

/// Builds the promotion report of one pair. See the module contract.
pub async fn action_promotion_report(
    pool: &SqlitePool,
    cfg: &DevtimeActionsConfig,
    precision: &RulePrecision,
    scope: &PromotionScope<'_>,
) -> sqlx::Result<PromotionReport> {
    let rows = crate::devtime::store::decisions_for(
        pool,
        scope.action,
        scope.rule_id,
        "shadow",
        &[scope.action_version],
    )
    .await?;
    let in_scope: Vec<_> = rows
        .iter()
        .filter(|r| r.rule_version == scope.rule_version)
        .filter(|r| scope.project_id.is_none_or(|p| r.project_id == p))
        .collect();

    let suppressed = in_scope.iter().filter(|r| r.decision == "suppress").count();
    let suppress_rate = (!in_scope.is_empty()).then(|| suppressed as f64 / in_scope.len() as f64);

    let counted: Vec<_> = in_scope
        .iter()
        .filter(|r| r.decision == "fire" && r.outcome_state == "resolved")
        .collect();

    // The human verdict prevails over the automatic outcome.
    let effective = |r: &crate::devtime::store::ShadowRow| match r.human_verdict.as_deref() {
        Some("useful") => Some(true),
        Some("noise") => Some(false),
        _ => match r.outcome.as_deref() {
            Some("hit") => Some(true),
            Some("miss") => Some(false),
            _ => None,
        },
    };
    let ignored = counted
        .iter()
        .filter(|r| r.human_verdict.is_none() && r.outcome.as_deref() == Some("ignored"))
        .count() as i64;

    let sample: Vec<_> = counted
        .iter()
        .filter_map(|r| effective(r).map(|hit| (**r, hit)))
        .collect();
    let n_resolved = sample.len() as i64;
    let sessions: std::collections::HashSet<&str> = sample
        .iter()
        .map(|(r, _)| r.session_id.as_deref().unwrap_or(r.subject_key.as_str()))
        .collect();
    let n_sessions = sessions.len() as i64;
    let earliest = sample
        .iter()
        .filter_map(|(r, _)| chrono::DateTime::parse_from_rfc3339(&r.decided_at).ok())
        .map(|t| t.timestamp_millis())
        .min();
    let days_covered = earliest.map_or(0, |e| (scope.now_ms - e).div_euclid(DAY_MS));
    let sample_ok = n_resolved >= i64::from(cfg.effect_min_resolved)
        && n_sessions >= i64::from(cfg.effect_min_sessions)
        && days_covered >= i64::from(cfg.effect_min_days);

    let hits: Vec<_> = sample
        .iter()
        .filter(|(_, hit)| *hit)
        .map(|(r, _)| *r)
        .collect();
    let hit_rate = (n_resolved > 0).then(|| hits.len() as f64 / n_resolved as f64);
    let mut costs: Vec<i64> = hits
        .iter()
        .map(|r| r.outcome_cost_ms.unwrap_or(0))
        .collect();
    costs.sort_unstable();
    let mean_hit_cost_ms =
        (!costs.is_empty()).then(|| costs.iter().sum::<i64>() as f64 / costs.len() as f64);
    let median_hit_cost_ms = match costs.len() {
        0 => None,
        n if n % 2 == 1 => Some(costs[n / 2] as f64),
        n => Some((costs[n / 2 - 1] + costs[n / 2]) as f64 / 2.0),
    };
    let hit_rate_min = match scope.action {
        "inject" => cfg.shadow_hit_rate_min.inject,
        _ => cfg.shadow_hit_rate_min.propose,
    };
    let shadow_approved = hit_rate.is_some_and(|h| h >= hit_rate_min)
        && mean_hit_cost_ms.is_some_and(|m| m >= cfg.min_mean_hit_cost_ms as f64);

    // Avoidable fraction per window, kept as (cost, active) integers so the +10% test is exact.
    let window_ms = i64::from(cfg.effect_min_days) * DAY_MS;
    let now = avoidable_window(pool, scope, scope.now_ms - window_ms, scope.now_ms).await?;
    let prev = avoidable_window(
        pool,
        scope,
        scope.now_ms - 2 * window_ms,
        scope.now_ms - window_ms,
    )
    .await?;
    let fraction = |w: Option<(i64, i64)>| w.map(|(cost, active)| cost as f64 / active as f64);
    let regression_ok = match (prev, now) {
        (Some((prev_cost, prev_active)), Some((now_cost, now_active))) if prev_cost > 0 => {
            // now/now_active <= prev/prev_active * 1.10, cross-multiplied.
            i128::from(now_cost) * i128::from(prev_active) * 10
                <= i128::from(prev_cost) * i128::from(now_active) * 11
        }
        _ => true,
    };

    let useful = counted
        .iter()
        .filter(|r| r.human_verdict.as_deref() == Some("useful"))
        .count() as i64;
    let noise = counted
        .iter()
        .filter(|r| r.human_verdict.as_deref() == Some("noise"))
        .count() as i64;
    let human_marks = useful + noise;
    let human_rate = (human_marks > 0).then(|| useful as f64 / human_marks as f64);
    let human_ok = human_marks >= i64::from(cfg.human_min_marks)
        && human_rate.is_some_and(|h| h >= hit_rate_min);

    let action_eligible = action_eligible(precision, cfg);
    let ready = action_eligible && sample_ok && shadow_approved && regression_ok && human_ok;
    Ok(PromotionReport {
        action_eligible,
        wilson_lower: wilson_lower(
            precision.n_cases - precision.n_not_rework,
            precision.n_cases,
        ),
        n_resolved,
        n_sessions,
        days_covered,
        sample_ok,
        hit_rate,
        mean_hit_cost_ms,
        shadow_approved,
        avoidable_now: fraction(now),
        avoidable_prev: fraction(prev),
        regression_ok,
        human_marks,
        human_rate,
        human_ok,
        ready,
        median_hit_cost_ms,
        suppress_rate,
        ignored,
    })
}

const DAY_MS: i64 = 86_400_000;

/// `(rule cost, active time)` of the window `(after_ms, until_ms]` by `started_at`; `None` when the
/// window has no active time.
async fn avoidable_window(
    pool: &SqlitePool,
    scope: &PromotionScope<'_>,
    after_ms: i64,
    until_ms: i64,
) -> sqlx::Result<Option<(i64, i64)>> {
    let stamp = |ms: i64| {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap_or_default()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    };
    let (after, until) = (stamp(after_ms), stamp(until_ms));
    let active: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(active_ms), 0) FROM devtime_turn_stats \
         WHERE started_at > ?1 AND started_at <= ?2 AND (?3 IS NULL OR project_id = ?3)",
    )
    .bind(&after)
    .bind(&until)
    .bind(scope.project_id)
    .fetch_one(pool)
    .await?;
    if active <= 0 {
        return Ok(None);
    }
    let cost: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(cost_ms), 0) FROM devtime_findings \
         WHERE rule_id = ?4 AND rule_version = ?5 AND scope <> 'cross' \
         AND started_at > ?1 AND started_at <= ?2 AND (?3 IS NULL OR project_id = ?3)",
    )
    .bind(&after)
    .bind(&until)
    .bind(scope.project_id)
    .bind(scope.rule_id)
    .bind(scope.rule_version)
    .fetch_one(pool)
    .await?;
    Ok(Some((cost, active)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime::rules::Level;
    use crate::devtime::store as st;

    /// `2026-10-05T10:30:00.000Z`.
    const NOW_MS: i64 = 1_791_196_200_000;
    const DAY_MS: i64 = 86_400_000;
    const HOUR_MS: i64 = 3_600_000;
    const ACTION_VERSION: i64 = 1;
    const RULE_VERSION: i64 = 1;

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    fn ts(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    fn scope() -> PromotionScope<'static> {
        PromotionScope {
            action: "inject",
            rule_id: "D3",
            project_id: Some("p1"),
            rule_version: RULE_VERSION,
            action_version: ACTION_VERSION,
            now_ms: NOW_MS,
        }
    }

    /// D3 with 38 of 40 right: Wilson lower ~0.83, above the 0.6 of D1.
    fn good_precision() -> RulePrecision {
        RulePrecision {
            rule_id: "D3".into(),
            level: Level::Base,
            n_cases: 40,
            n_not_rework: 2,
            precision: Some(0.95),
            prior: 0.5,
            eligible: true,
        }
    }

    /// What a fixture world is made of. `Default` is the all-criteria-met world below.
    struct World {
        /// Shadow `fire` rows of the current versions, resolved.
        rows: usize,
        /// Distinct sessions the rows are spread over.
        sessions: usize,
        /// The earliest row was decided this many days before now; the rest follow hourly.
        first_days_ago: i64,
        /// The first `hits` rows are automatic hits (cost `hit_cost_ms`), the rest misses.
        hits: usize,
        hit_cost_ms: i64,
        /// `useful` marks on the first rows (hits unless `hits` is smaller).
        useful_marks: usize,
        /// `noise` marks on the LAST rows (misses unless `hits` reaches them).
        noise_marks: usize,
        /// `avoidable` cost of the rule in the last 14 days / in the 14 before.
        cost_now: i64,
        cost_prev: Option<i64>,
    }

    impl Default for World {
        fn default() -> Self {
            World {
                rows: 30,
                sessions: 12,
                first_days_ago: 20,
                hits: 24,
                hit_cost_ms: 60_000,
                useful_marks: 6,
                noise_marks: 0,
                cost_now: 100_000,
                cost_prev: Some(100_000),
            }
        }
    }

    async fn shadow_row(
        pool: &SqlitePool,
        tag: &str,
        session: &str,
        decided_ms: i64,
        edit: impl FnOnce(&mut st::ShadowRow),
    ) -> i64 {
        let mut row = st::ShadowRow {
            action: "inject".into(),
            rule_id: "D3".into(),
            project_id: "p1".into(),
            session_id: Some(session.into()),
            subject_key: format!("D3|p1|{tag}"),
            mode: "shadow".into(),
            decision: "fire".into(),
            template_id: "inject.d3.v1".into(),
            rule_version: RULE_VERSION,
            action_version: ACTION_VERSION,
            thresholds_hash: "th".into(),
            rules_version: "rv".into(),
            decided_at: ts(decided_ms),
            window_end_at: ts(decided_ms + HOUR_MS / 2),
            outcome_state: "resolved".into(),
            outcome: Some("miss".into()),
            outcome_cost_ms: Some(0),
            resolved_at: Some(ts(decided_ms + HOUR_MS)),
            ..st::ShadowRow::default()
        };
        edit(&mut row);
        assert!(st::insert_decision(pool, &row).await.unwrap());
        sqlx::query_scalar("SELECT id FROM devtime_shadow WHERE subject_key = ? AND mode = ?")
            .bind(&row.subject_key)
            .bind(&row.mode)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn finding(pool: &SqlitePool, key: &str, cost_ms: i64, started_ms: i64) {
        sqlx::query(
            "INSERT INTO devtime_findings (finding_key, project_id, session_id, scope, rule_id, \
             rule_version, level, waste, lever, confidence, lane, started_at, ended_at, cost_ms, \
             count, sessions, rules_version, parser_version) VALUES (?, 'p1', 's0', 'session', \
             'D3', ?, 'base', 'rework', 'x', 'exact', 'main', ?, ?, ?, 1, '[]', 'rv', 1)",
        )
        .bind(key)
        .bind(RULE_VERSION)
        .bind(ts(started_ms))
        .bind(ts(started_ms + 60_000))
        .bind(cost_ms)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn turn_stat(pool: &SqlitePool, seq: i64, active_ms: i64, started_ms: i64) {
        sqlx::query(
            "INSERT INTO devtime_turn_stats (session_id, turn_seq, project_id, turn_class, \
             started_at, calls, active_ms, explained_ms, rules_version) \
             VALUES ('s0', ?, 'p1', 'work', ?, 1, ?, 0, 'rv')",
        )
        .bind(seq)
        .bind(ts(started_ms))
        .bind(active_ms)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Seeds the world; returns the ids of the shadow rows in order.
    async fn seed(pool: &SqlitePool, w: &World) -> Vec<i64> {
        let mut ids = Vec::new();
        for i in 0..w.rows {
            let decided = NOW_MS - w.first_days_ago * DAY_MS + (i as i64) * HOUR_MS;
            let hit = i < w.hits;
            let session = format!("s{}", i % w.sessions);
            let id = shadow_row(pool, &i.to_string(), &session, decided, |r| {
                if hit {
                    r.outcome = Some("hit".into());
                    r.outcome_cost_ms = Some(w.hit_cost_ms);
                    r.outcome_finding_key = Some(format!("D3-{i}"));
                }
            })
            .await;
            ids.push(id);
        }
        for id in ids.iter().take(w.useful_marks) {
            assert!(st::set_human_verdict(pool, *id, "useful").await.unwrap());
        }
        for id in ids.iter().rev().take(w.noise_marks) {
            assert!(st::set_human_verdict(pool, *id, "noise").await.unwrap());
        }
        // One turn of 1_000_000 ms of activity in each 14-day window; the rule's cost decides the fraction.
        turn_stat(pool, 0, 1_000_000, NOW_MS - 5 * DAY_MS).await;
        finding(pool, "now", w.cost_now, NOW_MS - 5 * DAY_MS).await;
        if let Some(prev) = w.cost_prev {
            turn_stat(pool, 1, 1_000_000, NOW_MS - 20 * DAY_MS).await;
            finding(pool, "prev", prev, NOW_MS - 20 * DAY_MS).await;
        }
        ids
    }

    async fn report(pool: &SqlitePool, p: &RulePrecision) -> PromotionReport {
        action_promotion_report(pool, &DevtimeActionsConfig::default(), p, &scope())
            .await
            .unwrap()
    }

    fn close(got: Option<f64>, want: f64) -> bool {
        got.is_some_and(|g| (g - want).abs() < 1e-9)
    }

    #[tokio::test]
    async fn promotion_ready_when_every_criterion_holds() {
        let pool = pool().await;
        seed(&pool, &World::default()).await;
        let r = report(&pool, &good_precision()).await;
        assert!(r.action_eligible && r.wilson_lower >= 0.6);
        assert_eq!(r.n_resolved, 30);
        assert_eq!(r.n_sessions, 12);
        assert_eq!(r.days_covered, 20);
        assert!(r.sample_ok);
        assert!(close(r.hit_rate, 0.8));
        assert!(close(r.mean_hit_cost_ms, 60_000.0));
        assert!(r.shadow_approved);
        assert!(close(r.avoidable_now, 0.1) && close(r.avoidable_prev, 0.1));
        assert!(r.regression_ok);
        assert_eq!(r.human_marks, 6);
        assert!(close(r.human_rate, 1.0));
        assert!(r.human_ok);
        assert!(r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_when_rule_is_not_action_eligible() {
        let pool = pool().await;
        seed(&pool, &World::default()).await;
        let p = RulePrecision {
            eligible: false,
            ..good_precision()
        };
        let r = report(&pool, &p).await;
        assert!(!r.action_eligible);
        assert!(r.sample_ok && r.shadow_approved && r.regression_ok && r.human_ok);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_when_wilson_lower_is_below_the_bound() {
        let pool = pool().await;
        seed(&pool, &World::default()).await;
        // 16 right of 20: still `eligible` by floor, but the Wilson lower bound is ~0.58 < 0.6.
        let p = RulePrecision {
            n_cases: 20,
            n_not_rework: 4,
            precision: Some(0.8),
            ..good_precision()
        };
        let r = report(&pool, &p).await;
        assert!(r.wilson_lower < 0.6 && r.wilson_lower > 0.5);
        assert!(!r.action_eligible);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_below_30_resolved() {
        let pool = pool().await;
        seed(
            &pool,
            &World {
                rows: 29,
                hits: 23,
                ..World::default()
            },
        )
        .await;
        let r = report(&pool, &good_precision()).await;
        assert_eq!(r.n_resolved, 29);
        assert!(!r.sample_ok);
        assert!(r.action_eligible && r.shadow_approved && r.regression_ok && r.human_ok);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_below_10_sessions() {
        let pool = pool().await;
        seed(
            &pool,
            &World {
                sessions: 9,
                ..World::default()
            },
        )
        .await;
        let r = report(&pool, &good_precision()).await;
        assert_eq!(r.n_resolved, 30);
        assert_eq!(r.n_sessions, 9);
        assert!(!r.sample_ok);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_under_14_days_of_coverage() {
        let pool = pool().await;
        seed(
            &pool,
            &World {
                first_days_ago: 13,
                ..World::default()
            },
        )
        .await;
        let r = report(&pool, &good_precision()).await;
        assert_eq!(r.days_covered, 13);
        assert_eq!(r.n_resolved, 30);
        assert!(!r.sample_ok);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn promotion_ignores_live_rows() {
        let pool = pool().await;
        seed(&pool, &World::default()).await;
        for i in 0..5 {
            shadow_row(
                &pool,
                &format!("live{i}"),
                &format!("s{i}"),
                NOW_MS - 3 * DAY_MS,
                |r| {
                    r.mode = "live".into();
                    r.outcome = Some("hit".into());
                    r.outcome_cost_ms = Some(60_000);
                },
            )
            .await;
        }
        let r = report(&pool, &good_precision()).await;
        assert_eq!(r.n_resolved, 30, "live rows are not shadow evidence");
        assert!(close(r.hit_rate, 0.8));
        assert!(r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_when_live_rows_were_the_only_surplus() {
        let pool = pool().await;
        // 25 shadow rows plus 5 live ones make 30 rows in total, but only 25 are shadow.
        seed(
            &pool,
            &World {
                rows: 25,
                hits: 20,
                ..World::default()
            },
        )
        .await;
        for i in 0..5 {
            shadow_row(
                &pool,
                &format!("live{i}"),
                &format!("s{i}"),
                NOW_MS - 3 * DAY_MS,
                |r| {
                    r.mode = "live".into();
                    r.outcome = Some("hit".into());
                },
            )
            .await;
        }
        let r = report(&pool, &good_precision()).await;
        assert_eq!(r.n_resolved, 25);
        assert!(!r.sample_ok && !r.ready);
    }

    #[tokio::test]
    async fn promotion_does_not_count_rows_of_an_old_action_version() {
        let pool = pool().await;
        seed(
            &pool,
            &World {
                rows: 25,
                hits: 20,
                ..World::default()
            },
        )
        .await;
        for i in 0..5 {
            shadow_row(
                &pool,
                &format!("old{i}"),
                &format!("s{i}"),
                NOW_MS - 3 * DAY_MS,
                |r| {
                    r.action_version = 0;
                    r.outcome = Some("hit".into());
                },
            )
            .await;
        }
        let r = report(&pool, &good_precision()).await;
        assert_eq!(r.n_resolved, 25);
        assert!(!r.sample_ok && !r.ready);
    }

    #[tokio::test]
    async fn promotion_does_not_count_rows_of_an_old_rule_version() {
        let pool = pool().await;
        seed(
            &pool,
            &World {
                rows: 25,
                hits: 20,
                ..World::default()
            },
        )
        .await;
        for i in 0..5 {
            shadow_row(
                &pool,
                &format!("oldrule{i}"),
                &format!("s{i}"),
                NOW_MS - 3 * DAY_MS,
                |r| {
                    r.rule_version = 0;
                    r.outcome = Some("hit".into());
                },
            )
            .await;
        }
        let r = report(&pool, &good_precision()).await;
        assert_eq!(r.n_resolved, 25);
        assert!(!r.sample_ok && !r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_when_hit_rate_is_below_the_minimum() {
        let pool = pool().await;
        // 17 of 30 = 0.567 < 0.6 (inject); the 6 useful marks sit on hits, so they change nothing.
        seed(
            &pool,
            &World {
                hits: 17,
                ..World::default()
            },
        )
        .await;
        let r = report(&pool, &good_precision()).await;
        assert!(close(r.hit_rate, 17.0 / 30.0));
        assert!(!r.shadow_approved);
        assert!(r.action_eligible && r.sample_ok && r.regression_ok && r.human_ok);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_when_mean_hit_cost_is_below_the_minimum() {
        let pool = pool().await;
        seed(
            &pool,
            &World {
                hit_cost_ms: 20_000,
                ..World::default()
            },
        )
        .await;
        let r = report(&pool, &good_precision()).await;
        assert!(close(r.hit_rate, 0.8), "the rate alone would pass");
        assert!(close(r.mean_hit_cost_ms, 20_000.0));
        assert!(!r.shadow_approved);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_by_a_relative_regression_over_10_percent() {
        let pool = pool().await;
        // 0.12 now against 0.10 before: +20% relative.
        seed(
            &pool,
            &World {
                cost_now: 120_000,
                ..World::default()
            },
        )
        .await;
        let r = report(&pool, &good_precision()).await;
        assert!(close(r.avoidable_now, 0.12) && close(r.avoidable_prev, 0.10));
        assert!(!r.regression_ok);
        assert!(r.sample_ok && r.shadow_approved && r.human_ok);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn promotion_allows_a_small_rise_and_an_improvement() {
        let pool = pool().await;
        // +5% relative (0.105 against 0.10) is within the 10% tolerance.
        seed(
            &pool,
            &World {
                cost_now: 105_000,
                ..World::default()
            },
        )
        .await;
        let r = report(&pool, &good_precision()).await;
        assert!(r.regression_ok && r.ready);
    }

    #[tokio::test]
    async fn promotion_without_a_baseline_window_does_not_block() {
        let pool = pool().await;
        seed(
            &pool,
            &World {
                cost_prev: None,
                ..World::default()
            },
        )
        .await;
        let r = report(&pool, &good_precision()).await;
        assert!(close(r.avoidable_now, 0.1));
        assert_eq!(r.avoidable_prev, None);
        assert!(r.regression_ok && r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_below_5_human_marks() {
        let pool = pool().await;
        seed(
            &pool,
            &World {
                useful_marks: 4,
                ..World::default()
            },
        )
        .await;
        let r = report(&pool, &good_precision()).await;
        assert_eq!(r.human_marks, 4);
        assert!(
            close(r.human_rate, 1.0),
            "a perfect rate does not make up for few marks"
        );
        assert!(!r.human_ok);
        assert!(r.action_eligible && r.sample_ok && r.shadow_approved && r.regression_ok);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn promotion_blocked_when_the_human_rate_is_below_the_threshold() {
        let pool = pool().await;
        // 3 useful on hits, 3 noise on the last rows (misses): 3 / 6 = 0.5 < 0.6, hit rate unchanged.
        seed(
            &pool,
            &World {
                useful_marks: 3,
                noise_marks: 3,
                ..World::default()
            },
        )
        .await;
        let r = report(&pool, &good_precision()).await;
        assert_eq!(r.human_marks, 6);
        assert!(close(r.human_rate, 0.5));
        assert!(close(r.hit_rate, 0.8));
        assert!(!r.human_ok);
        assert!(r.shadow_approved);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn human_noise_prevails_over_an_automatic_hit() {
        let pool = pool().await;
        // 24 automatic hits, but 12 of them marked noise: effective 12 / 30 = 0.4.
        let ids = seed(
            &pool,
            &World {
                useful_marks: 0,
                ..World::default()
            },
        )
        .await;
        for id in ids.iter().take(12) {
            st::set_human_verdict(&pool, *id, "noise").await.unwrap();
        }
        let r = report(&pool, &good_precision()).await;
        assert!(close(r.hit_rate, 0.4));
        assert!(!r.shadow_approved);
        assert_eq!(r.human_marks, 12);
        assert!(close(r.human_rate, 0.0));
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn human_useful_prevails_over_an_automatic_miss() {
        let pool = pool().await;
        // 16 automatic hits: 16 / 30 = 0.533 would fail on its own...
        let ids = seed(
            &pool,
            &World {
                hits: 16,
                useful_marks: 0,
                ..World::default()
            },
        )
        .await;
        // ...then 6 misses (rows 16..22) marked useful: effective 22 / 30 = 0.733.
        for id in &ids[16..22] {
            st::set_human_verdict(&pool, *id, "useful").await.unwrap();
        }
        let r = report(&pool, &good_precision()).await;
        assert!(close(r.hit_rate, 22.0 / 30.0));
        assert!(
            r.shadow_approved,
            "the cost of a useful-marked miss counts as 0 in the mean"
        );
        assert!(close(r.mean_hit_cost_ms, 16.0 * 60_000.0 / 22.0));
        assert_eq!(r.human_marks, 6);
        assert!(r.human_ok);
        assert!(r.ready);
    }

    #[tokio::test]
    async fn median_cost_is_reported_aside_and_does_not_gate_ready() {
        let pool = pool().await;
        let ids = seed(&pool, &World::default()).await;
        // Two hits become 600_000: 22 values at 60_000 and 2 at 600_000.
        sqlx::query("UPDATE devtime_shadow SET outcome_cost_ms = 600000 WHERE id IN (?, ?)")
            .bind(ids[10])
            .bind(ids[11])
            .execute(&pool)
            .await
            .unwrap();
        let r = report(&pool, &good_precision()).await;
        assert!(close(r.median_hit_cost_ms, 60_000.0));
        assert!(close(r.mean_hit_cost_ms, 105_000.0));
        assert!(r.ready);
    }

    #[tokio::test]
    async fn suppress_rate_is_suppressed_over_all_current_decisions() {
        let pool = pool().await;
        seed(&pool, &World::default()).await;
        for i in 0..10 {
            shadow_row(
                &pool,
                &format!("sup{i}"),
                &format!("s{i}"),
                NOW_MS - 2 * DAY_MS,
                |r| {
                    r.decision = "suppress".into();
                    r.suppress_reason = Some("cap".into());
                    r.outcome = Some("hit".into());
                },
            )
            .await;
        }
        let r = report(&pool, &good_precision()).await;
        assert!(close(r.suppress_rate, 10.0 / 40.0));
        assert_eq!(
            r.n_resolved, 30,
            "suppressed rows are not part of the sample"
        );
        assert!(r.ready, "the suppress rate never gates ready");
    }

    #[tokio::test]
    async fn ignored_counts_rows_without_a_hit_or_miss_and_stays_out_of_the_sample() {
        let pool = pool().await;
        seed(&pool, &World::default()).await;
        for i in 0..3 {
            shadow_row(
                &pool,
                &format!("ign{i}"),
                &format!("s{i}"),
                NOW_MS - 2 * DAY_MS,
                |r| r.outcome = Some("ignored".into()),
            )
            .await;
        }
        let r = report(&pool, &good_precision()).await;
        assert_eq!(r.ignored, 3);
        assert_eq!(r.n_resolved, 30);
        assert!(close(r.hit_rate, 0.8));
        assert!(r.ready, "ignored never gates ready");
    }
}
