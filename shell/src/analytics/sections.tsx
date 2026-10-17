import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import type { BarDatum } from "../ui/BarChart";
import type { AnalyticsReport, QuotaHistory } from "../data/analytics";
import { compactTokens, humanMinutes } from "../data/project-readings";
import { BarChart, ErrorNote, Section, StatCard, StepLine, usd } from "../ui";
import {
  DELIVERED_KEYS,
  GATE_KEYS,
  TOKEN_KEYS,
  costBars,
  costKeys,
  deliveredBars,
  gateBars,
  passRate,
  quotaSeries,
  tokenBars,
  totalTokens,
  versus,
} from "./series";

/**
 * The view's sections, one component each (spec §12): a later phase adds a section to the list in
 * `AnalyticsView`, not a page. Every section takes the whole report and reads only its block.
 */

export interface SectionProps {
  report: AnalyticsReport;
}

const EMPTY = "No runs in this window";
const count = (value: number) => String(Math.round(value));
const percent = (fraction: number) => `${Math.round(fraction * 100)}%`;

/**
 * One sentence for a screen reader: the charts are `role="img"`, so their per-segment titles do not
 * reach it. Total and the busiest day; nothing to say when the window is empty.
 */
function summary(data: BarDatum[], format: (value: number) => string): ReactNode {
  const days = data.map((datum) => ({ day: datum.day, total: datum.parts.reduce((sum, part) => sum + part.value, 0) }));
  const total = days.reduce((sum, entry) => sum + entry.total, 0);
  if (total <= 0) return null;
  const peak = days.reduce((best, entry) => (entry.total > best.total ? entry : best), days[0]);
  return <p className="sr-only">{`Total ${format(total)}; peak ${format(peak.total)} on ${peak.day}.`}</p>;
}

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** The clauses of a tile's detail line, the empty ones dropped. */
function joined(parts: string[]): string {
  return parts.filter((part) => part !== "").join(" · ");
}

export function CostSection({ report }: SectionProps) {
  const { cost, tokens, days } = report;
  const tokensNow = tokens.total.measured_sessions === 0 ? null : totalTokens(tokens.total);
  const tokensBefore = tokens.previous.measured_sessions === 0 ? null : totalTokens(tokens.previous);
  const ratio = tokens.total.cache_hit_ratio;
  return (
    <Section label="Cost and tokens">
      <div className="an-tiles">
        <StatCard
          label="Cost"
          value={usd(cost.total.measured_usd)}
          detail={joined([
            plural(cost.total.runs, "run", "runs"),
            cost.total.judge_usd > 0 ? `${usd(cost.total.judge_usd)} judge, counted apart` : "",
            versus(cost.total.measured_usd, cost.previous.measured_usd, days),
          ])}
        />
        {cost.total.estimated_runs > 0 ? (
          <StatCard
            label="Estimated cost"
            value={usd(cost.total.estimated_usd)}
            detail={joined([
              `${plural(cost.total.estimated_runs, "run", "runs")} with no reported cost, estimated from run time`,
              versus(cost.total.estimated_usd, cost.previous.estimated_usd, days),
            ])}
          />
        ) : null}
        <StatCard
          label="Tokens"
          value={tokensNow === null ? undefined : compactTokens(tokensNow)}
          detail={
            tokensNow === null
              ? "no session reported usage"
              : joined([plural(tokens.total.measured_sessions, "session", "sessions"), versus(tokensNow, tokensBefore, days)])
          }
        />
        <StatCard
          label="Cache hit ratio"
          value={ratio === null ? undefined : percent(ratio)}
          detail={ratio === null ? "nothing measured" : versus(ratio, tokens.previous.cache_hit_ratio, days)}
        />
      </div>
      {tokens.total.unmeasured_runs > 0 ? (
        <p className="an-note">{`${plural(tokens.total.unmeasured_runs, "run", "runs")} not measured`}</p>
      ) : null}
      <div className="an-charts">
        {summary(costBars(cost), usd)}
        <BarChart
          data={costBars(cost)}
          keys={costKeys(cost)}
          label={`Cost per day by ${cost.by_label}, last ${days} days`}
          format={usd}
          empty={EMPTY}
        />
        {summary(tokenBars(tokens), compactTokens)}
        <BarChart
          data={tokenBars(tokens)}
          keys={TOKEN_KEYS}
          label={`Tokens per day by kind, last ${days} days`}
          format={compactTokens}
          empty={EMPTY}
        />
      </div>
    </Section>
  );
}

