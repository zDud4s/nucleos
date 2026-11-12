//! Automatic rollback of a live `(action, rule)` pair back to `shadow` (actions spec §6.2) and the
//! re-entry guard that follows it.
//!
//! The pair is judged over its last `rollback_window` resolved `live` decisions taken since it was
//! promoted. It goes back to `shadow` (`changed_by = 'rollback'`, with a closed `reason`) when any of
//! four conditions holds: its avoidable fraction is at least 15% worse than the control's, the owner
//! rejects or ignores at least 70% of its proposals, the owner marks two decisions in a row `noise`,
//! or its rule is no longer eligible. The first three need the window; the fourth is immediate.
//!
//! Carries ids, counts, ratios and milliseconds only, never message text.

use sqlx::SqlitePool;

use super::{ACTION_VERSION, ms_text, parse_ms};
use crate::config::{DevtimeActionsConfig, DevtimeConfig};
use crate::devtime::precision::rule_report;
use crate::devtime::store::{self as st, ActionStateRow, ShadowRow};

const DAY_MS: i64 = 86_400_000;
/// The live fraction is worse than the control's by at least this much, relatively.
pub const WORSE_RATIO: f64 = 1.15;
/// A `propose` pair whose `rejected + ignored` share reaches this rolls back.
pub const REJECT_RATE: f64 = 0.7;
/// The baseline looks this far before the promotion when there is no holdout control.
pub const BASELINE_DAYS: i64 = 30;
/// New shadow decisions a rolled-back pair needs before it can be promoted again.
pub const REENTRY_NEW_DECISIONS: i64 = 30;

/// Closed reason tokens written to `devtime_action_state.reason` (followed by `:` and numbers only).
pub const REASON_AVOIDABLE_WORSE: &str = "avoidable_worse";
pub const REASON_REJECTED_IGNORED: &str = "rejected_ignored";
pub const REASON_NOISE_STREAK: &str = "noise_streak";
pub const REASON_NOT_ELIGIBLE: &str = "not_eligible";

/// One resolved live decision, as far as the rollback conditions look at it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveDecision {
    /// `accepted` | `rejected` | `ignored` | `hit` | `miss`.
    pub outcome: Option<String>,
    /// `useful` | `noise`.
    pub verdict: Option<String>,
}

/// Everything [`decide_rollback`] weighs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RollbackInput {
    pub action: String,
    /// The rule is still `precision::eligible`.
    pub eligible: bool,
    /// `rollback_window`.
    pub window: usize,
    /// The last resolved live decisions since the promotion, oldest first (at most `window`).
    pub decisions: Vec<LiveDecision>,
    /// Avoidable fraction in the sessions where the live action ran.
    pub live_fraction: Option<f64>,
    /// Avoidable fraction of the control (holdout sessions, else the baseline before promotion).
    pub control_fraction: Option<f64>,
}

/// PURE: the reason to roll back, or `None`. Conditions run in the order eligibility, noise streak,
/// rejection rate, avoidable fraction; the first that holds names the reason.
pub fn decide_rollback(input: &RollbackInput) -> Option<String> {
    if !input.eligible {
        return Some(REASON_NOT_ELIGIBLE.to_string());
    }
    let marked: Vec<&str> = input
        .decisions
        .iter()
        .filter_map(|d| d.verdict.as_deref())
        .collect();
    if marked.len() >= 2 && marked[marked.len() - 2..].iter().all(|v| *v == "noise") {
        return Some(REASON_NOISE_STREAK.to_string());
    }
    // The rate and the comparison are over a full window: a handful of early decisions is not
    // evidence, while the two conditions above are cheap to trip on purpose (spec §6.2).
    let full = input.window > 0 && input.decisions.len() >= input.window;
    if !full {
        return None;
    }
    if input.action == "propose" {
        let refused = input
            .decisions
            .iter()
            .filter(|d| matches!(d.outcome.as_deref(), Some("rejected" | "ignored")))
            .count();
        let rate = refused as f64 / input.decisions.len() as f64;
        if rate >= REJECT_RATE {
            return Some(format!("{REASON_REJECTED_IGNORED}:{rate:.2}"));
        }
    }
    match (input.live_fraction, input.control_fraction) {
        (Some(live), Some(control)) if live > 0.0 && live >= control * WORSE_RATIO => Some(
            format!("{REASON_AVOIDABLE_WORSE}:live={live:.3},control={control:.3}"),
        ),
        _ => None,
    }
}

