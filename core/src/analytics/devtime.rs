//! Development time: the owner's own Claude Code sessions, read from the devtime tables for the
//! analytics page (spec 2026-10-17-analytics-devtime). Read and assembled per request, no rollup.
//!
//! Never summed with phase 1's cost: that is the daemon's runs, this is the owner's manual
//! sessions — different populations (spec §2, non-goals). `devtime_findings.cost_ms` is a sort
//! weight and is never added into a total here (§6.7); totals come from spans.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

use super::{Scope, Window};
use crate::config::{DevtimeRulesConfig, ModelPrice, ModelsConfig, ThresholdsSource};
use crate::devtime::precision::{self, MarkError, RulePrecision};
use crate::devtime::rules::RULES;
use crate::devtime::store::{
    self as store, FindingCursor, FindingOrder, IngestStatus, MessageDayRow, SessionCounts,
    SpanDayRow, TurnDayRow,
};

/// The span kinds that are active time on the `main` lane (spec §6.3).
pub const ACTIVE_KINDS: [&str; 5] = [
    "model",
    "tool",
    "subagent",
    "wait_background",
    "wait_machine",
];
/// A waste span with no rule or lever.
pub const NONE_KEY: &str = "__none__";
/// Rows past the cap, summed.
pub const OTHER_KEY: &str = "__other__";
/// NULL model or effort.
pub const UNKNOWN: &str = "unknown";
const LIST_CAP: usize = 20;

