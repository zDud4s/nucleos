import type { ChartDatum, ChartKey } from "../../ui";
import type { Autopilot, Gate, Jobs, Landing, PipelineReport, Postgate, Pressure } from "../../data/analytics-pipeline";
import { isBlockError } from "../../data/analytics-pipeline";
import { plural } from "../../lib/plural";
import { versus } from "../series";
import type { Metric } from "../sections";
import { NO_DATA, clauses, humanSeconds, percentOr } from "./format";

/**
 * One card per block of the pipeline report (spec §8), pure. A card is a number, one line about it
 * and a sparkline; a block that arrived as `{error}` is a card that says so (`error: true`) and
 * nothing more, so one failed read never takes the others down (D5).
 *
 * **Adding a card** (a seventh): extend `PipelineKey`, `ORDER`, `LABELS` and
 * `BUILDERS` here, then its panel in `panels.tsx`'s `PANELS`. Slot pressure is the sixth. Conflicts, owner-blocked, cache and
 * bypass are not cards: they are extra panel pairs of their block.
 */

export type PipelineKey = "postgate" | "landing" | "gate" | "jobs" | "autopilot" | "pressure";

/** Spec §8 order: the post-merge gate first, because red is what matters most. */
export const ORDER: PipelineKey[] = ["postgate", "landing", "gate", "jobs", "autopilot", "pressure"];

export const LABELS: Record<PipelineKey, string> = {
  postgate: "Post-merge gate",
  landing: "Landing queue",
  gate: "Gate",
  jobs: "Jobs",
  autopilot: "Autopilot",
  pressure: "Slot pressure",
};

export const EMPTY = "No activity in this window";

const count = (value: number) => String(Math.round(value));

/** One-part series from a block's days. */
export function series<T extends { day: string; partial: boolean }>(
  days: T[],
  key: ChartKey,
  part: (day: T) => number,
): ChartDatum[] {
  return days.map((day) => ({ day: day.day, partial: day.partial, parts: [{ key: key.key, value: part(day) }] }));
}

function redFor(postgate: Postgate, now: string): string {
  if (postgate.red_since === null) return "Red since before the record";
  const seconds = (Date.parse(now) - Date.parse(postgate.red_since)) / 1000;
  const phase = postgate.red_phase === null ? "" : ` (phase: ${postgate.red_phase})`;
  return `Red for ${humanSeconds(seconds)}${phase}`;
}

function postgateCard(postgate: Postgate, report: PipelineReport): Metric<PipelineKey> {
  const key: ChartKey = { key: "red", label: "Red time", className: "ui-chart-failed" };
  const reverts = postgate.reverts > 0 ? plural(postgate.reverts, "revert") : "";
  const window =
    postgate.red_s_in_window > 0
      ? `${humanSeconds(postgate.red_s_in_window)} red in ${report.days} days`
      : `no red in ${report.days} days`;
  return {
    key: "postgate",
    label: LABELS.postgate,
    value: postgate.red_now ? "Red" : "Green",
    sub: clauses([postgate.red_now ? redFor(postgate, report.now) : window, reverts]),
    spark: {
      data: series(postgate.red_s_daily, key, (day) => day.red_s),
      keys: [key],
      tone: "ui-chart-failed",
      format: humanSeconds,
    },
    alert: postgate.red_now,
  };
}

/** What waits on the owner right now (not windowed); nothing is said when nothing waits. */
export function ownerBlockedLine(landing: Landing): string {
  const blocked = landing.owner_blocked;
  if (blocked === null || blocked.count <= 0) return "";
  const oldest = blocked.oldest_age_s === null ? "" : `oldest ${humanSeconds(blocked.oldest_age_s)}`;
  return clauses([`${blocked.count} waiting on you`, oldest]);
}

function landingCard(landing: Landing): Metric<PipelineKey> {
  const key: ChartKey = { key: "total", label: "Landings" };
  const wait = (p50: number | null) => (p50 === null ? NO_DATA : humanSeconds(p50));
  return {
    key: "landing",
    label: LABELS.landing,
    value: landing.total,
    sub: clauses([
      landing.total === 0
        ? EMPTY
        : clauses([
            `wait p50 ${wait(landing.wait_s_machine.p50)} (all ${wait(landing.wait_s.p50)})`,
            "last attempt",
            `${percentOr(landing.bad_share)} went wrong`,
          ]),
      ownerBlockedLine(landing),
    ]),
    spark: { data: series(landing.daily, key, (day) => day.total), keys: [key], tone: "ui-chart-runs", format: count },
  };
}