/// What keeps a rolled-back pair from being promoted again yet.
#[derive(Debug, Clone, PartialEq)]
pub enum ReentryBlock {
    /// Fewer than `reentry_days` since the rollback.
    TooSoon { days_left: i64 },
    /// Fewer than [`REENTRY_NEW_DECISIONS`] resolved shadow decisions since, of the current
    /// `action_version`.
    TooFewDecisions { have: i64 },
}

/// The re-entry guard (spec §6.2): a pair whose state is a `rollback` back to `shadow` may be
/// promoted again only after `reentry_days` and 30 new resolved shadow decisions of the current
/// `action_version` (older ones do not count). A pair in any other state is not blocked here; the
/// §5 criteria still apply on top.
pub async fn reentry_blocked(
    pool: &SqlitePool,
    cfg: &DevtimeActionsConfig,
    action: &str,
    rule_id: &str,
    now_ms: i64,
) -> sqlx::Result<Option<ReentryBlock>> {
    let Some(state) = st::get_action_state(pool, action, rule_id).await? else {
        return Ok(None);
    };
    if state.changed_by != "rollback" || state.mode != "shadow" {
        return Ok(None);
    }
    let since_ms = parse_ms(&state.since).unwrap_or(0);
    let ready_at = since_ms + i64::from(cfg.reentry_days) * DAY_MS;
    if now_ms < ready_at {
        let days_left = (ready_at - now_ms + DAY_MS - 1) / DAY_MS;
        return Ok(Some(ReentryBlock::TooSoon { days_left }));
    }
    let have: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM devtime_shadow WHERE action = ? AND rule_id = ? AND mode = 'shadow' \
         AND decision = 'fire' AND outcome_state = 'resolved' AND action_version = ? \
         AND decided_at >= ?",
    )
    .bind(action)
    .bind(rule_id)
    .bind(ACTION_VERSION)
    .bind(&state.since)
    .fetch_one(pool)
    .await?;
    if have < REENTRY_NEW_DECISIONS {
        return Ok(Some(ReentryBlock::TooFewDecisions { have }));
    }
    Ok(None)
}

/// `Σ cost_ms of the rule's findings / Σ active_ms` over the sessions, `None` without active time.
async fn fraction_for_sessions(
    pool: &SqlitePool,
    rule_id: &str,
    rule_version: i64,
    sessions: &[String],
) -> sqlx::Result<Option<f64>> {
    if sessions.is_empty() {
        return Ok(None);
    }
    let mut qb =
        sqlx::QueryBuilder::new("SELECT COALESCE(SUM(active_ms), 0) FROM devtime_turn_stats WHERE");
    push_in(&mut qb, " session_id", sessions);
    let active: i64 = qb.build_query_scalar().fetch_one(pool).await?;
    if active <= 0 {
        return Ok(None);
    }
    let mut qb = sqlx::QueryBuilder::new(
        "SELECT COALESCE(SUM(cost_ms), 0) FROM devtime_findings WHERE scope <> 'cross' AND rule_id = ",
    );
    qb.push_bind(rule_id.to_string());
    qb.push(" AND rule_version = ").push_bind(rule_version);
    qb.push(" AND");
    push_in(&mut qb, " session_id", sessions);
    let cost: i64 = qb.build_query_scalar().fetch_one(pool).await?;
    Ok(Some(cost as f64 / active as f64))
}

/// Appends `<column> IN (?, ?, ...)` with one bound value per item.
fn push_in(qb: &mut sqlx::QueryBuilder, column: &str, items: &[String]) {
    qb.push(column).push(" IN (");
    let mut sep = qb.separated(", ");
    for item in items {
        sep.push_bind(item.clone());
    }
    qb.push(")");
}