/// The devtime timestamp format; text order is time order.
pub(crate) fn stamp(moment: DateTime<Utc>) -> String {
    moment.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn ratio(numerator: i64, denominator: i64) -> Option<f64> {
    (denominator != 0).then(|| numerator as f64 / denominator as f64)
}

/* ------------------------------------------------------------- response -- */

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ByKind {
    pub model: i64,
    pub tool: i64,
    pub subagent: i64,
    pub wait_background: i64,
    pub wait_machine: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ByWaste {
    pub useful: i64,
    pub rework: i64,
    pub avoidable: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DevtimeDay {
    pub day: String,
    pub partial: bool,
    pub active_ms: i64,
    pub wasted_ms: i64,
    pub by_kind: ByKind,
    pub by_waste: ByWaste,
    pub turns: i64,
    pub interrupted: i64,
    pub corrections: i64,
    /// The day's estimated cost, priced exactly like `tokens.cost_usd` (Task 2.3 fills it); `None`
    /// when none of the day's message rows could be priced (owner, 2026-10-18, open decision 5).
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PreviousTotals {
    pub active_ms: i64,
    pub wasted_ms: i64,
    pub waste_ratio: Option<f64>,
    pub interruption_rate: Option<f64>,
    pub correction_rate: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DevtimeTotals {
    pub active_ms: i64,
    pub wasted_ms: i64,
    pub waste_ratio: Option<f64>,
    pub wait_human_ms: i64,
    pub idle_ms: i64,
    pub inferred_waste_ms: i64,
    pub turns: i64,
    pub interruption_rate: Option<f64>,
    pub correction_rate: Option<f64>,
    pub correction_coverage: Option<f64>,
    pub previous: PreviousTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuleWaste {
    pub rule_id: String,
    pub level: Option<&'static str>,
    pub waste: Option<&'static str>,
    pub lever: Option<&'static str>,
    pub ms: i64,
    pub findings: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LeverWaste {
    pub lever: String,
    pub ms: i64,
}

/// The data-quality strip (spec §6.5).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Quality {
    pub ingest_enabled: bool,
    pub last_cycle_at: Option<String>,
    /// From `devtime_ingest_status`, the LAST cycle only — those sessions are never stored, so they
    /// cannot be counted per day or per project (spec risk 1). `None` before the first cycle.
    pub unmapped_sessions_last_cycle: Option<i64>,
    pub skipped_daemon_sessions_last_cycle: Option<i64>,
    pub sessions: i64,
    pub dirty_sessions: i64,
    pub unruled_sessions: i64,
}

/// One (model, effort) of the window. `cost_usd` is `None` when the model has no price, or lacks
/// one for a kind of token it used (a fast or US-resident request counts as its own kind).
///
/// `cost_usd` is the best estimate; `cost_low`/`cost_high` bound it where the cache writes' duration
/// is unknown (messages ingested before migration 0214): low prices them all at the 5-minute rate,
/// high at the 1-hour one, and the estimate takes high, since Claude Code writes 1-hour cache. All
/// three are equal when every split is known.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ModelTokens {
    pub model: String,
    pub effort: String,
    pub input: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub output: i64,
    pub messages: i64,
    pub cost_usd: Option<f64>,
    pub cost_low: Option<f64>,
    pub cost_high: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PreviousTokens {
    pub cost_usd: Option<f64>,
    pub cache_hit_ratio: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DevtimeTokens {
    pub by_model: Vec<ModelTokens>,
    pub cache_hit_ratio: Option<f64>,
    pub cost_usd: Option<f64>,
    /// The bounds of `cost_usd` over an unknown cache-write duration, as in [`ModelTokens`].
    pub cost_low: Option<f64>,
    pub cost_high: Option<f64>,
    /// Always true: transcripts carry no cost, this is tokens x published price (spec §4.4).
    pub cost_estimated: bool,
    pub prices_as_of: Option<String>,
    pub unpriced_models: Vec<String>,
    /// One `"<model id>: <reason>"` per price the models file had and the parse dropped as unreadable,
    /// so a model that reads unpriced because of a bad line says so beside the cost.
    pub dropped_prices: Vec<String>,
    pub previous: PreviousTokens,
    /// How the daemon's last price refresh went, or `None` when none has run since it started.
    /// Never set here: the HTTP handler fills it after [`devtime_overview`], because it is daemon
    /// state and not a reading of the window, and this module stays a function of its inputs.
    pub price_refresh: Option<crate::price_refresh::PriceRefreshStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DevtimeOverview {
    pub scope: &'static str,
    pub project_id: Option<String>,
    pub days: i64,
    pub timezone: &'static str,
    pub now: String,
    pub window: super::Span,
    pub previous: super::Span,
    pub quality: Quality,
    pub days_series: Vec<DevtimeDay>,
    pub totals: DevtimeTotals,
    pub waste_by_rule: Vec<RuleWaste>,
    pub waste_by_lever: Vec<LeverWaste>,
    pub tokens: DevtimeTokens,
}

/* ---------------------------------------------------------------- reads -- */

/// Everything one request reads, both windows at once.
#[derive(Debug, Default)]
pub(crate) struct Reads {
    pub spans: Vec<SpanDayRow>,
    /// Bucketed by `thresholds` (see [`MessageDayRow::bucket`]).
    pub messages: Vec<MessageDayRow>,
    pub thresholds: Vec<u64>,
    pub turns: Vec<TurnDayRow>,
    pub sessions: SessionCounts,
    pub findings_by_rule: Vec<(String, i64)>,
    pub ingest: Option<IngestStatus>,
}

pub async fn devtime_overview(
    pool: &SqlitePool,
    models: &ModelsConfig,
    scope: Scope<'_>,
    window: Window,
    now: DateTime<Utc>,
) -> sqlx::Result<DevtimeOverview> {
    let project = scope.project();
    let since = stamp(window.previous().start());
    let current = stamp(window.start());
    let until = stamp(now.min(window.end()));
    // One read per tier bucket, so the buckets are only the thresholds of models actually used in
    // the window: a first base-only read names them, and a second read runs only when one of them
    // is tiered. The price table can hold hundreds of models that never show up here.
    let mut messages = store::message_day_rows(pool, project, &since, &until, &[]).await?;
    let thresholds = prompt_thresholds(
        models,
        messages.iter().filter_map(|row| row.model.as_deref()),
    );
    if !thresholds.is_empty() {
        messages = store::message_day_rows(pool, project, &since, &until, &thresholds).await?;
    }
    let reads = Reads {
        spans: store::span_day_rows(pool, project, &since, &until).await?,
        messages,
        thresholds,
        turns: store::turn_day_rows(pool, project, &since, &until).await?,
        sessions: store::session_counts(pool, project, &current, &until).await?,
        findings_by_rule: store::finding_rule_counts(pool, project, &current, &until).await?,
        ingest: store::read_ingest_status(pool).await?,
    };
    Ok(assemble(&reads, models, scope, window, now))
}

/* ------------------------------------------------------------- assembly -- */

/// Turn counters of one window, summed over its days.
#[derive(Default)]
struct TurnTally {
    turns: i64,
    interrupted: i64,
    corrections: i64,
    known: i64,
}

impl TurnTally {
    fn add(&mut self, row: &TurnDayRow) {
        self.turns += row.turns;
        self.interrupted += row.interrupted;
        self.corrections += row.corrections;
        self.known += row.correction_known;
    }
}

pub(crate) fn assemble(
    reads: &Reads,
    models: &ModelsConfig,
    scope: Scope<'_>,
    window: Window,
    now: DateTime<Utc>,
) -> DevtimeOverview {
    let previous = window.previous();
    let mut series = day_series(window);
    let mut totals = DevtimeTotals::default();
    let mut prev = PreviousTotals::default();
    for row in reads.spans.iter().filter(|row| row.lane_class == "main") {
        let wasted = matches!(row.waste.as_deref(), Some("rework" | "avoidable"));
        if window.holds(&row.day) {
            match row.kind.as_str() {
                "wait_human" => totals.wait_human_ms += row.ms,
                "idle" => totals.idle_ms += row.ms,
                kind if ACTIVE_KINDS.contains(&kind) => {
                    let day = series.get_mut(&row.day).expect("window day");
                    add_active(day, kind, row.waste.as_deref(), row.ms);
                    totals.active_ms += row.ms;
                    if wasted {
                        totals.wasted_ms += row.ms;
                        if row.confidence == "inferred" {
                            totals.inferred_waste_ms += row.ms;
                        }
                    }
                }
                _ => {}
            }
        } else if previous.holds(&row.day) && ACTIVE_KINDS.contains(&row.kind.as_str()) {
            prev.active_ms += row.ms;
            if wasted {
                prev.wasted_ms += row.ms;
            }
        }
    }
    totals.waste_ratio = ratio(totals.wasted_ms, totals.active_ms);
    prev.waste_ratio = ratio(prev.wasted_ms, prev.active_ms);

    let (mut current_turns, mut previous_turns) = (TurnTally::default(), TurnTally::default());
    for row in &reads.turns {
        if let Some(day) = series.get_mut(&row.day) {
            day.turns += row.turns;
            day.interrupted += row.interrupted;
            day.corrections += row.corrections;
            current_turns.add(row);
        } else if previous.holds(&row.day) {
            previous_turns.add(row);
        }
    }
    totals.turns = current_turns.turns;
    totals.interruption_rate = ratio(current_turns.interrupted, current_turns.turns);
    totals.correction_rate = ratio(current_turns.corrections, current_turns.known);
    // `None` without a known turn (spec §6.4's last sentence), so `turns` is non-zero here.
    totals.correction_coverage =
        (current_turns.known != 0).then(|| current_turns.known as f64 / current_turns.turns as f64);
    prev.interruption_rate = ratio(previous_turns.interrupted, previous_turns.turns);
    prev.correction_rate = ratio(previous_turns.corrections, previous_turns.known);
    totals.previous = prev;
    let tokens = tokens_block(window, reads, models);
    for (day, usd) in cost_by_day(window, reads, models, &tokens.by_model) {
        if let Some(entry) = series.get_mut(&day) {
            entry.cost_usd = Some(usd);
        }
    }
    DevtimeOverview {
        scope: match scope {
            Scope::Global => "global",
            Scope::Project(_) => "project",
        },
        project_id: scope.project().map(str::to_string),
        days: window.days,
        timezone: "UTC",
        now: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        window: window.span(),
        previous: previous.span(),
        quality: quality(reads.ingest.as_ref(), &reads.sessions),
        days_series: series.into_values().collect(),
        totals,
        waste_by_rule: waste_by_rule(window, &reads.spans, &reads.findings_by_rule),
        waste_by_lever: waste_by_lever(window, &reads.spans),
        tokens,
    }
}

fn quality(ingest: Option<&IngestStatus>, sessions: &SessionCounts) -> Quality {
    Quality {
        ingest_enabled: ingest.is_some_and(|status| status.enabled),
        last_cycle_at: ingest.map(|status| status.cycle_at.clone()),
        unmapped_sessions_last_cycle: ingest.map(|status| status.unmapped_sessions),
        skipped_daemon_sessions_last_cycle: ingest.map(|status| status.skipped_daemon_sessions),
        sessions: sessions.sessions,
        dirty_sessions: sessions.dirty,
        unruled_sessions: sessions.unruled,
    }
}

/// One zeroed entry per window day, keyed by day so `into_values` is oldest first; today is
/// partial.
fn day_series(window: Window) -> BTreeMap<String, DevtimeDay> {
    let days = window.day_list();
    let last = days.len() - 1;
    days.into_iter()
        .enumerate()
        .map(|(index, day)| {
            let entry = DevtimeDay {
                day: day.clone(),
                partial: index == last,
                active_ms: 0,
                wasted_ms: 0,
                by_kind: ByKind::default(),
                by_waste: ByWaste::default(),
                turns: 0,
                interrupted: 0,
                corrections: 0,
                cost_usd: None,
            };
            (day, entry)
        })
        .collect()
}

fn add_active(day: &mut DevtimeDay, kind: &str, waste: Option<&str>, ms: i64) {
    day.active_ms += ms;
    match kind {
        "model" => day.by_kind.model += ms,
        "tool" => day.by_kind.tool += ms,
        "subagent" => day.by_kind.subagent += ms,
        "wait_background" => day.by_kind.wait_background += ms,
        _ => day.by_kind.wait_machine += ms,
    }
    match waste {
        Some("rework") => {
            day.by_waste.rework += ms;
            day.wasted_ms += ms;
        }
        Some("avoidable") => {
            day.by_waste.avoidable += ms;
            day.wasted_ms += ms;
        }
        // NULL waste is a session the rules have not reached yet: useful (spec §6.6).
        _ => day.by_waste.useful += ms,
    }
}

/// Sorted by the caller (ms desc, then key); keeps `LIST_CAP` rows and folds the rest with `other`.
pub(crate) fn cap_list<T>(mut rows: Vec<T>, other: impl FnOnce(&[T]) -> T) -> Vec<T> {
    if rows.len() > LIST_CAP {
        let rest = rows.split_off(LIST_CAP);
        rows.push(other(&rest));
    }
    rows
}

/// Wasted active time of the current window, per rule (spec §6.1): `level`/`waste`/`lever` from the
/// registry, `findings` from the window's findings.
fn waste_by_rule(
    window: Window,
    spans: &[SpanDayRow],
    findings: &[(String, i64)],
) -> Vec<RuleWaste> {
    let mut by_rule: BTreeMap<String, i64> = BTreeMap::new();
    for row in wasted_active(window, spans) {
        let key = row.rule_id.clone().unwrap_or_else(|| NONE_KEY.into());
        *by_rule.entry(key).or_default() += row.ms;
    }
    let mut rows: Vec<RuleWaste> = by_rule
        .into_iter()
        .map(|(rule_id, ms)| {
            let rule = RULES.iter().find(|rule| rule.id == rule_id);
            RuleWaste {
                level: rule.map(|rule| rule.level.as_str()),
                waste: rule.map(|rule| rule.waste.as_str()),
                lever: rule.and_then(|rule| rule.levers.first().copied()),
                findings: findings
                    .iter()
                    .find(|(id, _)| *id == rule_id)
                    .map_or(0, |(_, count)| *count),
                rule_id,
                ms,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.ms.cmp(&a.ms).then_with(|| a.rule_id.cmp(&b.rule_id)));
    cap_list(rows, |rest| RuleWaste {
        rule_id: OTHER_KEY.into(),
        level: None,
        waste: None,
        lever: None,
        ms: rest.iter().map(|row| row.ms).sum(),
        findings: rest.iter().map(|row| row.findings).sum(),
    })
}

fn waste_by_lever(window: Window, spans: &[SpanDayRow]) -> Vec<LeverWaste> {
    let mut by_lever: BTreeMap<String, i64> = BTreeMap::new();
    for row in wasted_active(window, spans) {
        let key = row.lever.clone().unwrap_or_else(|| NONE_KEY.into());
        *by_lever.entry(key).or_default() += row.ms;
    }
    let mut rows: Vec<LeverWaste> = by_lever
        .into_iter()
        .map(|(lever, ms)| LeverWaste { lever, ms })
        .collect();
    rows.sort_by(|a, b| b.ms.cmp(&a.ms).then_with(|| a.lever.cmp(&b.lever)));
    cap_list(rows, |rest| LeverWaste {
        lever: OTHER_KEY.into(),
        ms: rest.iter().map(|row| row.ms).sum(),
    })
}

fn wasted_active(window: Window, spans: &[SpanDayRow]) -> impl Iterator<Item = &SpanDayRow> {
    spans.iter().filter(move |row| {
        row.lane_class == "main"
            && window.holds(&row.day)
            && ACTIVE_KINDS.contains(&row.kind.as_str())
            && matches!(row.waste.as_deref(), Some("rework" | "avoidable"))
    })
}

/* --------------------------------------------------------------- tokens -- */

/// Every distinct prompt threshold of the used models' prices, ascending: the buckets the day read
/// splits messages by, so each can be priced at its own tier. A 0 threshold is the base, bucket 0
/// already.
fn prompt_thresholds<'a>(models: &ModelsConfig, used: impl Iterator<Item = &'a str>) -> Vec<u64> {
    let thresholds: std::collections::BTreeSet<u64> = used
        .filter_map(|model| models.price_of(model))
        .flat_map(|price| price.tiers.iter().map(|tier| tier.min_prompt_tokens))
        .filter(|threshold| *threshold > 0)
        .collect();
    thresholds.into_iter().collect()
}

/// An estimate and its bounds, USD (see [`ModelTokens`]).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Cost {
    usd: f64,
    low: f64,
    high: f64,
}

impl std::ops::Add for Cost {
    type Output = Cost;
    fn add(self, other: Cost) -> Cost {
        Cost {
            usd: self.usd + other.usd,
            low: self.low + other.low,
            high: self.high + other.high,
        }
    }
}

/// USD for one day row, or `None` when the model has no price or lacks one for a kind of token the
/// row used. Every message of a row shares its tier, speed and residency, so the row is priced at
/// [`ModelPrice::rates_for`] of its bucket's threshold: within a bucket the tier cannot change.
/// An unknown speed or residency (before 0214) is the standard one.
///
/// Cache writes: the known 1-hour ones at `cache_write_1h`, the known rest at `cache_write`, and an
/// unknown split low at the 5-minute rate, high and best at the 1-hour one. An entry with only one
/// of the two write rates prices an unknown split at it on both sides, and never a known write of
/// the missing duration (a wrong cost is worse than none, spec devtime §4.4).
fn row_cost(price: Option<&ModelPrice>, row: &MessageDayRow, thresholds: &[u64]) -> Option<Cost> {
    let price = price?;
    let prompt = match usize::try_from(row.bucket).unwrap_or(0) {
        0 => 0,
        bucket => *thresholds.get(bucket - 1)?,
    };
    let rates = price.rates_for(
        prompt,
        row.speed.as_deref() == Some("fast"),
        row.inference_geo.as_deref() == Some("us"),
    );
    let usd = |tokens: i64, rate: Option<f64>| match tokens {
        0 => Some(0.0),
        _ => rate.map(|rate| tokens as f64 * rate / 1_000_000.0),
    };
    let five_minute =
        (row.cache_creation - row.cache_creation_1h - row.cache_creation_unknown).max(0);
    let known = usd(row.input, rates.input)?
        + usd(row.output, rates.output)?
        + usd(row.cache_read, rates.cache_read)?
        + usd(five_minute, rates.cache_write)?
        + usd(row.cache_creation_1h, rates.cache_write_1h)?;
    let unknown = row.cache_creation_unknown;
    let low = usd(unknown, rates.cache_write.or(rates.cache_write_1h))?;
    let high = usd(unknown, rates.cache_write_1h.or(rates.cache_write))?;
    Some(Cost {
        usd: known + high,
        low: known + low,
        high: known + high,
    })
}

/// Sums one window's message rows per (model, effort), pricing each row at its own tier and mode;
/// the SQL never sees a price.
fn tally<'a>(
    rows: impl Iterator<Item = &'a MessageDayRow>,
    models: &ModelsConfig,
    thresholds: &[u64],
) -> Vec<ModelTokens> {
    let mut by: BTreeMap<(Option<String>, String), (ModelTokens, Option<Cost>)> = BTreeMap::new();
    for row in rows {
        let effort = row.effort.clone().unwrap_or_else(|| UNKNOWN.into());
        let (entry, cost) = by
            .entry((row.model.clone(), effort.clone()))
            .or_insert_with(|| {
                let entry = ModelTokens {
                    model: row.model.clone().unwrap_or_else(|| UNKNOWN.into()),
                    effort,
                    ..ModelTokens::default()
                };
                (entry, Some(Cost::default()))
            });
        entry.input += row.input;
        entry.cache_read += row.cache_read;
        entry.cache_creation += row.cache_creation;
        entry.output += row.output;
        entry.messages += row.messages;
        // A NULL model is never priced, even if somebody wrote a price under "unknown".
        let price = row.model.as_deref().and_then(|m| models.price_of(m));
        *cost = cost
            .zip(row_cost(price, row, thresholds))
            .map(|(a, b)| a + b);
    }
    by.into_values()
        .map(|(mut entry, cost)| {
            entry.cost_usd = cost.map(|c| c.usd);
            entry.cost_low = cost.map(|c| c.low);
            entry.cost_high = cost.map(|c| c.high);
            entry
        })
        .collect()
}

fn hit_ratio(rows: &[ModelTokens]) -> Option<f64> {
    let read: i64 = rows.iter().map(|row| row.cache_read).sum();
    let base: i64 = rows
        .iter()
        .map(|row| row.input + row.cache_read + row.cache_creation)
        .sum();
    ratio(read, base)
}

/// The sum of one cost field over the priced rows; `None` when no row is priced.
fn total_cost(rows: &[ModelTokens], field: fn(&ModelTokens) -> Option<f64>) -> Option<f64> {
    let priced: Vec<f64> = rows.iter().filter_map(field).collect();
    (!priced.is_empty()).then(|| priced.iter().sum())
}

fn tokens_block(window: Window, reads: &Reads, models: &ModelsConfig) -> DevtimeTokens {
    let previous = window.previous();
    let rows = &reads.messages;
    let mut current = tally(
        rows.iter().filter(|r| window.holds(&r.day)),
        models,
        &reads.thresholds,
    );
    let before = tally(
        rows.iter().filter(|r| previous.holds(&r.day)),
        models,
        &reads.thresholds,
    );
    current.sort_by(|a, b| {
        b.cost_usd
            .is_some()
            .cmp(&a.cost_usd.is_some())
            .then_with(|| {
                b.cost_usd
                    .unwrap_or(0.0)
                    .total_cmp(&a.cost_usd.unwrap_or(0.0))
            })
            .then_with(|| (b.input + b.output).cmp(&(a.input + a.output)))
            .then_with(|| a.model.cmp(&b.model))
    });
    let mut unpriced: Vec<String> = current
        .iter()
        .filter(|row| row.cost_usd.is_none())
        .map(|row| row.model.clone())
        .collect();
    unpriced.sort();
    unpriced.dedup();
    DevtimeTokens {
        cache_hit_ratio: hit_ratio(&current),
        cost_usd: total_cost(&current, |row| row.cost_usd),
        cost_low: total_cost(&current, |row| row.cost_low),
        cost_high: total_cost(&current, |row| row.cost_high),
        cost_estimated: true,
        prices_as_of: models.prices_as_of.clone(),
        unpriced_models: unpriced,
        dropped_prices: models.price_warnings.clone(),
        previous: PreviousTokens {
            cost_usd: total_cost(&before, |row| row.cost_usd),
            cache_hit_ratio: hit_ratio(&before),
        },
        by_model: current,
        price_refresh: None,
    }
}

/// Each window day's estimated cost (owner, 2026-10-18, open decision 5), by the window's rules:
/// only a (model, effort) the window priced counts, so a model unpriced over the window (no entry,
/// or no price for a token kind it used on some day) adds nothing on any day and the days sum to
/// `tokens.cost_usd`. A day absent from the map — no usage, or only unpriced usage — reads
/// `cost_usd: null`, as the window total does when no row could be priced. Each row is priced as
/// in `tally`, and a day takes the best estimate.
fn cost_by_day(
    window: Window,
    reads: &Reads,
    models: &ModelsConfig,
    priced_rows: &[ModelTokens],
) -> BTreeMap<String, f64> {
    let priced: std::collections::BTreeSet<(&str, &str)> = priced_rows
        .iter()
        .filter(|row| row.cost_usd.is_some())
        .map(|row| (row.model.as_str(), row.effort.as_str()))
        .collect();
    let mut by_day: BTreeMap<String, f64> = BTreeMap::new();
    for row in reads.messages.iter().filter(|row| window.holds(&row.day)) {
        // A NULL model is never priced (as in `tally`).
        let Some(model) = row.model.as_deref() else {
            continue;
        };
        if !priced.contains(&(model, row.effort.as_deref().unwrap_or(UNKNOWN))) {
            continue;
        }
        // Priced over the window implies every row of it priced.
        if let Some(cost) = row_cost(models.price_of(model), row, &reads.thresholds) {
            *by_day.entry(row.day.clone()).or_default() += cost.usd;
        }
    }
    by_day
}

/* ------------------------------------------------------------- findings -- */

/// `RulePrecision` as the page shows it (spec §4.5, §6.7): below `min_cases` the precision is
/// `null` ("few cases"), never 0 % or 100 % and never the prior; `low_precision` needs enough
/// cases AND a precision under the floor. Global: `rule_report` does not filter by project.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuleView {
    pub rule_id: String,
    pub level: &'static str,
    pub n_cases: i64,
    pub n_not_rework: i64,
    pub precision: Option<f64>,
    pub prior: f64,
    pub eligible: bool,
    pub low_precision: bool,
}

pub fn rule_view(p: &RulePrecision, cfg: &DevtimeRulesConfig) -> RuleView {
    let enough = p.n_cases >= i64::from(cfg.precision.min_cases);
    RuleView {
        rule_id: p.rule_id.clone(),
        level: p.level.as_str(),
        n_cases: p.n_cases,
        n_not_rework: p.n_not_rework,
        precision: if enough { p.precision } else { None },
        prior: p.prior,
        eligible: p.eligible,
        low_precision: enough && p.precision.is_some_and(|value| value < cfg.precision.floor),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedbackView {
    pub verdict: String,
    pub cause: Option<String>,
    pub marked_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FindingItem {
    pub finding_key: String,
    pub session_id: String,
    pub project_id: String,
    pub day: String,
    pub rule_id: String,
    pub scope: String,
    pub level: String,
    pub waste: String,
    pub lever: String,
    pub confidence: String,
    pub count: i64,
    /// A sort weight ("weight" on the page), never minutes and never summed (spec §6.7).
    pub cost_ms: i64,
    pub feedback: Option<FeedbackView>,
    pub rule: Option<RuleView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DevtimeFindings {
    pub items: Vec<FindingItem>,
    pub next_cursor: Option<String>,
    pub rules: Vec<RuleView>,
    /// Which `devtime.yaml` the precision thresholds above were computed under.
    pub thresholds_source: ThresholdsSource,
}

pub struct FindingsQuery<'a> {
    pub rule_id: Option<&'a str>,
    pub order: FindingOrder,
    pub limit: i64,
    pub cursor: Option<FindingCursor>,
}

fn order_tag(order: FindingOrder) -> &'static str {
    match order {
        FindingOrder::Cost => "cost",
        FindingOrder::Recent => "recent",
    }
}

/// Opaque to the client: base64url of `["cost"|"recent", sort, key]` (spec §6.7).
pub fn encode_cursor(cursor: &FindingCursor, order: FindingOrder) -> String {
    let json = serde_json::to_string(&(order_tag(order), &cursor.sort, &cursor.key))
        .expect("strings serialise");
    URL_SAFE_NO_PAD.encode(json)
}

/// `None` for anything that is not a cursor this order wrote — the route answers 400 `bad_cursor`.
pub fn decode_cursor(raw: &str, order: FindingOrder) -> Option<FindingCursor> {
    let bytes = URL_SAFE_NO_PAD.decode(raw).ok()?;
    let (tag, sort, key): (String, String, String) = serde_json::from_slice(&bytes).ok()?;
    (tag == order_tag(order)).then_some(FindingCursor { sort, key })
}

pub async fn devtime_findings(
    pool: &SqlitePool,
    rules: &DevtimeRulesConfig,
    thresholds_source: ThresholdsSource,
    scope: Scope<'_>,
    window: Window,
    now: DateTime<Utc>,
    query: &FindingsQuery<'_>,
) -> sqlx::Result<DevtimeFindings> {
    let since = stamp(window.start());
    let until = stamp(now.min(window.end()));
    let page = store::findings_page(
        pool,
        scope.project(),
        &since,
        &until,
        query.rule_id,
        query.order,
        query.limit,
        query.cursor.as_ref(),
    )
    .await?;
    let views: Vec<RuleView> = precision::rule_report(pool, rules, Some(since.as_str()))
        .await?
        .iter()
        .map(|p| rule_view(p, rules))
        .collect();
    let items = page
        .items
        .into_iter()
        .map(|row| FindingItem {
            day: row.started_at.get(..10).unwrap_or_default().to_string(),
            rule: views.iter().find(|v| v.rule_id == row.rule_id).cloned(),
            feedback: row.verdict.map(|verdict| FeedbackView {
                verdict,
                cause: row.cause,
                marked_at: row.marked_at.unwrap_or_default(),
            }),
            finding_key: row.finding_key,
            session_id: row.session_id,
            project_id: row.project_id,
            rule_id: row.rule_id,
            scope: row.scope,
            level: row.level,
            waste: row.waste,
            lever: row.lever,
            confidence: row.confidence,
            count: row.count,
            cost_ms: row.cost_ms,
        })
        .collect();
    Ok(DevtimeFindings {
        items,
        next_cursor: page.next.map(|next| encode_cursor(&next, query.order)),
        rules: views,
        thresholds_source,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MarkOutcome {
    pub finding_key: String,
    pub feedback: FeedbackView,
    pub rule: Option<RuleView>,
}

/// `precision::mark_case` unchanged (spec §4.5), then the mark and the rule's precision read back:
/// `mark_case` returns `()`. Re-marking replaces; the same mark twice is the same state.
pub async fn mark_finding(
    pool: &SqlitePool,
    rules: &DevtimeRulesConfig,
    window: Window,
    key: &str,
    verdict: &str,
    cause: Option<&str>,
) -> Result<MarkOutcome, MarkError> {
    precision::mark_case(pool, key, verdict, cause).await?;
    let mark = store::feedback_by_key(pool, key)
        .await?
        .ok_or(MarkError::UnknownFinding)?;
    let since = stamp(window.start());
    let rule = precision::rule_report(pool, rules, Some(since.as_str()))
        .await?
        .iter()
        .find(|p| p.rule_id == mark.rule_id)
        .map(|p| rule_view(p, rules));
    Ok(MarkOutcome {
        finding_key: mark.finding_key,
        feedback: FeedbackView {
            verdict: mark.verdict,
            cause: mark.cause,
            marked_at: mark.marked_at,
        },
        rule,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FastPrice, PriceTier};
    // Named, not globbed: the fixture's `Span` struct is what the tests build.
    use crate::devtime::store::fixture::{
        Span, billed_message, finding, insert_span, message, session, span, turn,
    };

    const MIN: i64 = 60_000;
    /// A real rule id from `RULES`.
    const RULE: &str = "A1";

    pub(crate) fn at(moment: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(moment)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Noon on 2026-10-17: the 7-day window is 2026-10-11 ..= 2026-10-17, the previous
    /// 2026-10-04 ..= 10.
    fn now() -> DateTime<Utc> {
        at("2026-10-17T12:00:00Z")
    }

    async fn pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn overview(pool: &sqlx::SqlitePool, scope: Scope<'_>, days: i64) -> DevtimeOverview {
        overview_priced(pool, scope, days, &ModelsConfig::default()).await
    }

    async fn overview_priced(
        pool: &sqlx::SqlitePool,
        scope: Scope<'_>,
        days: i64,
        models: &ModelsConfig,
    ) -> DevtimeOverview {
        devtime_overview(
            pool,
            models,
            scope,
            Window::new(days, now()).unwrap(),
            now(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn series_has_one_entry_per_day_including_empty_days() {
        let pool = pool().await;
        for days in crate::analytics::ALLOWED_DAYS {
            let report = overview(&pool, Scope::Global, days).await;
            assert_eq!(report.days_series.len() as i64, days);
            assert!(report.days_series.last().unwrap().partial);
            let before_today = &report.days_series[..report.days_series.len() - 1];
            assert!(before_today.iter().all(|day| !day.partial));
            assert_eq!(report.days_series.last().unwrap().day, "2026-10-17");
            assert!(
                report.totals.waste_ratio.is_none(),
                "waste_ratio_is_null_without_active_time"
            );
        }
    }

    /// `agent:*` / `bg:*` lanes run beside `main` and never add to active time (spec §6.3).
    #[tokio::test]
    async fn only_main_lane_counts_as_active() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        let (from, to) = ("2026-10-15T09:00:00.000Z", "2026-10-15T09:10:00.000Z");
        insert_span(&pool, span("s1", from, to)).await;
        insert_span(
            &pool,
            Span {
                lane: "agent:x",
                ..span("s1", from, to)
            },
        )
        .await;
        insert_span(
            &pool,
            Span {
                lane: "bg:y",
                ..span("s1", from, to)
            },
        )
        .await;
        let report = overview(&pool, Scope::Global, 7).await;
        assert_eq!(report.totals.active_ms, 10 * MIN);
        let day = report
            .days_series
            .iter()
            .find(|day| day.day == "2026-10-15")
            .unwrap();
        assert_eq!(
            (day.active_ms, day.by_kind.tool, day.by_waste.useful),
            (10 * MIN, 10 * MIN, 10 * MIN)
        );
    }

    #[tokio::test]
    async fn wait_human_and_idle_are_outside_active() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        insert_span(
            &pool,
            Span {
                kind: "wait_human",
                ..span("s1", "2026-10-15T09:00:00.000Z", "2026-10-15T09:05:00.000Z")
            },
        )
        .await;
        insert_span(
            &pool,
            Span {
                kind: "idle",
                ..span("s1", "2026-10-15T09:05:00.000Z", "2026-10-15T10:05:00.000Z")
            },
        )
        .await;
        insert_span(
            &pool,
            Span {
                kind: "wait_machine",
                ..span("s1", "2026-10-15T10:05:00.000Z", "2026-10-15T10:07:00.000Z")
            },
        )
        .await;
        let report = overview(&pool, Scope::Global, 7).await;
        assert_eq!(report.totals.active_ms, 2 * MIN);
        assert_eq!(
            (report.totals.wait_human_ms, report.totals.idle_ms),
            (5 * MIN, 60 * MIN)
        );
    }

    /// Waste classes, the ratio, the inferred share, and NULL waste counted as useful (spec §6.6).
    #[tokio::test]
    async fn waste_is_split_by_class_and_the_ratio_is_over_active_time() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        let useful = span("s1", "2026-10-15T09:00:00.000Z", "2026-10-15T09:06:00.000Z");
        insert_span(&pool, useful).await;
        insert_span(
            &pool,
            Span {
                waste: Some("rework"),
                rule: Some(RULE),
                lever: Some("spec_gap"),
                confidence: "inferred",
                ..span("s1", "2026-10-15T09:06:00.000Z", "2026-10-15T09:09:00.000Z")
            },
        )
        .await;
        insert_span(
            &pool,
            Span {
                waste: Some("avoidable"),
                ..span("s1", "2026-10-15T09:09:00.000Z", "2026-10-15T09:10:00.000Z")
            },
        )
        .await;
        let report = overview(&pool, Scope::Global, 7).await;
        assert_eq!(
            (report.totals.active_ms, report.totals.wasted_ms),
            (10 * MIN, 4 * MIN)
        );
        assert_eq!(report.totals.waste_ratio, Some(0.4));
        assert_eq!(report.totals.inferred_waste_ms, 3 * MIN);
        assert_eq!(report.waste_by_rule[0].rule_id, RULE);
        assert_eq!(report.waste_by_rule[1].rule_id, NONE_KEY);
        assert_eq!(
            report.waste_by_lever[0],
            LeverWaste {
                lever: "spec_gap".into(),
                ms: 3 * MIN
            }
        );
    }

    /// The window before has the same length and the same clipping (spec §4.3).
    #[tokio::test]
    async fn previous_window_uses_the_same_clipping() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-10T23:00:00.000Z", None).await;
        // 23:30 on the previous window's last day -> 00:45 on the current window's first day.
        insert_span(
            &pool,
            span("s1", "2026-10-10T23:30:00.000Z", "2026-10-11T00:45:00.000Z"),
        )
        .await;
        let report = overview(&pool, Scope::Global, 7).await;
        assert_eq!(report.totals.previous.active_ms, 30 * MIN);
        assert_eq!(report.totals.active_ms, 45 * MIN);
    }

    /// A span recorded past `now` (clock skew) counts only up to `now`, on a partial today.
    #[tokio::test]
    async fn open_span_counts_up_to_now_and_marks_day_partial() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-17T11:00:00.000Z", None).await;
        insert_span(
            &pool,
            span("s1", "2026-10-17T11:30:00.000Z", "2026-10-17T13:00:00.000Z"),
        )
        .await;
        let report = overview(&pool, Scope::Global, 7).await;
        let today = report.days_series.last().unwrap();
        assert_eq!((today.active_ms, today.partial), (30 * MIN, true));
    }

    #[tokio::test]
    async fn waste_lists_are_sorted_capped_and_fold_the_rest_into_other() {
        let rows: Vec<(String, i64)> = (0..25).map(|i| (format!("r{i:02}"), 1000 - i)).collect();
        let capped = cap_list(rows, |rest| {
            (OTHER_KEY.to_string(), rest.iter().map(|(_, ms)| ms).sum())
        });
        assert_eq!(capped.len(), LIST_CAP + 1);
        assert_eq!(capped[0].0, "r00");
        assert_eq!(
            capped[LIST_CAP],
            (OTHER_KEY.to_string(), (20..25).map(|i| 1000 - i).sum())
        );
    }

    #[tokio::test]
    async fn correction_rate_uses_known_turns_only() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        turn(&pool, "s1", 1, "2026-10-15T09:00:00.000Z", true, Some(true)).await;
        turn(
            &pool,
            "s1",
            2,
            "2026-10-15T09:10:00.000Z",
            false,
            Some(false),
        )
        .await;
        turn(&pool, "s1", 3, "2026-10-15T09:20:00.000Z", false, None).await;
        turn(&pool, "s1", 4, "2026-10-15T09:30:00.000Z", false, None).await;
        let report = overview(&pool, Scope::Global, 7).await;
        let t = &report.totals;
        assert_eq!(t.turns, 4);
        assert_eq!(t.interruption_rate, Some(0.25));
        assert_eq!(
            t.correction_rate,
            Some(0.5),
            "1 correction over 2 known turns"
        );
        assert_eq!(t.correction_coverage, Some(0.5));
        let day = report
            .days_series
            .iter()
            .find(|day| day.day == "2026-10-15")
            .unwrap();
        assert_eq!((day.turns, day.interrupted, day.corrections), (4, 1, 1));
    }

    #[tokio::test]
    async fn rates_are_null_without_turns() {
        let report = overview(&pool().await, Scope::Global, 30).await;
        assert_eq!(
            (
                report.totals.interruption_rate,
                report.totals.correction_rate,
                report.totals.correction_coverage
            ),
            (None, None, None)
        );
    }

    /// Spec risk 1: the unmapped and daemon counts are the last cycle's, read from the one status
    /// row; nothing new is persisted for them.
    #[tokio::test]
    async fn unmapped_and_daemon_counts_come_from_ingest_status() {
        let pool = pool().await;
        let none = overview(&pool, Scope::Global, 7).await.quality;
        assert_eq!(
            (
                none.ingest_enabled,
                none.last_cycle_at.clone(),
                none.unmapped_sessions_last_cycle
            ),
            (false, None, None)
        );
        crate::devtime::store::write_ingest_status(
            &pool,
            &IngestStatus {
                enabled: true,
                cycle_at: "2026-10-17T11:59:00.000Z".into(),
                unmapped_sessions: 3,
                skipped_daemon_sessions: 12,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        let q = overview(&pool, Scope::Global, 7).await.quality;
        assert_eq!(
            q,
            Quality {
                ingest_enabled: true,
                last_cycle_at: Some("2026-10-17T11:59:00.000Z".into()),
                unmapped_sessions_last_cycle: Some(3),
                skipped_daemon_sessions_last_cycle: Some(12),
                sessions: 1,
                dirty_sessions: 0,
                unruled_sessions: 0,
            }
        );
    }

    fn priced(entries: &[(&str, ModelPrice)]) -> ModelsConfig {
        let mut models = ModelsConfig {
            prices_as_of: Some("2026-10-17".into()),
            ..ModelsConfig::default()
        };
        for (id, price) in entries {
            models.model_prices.insert((*id).to_string(), price.clone());
        }
        models
    }

    fn full(input: f64, output: f64, cache_read: f64, cache_write: f64) -> ModelPrice {
        ModelPrice {
            input: Some(input),
            output: Some(output),
            cache_read: Some(cache_read),
            cache_write: Some(cache_write),
            ..ModelPrice::default()
        }
    }

    #[tokio::test]
    async fn cost_estimate_is_tokens_times_price_per_million() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        // 1M input, 2M cache read, 0.5M cache write, 0.1M output.
        message(
            &pool,
            "s1",
            "main",
            "m1",
            "2026-10-15T09:00:00.000Z",
            Some("claude-sonnet-5-20261001"),
            [1_000_000, 2_000_000, 500_000, 100_000],
        )
        .await;
        let models = priced(&[("claude-sonnet-5", full(3.0, 15.0, 0.3, 3.75))]);
        let tokens = overview_priced(&pool, Scope::Global, 7, &models)
            .await
            .tokens;
        let want = 3.0 + 2.0 * 0.3 + 0.5 * 3.75 + 0.1 * 15.0;
        assert!(
            (tokens.cost_usd.unwrap() - want).abs() < 1e-9,
            "{:?}",
            tokens.cost_usd
        );
        assert!(tokens.cost_estimated);
        assert_eq!(tokens.prices_as_of.as_deref(), Some("2026-10-17"));
        assert!(tokens.unpriced_models.is_empty());
        assert_eq!(tokens.cache_hit_ratio, Some(2.0 / 3.5));
    }

    /// No entry, or a missing field for a kind of token it used: tokens counted, cost not, model
    /// listed.
    #[tokio::test]
    async fn unpriced_model_is_excluded_from_cost_and_listed() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        let one_m = [1_000_000, 0, 0, 0];
        let at9 = "2026-10-15T09:00:00.000Z";
        message(
            &pool,
            "s1",
            "main",
            "m1",
            at9,
            Some("claude-sonnet-5"),
            one_m,
        )
        .await;
        let at901 = "2026-10-15T09:01:00.000Z";
        message(&pool, "s1", "main", "m2", at901, Some("sonnet"), one_m).await;
        let at902 = "2026-10-15T09:02:00.000Z";
        message(
            &pool,
            "s1",
            "main",
            "m3",
            at902,
            Some("gpt-5.5"),
            [0, 0, 10, 0],
        )
        .await;
        let mut half = full(1.25, 10.0, 0.125, 0.0);
        half.cache_write = None;
        let models = priced(&[
            ("claude-sonnet-5", full(3.0, 15.0, 0.3, 3.75)),
            ("gpt-5.5", half),
        ]);
        let tokens = overview_priced(&pool, Scope::Global, 7, &models)
            .await
            .tokens;
        assert_eq!(tokens.cost_usd, Some(3.0));
        assert_eq!(tokens.unpriced_models, ["gpt-5.5", "sonnet"]);
        let alias = tokens.by_model.iter().find(|row| row.model == "sonnet");
        let alias = alias.unwrap();
        assert_eq!((alias.input, alias.cost_usd), (1_000_000, None));
    }

    /// A price the models file had and the parse dropped is named beside the cost, not only logged.
    #[tokio::test]
    async fn a_dropped_price_is_listed_beside_the_cost() {
        let pool = pool().await;
        let mut models = priced(&[]);
        let tokens = overview_priced(&pool, Scope::Global, 7, &models)
            .await
            .tokens;
        assert!(tokens.dropped_prices.is_empty());
        models.price_warnings =
            vec!["claude-haiku-5-5: input is not a finite, non-negative number".into()];
        let tokens = overview_priced(&pool, Scope::Global, 7, &models)
            .await
            .tokens;
        assert_eq!(tokens.dropped_prices, models.price_warnings);
    }

    #[tokio::test]
    async fn cost_null_when_no_model_priced() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        let at9 = "2026-10-15T09:00:00.000Z";
        message(
            &pool,
            "s1",
            "main",
            "m1",
            at9,
            Some("claude-sonnet-5"),
            [10, 0, 0, 0],
        )
        .await;
        // No model_prices at all.
        let tokens = overview(&pool, Scope::Global, 7).await.tokens;
        assert_eq!((tokens.cost_usd, tokens.prices_as_of.clone()), (None, None));
        assert_eq!(tokens.unpriced_models, ["claude-sonnet-5"]);
    }

    /// `days_series[].cost_usd` (owner, 2026-10-18, open decision 5): priced by the window's rules,
    /// so a (model, effort) the window cannot price adds nothing on ANY day, and the days sum to
    /// `tokens.cost_usd`.
    #[tokio::test]
    async fn day_cost_is_priced_like_the_window_and_sums_to_it() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-13T09:00:00.000Z", None).await;
        let one_m = [1_000_000, 0, 0, 0];
        let rows = [
            ("m1", "2026-10-13T09:00:00.000Z", "sonnet", one_m),
            ("m2", "2026-10-14T09:00:00.000Z", "claude-sonnet-5", one_m),
            (
                "m3",
                "2026-10-15T09:00:00.000Z",
                "claude-sonnet-5",
                [0, 0, 0, 100_000],
            ),
            // gpt-5.5 is priced for input but not cache writes, and writes cache on the 16th:
            // unpriced over the window, so its input on the 15th adds nothing either.
            ("m4", "2026-10-15T10:00:00.000Z", "gpt-5.5", one_m),
            ("m5", "2026-10-16T09:00:00.000Z", "gpt-5.5", [0, 0, 10, 0]),
        ];
        for (id, at, model, tokens) in rows {
            message(&pool, "s1", "main", id, at, Some(model), tokens).await;
        }
        let mut half = full(1.25, 10.0, 0.125, 0.0);
        half.cache_write = None;
        let models = priced(&[
            ("claude-sonnet-5", full(3.0, 15.0, 0.3, 3.75)),
            ("gpt-5.5", half),
        ]);
        let report = overview_priced(&pool, Scope::Global, 7, &models).await;
        let cost = |day: &str| {
            report
                .days_series
                .iter()
                .find(|d| d.day == day)
                .unwrap()
                .cost_usd
        };
        assert_eq!(cost("2026-10-12"), None, "no usage at all");
        assert_eq!(cost("2026-10-13"), None, "only an unpriced alias");
        assert_eq!(cost("2026-10-14"), Some(3.0));
        assert_eq!(
            cost("2026-10-15"),
            Some(1.5),
            "the window-unpriced gpt-5.5 adds nothing"
        );
        assert_eq!(cost("2026-10-16"), None, "only the window-unpriced gpt-5.5");
        let summed: f64 = report.days_series.iter().filter_map(|d| d.cost_usd).sum();
        assert!(
            (summed - report.tokens.cost_usd.unwrap()).abs() < 1e-9,
            "{summed} vs {:?}",
            report.tokens.cost_usd
        );
    }

    /// Claude Haiku 5.5's published shape: $0.10 / $0.50 up to 100,000 prompt tokens and
    /// $0.50 / $2.50 from 100,001, cache rates scaling with it.
    fn haiku() -> ModelPrice {
        ModelPrice {
            cache_write_1h: Some(0.20),
            tiers: vec![PriceTier {
                min_prompt_tokens: 100_001,
                input: Some(0.50),
                output: Some(2.50),
                cache_read: Some(0.05),
                cache_write: Some(0.625),
                cache_write_1h: Some(1.0),
            }],
            ..full(0.10, 0.50, 0.01, 0.125)
        }
    }

    const STANDARD: (Option<&str>, Option<&str>) = (Some("standard"), Some("global"));

    fn close(got: Option<f64>, want: f64) -> bool {
        got.is_some_and(|got| (got - want).abs() < 1e-9)
    }

    /// The read splits by the thresholds of the models used in the window only: a tiered price for
    /// a model nobody ran adds no bucket, and so no extra walk of the day index.
    #[test]
    fn only_the_used_models_bring_their_tier_thresholds() {
        let models = priced(&[("claude-haiku-5-5", haiku())]);
        assert!(prompt_thresholds(&models, ["claude-opus-5-5"].into_iter()).is_empty());
        assert!(prompt_thresholds(&models, std::iter::empty()).is_empty());
        assert_eq!(
            prompt_thresholds(
                &models,
                ["claude-haiku-5-5", "claude-haiku-5-5"].into_iter()
            ),
            vec![100_001]
        );
    }

    /// The tier keys on ONE request's prompt (input + cache reads + cache writes): a message at
    /// 100,001 is billed whole at the long rates, one at 100,000 at the base, and a day holding both
    /// sums them exactly — in the window total, the model row and the day series alike.
    #[tokio::test]
    async fn each_message_is_priced_at_the_tier_its_own_prompt_reaches() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        let rows = [
            ("long", "2026-10-15T09:00:00.000Z", [1, 100_000, 0, 1_000]),
            ("base", "2026-10-15T10:00:00.000Z", [100_000, 0, 0, 1_000]),
            ("small", "2026-10-15T11:00:00.000Z", [10, 0, 0, 10]),
        ];
        for (id, at, tokens) in rows {
            let model = "claude-haiku-5-5";
            billed_message(&pool, "s1", id, at, model, tokens, Some(0), STANDARD).await;
        }
        let models = priced(&[("claude-haiku-5-5", haiku())]);
        let report = overview_priced(&pool, Scope::Global, 7, &models).await;
        let long = (1.0 * 0.50 + 100_000.0 * 0.05 + 1_000.0 * 2.50) / 1e6;
        let base = (100_000.0 * 0.10 + 1_000.0 * 0.50) / 1e6;
        let small = (10.0 * 0.10 + 10.0 * 0.50) / 1e6;
        let want = long + base + small;
        let tokens = &report.tokens;
        assert!(
            close(tokens.cost_usd, want),
            "{:?} vs {want}",
            tokens.cost_usd
        );
        assert!(close(tokens.by_model[0].cost_usd, want));
        assert_eq!(
            (tokens.cost_low, tokens.cost_high),
            (tokens.cost_usd, tokens.cost_usd)
        );
        let day = report.days_series.iter().find(|d| d.day == "2026-10-15");
        assert!(close(day.unwrap().cost_usd, want));
        let flat = (100_001.0 * 0.10 + 100_000.0 * 0.01 + 2_010.0 * 0.50 + 10.0 * 0.10) / 1e6;
        assert!(
            want > flat,
            "summing before pricing would have billed the base tier"
        );
    }

    /// `speed: fast` replaces input and output (and scales the cache rates by the same factor);
    /// `inference_geo: us` multiplies everything. A fast request of a model with no fast price
    /// makes the model unpriced rather than standard-priced.
    #[tokio::test]
    async fn fast_and_us_resident_requests_are_priced_at_their_multipliers() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        let opus = ModelPrice {
            cache_write_1h: Some(10.0),
            fast: Some(FastPrice {
                input: 8.0,
                output: 40.0,
            }),
            geo_us_mult: Some(1.1),
            ..full(5.0, 25.0, 0.5, 6.25)
        };
        let models = priced(&[
            ("claude-opus-5-5", opus),
            ("claude-sonnet-5", full(3.0, 15.0, 0.3, 3.75)),
        ]);
        let one_m = [1_000_000, 0, 0, 100_000];
        let (fast_us, us) = ((Some("fast"), Some("us")), (Some("standard"), Some("us")));
        let rows = [
            ("a", "2026-10-15T09:00:00.000Z", "claude-opus-5-5", fast_us),
            ("b", "2026-10-15T10:00:00.000Z", "claude-opus-5-5", us),
            ("c", "2026-10-15T11:00:00.000Z", "claude-opus-5-5", STANDARD),
            ("d", "2026-10-15T12:00:00.000Z", "claude-sonnet-5", fast_us),
        ];
        for (id, at, model, mode) in rows {
            billed_message(&pool, "s1", id, at, model, one_m, Some(0), mode).await;
        }
        let tokens = overview_priced(&pool, Scope::Global, 7, &models)
            .await
            .tokens;
        let want = (8.0 + 4.0) * 1.1 + (5.0 + 2.5) * 1.1 + (5.0 + 2.5);
        assert!(
            close(tokens.cost_usd, want),
            "{:?} vs {want}",
            tokens.cost_usd
        );
        assert_eq!(tokens.unpriced_models, ["claude-sonnet-5"]);
    }

    /// Known 1-hour writes at the 1-hour rate, the known rest at the 5-minute one; the bounds meet.
    #[tokio::test]
    async fn one_hour_cache_writes_are_priced_at_the_one_hour_rate() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        let at = "2026-10-15T09:00:00.000Z";
        let tokens = [0, 0, 1_000_000, 0];
        let model = "claude-sonnet-5";
        billed_message(
            &pool,
            "s1",
            "m1",
            at,
            model,
            tokens,
            Some(600_000),
            STANDARD,
        )
        .await;
        let sonnet = ModelPrice {
            cache_write_1h: Some(6.0),
            ..full(3.0, 15.0, 0.3, 3.75)
        };
        let models = priced(&[("claude-sonnet-5", sonnet)]);
        let tokens = overview_priced(&pool, Scope::Global, 7, &models)
            .await
            .tokens;
        let want = 0.4 * 3.75 + 0.6 * 6.0;
        assert!(
            close(tokens.cost_usd, want),
            "{:?} vs {want}",
            tokens.cost_usd
        );
        assert_eq!(
            (tokens.cost_low, tokens.cost_high),
            (tokens.cost_usd, tokens.cost_usd)
        );
    }

    /// A message ingested before 0214 has no cache split: low prices its writes at 5 minutes, high
    /// at 1 hour, and the estimate is high, which is what Claude Code writes. Known usage beside it
    /// is the same on both sides.
    #[tokio::test]
    async fn an_unknown_cache_split_gives_a_range_whose_estimate_is_the_high_end() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        let model = "claude-sonnet-5";
        let rows = [
            (
                "old",
                "2026-10-15T09:00:00.000Z",
                [0, 0, 1_000_000, 0],
                None,
            ),
            (
                "new",
                "2026-10-15T10:00:00.000Z",
                [1_000_000, 0, 0, 0],
                Some(0),
            ),
        ];
        for (id, at, tokens, one_hour) in rows {
            billed_message(&pool, "s1", id, at, model, tokens, one_hour, STANDARD).await;
        }
        let sonnet = ModelPrice {
            cache_write_1h: Some(6.0),
            ..full(3.0, 15.0, 0.3, 3.75)
        };
        let models = priced(&[("claude-sonnet-5", sonnet)]);
        let report = overview_priced(&pool, Scope::Global, 7, &models).await;
        let tokens = &report.tokens;
        assert!(close(tokens.cost_low, 3.0 + 3.75), "{:?}", tokens.cost_low);
        assert!(close(tokens.cost_high, 3.0 + 6.0), "{:?}", tokens.cost_high);
        assert_eq!(tokens.cost_usd, tokens.cost_high);
        let row = &tokens.by_model[0];
        assert!(row.cost_low < row.cost_high);
        assert_eq!(row.cost_usd, row.cost_high);
        let day = report.days_series.iter().find(|d| d.day == "2026-10-15");
        assert_eq!(
            day.unwrap().cost_usd,
            tokens.cost_usd,
            "a day takes the estimate"
        );
    }

    #[tokio::test]
    async fn cache_hit_ratio_null_when_no_input() {
        let tokens = overview(&pool().await, Scope::Global, 7).await.tokens;
        assert_eq!(
            (tokens.cache_hit_ratio, tokens.previous.cache_hit_ratio),
            (None, None)
        );
    }

    /// The token columns are `NOT NULL DEFAULT 0` (0164), so a NULL cannot be stored; a row written
    /// without them still counts in `messages` with zero tokens, and NULL model/effort group as
    /// `unknown`.
    #[tokio::test]
    async fn null_token_columns_count_as_zero() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        sqlx::query(
            "INSERT INTO devtime_messages (session_id, lane, message_id, first_at, last_at,
                                           parser_version)
             VALUES ('s1', 'main', 'm1', '2026-10-15T09:00:00.000Z', '2026-10-15T09:00:00.000Z', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let tokens = overview(&pool, Scope::Global, 7).await.tokens;
        let row = &tokens.by_model[0];
        assert_eq!(
            (
                row.model.as_str(),
                row.effort.as_str(),
                row.messages,
                row.input
            ),
            (UNKNOWN, UNKNOWN, 1, 0)
        );
    }

    fn precision_cfg() -> DevtimeRulesConfig {
        DevtimeRulesConfig::default() // floor 0.8, min_cases 20 (spec §4.5)
    }

    fn counts(n_cases: i64, n_not_rework: i64) -> RulePrecision {
        RulePrecision {
            rule_id: RULE.into(),
            level: crate::devtime::rules::Level::Base,
            n_cases,
            n_not_rework,
            precision: (n_cases > 0).then(|| (n_cases - n_not_rework) as f64 / n_cases as f64),
            prior: 0.7,
            eligible: false,
        }
    }

    #[test]
    fn rule_with_few_cases_has_null_precision() {
        let view = rule_view(&counts(3, 0), &precision_cfg());
        assert_eq!(
            (view.precision, view.low_precision, view.n_cases),
            (None, false, 3)
        );
    }

    #[test]
    fn low_precision_requires_min_cases_and_below_floor() {
        let cfg = precision_cfg();
        assert!(
            rule_view(&counts(20, 5), &cfg).low_precision,
            "0.75 over 20 cases"
        );
        assert!(
            !rule_view(&counts(20, 4), &cfg).low_precision,
            "0.80 is at the floor"
        );
        assert!(
            !rule_view(&counts(19, 10), &cfg).low_precision,
            "too few cases to judge"
        );
    }

    #[test]
    fn a_cursor_round_trips_and_a_foreign_one_is_refused() {
        let cursor = FindingCursor {
            sort: "400".into(),
            key: "k/2".into(),
        };
        let raw = encode_cursor(&cursor, FindingOrder::Cost);
        assert_eq!(decode_cursor(&raw, FindingOrder::Cost), Some(cursor));
        assert_eq!(
            decode_cursor(&raw, FindingOrder::Recent),
            None,
            "an order's cursor fits only it"
        );
        assert_eq!(decode_cursor("not base64 !!", FindingOrder::Cost), None);
    }

    #[tokio::test]
    async fn findings_cost_ms_is_never_summed_into_totals() {
        let pool = pool().await;
        session(&pool, "s1", "alpha", "2026-10-15T09:00:00.000Z", None).await;
        insert_span(
            &pool,
            Span {
                waste: Some("rework"),
                rule: Some(RULE),
                ..span("s1", "2026-10-15T09:00:00.000Z", "2026-10-15T09:02:00.000Z")
            },
        )
        .await;
        let before = overview(&pool, Scope::Global, 7).await.totals;
        finding(
            &pool,
            "k1",
            "alpha",
            RULE,
            "2026-10-15T09:00:00.000Z",
            9_999_999,
        )
        .await;
        let after = overview(&pool, Scope::Global, 7).await;
        assert_eq!(after.totals, before);
        assert_eq!(after.waste_by_rule[0].findings, 1);
        assert_eq!(after.waste_by_rule[0].ms, 2 * MIN);
    }

    #[tokio::test]
    async fn findings_list_carries_the_mark_and_the_global_rule_precision() {
        let pool = pool().await;
        finding(&pool, "k1", "alpha", RULE, "2026-10-15T09:00:00.000Z", 10).await;
        finding(&pool, "k2", "beta", RULE, "2026-10-15T09:00:00.000Z", 5).await;
        precision::mark_case(&pool, "k2", "not_rework", None)
            .await
            .unwrap();
        let window = Window::new(7, now()).unwrap();
        let query = FindingsQuery {
            rule_id: None,
            order: FindingOrder::Cost,
            limit: 50,
            cursor: None,
        };
        let scope = Scope::Project("alpha");
        let source = ThresholdsSource::File;
        let alpha = devtime_findings(
            &pool,
            &precision_cfg(),
            source,
            scope,
            window,
            now(),
            &query,
        )
        .await
        .unwrap();
        assert_eq!(alpha.thresholds_source, ThresholdsSource::File);
        assert_eq!(alpha.items.len(), 1, "only alpha's finding is listed");
        // ...but the rule's precision is global (spec §4.5): beta's marked finding is its one case.
        let rule = alpha.items[0].rule.as_ref().unwrap();
        assert_eq!((rule.n_cases, rule.n_not_rework), (1, 1));
        assert_eq!(alpha.rules.len(), RULES.len());
        assert!(alpha.items[0].feedback.is_none());
        assert_eq!(alpha.items[0].day, "2026-10-15");
    }

    #[tokio::test]
    async fn mark_roundtrip_updates_rule_precision_in_response() {
        let pool = pool().await;
        finding(&pool, "k1", "alpha", RULE, "2026-10-15T09:00:00.000Z", 10).await;
        let window = Window::new(30, now()).unwrap();
        let cfg = precision_cfg();
        let out = mark_finding(&pool, &cfg, window, "k1", "not_rework", None)
            .await
            .unwrap();
        assert_eq!(out.feedback.verdict, "not_rework");
        let rule = out.rule.unwrap();
        assert_eq!(
            (rule.n_cases, rule.n_not_rework, rule.precision),
            (1, 1, None),
            "1 case < min_cases"
        );
    }

    #[tokio::test]
    async fn mark_again_replaces_previous_verdict() {
        let pool = pool().await;
        finding(&pool, "k1", "alpha", RULE, "2026-10-15T09:00:00.000Z", 10).await;
        let window = Window::new(30, now()).unwrap();
        let cfg = precision_cfg();
        mark_finding(&pool, &cfg, window, "k1", "not_rework", None)
            .await
            .unwrap();
        let again = mark_finding(&pool, &cfg, window, "k1", "confirmed", Some("spec_gap"))
            .await
            .unwrap();
        assert_eq!(
            (
                again.feedback.verdict.as_str(),
                again.feedback.cause.as_deref()
            ),
            ("confirmed", Some("spec_gap"))
        );
        assert_eq!(again.rule.unwrap().n_not_rework, 0);
        let same = mark_finding(&pool, &cfg, window, "k1", "confirmed", Some("spec_gap"))
            .await
            .unwrap();
        assert_eq!(same.feedback.verdict, "confirmed", "idempotent");
        assert!(matches!(
            mark_finding(&pool, &cfg, window, "nope", "confirmed", None).await,
            Err(MarkError::UnknownFinding)
        ));
    }

    /// `EXPLAIN QUERY PLAN` of a read over 90 days, for `alpha` when `project` (the `$project`
    /// form binds `?3`, the global one does not), one `detail` per step.
    async fn read_plan(pool: &sqlx::SqlitePool, sql: &str, project: bool) -> Vec<String> {
        use sqlx::Row as _;
        let query = sqlx::query(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {sql}")))
            .bind("2026-07-19T00:00:00.000Z")
            .bind("2026-10-17T12:00:00.000Z");
        let query = if project { query.bind("alpha") } else { query };
        let rows = query.fetch_all(pool).await.unwrap();
        rows.iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect()
    }

    fn mentions(plan: &[String], needle: &str) -> bool {
        plan.iter().any(|line| line.contains(needle))
    }

    /// `SCAN` steps over a table; the span read's own CTEs (`one_day`, `pieces`, `parts`) are not.
    fn table_scans(plan: &[String]) -> Vec<&String> {
        plan.iter()
            .filter(|line| line.starts_with("SCAN "))
            .filter(|line| {
                !["one_day", "pieces", "parts"]
                    .iter()
                    .any(|cte| line.contains(cte))
            })
            .collect()
    }

    /// Spec §6.5: the planner's answer is recorded. The turn and session project reads start from
    /// `idx_devtime_sessions_project` (migration 0210); no devtime table is scanned. The one-form
    /// `(?3 IS NULL OR ss.project_id = ?3)` reads these replaced planned `SCAN sp` for a single
    /// project. The span and message reads walk a day index instead — see the next test.
    #[tokio::test]
    async fn the_project_span_read_reaches_spans_through_an_index() {
        let pool = pool().await;
        for sql in [
            crate::devtime::store::TURN_DAY_PROJECT_SQL,
            crate::devtime::store::SESSION_COUNTS_PROJECT_SQL,
        ] {
            let plan = read_plan(&pool, sql, true).await;
            println!("{plan:#?}");
            assert!(
                mentions(&plan, "idx_devtime_sessions_project"),
                "the project's sessions are found by index: {plan:#?}"
            );
            assert!(
                table_scans(&plan).is_empty(),
                "no devtime table is scanned: {plan:#?}"
            );
        }
    }

    /// Spec §9 performance, the planner's answer recorded: both forms of the span and message
    /// reads take their rows from the day indexes of migrations 0211/0214 in GROUP BY order, so the
    /// only sort left is the span read's last merge of a few rows per day. Joining spans to a
    /// recursive day list instead paired every span with every day: 20 s for the 50,000 spans of
    /// the timing test below. The message read holds with any number of prompt thresholds: each
    /// bucket's arm walks the index in its own GROUP BY order (`message_day_sql`).
    #[tokio::test]
    async fn the_day_reads_take_their_rows_in_group_order_from_the_day_indexes() {
        use crate::devtime::store::{SPAN_DAY_PROJECT_SQL, SPAN_DAY_SQL, message_day_sql};
        let pool = pool().await;
        let mut reads = vec![
            (SPAN_DAY_SQL.to_string(), false, "idx_devtime_spans_day", 1),
            (
                SPAN_DAY_PROJECT_SQL.to_string(),
                true,
                "idx_devtime_spans_day",
                1,
            ),
        ];
        for thresholds in 0..=2 {
            for project in [false, true] {
                let sql = message_day_sql(project, thresholds);
                reads.push((sql, project, "idx_devtime_messages_day", 0));
            }
        }
        for (sql, project, index, sorts) in reads {
            let plan = read_plan(&pool, &sql, project).await;
            println!("{plan:#?}");
            assert!(
                mentions(&plan, index),
                "the rows come from {index}: {plan:#?}"
            );
            assert!(
                table_scans(&plan).is_empty(),
                "no devtime table is scanned: {plan:#?}"
            );
            // `USE TEMP B-TREE FOR GROUP BY` is the only plan step naming a GROUP BY.
            let grouping_sorts = plan.iter().filter(|line| line.contains("FOR GROUP BY"));
            assert_eq!(
                grouping_sorts.count(),
                sorts,
                "rows are grouped in index order: {plan:#?}"
            );
        }
        for (sql, project) in [(SPAN_DAY_SQL, false), (SPAN_DAY_PROJECT_SQL, true)] {
            let plan = read_plan(&pool, sql, project).await;
            assert!(
                mentions(&plan, "idx_devtime_spans_multiday"),
                "the spans that cross a midnight are found by index: {plan:#?}"
            );
        }
    }

    /// Spec §9 performance: 90 days, 50,000 spans. Ignored by default — a timing test is not
    /// evidence under load (CLAUDE.md, "Full-suite runs are not reliable under load"); run it
    /// alone, on purpose, and read the printed timings.
    #[tokio::test]
    #[ignore]
    async fn the_overview_of_ninety_days_stays_under_budget() {
        let pool = pool().await;
        let mut tx = pool.begin().await.unwrap();
        // 500 sessions x 100 spans of 3 minutes, spread over 90 days, 10 projects.
        for s in 0..500_u32 {
            let day = i64::from(s % 90);
            let opened_at = now() - chrono::Duration::days(day) - chrono::Duration::hours(6);
            let opened = stamp(opened_at);
            sqlx::query(
                "INSERT INTO devtime_sessions (session_id, project_id, started_at, dirty,
                     parser_version, updated_at, rules_version)
                 VALUES (?, ?, ?, 0, 1, ?, 'v1')",
            )
            .bind(format!("s{s}"))
            .bind(format!("p{}", s % 10))
            .bind(opened.clone())
            .bind(opened)
            .execute(&mut *tx)
            .await
            .unwrap();
            for k in 0..100_i64 {
                let ago = chrono::Duration::days(day) + chrono::Duration::minutes(3 * (k + 1));
                let start = now() - ago;
                sqlx::query(
                    "INSERT INTO devtime_spans (session_id, lane, kind, started_at, ended_at,
                         waste, confidence, parser_version)
                     VALUES (?, 'main', 'tool', ?, ?, CASE WHEN ? % 7 = 0 THEN 'rework' END,
                             'exact', 1)",
                )
                .bind(format!("s{s}"))
                .bind(stamp(start))
                .bind(stamp(start + chrono::Duration::minutes(3)))
                .bind(k)
                .execute(&mut *tx)
                .await
                .unwrap();
            }
        }
        tx.commit().await.unwrap();
        let started = std::time::Instant::now();
        let report = overview(&pool, Scope::Global, 90).await;
        let global = started.elapsed();
        let started = std::time::Instant::now();
        overview(&pool, Scope::Project("p3"), 90).await;
        let project = started.elapsed();
        println!(
            "global {global:?}, project {project:?}, active {}",
            report.totals.active_ms
        );
        assert!(
            global.as_millis() < 300 && project.as_millis() < 300,
            "global {global:?} project {project:?}"
        );
    }
}
