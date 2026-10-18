import type { DevtimeDay, DevtimeQuality, LeverWaste, RuleView, RuleWaste } from "../data/devtime";
import type { ChartDatum, ChartKey, ShareItem } from "../ui";

/** The devtime blocks as chart props and the words around them. Pure, tested without rendering. */

export const NONE_KEY = "__none__";
export const OTHER_KEY = "__other__";
const TOP = 8;
/** Five minutes: five ingest cycles of 60 s (spec §7.1). */
export const STALE_AFTER_MS = 5 * 60_000;

export const WASTE_KEYS: ChartKey[] = [
  { key: "useful", label: "Useful", className: "ui-chart-useful" },
  { key: "rework", label: "Rework", className: "ui-chart-rework" },
  { key: "avoidable", label: "Avoidable", className: "ui-chart-avoidable" },
];

/** Categorical by position (`ui-chart-s1…s5`): kinds have no meaning a colour should carry. */
export const KIND_KEYS: ChartKey[] = [
  { key: "model", label: "Model" },
  { key: "tool", label: "Tools" },
  { key: "subagent", label: "Subagents" },
  { key: "wait_background", label: "Background wait" },
  { key: "wait_machine", label: "Machine wait" },
];

export const TURN_KEYS: ChartKey[] = [
  { key: "interrupted", label: "Interrupted", className: "ui-chart-rework" },
  { key: "corrections", label: "Opened with a correction", className: "ui-chart-avoidable" },
];

function datum(day: DevtimeDay, parts: ChartDatum["parts"]): ChartDatum {
  return { day: day.day, partial: day.partial, parts };
}

export function activeByWaste(series: DevtimeDay[]): ChartDatum[] {
  return series.map((day) => datum(day, WASTE_KEYS.map(({ key }) => ({ key, value: day.by_waste[key as keyof DevtimeDay["by_waste"]] }))));
}

export function activeByKind(series: DevtimeDay[]): ChartDatum[] {
  return series.map((day) => datum(day, KIND_KEYS.map(({ key }) => ({ key, value: day.by_kind[key as keyof DevtimeDay["by_kind"]] }))));
}

export function wastedByClass(series: DevtimeDay[]): ChartDatum[] {
  return series.map((day) => datum(day, [
    { key: "rework", value: day.by_waste.rework },
    { key: "avoidable", value: day.by_waste.avoidable },
  ]));
}

export function turnBars(series: DevtimeDay[]): ChartDatum[] {
  return series.map((day) => datum(day, [
    { key: "interrupted", value: day.interrupted },
    { key: "corrections", value: day.corrections },
  ]));
}

export const COST_KEYS: ChartKey[] = [{ key: "cost", label: "Estimated cost", className: "ui-chart-cost" }];

/**
 * The cost card's sparkline (owner, 2026-10-18, open decision 5). A `null` day carries no part — never a
 * written `0`, since null is absent — and with no priced day at all there is no series, so the card
 * draws no sparkline, just as its number shows `Unread` when `tokens.cost_usd` is null.
 */
export function costSeries(series: DevtimeDay[]): ChartDatum[] {
  if (series.every((day) => day.cost_usd === null)) return [];
  return series.map((day) => datum(day, day.cost_usd === null ? [] : [{ key: "cost", value: day.cost_usd }]));
}

function label(key: string, none: string): string {
  if (key === NONE_KEY) return none;
  if (key === OTHER_KEY) return "Other";
  return key;
}

function topShares(rows: { key: string; ms: number }[], none: string): ShareItem[] {
  const head = rows.slice(0, TOP).map((row) => ({ key: row.key, label: label(row.key, none), value: row.ms }));
  const rest = rows.slice(TOP).reduce((sum, row) => sum + row.ms, 0);
  return rest > 0 ? [...head, { key: OTHER_KEY, label: "Other", value: rest, className: "ui-chart-other" }] : head;
}

/** Top eight, the rest (including the daemon's own `__other__`) folded into one "Other" (spec §7.4). */
export function ruleShares(rows: RuleWaste[]): ShareItem[] {
  return topShares(rows.map((row) => ({ key: row.rule_id, ms: row.ms })), "No rule");
}

export function leverShares(rows: LeverWaste[]): ShareItem[] {
  return topShares(rows.map((row) => ({ key: row.lever, ms: row.ms })), "No lever");
}

/** Minutes and hours from milliseconds, the way the rest of the page writes durations. */
export function duration(ms: number, humanMinutes: (minutes: number) => string): string {
  return humanMinutes(Math.round(ms / 60_000));
}

/** A ratio's change, in percentage points (spec §6.4); under one point is level. */
export function versusPoints(now: number | null, before: number | null, days: number): string {
  if (now === null || before === null) return "";
  const window = `the previous ${days} days`;
  const points = Math.round((now - before) * 100);
  if (points === 0) return `level with ${window}`;
  const n = Math.abs(points);
  return `${n} ${n === 1 ? "point" : "points"} ${points > 0 ? "more" : "less"} than ${window}`;
}

/** "83% in 12 cases" or "few cases (3)" — `n` always shown (spec risk 4); global in a project's view. */
export function precisionText(rule: RuleView, inProject: boolean): string {
  const text = rule.precision === null ? `few cases (${rule.n_cases})` : `${Math.round(rule.precision * 100)}% in ${rule.n_cases} cases`;
  return inProject ? `${text} · global, all projects` : text;
}

function ago(ms: number): string {
  const minutes = Math.max(0, Math.round(ms / 60_000));
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  return hours < 48 ? `${hours} h ago` : `${Math.round(hours / 24)} days ago`;
}

/** The strip's clauses; zero counts are left out. Amber past five cycles or with ingest off (spec §7.1). */
export function qualityLine(quality: DevtimeQuality, now: number): { clauses: string[]; amber: boolean; quality: DevtimeQuality } {
  const read = quality.last_cycle_at === null ? null : Date.parse(quality.last_cycle_at);
  const clauses = [
    read === null ? "Never read" : `Last read ${ago(now - read)}`,
    (quality.unmapped_sessions_last_cycle ?? 0) > 0
      ? `${quality.unmapped_sessions_last_cycle} ${quality.unmapped_sessions_last_cycle === 1 ? "session" : "sessions"} without a project in the last cycle` : "",
    (quality.skipped_daemon_sessions_last_cycle ?? 0) > 0
      ? `${quality.skipped_daemon_sessions_last_cycle} daemon ${quality.skipped_daemon_sessions_last_cycle === 1 ? "session" : "sessions"} skipped in the last cycle` : "",
    quality.dirty_sessions > 0 ? `${quality.dirty_sessions} dirty` : "",
    quality.unruled_sessions > 0 ? `${quality.unruled_sessions} not yet classified` : "",
  ].filter((clause) => clause !== "");
  const amber = !quality.ingest_enabled || read === null || now - read > STALE_AFTER_MS;
  return { clauses, amber, quality };
}