export function ThroughputSection({ report }: SectionProps) {
  const { throughput, days } = report;
  const { total } = throughput;
  const rate = passRate(total);
  const judged = total.passed + total.failed + total.errored;
  return (
    <Section label="Throughput and gate">
      <div className="an-tiles">
        <StatCard
          label="Runs completed"
          value={total.runs_completed}
          detail={joined([
            versus(total.runs_completed, throughput.previous.runs_completed, days),
            throughput.in_flight > 0 ? `${throughput.in_flight} still running` : "",
          ])}
        />
        <StatCard
          label="Pass rate"
          value={rate === null ? undefined : percent(rate)}
          detail={rate === null ? "nothing judged" : `of ${judged} judged`}
        />
        <StatCard
          label="Failed or could not run"
          value={total.failed + total.errored}
          detail={
            <span className={total.failed > 0 ? "ui-wrong" : undefined}>
              {`${total.failed} failed · ${total.errored} could not run`}
            </span>
          }
        />
      </div>
      {summary(gateBars(throughput), count)}
      <BarChart
        data={gateBars(throughput)}
        keys={GATE_KEYS}
        label={`Runs per day by gate result, last ${days} days`}
        format={count}
        empty={EMPTY}
      />
      {throughput.by_mode.length === 0 ? null : (
        <ul className="an-pairs" aria-label="Runs by mode">
          {throughput.by_mode.map((row) => (
            <li key={row.mode}>
              <span className="an-pair-mode">{row.mode}</span>
              <span className="an-pair-count">{row.runs_completed}</span>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

export function DeliveredSection({ report }: SectionProps) {
  const { delivered, days } = report;
  const median = delivered.total.median_minutes;
  return (
    <Section label="Delivered">
      <div className="an-tiles">
        <StatCard
          label="Delivered"
          value={delivered.total.landed}
          detail={versus(delivered.total.landed, delivered.previous.landed, days)}
        />
        <StatCard
          label="Run to merge"
          value={median === null ? undefined : humanMinutes(Math.round(median))}
          detail={
            median === null
              ? "no merge with an associated run"
              : `median, ${delivered.total.timed} with a measured time`
          }
        />
      </div>
      {summary(deliveredBars(delivered), count)}
      <BarChart
        data={deliveredBars(delivered)}
        keys={DELIVERED_KEYS}
        label={`Merges per day, last ${days} days`}
        format={count}
        empty="Nothing landed in this window"
      />
    </Section>
  );
}

export function TopRunsSection({ report }: SectionProps) {
  const global = report.scope === "global";
  return (
    <Section label="Most expensive sessions">
      {report.top_runs.length === 0 ? (
        <p className="an-note">No run finished in this window.</p>
      ) : (
        <ol className="an-top">
          {report.top_runs.map((run) => (
            <li key={run.run_id}>
              {/* `run_id` is the session's last run, an integer: `/runs/$runId` as the rest of the shell links it. */}
              <Link to={`/runs/${run.run_id}`} className="an-top-link">
                {global ? <span>{run.project_id ?? "No project"}</span> : null}
                <span className="an-top-meta">{run.mode}</span>
                <span className="an-top-meta">{run.completed_at.slice(0, 10)}</span>
                <span className="an-top-cost">
                  {usd(run.cost_usd)}
                  {run.cost_is_estimated ? " (estimated)" : ""}
                </span>
              </Link>
            </li>
          ))}
        </ol>
      )}
    </Section>
  );
}

export interface QuotaSectionProps extends SectionProps {
  history: QuotaHistory | undefined;
  /** The quota read failed with nothing cached. */
  failed: boolean;
}

/** Global only (§9.8). The time axis is the report's window, so quota lines up with the bars above. */
export function QuotaSection({ report, history, failed }: QuotaSectionProps) {
  return (
    <Section label="Quota">
      <QuotaBody report={report} history={history} failed={failed} />
    </Section>
  );
}

function QuotaBody({ report, history, failed }: QuotaSectionProps) {
  if (history === undefined) {
    return failed ? (
      <ErrorNote>The núcleo did not answer for the quota history.</ErrorNote>
    ) : (
      <p className="an-note">reading…</p>
    );
  }
  const since = history.since === null ? null : history.since.slice(0, 10);
  return (
    <>
      <p className="an-note">
        {since === null
          ? "Nothing recorded yet: the history starts with the first quota reading taken while the app is open."
          : `Recorded since ${since}, only while the app is open. Each reading holds until the next.`}
      </p>
      <StepLine
        series={quotaSeries(history)}
        from={Date.parse(`${report.window.from}T00:00:00Z`)}
        to={Date.parse(report.now)}
        label={`Quota used, 5h and 7d windows, last ${report.days} days`}
        format={percent}
        empty={since === null ? "No quota history yet" : `History started on ${since}; nothing recorded in this window`}
      />
    </>
  );
}