function gateCard(gate: Gate, days: number): Metric<PipelineKey> {
  const key: ChartKey = { key: "p50", label: "Median duration" };
  return {
    key: "gate",
    label: LABELS.gate,
    value: percentOr(gate.pass_rate),
    sub:
      gate.terminal === 0
        ? EMPTY
        : clauses([`first try ${percentOr(gate.first_pass_rate)}`, versus(gate.pass_rate, gate.prev_pass_rate, days)]),
    spark: {
      data: series(gate.duration_ms_daily, key, (day) => day.p50 ?? 0),
      keys: [key],
      tone: "ui-chart-runs",
      format: (ms) => humanSeconds(ms / 1000),
    },
  };
}

/** The sum of the núcleo's stuck signals (a job can trip more than one). */
export function stuckCount(jobs: Jobs): number {
  const { age_over_lifetime, dry_rounds_at_limit, rounds_exhausted, gate_retry_loop, item_stuck } = jobs.stuck;
  return age_over_lifetime + dry_rounds_at_limit + rounds_exhausted + gate_retry_loop + item_stuck;
}

function jobsCard(jobs: Jobs, days: number): Metric<PipelineKey> {
  const key: ChartKey = { key: "created", label: "Jobs created" };
  return {
    key: "jobs",
    label: LABELS.jobs,
    value: percentOr(jobs.completion_rate),
    sub:
      jobs.total === 0
        ? EMPTY
        : clauses([
            `${jobs.completed} of ${jobs.total} completed`,
            `${stuckCount(jobs)} stuck`,
            versus(jobs.completion_rate, jobs.prev_completion_rate, days),
          ]),
    spark: { data: series(jobs.daily, key, (day) => day.created), keys: [key], tone: "ui-chart-delivered", format: count },
  };
}

/** No series: the card draws no sparkline. */
function autopilotCard(autopilot: Autopilot): Metric<PipelineKey> {
  const { judge, refused_actions: refused } = autopilot;
  const empty = judge.n === 0 && autopilot.classifier.length === 0 && refused.total === 0;
  return {
    key: "autopilot",
    label: LABELS.autopilot,
    value: percentOr(judge.agree),
    sub: empty ? EMPTY : clauses([`${judge.reviewed} actions with your verdict`, `${refused.total} refused`]),
  };
}

/** The one series the pressure card and panel draw: average slots in use (a day nobody sampled is 0). */
export const PRESSURE_KEY: ChartKey = { key: "avg", label: "Average slots in use" };
const tenth = (value: number) => value.toFixed(1);

function pressureCard(pressure: Pressure, days: number): Metric<PipelineKey> {
  return {
    key: "pressure",
    label: LABELS.pressure,
    value: percentOr(pressure.at_limit_share),
    sub:
      pressure.peak_in_use === null
        ? "No samples in this window"
        : clauses([
            `peak ${pressure.peak_in_use} of ${pressure.limit === null ? NO_DATA : pressure.limit}`,
            versus(pressure.at_limit_share, pressure.prev_at_limit_share, days),
          ]),
    spark: {
      data: series(pressure.daily, PRESSURE_KEY, (day) => day.avg_in_use ?? 0),
      keys: [PRESSURE_KEY],
      tone: "ui-chart-runs",
      format: tenth,
    },
  };
}

function failed(key: PipelineKey): Metric<PipelineKey> {
  return { key, label: LABELS[key], value: undefined, sub: "", error: true };
}

const BUILDERS: Record<PipelineKey, (report: PipelineReport) => Metric<PipelineKey>> = {
  postgate: (r) => (isBlockError(r.postgate) ? failed("postgate") : postgateCard(r.postgate, r)),
  landing: (r) => (isBlockError(r.landing) ? failed("landing") : landingCard(r.landing)),
  gate: (r) => (isBlockError(r.gate) ? failed("gate") : gateCard(r.gate, r.days)),
  jobs: (r) => (isBlockError(r.jobs) ? failed("jobs") : jobsCard(r.jobs, r.days)),
  autopilot: (r) => (isBlockError(r.autopilot) ? failed("autopilot") : autopilotCard(r.autopilot)),
  pressure: (r) =>
    r.pressure === null || isBlockError(r.pressure) ? failed("pressure") : pressureCard(r.pressure, r.days),
};

/** The row's cards, in order. */
export function pipelineMetrics(report: PipelineReport): Metric<PipelineKey>[] {
  return ORDER.map((key) => BUILDERS[key](report));
}

/** The row while the report is still being read: every card present, none with a number. */
export function pendingMetrics(): Metric<PipelineKey>[] {
  return ORDER.map((key) => ({ key, label: LABELS[key], value: undefined, sub: "reading…" }));
}