/// Appends the window and project filter shared by both queries of [`fraction_for_window`].
fn push_window(
    qb: &mut sqlx::QueryBuilder,
    projects: &[String],
    after: &str,
    until: &str,
) {
    qb.push(" AND started_at > ").push_bind(after.to_string());
    qb.push(" AND started_at <= ").push_bind(until.to_string());
    qb.push(" AND");
    push_in(qb, " project_id", projects);
}

/// The same fraction over the window `(after, until]` by `started_at`, for the given projects.
async fn fraction_for_window(
    pool: &SqlitePool,
    rule_id: &str,
    rule_version: i64,
    projects: &[String],
    after: &str,
    until: &str,
) -> sqlx::Result<Option<f64>> {
    if projects.is_empty() {
        return Ok(None);
    }
    let mut qb = sqlx::QueryBuilder::new(
        "SELECT COALESCE(SUM(active_ms), 0) FROM devtime_turn_stats WHERE 1 = 1",
    );
    push_window(&mut qb, projects, after, until);
    let active: i64 = qb.build_query_scalar().fetch_one(pool).await?;
    if active <= 0 {
        return Ok(None);
    }
    let mut qb = sqlx::QueryBuilder::new(
        "SELECT COALESCE(SUM(cost_ms), 0) FROM devtime_findings WHERE scope <> 'cross' AND rule_id = ",
    );
    qb.push_bind(rule_id.to_string());
    qb.push(" AND rule_version = ").push_bind(rule_version);
    push_window(&mut qb, projects, after, until);
    let cost: i64 = qb.build_query_scalar().fetch_one(pool).await?;
    Ok(Some(cost as f64 / active as f64))
}

/// Live measurement of one pair: its last resolved live decisions since `state.since`, and the
/// live and control avoidable fractions. Control is the holdout sessions (shadow `fire` rows with
/// `suppress_reason = 'holdout'` since the promotion) when they have active time, else the 30 days
/// before the promotion in the same projects.
pub async fn measure(
    pool: &SqlitePool,
    cfg: &DevtimeActionsConfig,
    state: &ActionStateRow,
    rule_version: i64,
) -> sqlx::Result<(Vec<ShadowRow>, Option<f64>, Option<f64>)> {
    let mut rows: Vec<ShadowRow> = sqlx::query_as(
        "SELECT * FROM devtime_shadow WHERE action = ? AND rule_id = ? AND mode = 'live' \
         AND decision = 'fire' AND outcome_state = 'resolved' AND action_version = ? \
         AND decided_at >= ? ORDER BY decided_at DESC, id DESC LIMIT ?",
    )
    .bind(&state.action)
    .bind(&state.rule_id)
    .bind(ACTION_VERSION)
    .bind(&state.since)
    .bind(i64::from(cfg.rollback_window))
    .fetch_all(pool)
    .await?;
    rows.reverse();
    let mut sessions: Vec<String> = rows.iter().filter_map(|r| r.session_id.clone()).collect();
    sessions.sort();
    sessions.dedup();
    let live = fraction_for_sessions(pool, &state.rule_id, rule_version, &sessions).await?;

    let holdout: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT session_id FROM devtime_shadow WHERE action = ? AND rule_id = ? \
         AND mode = 'shadow' AND decision = 'fire' AND suppress_reason = 'holdout' \
         AND outcome_state <> 'void' AND session_id IS NOT NULL AND decided_at >= ?",
    )
    .bind(&state.action)
    .bind(&state.rule_id)
    .bind(&state.since)
    .fetch_all(pool)
    .await?;
    let mut control = fraction_for_sessions(pool, &state.rule_id, rule_version, &holdout).await?;
    if control.is_none() {
        let mut projects: Vec<String> = rows.iter().map(|r| r.project_id.clone()).collect();
        projects.sort();
        projects.dedup();
        let since_ms = parse_ms(&state.since).unwrap_or(0);
        control = fraction_for_window(
            pool,
            &state.rule_id,
            rule_version,
            &projects,
            &ms_text(since_ms - BASELINE_DAYS * DAY_MS),
            &state.since,
        )
        .await?;
    }
    Ok((rows, live, control))
}

