//! `GET /agents/route-report`: what routing subagent spawns would cost per completed task, and the
//! gate for moving the `agents` surface from `shadow` to `apply`.
//!
//! The report only shows numbers. Nothing here ever switches the surface to `apply`; the owner does
//! that. The one automatic move goes the other way: [`should_fall_back_to_shadow`] sends `apply`
//! back to `shadow` when re-dispatches climb. Rows with source `skipped` (the `wf-*` cockpits the
//! router leaves alone) are in neither the savings nor the re-dispatch figures.

use axum::{Json, extract::Query, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::agent_route::{Rung, SOURCE_SKIPPED};
use super::agent_spawns::{rung_of, similar};
use crate::state::AppState;

/// The gate stays closed until the data spans this many days AND this many spawns.
const MIN_DAYS: f64 = 3.0;
const MIN_SPAWNS: u64 = 200;
/// Projected saving needed, in percent, and the re-dispatch rise allowed, in percentage points.
const READY_SAVING_PCT: f64 = 20.0;
const READY_REDISPATCH_PP: f64 = 2.0;
/// In `apply`, a re-dispatch rate this far above the base (percentage points) falls back to shadow,
/// judged over this many of the latest spawns.
const FALLBACK_PP: f64 = 5.0;
const FALLBACK_WINDOW: usize = 100;
/// Attempts of one task are near-identical descriptions in one session at most this far apart.
const TASK_GAP_SECS: i64 = 3600;
/// A spawn with no outcome after this long is `lost`, not in progress.
const LOST_AFTER_SECS: i64 = 3600;
const DEFAULT_DAYS: i64 = 30;
const MAX_DAYS: i64 = 365;
const SECS_PER_DAY: f64 = 86_400.0;

/// Relative list prices per rung (haiku 1, sonnet 3, opus 5), used only to project what an advised
/// model would have cost against the one that ran.
fn price_weight(rung: Rung) -> f64 {
    match rung {
        Rung::Haiku => 1.0,
        Rung::Sonnet => 3.0,
        Rung::Opus => 5.0,
    }
}

/// The columns of `agent_spawns` the report reads.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SpawnRow {
    pub session_id: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub source: String,
    pub mode: String,
    pub advised_model: Option<String>,
    pub outcome: Option<String>,
    pub cost_usd: Option<f64>,
    pub spawned_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    NotEnoughData,
    Hold,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentReport {
    pub spawns: u64,
    pub days: f64,
    pub cost_per_done_actual: Option<f64>,
    pub cost_per_done_projected: Option<f64>,
    pub projected_saving_pct: Option<f64>,
    /// Re-dispatch rate in percent: the `apply` rows' when there are any, else the base.
    pub redispatch_rate: f64,
    /// The re-dispatch rate of the `shadow` rows, which ran on the unrouted model.
    pub redispatch_base: f64,
    /// Spawns left out of the cost figures because the task had an attempt with no cost.
    pub rows_without_cost: u64,
    pub gate: Gate,
}

type Chain<'a> = Vec<&'a SpawnRow>;

/// Groups rows (oldest first) into tasks: a spawn joins the chain of its session whose last attempt
/// has a near-identical description and is recent enough.
fn tasks<'a>(rows: &[&'a SpawnRow]) -> Vec<Chain<'a>> {
    let mut ordered: Vec<&SpawnRow> = rows.to_vec();
    ordered.sort_by_key(|r| r.spawned_at);
    let mut chains: Vec<Chain<'a>> = Vec::new();
    for row in ordered {
        let text = row.description.as_deref().unwrap_or("");
        let found = chains.iter_mut().rev().find(|chain| {
            let last = chain[chain.len() - 1];
            last.session_id == row.session_id
                && row.spawned_at - last.spawned_at <= TASK_GAP_SECS
                && similar(last.description.as_deref().unwrap_or(""), text)
        });
        match found {
            Some(chain) => chain.push(row),
            None => chains.push(vec![row]),
        }
    }
    chains
}

/// `(failed, counted)` for one attempt. A lost spawn is neither; the last attempt with no outcome is
/// still running and not yet counted. Any earlier attempt was re-dispatched, so it failed.
fn judge(row: &SpawnRow, is_last: bool) -> (bool, bool) {
    match row.outcome.as_deref() {
        Some("lost") => (false, false),
        None if is_last => (false, false),
        Some("error") => (true, true),
        _ => (!is_last, true),
    }
}

fn pct(failed: u64, counted: u64) -> Option<f64> {
    (counted > 0).then(|| failed as f64 * 100.0 / counted as f64)
}

