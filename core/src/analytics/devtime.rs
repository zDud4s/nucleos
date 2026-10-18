//! Development time: the owner's own Claude Code sessions, read from the devtime tables for the
//! analytics page (spec 2026-10-17-analytics-devtime). Read and assembled per request, no rollup.
//!
//! Never summed with phase 1's cost: that is the daemon's runs, this is the owner's manual
//! sessions — different populations (spec §2, non-goals). `devtime_findings.cost_ms` is a sort weight and is
//! never added into a total here (§6.7); totals come from spans.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

use super::{Scope, Window};
use crate::config::ModelsConfig;
use crate::devtime::rules::RULES;
use crate::devtime::store::{
    self as store, IngestStatus, MessageDayRow, SessionCounts, SpanDayRow, TurnDayRow,
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

/// Placeholder until Task 2.3 gives it the token and cost fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DevtimeTokens {}

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
    pub messages: Vec<MessageDayRow>,
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
    let reads = Reads {
        spans: store::span_day_rows(pool, project, &since, &until).await?,
        messages: store::message_day_rows(pool, project, &since, &until).await?,
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

// `_models` is read by Task 2.3 (token pricing).
pub(crate) fn assemble(
    reads: &Reads,
    _models: &ModelsConfig,
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
    totals.correction_coverage = (current_turns.known != 0)
        .then(|| current_turns.known as f64 / current_turns.turns as f64);
    prev.interruption_rate = ratio(previous_turns.interrupted, previous_turns.turns);
    prev.correction_rate = ratio(previous_turns.corrections, previous_turns.known);
    // tokens and the per-day cost: Task 2.3.
    totals.previous = prev;
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
        tokens: DevtimeTokens::default(),
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

#[cfg(test)]
mod tests {
    use super::*;
    // Named, not globbed: the fixture's `Span` struct is what the tests build. Task 2.3's tests add
    // `message` to this list.
    use crate::devtime::store::fixture::{Span, insert_span, session, span, turn};

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
        devtime_overview(pool, models, scope, Window::new(days, now()).unwrap(), now())
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
        turn(&pool, "s1", 2, "2026-10-15T09:10:00.000Z", false, Some(false)).await;
        turn(&pool, "s1", 3, "2026-10-15T09:20:00.000Z", false, None).await;
        turn(&pool, "s1", 4, "2026-10-15T09:30:00.000Z", false, None).await;
        let report = overview(&pool, Scope::Global, 7).await;
        let t = &report.totals;
        assert_eq!(t.turns, 4);
        assert_eq!(t.interruption_rate, Some(0.25));
        assert_eq!(t.correction_rate, Some(0.5), "1 correction over 2 known turns");
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
}
