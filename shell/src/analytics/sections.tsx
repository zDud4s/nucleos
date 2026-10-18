import { useId, useState, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { TriangleAlert } from "lucide-react";
import type { AnalyticsReport, QuotaHistory } from "../data/analytics";
import { compactTokens, humanMinutes } from "../data/project-readings";
import { AreaChart, ErrorNote, ShareBar, StaleNote, StepLine, usd, type ChartDatum, type ChartKey } from "../ui";
import {
  DELIVERED_KEYS,
  ESTIMATED_KEY,
  GATE_KEYS,
  TOKEN_KEYS,
  costBars,
  costKeys,
  deliveredBars,
  gateBars,
  gateShares,
  passRate,
  quotaSeries,
  runningTotal,
  tokenBars,
  tokenShares,
  totalTokens,
  versus,
  windowShares,
} from "./series";

/**
 * The view's pieces (spec §12), "numbers first": four metric cards, the one detail chart the
 * selected card opens, and a lower row of the costliest sessions and, global only, quota. Every
 * piece takes the whole report and reads only its block, so a later phase adds a metric to
 * `metrics` and a panel to `PANELS`, not a page.
 */

export interface SectionProps {
  report: AnalyticsReport;
}

/** The four answers a card gives, in the order the row shows them. */
export type MetricKey = "cost" | "runs" | "delivered" | "tokens";

const EMPTY = "No runs in this window";
const DETAIL_HEIGHT = 220;
const count = (value: number) => String(Math.round(value));
const percent = (fraction: number) => `${Math.round(fraction * 100)}%`;

/**
 * One sentence for a screen reader: the charts are `role="img"`, and their tooltip only speaks to
 * whoever moves through it. Total and the busiest day; nothing to say when the window is empty. The
 * part named by `apart` (the estimate) is never folded into the total or the peak: it is stated on
 * its own.
 */
export function summary(data: ChartDatum[], format: (value: number) => string, apart?: string): ReactNode {
  const days = data.map((datum) => ({
    day: datum.day,
    total: datum.parts.reduce((sum, part) => (part.key === apart ? sum : sum + part.value), 0),
    apart: datum.parts.reduce((sum, part) => (part.key === apart ? sum + part.value : sum), 0),
  }));
  const total = days.reduce((sum, entry) => sum + entry.total, 0);
  const estimated = days.reduce((sum, entry) => sum + entry.apart, 0);
  if (total <= 0 && estimated <= 0) return null;
  const peak = days.reduce((best, entry) => (entry.total > best.total ? entry : best), days[0]);
  const head = estimated > 0 ? `Total ${format(total)} measured; ${format(estimated)} estimated from run time` : `Total ${format(total)}`;
  return <p className="sr-only">{`${head}; peak ${format(peak.total)} on ${peak.day}.`}</p>;
}

export function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** The clauses of a line, the empty ones dropped. */
export function joined(parts: string[]): string {
  return parts.filter((part) => part !== "").join(" · ");
}

/** An em dash for a figure nobody read — never a zero standing in for it (`StatCard`'s reading). */
export function Unread() {
  return (
    <>
      <span aria-hidden="true">—</span>
      <span className="sr-only">not read</span>
    </>
  );
}

/* --------------------------------------------------------------- the cards -- */

export interface Metric<K extends string = MetricKey> {
  key: K;
  label: string;
  /** `undefined` is absent, drawn as an em dash. */
  value: ReactNode | undefined;
  sub: string;
  /** Absent draws no sparkline (a metric with no series, e.g. devtime cost with no priced day). */
  spark?: { data: ChartDatum[]; keys: ChartKey[]; tone: string; apart?: string; format: (value: number) => string };
  /** The card is in a state that needs attention: drawn with the danger border, the words say why. */
  alert?: boolean;
  /** The block behind the card could not be computed: it says so and shows nothing else. */
  error?: boolean;
}

/** The tokens the window measured, or `null` when no session reported usage — absent, never 0. */
function measuredTokens(report: AnalyticsReport): number | null {
  return report.tokens.total.measured_sessions === 0 ? null : totalTokens(report.tokens.total);
}

/** The four cards' readings: a big number, one line about it, and that metric's days as a sparkline. */
export function metrics(report: AnalyticsReport): Metric[] {
  const { cost, throughput, delivered, tokens, days } = report;
  const rate = passRate(throughput.total);
  const median = delivered.total.median_minutes;
  const tokensNow = measuredTokens(report);
  const ratio = tokens.total.cache_hit_ratio;
  return [
    {
      key: "cost",
      label: "Spent",
      value: usd(cost.total.measured_usd),
      sub: joined([
        versus(cost.total.measured_usd, cost.previous.measured_usd, days),
        cost.total.estimated_runs > 0 ? `${usd(cost.total.estimated_usd)} estimated` : "",
      ]),
      spark: { data: costBars(cost), keys: costKeys(cost), tone: "ui-chart-cost", apart: ESTIMATED_KEY, format: usd },
    },
    {
      key: "runs",
      label: "Runs",
      value: throughput.total.runs_completed,
      sub: joined([
        rate === null ? "nothing judged" : `${percent(rate)} passed the gate`,
        throughput.in_flight > 0 ? `${throughput.in_flight} running` : "",
      ]),
      spark: { data: gateBars(throughput), keys: GATE_KEYS, tone: "ui-chart-runs", format: count },
    },
    {
      key: "delivered",
      label: "Delivered",
      value: delivered.total.landed,
      sub: median === null ? "no merge with an associated run" : `${humanMinutes(Math.round(median))} median to merge`,
      spark: { data: runningTotal(deliveredBars(delivered), "landed"), keys: DELIVERED_KEYS, tone: "ui-chart-delivered", format: count },
    },
    {
      key: "tokens",
      label: "Tokens",
      value: tokensNow === null ? undefined : compactTokens(tokensNow),
      sub:
        ratio !== null
          ? `${percent(ratio)} from cache`
          : tokensNow === null
            ? "no session reported usage"
            : plural(tokens.total.measured_sessions, "session", "sessions"),
      spark: { data: tokenBars(tokens), keys: TOKEN_KEYS, tone: "ui-chart-tokens", format: compactTokens },
    },
  ];
}

export interface MetricCardProps {
  metric: Metric<string>;
  open: boolean;
  /** The detail panel's id: every card controls the same one. */
  controls: string;
  onOpen: () => void;
}

/**
 * A card is a button that picks which chart the panel below shows. The button holds the words; the
 * sparkline sits beside it in the card (an svg in a `<figure>` is not a button's content), and the
 * button's `::after` is stretched over the whole card in CSS, so a click anywhere on it counts.
 */
export function MetricCard({ metric, open, controls, onOpen }: MetricCardProps) {
  const { spark } = metric;
  const failed = metric.error === true;
  const classes = ["an-metric", open ? "an-metric-open" : "", metric.alert === true ? "an-metric-alert" : ""];
  return (
    <div className={classes.filter((name) => name !== "").join(" ")}>
      <button type="button" className="an-metric-button" aria-expanded={open} aria-controls={controls} onClick={onOpen}>
        <span className="an-metric-top">
          <span className="an-metric-label">{metric.label}</span>
          <span className="an-metric-hint" aria-hidden="true">
            {open ? "showing" : "show chart"}
          </span>
        </span>
        <span className="an-metric-value">
          {metric.alert === true && !failed ? (
            // Text and icon, never colour alone: the icon is named after the word beside it.
            <TriangleAlert className="an-metric-icon" role="img" aria-hidden={false} aria-label={String(metric.value)} />
          ) : null}
          {failed ? "Could not compute" : metric.value === undefined ? <Unread /> : metric.value}
        </span>
        {failed ? null : <span className="an-metric-sub">{metric.sub}</span>}
      </button>
      {spark === undefined || failed ? null : (
        <AreaChart
          data={spark.data}
          keys={spark.keys}
          label={metric.label}
          format={spark.format}
          empty=""
          tone={spark.tone}
          apart={spark.apart}
          compact
        />
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ a card -- */

interface CardProps {
  label: string;
  aside?: string;
  id?: string;
  className?: string;
  children: ReactNode;
}

/** A titled card; its title is its region's name, so a screen reader can jump to it by that name. */
export function Card({ label, aside, id, className, children }: CardProps) {
  const titleId = `an-card-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  return (
    <section id={id} className={className === undefined ? "an-card" : `an-card ${className}`} aria-labelledby={titleId}>
      <div className="an-card-head">
        <h2 id={titleId} className="an-card-title">
          {label}
        </h2>
        {aside === undefined || aside === "" ? null : <p className="an-card-aside">{aside}</p>}
      </div>
      {children}
    </section>
  );
}

/* --------------------------------------------------------- the detail panel -- */

export interface PanelContent {
  title: string;
  aside: string;
  body: ReactNode;
}

/**
 * Spend per day, its breakdown by project or mode in the tooltip and the share bar (§9.3). The
 * measured total excludes both the estimate and the judge, so each is named beside it, never folded
 * in silently (§4.4, §4.9).
 */
function costPanel({ cost, days }: AnalyticsReport): PanelContent {
  const data = costBars(cost);
  const keys = costKeys(cost);
  return {
    title: "Spend per day",
    aside: joined([
      `${usd(cost.total.measured_usd)} in ${days} days`,
      cost.total.estimated_runs > 0 ? `${usd(cost.total.estimated_usd)} estimated, counted apart` : "",
      cost.total.judge_usd > 0 ? `${usd(cost.total.judge_usd)} judge` : "",
    ]),
    body: (
      <>
        <AreaChart
          data={data}
          keys={keys}
          label={`Spend per day by ${cost.by_label}, last ${days} days`}
          format={usd}
          empty={EMPTY}
          tone="ui-chart-cost"
          apart={ESTIMATED_KEY}
          height={DETAIL_HEIGHT}
        />
        {summary(data, usd, ESTIMATED_KEY)}
        <ShareBar items={windowShares(data, keys)} format={usd} label={`Spend by ${cost.by_label}, last ${days} days`} />
      </>
    ),
  };
}

/** Runs per day; the verdicts stay five, never merged (§4.6), in the share bar under it. */
function runsPanel({ throughput, days }: AnalyticsReport): PanelContent {
  const data = gateBars(throughput);
  return {
    title: "Runs per day",
    aside: joined([
      `${throughput.total.runs_completed} completed`,
      throughput.in_flight > 0 ? `${throughput.in_flight} still running` : "",
    ]),
    body: (
      <>
        <AreaChart
          data={data}
          keys={GATE_KEYS}
          label={`Runs per day by gate result, last ${days} days`}
          format={count}
          empty={EMPTY}
          tone="ui-chart-runs"
          height={DETAIL_HEIGHT}
        />
        {summary(data, count)}
        <ShareBar items={gateShares(throughput.total)} format={count} label={`Runs by gate result, last ${days} days`} />
        {throughput.by_mode.length === 0 ? null : (
          <ul className="an-pairs" aria-label="Runs by mode">
            {throughput.by_mode.map((row) => (
              <li key={row.mode} className="an-pair">
                <span className="an-pair-mode">{row.mode}</span>
                <span className="an-pair-count">{row.runs_completed}</span>
              </li>
            ))}
          </ul>
        )}
      </>
    ),
  };
}

/**
 * Delivered as a running total. Merges are rare, and a day-by-day chart of them is a row of 0s and
 * 1s; the running total says how the window went. The screen-reader sentence still reads the days
 * themselves: the window's total and its busiest day.
 */
function deliveredPanel({ delivered, days }: AnalyticsReport): PanelContent {
  const daily = deliveredBars(delivered);
  const median = delivered.total.median_minutes;
  return {
    title: "Delivered, running total",
    aside: `${delivered.total.landed} landed`,
    body: (
      <>
        <AreaChart
          data={runningTotal(daily, "landed")}
          keys={DELIVERED_KEYS}
          label={`Delivered, running total, last ${days} days`}
          format={count}
          empty="Nothing landed in this window"
          tone="ui-chart-delivered"
          height={DETAIL_HEIGHT}
        />
        {summary(daily, count)}
        <dl className="an-stats">
          <div className="an-stat">
            <dt>Median, run to merge</dt>
            <dd>{median === null ? <Unread /> : humanMinutes(Math.round(median))}</dd>
          </div>
          <div className="an-stat">
            <dt>With a measured time</dt>
            <dd>{delivered.total.timed}</dd>
          </div>
        </dl>
        {median === null ? <p className="an-note">No merge with an associated run.</p> : null}
      </>
    ),
  };
}

/** Tokens per day by kind; runs that reported nothing are named, never counted as zero tokens. */
function tokensPanel({ tokens, days }: AnalyticsReport): PanelContent {
  const data = tokenBars(tokens);
  const ratio = tokens.total.cache_hit_ratio;
  return {
    title: "Tokens per day",
    aside: joined([
      ratio === null ? "" : `${percent(ratio)} served from cache`,
      tokens.total.unmeasured_runs > 0 ? `${plural(tokens.total.unmeasured_runs, "run", "runs")} not measured` : "",
    ]),
    body: (
      <>
        <AreaChart
          data={data}
          keys={TOKEN_KEYS}
          label={`Tokens per day by kind, last ${days} days`}
          format={compactTokens}
          empty="No usage reported in this window"
          tone="ui-chart-tokens"
          height={DETAIL_HEIGHT}
        />
        {summary(data, compactTokens)}
        <ShareBar items={tokenShares(tokens.total)} format={compactTokens} label={`Tokens by kind, last ${days} days`} />
      </>
    ),
  };
}

const PANELS: Record<MetricKey, (report: AnalyticsReport) => PanelContent> = {
  cost: costPanel,
  runs: runsPanel,
  delivered: deliveredPanel,
  tokens: tokensPanel,
};

export interface DetailPanelProps extends SectionProps {
  metric: MetricKey;
  /** The id the cards' `aria-controls` name. */
  id: string;
}

/** The selected card's chart: exactly one is open, so the page never loses its chart. */
export function DetailPanel({ report, metric, id }: DetailPanelProps) {
  const { title, aside, body } = PANELS[metric](report);
  return (
    <Card id={id} label={title} aside={aside} className="an-detail">
      {body}
    </Card>
  );
}

/* --------------------------------------------------------------- lower row -- */

/** The sessions shown before `Show N more`: enough to rank, and a row about as tall as the quota card. */
const TOP_SHOWN = 5;

export function TopRunsSection({ report }: SectionProps) {
  const global = report.scope === "global";
  const listId = useId();
  const [all, setAll] = useState(false);
  const top = Math.max(0, ...report.top_runs.map((run) => run.cost_usd));
  const hidden = Math.max(0, report.top_runs.length - TOP_SHOWN);
  const runs = all ? report.top_runs : report.top_runs.slice(0, TOP_SHOWN);
  return (
    <Card label="Most expensive sessions">
      {report.top_runs.length === 0 ? (
        <p className="an-note">No run finished in this window.</p>
      ) : (
        <>
          <ol className="an-top" id={listId}>
            {runs.map((run) => {
              const day = run.completed_at.slice(0, 10);
              // A share of the costliest, as an svg attribute: a bar width in CSS would be inline style.
              const share = top > 0 ? Math.round((run.cost_usd / top) * 1000) / 10 : 0;
              return (
                <li key={run.run_id}>
                  {/* `run_id` is the session's last run, an integer: `/runs/$runId` as the rest of the shell links it. */}
                  <Link to={`/runs/${run.run_id}`} className="an-top-link">
                    <span className="an-top-name">{global ? (run.project_id ?? "No project") : run.mode}</span>
                    <span className="an-top-meta">{global ? `${run.mode} · ${day}` : day}</span>
                    <svg
                      className="an-top-bar ui-chart-cost"
                      viewBox="0 0 100 4"
                      preserveAspectRatio="none"
                      aria-hidden="true"
                      focusable="false"
                    >
                      <rect className="an-top-bar-fill" width={share} height={4} />
                    </svg>
                    <span className="an-top-cost">{usd(run.cost_usd)}</span>
                    {/* A track of its own, so the amount never moves: the tag is short to see, the words are whole to hear. */}
                    {run.cost_is_estimated ? (
                      <span className="an-top-est" title="Estimated from run time">
                        <span aria-hidden="true">est.</span>
                        <span className="sr-only"> (estimated)</span>
                      </span>
                    ) : null}
                  </Link>
                </li>
              );
            })}
          </ol>
          {hidden === 0 ? null : (
            <button
              type="button"
              className="ui-button ui-button-quiet an-top-more"
              aria-expanded={all}
              aria-controls={listId}
              onClick={() => setAll((open) => !open)}
            >
              {all ? "Show fewer" : `Show ${hidden} more`}
            </button>
          )}
        </>
      )}
    </Card>
  );
}

export interface QuotaSectionProps extends SectionProps {
  history: QuotaHistory | undefined;
  /** The quota read failed with nothing cached. */
  failed: boolean;
  /** When the quota read last succeeded; set only when a refetch failed over cached data. */
  staleSince?: number;
}

/**
 * Global only (§9.8). The time axis ends at the report's `now` and starts at the later of the
 * window's first day and the day of the first recorded reading: before that there is nothing to
 * draw, and a chart four-fifths empty says so less clearly than the note above it.
 * A step line, not an area: each reading holds until the next, and a curve between them would claim
 * values nobody read.
 */
export function QuotaSection({ report, history, failed, staleSince }: QuotaSectionProps) {
  return (
    <Card label="Quota">
      <QuotaBody report={report} history={history} failed={failed} staleSince={staleSince} />
    </Card>
  );
}

function QuotaBody({ report, history, failed, staleSince }: QuotaSectionProps) {
  if (history === undefined) {
    return failed ? (
      <ErrorNote>The núcleo did not answer for the quota history.</ErrorNote>
    ) : (
      <p className="an-note">reading…</p>
    );
  }
  const since = history.since === null ? null : history.since.slice(0, 10);
  const windowStart = Date.parse(`${report.window.from}T00:00:00Z`);
  const recordedFrom = since === null ? Number.NaN : Date.parse(`${since}T00:00:00Z`);
  const from = Number.isNaN(recordedFrom) ? windowStart : Math.max(windowStart, recordedFrom);
  return (
    <>
      {staleSince === undefined ? null : <StaleNote dataUpdatedAt={staleSince} />}
      <p className="an-note">
        {since === null
          ? "Nothing recorded yet: the history starts with the first quota reading taken while the app is open."
          : `Recorded since ${since}, only while the app is open. Each reading holds until the next.`}
      </p>
      <StepLine
        series={quotaSeries(history)}
        from={from}
        to={Date.parse(report.now)}
        label={`Quota used, 5h and 7d windows, last ${report.days} days`}
        format={percent}
        empty={since === null ? "No quota history yet" : `History started on ${since}; nothing recorded in this window`}
      />
    </>
  );
}