/// Re-dispatch rates in percent: `(shadow rows, apply rows, all rows)`.
fn redispatch_rates(chains: &[Chain<'_>]) -> (Option<f64>, Option<f64>, Option<f64>) {
    let mut by_mode = [(0u64, 0u64); 3]; // shadow, apply, all
    for chain in chains {
        for (pos, row) in chain.iter().enumerate() {
            let (failed, counted) = judge(row, pos + 1 == chain.len());
            if !counted {
                continue;
            }
            let slot = if row.mode == "apply" { 1 } else { 0 };
            for i in [slot, 2] {
                by_mode[i].0 += u64::from(failed);
                by_mode[i].1 += 1;
            }
        }
    }
    let rate = |i: usize| pct(by_mode[i].0, by_mode[i].1);
    (rate(0), rate(1), rate(2))
}

fn projected_cost(row: &SpawnRow, cost: f64) -> f64 {
    let actual = row.model.as_deref().and_then(rung_of);
    let advised = row.advised_model.as_deref().and_then(rung_of);
    match (actual, advised) {
        (Some(a), Some(b)) => cost * price_weight(b) / price_weight(a),
        _ => cost,
    }
}

/// The report over `rows`, as of `now` (unix seconds).
pub fn evaluate(rows: &[SpawnRow], now: i64) -> AgentReport {
    let eligible: Vec<&SpawnRow> = rows.iter().filter(|r| r.source != SOURCE_SKIPPED).collect();
    let spawns = eligible.len() as u64;
    let first = eligible.iter().map(|r| r.spawned_at).min().unwrap_or(now);
    let days = ((now - first).max(0)) as f64 / SECS_PER_DAY;

    let chains = tasks(&eligible);
    let (shadow_rate, apply_rate, all_rate) = redispatch_rates(&chains);
    let base = shadow_rate.or(all_rate).unwrap_or(0.0);
    let redispatch_rate = apply_rate.unwrap_or(base);

    let (mut done, mut actual, mut projected, mut uncosted) = (0u64, 0.0, 0.0, 0u64);
    for chain in &chains {
        let last = chain[chain.len() - 1];
        if last.outcome.is_none() {
            continue; // still running
        }
        if chain.iter().any(|r| r.cost_usd.is_none()) {
            uncosted += chain.iter().filter(|r| r.cost_usd.is_none()).count() as u64;
            continue;
        }
        for row in chain {
            let cost = row.cost_usd.unwrap_or(0.0);
            actual += cost;
            projected += projected_cost(row, cost);
        }
        done += u64::from(last.outcome.as_deref() == Some("ok"));
    }
    let per_done = |total: f64| (done > 0).then(|| total / done as f64);
    let cost_per_done_actual = per_done(actual);
    let cost_per_done_projected = per_done(projected);
    let projected_saving_pct = match (cost_per_done_actual, cost_per_done_projected) {
        (Some(a), Some(p)) if a > 0.0 => Some((a - p) / a * 100.0),
        _ => None,
    };

    let gate = if spawns < MIN_SPAWNS || days < MIN_DAYS {
        Gate::NotEnoughData
    } else if projected_saving_pct.is_some_and(|s| s >= READY_SAVING_PCT)
        && redispatch_rate <= base + READY_REDISPATCH_PP
    {
        Gate::Ready
    } else {
        Gate::Hold
    };
    AgentReport {
        spawns,
        days,
        cost_per_done_actual,
        cost_per_done_projected,
        projected_saving_pct,
        redispatch_rate,
        redispatch_base: base,
        rows_without_cost: uncosted,
        gate,
    }
}

/// Whether the latest spawns (oldest first) ran in `apply` with a re-dispatch rate more than 5
/// percentage points over `base_rate`. Fewer than 100 spawns is too little to judge.
pub fn should_fall_back_to_shadow(window: &[SpawnRow], base_rate: f64) -> bool {
    if window.len() < FALLBACK_WINDOW {
        return false;
    }
    let latest: Vec<&SpawnRow> = window[window.len() - FALLBACK_WINDOW..]
        .iter()
        .filter(|r| r.source != SOURCE_SKIPPED)
        .collect();
    let chains = tasks(&latest);
    redispatch_rates(&chains)
        .2
        .is_some_and(|rate| rate > base_rate + FALLBACK_PP)
}

/// Prefixes a literal query tail with the spawn columns, keeping the SQL a `&'static str`.
macro_rules! select_spawns {
    ($tail:literal) => {
        concat!(
            "SELECT session_id, description, model, source, mode, advised_model, outcome, \
             cost_usd, spawned_at ",
            $tail
        )
    };
}

async fn load_since(pool: &SqlitePool, since: i64) -> Result<Vec<SpawnRow>, sqlx::Error> {
    sqlx::query_as(select_spawns!(
        "FROM agent_spawns WHERE spawned_at >= ? ORDER BY spawned_at, id"
    ))
    .bind(since)
    .fetch_all(pool)
    .await
}

/// After an outcome: when the `agents` surface is in `apply` and its latest 100 spawns re-dispatch
/// more than 5 points above the shadow base, writes `surfaces.agents = "shadow"` through the settings
/// store. Never the other direction, and never fails the hook that called it.
pub async fn guard_apply(pool: &SqlitePool) {
    use crate::models::route_doc::RouterDoc;
    use crate::settings::{Scope, get_opt};
    let in_apply = get_opt::<RouterDoc>(&Scope::Machine)
        .is_some_and(|doc| doc.surfaces.get("agents").map(String::as_str) == Some("apply"));
    if !in_apply {
        return;
    }
    let latest: Result<Vec<SpawnRow>, _> = sqlx::query_as(select_spawns!(
        "FROM (SELECT * FROM agent_spawns WHERE mode = 'apply' AND source != ?
         ORDER BY spawned_at DESC, id DESC LIMIT ?) ORDER BY spawned_at, id"
    ))
    .bind(SOURCE_SKIPPED)
    .bind(FALLBACK_WINDOW as i64)
    .fetch_all(pool)
    .await;
    let shadow: Result<Vec<SpawnRow>, _> = sqlx::query_as(select_spawns!(
        "FROM agent_spawns WHERE mode = 'shadow' AND source != ?
         ORDER BY spawned_at, id"
    ))
    .bind(SOURCE_SKIPPED)
    .fetch_all(pool)
    .await;
    let (Ok(latest), Ok(shadow)) = (latest, shadow) else {
        tracing::warn!("agent apply guard could not read spawns");
        return;
    };
    // No shadow history means no base to compare against: leave the surface alone.
    let shadow_refs: Vec<&SpawnRow> = shadow.iter().collect();
    let Some(base) = redispatch_rates(&tasks(&shadow_refs)).2 else {
        return;
    };
    if !should_fall_back_to_shadow(&latest, base) {
        return;
    }
    let written =
        crate::settings::update::<RouterDoc, _>(pool, &Scope::Machine, "agent-guard", |d| {
            d.surfaces.insert("agents".into(), "shadow".into());
        })
        .await;
    match written {
        Ok(_) => tracing::warn!(
            base,
            "agent re-dispatches rose more than {FALLBACK_PP} points above the base; \
             the agents surface fell back from apply to shadow"
        ),
        Err(e) => tracing::warn!("agents surface could not fall back to shadow: {e}"),
    }
}