/// One rollback pass: every configured pair that is `live` is measured and, when a condition holds,
/// written back to `shadow` with `changed_by = 'rollback'`. Returns how many pairs rolled back.
/// A pair already in `shadow` or `off` is never touched, so a second pass is a no-op.
pub async fn run_rollback_pass(
    pool: &SqlitePool,
    cfg: &DevtimeConfig,
    now_ms: i64,
) -> sqlx::Result<usize> {
    if !cfg.actions.enabled {
        return Ok(0);
    }
    let precisions = rule_report(pool, &cfg.rules, None).await?;
    let mut rolled = 0;
    for (action, rule_ids) in &cfg.actions.pairs {
        for rule_id in rule_ids {
            let Some(state) = st::get_action_state(pool, action, rule_id).await? else {
                continue;
            };
            if state.mode != "live" {
                continue;
            }
            let rule_version = crate::devtime::rules::rule(rule_id)
                .map(|r| i64::from(r.version))
                .unwrap_or(0);
            let eligible = precisions
                .iter()
                .find(|p| &p.rule_id == rule_id)
                .is_some_and(|p| p.eligible);
            let (rows, live, control) = measure(pool, &cfg.actions, &state, rule_version).await?;
            let input = RollbackInput {
                action: action.clone(),
                eligible,
                window: cfg.actions.rollback_window as usize,
                decisions: rows
                    .iter()
                    .map(|r| LiveDecision {
                        outcome: r.outcome.clone(),
                        verdict: r.human_verdict.clone(),
                    })
                    .collect(),
                live_fraction: live,
                control_fraction: control,
            };
            let Some(reason) = decide_rollback(&input) else {
                continue;
            };
            st::set_action_state(
                pool,
                &ActionStateRow {
                    action: action.clone(),
                    rule_id: rule_id.clone(),
                    mode: "shadow".to_string(),
                    since: ms_text(now_ms),
                    changed_by: "rollback".to_string(),
                    reason: Some(reason),
                },
            )
            .await?;
            rolled += 1;
        }
    }
    Ok(rolled)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_MS: i64 = 1_791_196_200_000;
    const HOUR_MS: i64 = 3_600_000;

    fn d(outcome: &str, verdict: Option<&str>) -> LiveDecision {
        LiveDecision {
            outcome: Some(outcome.into()),
            verdict: verdict.map(Into::into),
        }
    }

    fn input(action: &str, decisions: Vec<LiveDecision>) -> RollbackInput {
        RollbackInput {
            action: action.into(),
            eligible: true,
            window: 4,
            decisions,
            live_fraction: Some(0.10),
            control_fraction: Some(0.10),
        }
    }

    fn hits(n: usize) -> Vec<LiveDecision> {
        (0..n).map(|_| d("hit", None)).collect()
    }

    #[test]
    fn a_healthy_pair_stays_live() {
        assert_eq!(decide_rollback(&input("inject", hits(4))), None);
    }

    #[test]
    fn a_rule_that_is_no_longer_eligible_rolls_back_at_once_even_without_a_window() {
        let mut i = input("inject", vec![]);
        i.eligible = false;
        assert_eq!(decide_rollback(&i).as_deref(), Some(REASON_NOT_ELIGIBLE));
    }

    #[test]
    fn a_live_fraction_15_percent_worse_rolls_back_and_14_does_not() {
        let mut i = input("inject", hits(4));
        i.live_fraction = Some(0.115);
        let reason = decide_rollback(&i).unwrap();
        assert!(reason.starts_with(REASON_AVOIDABLE_WORSE), "{reason}");
        i.live_fraction = Some(0.114);
        assert_eq!(decide_rollback(&i), None);
    }

    #[test]
    fn a_worse_fraction_needs_a_full_window_and_a_control() {
        let mut i = input("inject", hits(3));
        i.live_fraction = Some(0.5);
        assert_eq!(decide_rollback(&i), None, "window not full");
        let mut i = input("inject", hits(4));
        i.live_fraction = Some(0.5);
        i.control_fraction = None;
        assert_eq!(decide_rollback(&i), None, "no control");
    }

    #[test]
    fn a_zero_control_with_a_positive_live_fraction_is_worse() {
        let mut i = input("inject", hits(4));
        i.live_fraction = Some(0.05);
        i.control_fraction = Some(0.0);
        assert!(decide_rollback(&i).is_some());
    }

    #[test]
    fn propose_rolls_back_at_70_percent_rejected_or_ignored() {
        let mut v = vec![d("rejected", None), d("ignored", None), d("rejected", None)];
        v.push(d("accepted", None));
        let reason = decide_rollback(&input("propose", v)).unwrap();
        assert!(reason.starts_with(REASON_REJECTED_IGNORED), "{reason}");
        let ok = vec![
            d("rejected", None),
            d("accepted", None),
            d("accepted", None),
            d("ignored", None),
        ];
        assert_eq!(decide_rollback(&input("propose", ok)), None);
    }

    #[test]
    fn the_rejection_rate_is_a_propose_condition_only() {
        let v = vec![d("rejected", None); 4];
        assert_eq!(decide_rollback(&input("inject", v)), None);
    }

    #[test]
    fn two_noise_marks_in_a_row_roll_back_even_in_a_short_window() {
        let v = vec![
            d("hit", Some("noise")),
            d("hit", None),
            d("miss", Some("noise")),
        ];
        let reason = decide_rollback(&input("inject", v)).unwrap();
        assert_eq!(reason, REASON_NOISE_STREAK);
    }

    #[test]
    fn a_useful_mark_between_two_noise_marks_breaks_the_streak() {
        let v = vec![
            d("hit", Some("noise")),
            d("hit", Some("useful")),
            d("miss", Some("noise")),
        ];
        assert_eq!(decide_rollback(&input("inject", v)), None);
    }

    // ---- the store-backed parts ----------------------------------------------------------

    fn ts(ms: i64) -> String {
        ms_text(ms)
    }

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn state(pool: &SqlitePool, mode: &str, changed_by: &str, since_ms: i64) {
        st::set_action_state(
            pool,
            &ActionStateRow {
                action: "inject".into(),
                rule_id: "D3".into(),
                mode: mode.into(),
                since: ts(since_ms),
                changed_by: changed_by.into(),
                reason: None,
            },
        )
        .await
        .unwrap();
    }

    async fn decision(
        pool: &SqlitePool,
        tag: &str,
        session: &str,
        mode: &str,
        decided_ms: i64,
        edit: impl FnOnce(&mut ShadowRow),
    ) {
        let mut row = ShadowRow {
            action: "inject".into(),
            rule_id: "D3".into(),
            project_id: "p1".into(),
            session_id: Some(session.into()),
            subject_key: format!("D3|p1|{tag}"),
            mode: mode.into(),
            decision: "fire".into(),
            template_id: "inject.d3.v1".into(),
            rule_version: 1,
            action_version: ACTION_VERSION,
            thresholds_hash: "th".into(),
            rules_version: "rv".into(),
            decided_at: ts(decided_ms),
            window_end_at: ts(decided_ms + HOUR_MS),
            outcome_state: "resolved".into(),
            outcome: Some("hit".into()),
            resolved_at: Some(ts(decided_ms + HOUR_MS)),
            ..ShadowRow::default()
        };
        edit(&mut row);
        assert!(st::insert_decision(pool, &row).await.unwrap());
    }

    async fn finding(pool: &SqlitePool, key: &str, session: &str, cost_ms: i64, started_ms: i64) {
        sqlx::query(
            "INSERT INTO devtime_findings (finding_key, project_id, session_id, scope, rule_id, \
             rule_version, level, waste, lever, confidence, lane, started_at, ended_at, cost_ms, \
             count, sessions, rules_version, parser_version) VALUES (?, 'p1', ?, 'session', \
             'D3', 1, 'base', 'rework', 'x', 'exact', 'main', ?, ?, ?, 1, '[]', 'rv', 1)",
        )
        .bind(key)
        .bind(session)
        .bind(ts(started_ms))
        .bind(ts(started_ms + 60_000))
        .bind(cost_ms)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn turn(pool: &SqlitePool, session: &str, active_ms: i64, started_ms: i64) {
        sqlx::query(
            "INSERT INTO devtime_turn_stats (session_id, turn_seq, project_id, turn_class, \
             started_at, calls, active_ms, explained_ms, rules_version) \
             VALUES (?, 1, 'p1', 'work', ?, 1, ?, 0, 'rv')",
        )
        .bind(session)
        .bind(ts(started_ms))
        .bind(active_ms)
        .execute(pool)
        .await
        .unwrap();
    }

    fn cfg() -> DevtimeConfig {
        let mut c = DevtimeConfig::default();
        c.actions.rollback_window = 3;
        c
    }

    async fn mode_of(pool: &SqlitePool) -> ActionStateRow {
        st::get_action_state(pool, "inject", "D3")
            .await
            .unwrap()
            .unwrap()
    }

    /// Three resolved live decisions in sessions `l0..l2`, promoted 10 days ago.
    async fn live_world(pool: &SqlitePool) {
        let since = NOW_MS - 10 * DAY_MS;
        state(pool, "live", "owner", since).await;
        for i in 0..3 {
            let s = format!("l{i}");
            decision(pool, &s, &s, "live", since + (i + 1) * HOUR_MS, |_| {}).await;
        }
    }

    #[tokio::test]
    async fn the_pass_leaves_a_pair_that_is_not_live_alone() {
        let pool = pool().await;
        state(&pool, "shadow", "owner", NOW_MS - DAY_MS).await;
        assert_eq!(run_rollback_pass(&pool, &cfg(), NOW_MS).await.unwrap(), 0);
        assert_eq!(mode_of(&pool).await.mode, "shadow");
        assert_eq!(mode_of(&pool).await.changed_by, "owner");
    }

    #[tokio::test]
    async fn the_pass_rolls_a_live_pair_back_when_its_rule_is_not_eligible() {
        // No findings or marks: the rule has no cases, so it is not eligible.
        let pool = pool().await;
        live_world(&pool).await;
        assert_eq!(run_rollback_pass(&pool, &cfg(), NOW_MS).await.unwrap(), 1);
        let s = mode_of(&pool).await;
        assert_eq!(
            (s.mode.as_str(), s.changed_by.as_str()),
            ("shadow", "rollback")
        );
        assert_eq!(s.reason.as_deref(), Some(REASON_NOT_ELIGIBLE));
        assert_eq!(s.since, ts(NOW_MS));
    }

    #[tokio::test]
    async fn a_second_pass_after_a_rollback_changes_nothing() {
        let pool = pool().await;
        live_world(&pool).await;
        assert_eq!(run_rollback_pass(&pool, &cfg(), NOW_MS).await.unwrap(), 1);
        assert_eq!(
            run_rollback_pass(&pool, &cfg(), NOW_MS + HOUR_MS)
                .await
                .unwrap(),
            0
        );
        assert_eq!(mode_of(&pool).await.since, ts(NOW_MS));
    }

    #[tokio::test]
    async fn the_pass_does_nothing_when_actions_are_disabled() {
        let pool = pool().await;
        live_world(&pool).await;
        let mut c = cfg();
        c.actions.enabled = false;
        assert_eq!(run_rollback_pass(&pool, &c, NOW_MS).await.unwrap(), 0);
        assert_eq!(mode_of(&pool).await.mode, "live");
    }

    #[tokio::test]
    async fn measure_compares_live_sessions_with_the_baseline_before_the_promotion() {
        let pool = pool().await;
        live_world(&pool).await;
        let since = NOW_MS - 10 * DAY_MS;
        // Live: 30s of D3 cost in 100s of active time over the three live sessions.
        for i in 0..3 {
            let s = format!("l{i}");
            turn(&pool, &s, 100_000 / 3, since + HOUR_MS).await;
        }
        finding(&pool, "fl", "l0", 30_000, since + HOUR_MS).await;
        // Baseline: 10s of cost in 100s of active time, 5 days before the promotion.
        turn(&pool, "b0", 100_000, since - 5 * DAY_MS).await;
        finding(&pool, "fb", "b0", 10_000, since - 5 * DAY_MS).await;
        let state = mode_of(&pool).await;
        let (rows, live, control) = measure(&pool, &cfg().actions, &state, 1).await.unwrap();
        assert_eq!(rows.len(), 3);
        let (live, control) = (live.unwrap(), control.unwrap());
        assert!((live - 0.30).abs() < 0.01, "{live}");
        assert!((control - 0.10).abs() < 1e-9, "{control}");
    }

    #[tokio::test]
    async fn measure_prefers_the_holdout_sessions_over_the_baseline() {
        let pool = pool().await;
        live_world(&pool).await;
        let since = NOW_MS - 10 * DAY_MS;
        decision(&pool, "h0", "h0", "shadow", since + 2 * HOUR_MS, |r| {
            r.suppress_reason = Some("holdout".into());
        })
        .await;
        turn(&pool, "h0", 100_000, since + 2 * HOUR_MS).await;
        finding(&pool, "fh", "h0", 50_000, since + 2 * HOUR_MS).await;
        turn(&pool, "b0", 100_000, since - 5 * DAY_MS).await;
        let state = mode_of(&pool).await;
        let (_, _, control) = measure(&pool, &cfg().actions, &state, 1).await.unwrap();
        assert!((control.unwrap() - 0.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn measure_ignores_live_decisions_from_before_the_current_promotion() {
        let pool = pool().await;
        let since = NOW_MS - 2 * DAY_MS;
        state(&pool, "live", "owner", since).await;
        decision(&pool, "old", "o0", "live", since - DAY_MS, |_| {}).await;
        decision(&pool, "new", "n0", "live", since + HOUR_MS, |_| {}).await;
        let state = mode_of(&pool).await;
        let (rows, _, _) = measure(&pool, &cfg().actions, &state, 1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].subject_key, "D3|p1|new");
    }

    #[tokio::test]
    async fn reentry_is_not_blocked_for_a_pair_that_was_never_rolled_back() {
        let pool = pool().await;
        let c = cfg().actions;
        assert_eq!(
            reentry_blocked(&pool, &c, "inject", "D3", NOW_MS)
                .await
                .unwrap(),
            None
        );
        state(&pool, "shadow", "owner", NOW_MS - DAY_MS).await;
        assert_eq!(
            reentry_blocked(&pool, &c, "inject", "D3", NOW_MS)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn reentry_waits_for_reentry_days() {
        let pool = pool().await;
        let c = cfg().actions;
        state(&pool, "shadow", "rollback", NOW_MS - 10 * DAY_MS).await;
        assert_eq!(
            reentry_blocked(&pool, &c, "inject", "D3", NOW_MS)
                .await
                .unwrap(),
            Some(ReentryBlock::TooSoon { days_left: 4 })
        );
    }

    #[tokio::test]
    async fn reentry_needs_30_new_resolved_shadow_decisions_of_the_current_version() {
        let pool = pool().await;
        let c = cfg().actions;
        let since = NOW_MS - 15 * DAY_MS;
        state(&pool, "shadow", "rollback", since).await;
        for i in 0..29 {
            decision(
                &pool,
                &format!("n{i}"),
                "s",
                "shadow",
                since + (i + 1) * HOUR_MS,
                |_| {},
            )
            .await;
        }
        // Not counted: before the rollback, another version, still pending.
        decision(&pool, "old", "s", "shadow", since - HOUR_MS, |_| {}).await;
        decision(&pool, "v0", "s", "shadow", since + 40 * HOUR_MS, |r| {
            r.action_version = ACTION_VERSION + 1
        })
        .await;
        decision(&pool, "p0", "s", "shadow", since + 41 * HOUR_MS, |r| {
            r.outcome_state = "pending".into();
            r.outcome = None;
            r.resolved_at = None;
        })
        .await;
        assert_eq!(
            reentry_blocked(&pool, &c, "inject", "D3", NOW_MS)
                .await
                .unwrap(),
            Some(ReentryBlock::TooFewDecisions { have: 29 })
        );
        decision(&pool, "n29", "s", "shadow", since + 42 * HOUR_MS, |_| {}).await;
        assert_eq!(
            reentry_blocked(&pool, &c, "inject", "D3", NOW_MS)
                .await
                .unwrap(),
            None
        );
    }
}