#[derive(Deserialize)]
pub struct AgentReportQuery {
    days: Option<i64>,
}

pub async fn get_agent_report(
    State(state): State<AppState>,
    Query(query): Query<AgentReportQuery>,
) -> Result<Json<AgentReport>, StatusCode> {
    let now = chrono::Utc::now().timestamp();
    let days = query.days.unwrap_or(DEFAULT_DAYS).clamp(1, MAX_DAYS);
    // A dead agent must not sit "in progress" and skew the figures.
    super::agent_spawns::sweep_lost(&state.pool, LOST_AFTER_SECS).await;
    load_since(&state.pool, now - days * 86_400)
        .await
        .map(|rows| Json(evaluate(&rows, now)))
        .map_err(|err| {
            tracing::error!(error = %err, "agent report query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn row(session: &str, desc: &str, mode: &str, outcome: Option<&str>, at: i64) -> SpawnRow {
        SpawnRow {
            session_id: session.into(),
            description: Some(desc.into()),
            model: Some("opus".into()),
            source: "router".into(),
            mode: mode.into(),
            advised_model: None,
            outcome: outcome.map(Into::into),
            cost_usd: Some(5.0),
            spawned_at: at,
        }
    }

    /// `n` independent one-attempt tasks spread over four days, the first `errors` of them failed.
    fn batch(mode: &str, n: usize, errors: usize, advised: Option<&str>) -> Vec<SpawnRow> {
        (0..n)
            .map(|i| {
                let mut r = row(
                    &format!("{mode}-{i}"),
                    &format!("task {mode} {i}"),
                    mode,
                    Some(if i < errors { "error" } else { "ok" }),
                    T0 + (i as i64 * 4 * DAY) / n as i64,
                );
                r.advised_model = advised.map(Into::into);
                r
            })
            .collect()
    }

    fn now() -> i64 {
        T0 + 5 * DAY
    }

    #[test]
    fn under_three_days_or_under_200_spawns_is_not_enough_data() {
        let few = [batch("shadow", 199, 0, Some("sonnet"))].concat();
        assert_eq!(evaluate(&few, now()).gate, Gate::NotEnoughData);
        let many: Vec<SpawnRow> = batch("shadow", 500, 0, Some("sonnet"))
            .into_iter()
            .map(|mut r| {
                r.spawned_at = T0 + 3 * DAY + r.spawned_at % DAY;
                r
            })
            .collect();
        assert_eq!(evaluate(&many, T0 + 5 * DAY).gate, Gate::NotEnoughData);
    }

    #[test]
    fn a_20_percent_saving_with_a_flat_redispatch_rate_is_ready() {
        // Shadow rows would have run opus -> sonnet (5 -> 3), apply rows stay put: 200 of 1000.
        let rows = [
            batch("shadow", 100, 0, Some("sonnet")),
            batch("apply", 100, 2, None),
        ]
        .concat();
        let report = evaluate(&rows, now());
        assert_eq!(report.projected_saving_pct, Some(20.0));
        assert_eq!(report.redispatch_rate, 2.0);
        assert_eq!(report.redispatch_base, 0.0);
        assert_eq!(report.gate, Gate::Ready);
    }

    #[test]
    fn a_saving_eaten_by_retries_is_hold() {
        let retried = [
            batch("shadow", 100, 0, Some("sonnet")),
            batch("apply", 100, 3, None),
        ]
        .concat();
        assert_eq!(evaluate(&retried, now()).gate, Gate::Hold);
        let small = [
            batch("shadow", 100, 0, Some("opus")),
            batch("apply", 100, 0, None),
        ]
        .concat();
        assert_eq!(evaluate(&small, now()).gate, Gate::Hold);
    }

    #[test]
    fn cost_per_done_task_counts_the_failed_attempts() {
        let mut first = row("s", "fix the parser", "shadow", Some("error"), T0);
        first.cost_usd = Some(1.0);
        let mut second = row("s", "fix the parser", "shadow", Some("ok"), T0 + 60);
        second.cost_usd = Some(2.0);
        let report = evaluate(&[first, second], T0 + 120);
        assert_eq!(report.cost_per_done_actual, Some(3.0));
    }

    #[test]
    fn rows_without_a_cost_are_left_out_and_counted() {
        let mut priced = row("a", "write the notes", "shadow", Some("ok"), T0);
        priced.cost_usd = Some(4.0);
        let mut unpriced = row("b", "fix the lexer", "shadow", Some("ok"), T0 + 5);
        unpriced.cost_usd = None;
        let report = evaluate(&[priced, unpriced], T0 + 60);
        assert_eq!(report.cost_per_done_actual, Some(4.0));
        assert_eq!(report.rows_without_cost, 1);
    }

    #[test]
    fn skipped_spawns_are_not_in_any_figure() {
        let mut skipped = row("w", "wf cockpit", "shadow", Some("error"), T0);
        skipped.source = "skipped".into();
        let report = evaluate(&[skipped], T0 + 60);
        assert_eq!(report.spawns, 0);
        assert_eq!(report.redispatch_rate, 0.0);
        assert_eq!(report.cost_per_done_actual, None);
    }

    fn window(n: usize, errors: usize, lost: usize) -> Vec<SpawnRow> {
        (0..n)
            .map(|i| {
                let outcome = if i < errors {
                    "error"
                } else if i < errors + lost {
                    "lost"
                } else {
                    "ok"
                };
                row(
                    &format!("s{i}"),
                    &format!("task {i}"),
                    "apply",
                    Some(outcome),
                    T0 + i as i64,
                )
            })
            .collect()
    }

    #[test]
    fn the_fallback_trips_only_above_base_plus_5_points_over_100_spawns() {
        assert!(!should_fall_back_to_shadow(&window(100, 15, 0), 10.0));
        assert!(should_fall_back_to_shadow(&window(100, 15, 0), 9.9));
        assert!(!should_fall_back_to_shadow(&window(99, 99, 0), 0.0));
    }

    #[test]
    fn lost_spawns_do_not_count_as_redispatches() {
        assert!(!should_fall_back_to_shadow(&window(100, 0, 10), 0.0));
        assert!(should_fall_back_to_shadow(&window(100, 10, 0), 0.0));
        assert_eq!(evaluate(&window(10, 0, 10), T0 + 60).redispatch_rate, 0.0);
    }
}
