import { useId, useState, type ReactNode } from "react";
import type { AnalyticsDays } from "../data/analytics";
import {
  useDevtime,
  useDevtimeFindings,
  useMarkFinding,
  useRefreshPrices,
  type DevtimeFinding,
  type DevtimeOverview,
  type ModelTokens,
  type Verdict,
} from "../data/devtime";
import { compactTokens, humanMinutes } from "../data/project-readings";
import { AreaChart, Badge, Button, ErrorNote, ShareBar, Skeleton, StaleNote, usd } from "../ui";
import {
  COST_KEYS,
  KIND_KEYS,
  TURN_KEYS,
  WASTE_KEYS,
  activeByKind,
  activeByWaste,
  costSeries,
  duration,
  leverShares,
  precisionText,
  qualityLine,
  ruleShares,
  turnBars,
  versusPoints,
  wastedByClass,
} from "./devtime-series";
import { versus, windowShares } from "./series";
import { Card, MetricCard, joined, plural, summary, unreachable, type Metric } from "./sections";

/**
 * Development time (spec analytics-devtime §7): the owner's own sessions, under F1's readout, with the
 * same window and project. Its own query, so its failure never takes the run analytics down with it.
 */

export interface DevtimeSectionProps {
  projectId?: string;
  days: AnalyticsDays;
  /** The section's id, for the page's section links. */
  anchor?: string;
}

type DevtimeMetric = "active" | "waste" | "interruptions" | "cost";
const DETAIL_HEIGHT = 220;
const EMPTY = "No sessions in this window";
const minutes = (ms: number) => duration(ms, humanMinutes);
const percent = (fraction: number) => `${Math.round(fraction * 100)}%`;
const count = (value: number) => String(Math.round(value));
const safeId = (id: string) => id.replace(/[^a-zA-Z0-9_-]/g, "");

export function DevtimeSection({ projectId, days, anchor }: DevtimeSectionProps) {
  const report = useDevtime(projectId, days);
  const [open, setOpen] = useState<DevtimeMetric>("active");
  return (
    <Card id={anchor} label="Development time" className="an-dt">
      <Body report={report} projectId={projectId} days={days} open={open} onOpen={setOpen} />
    </Card>
  );
}

interface BodyProps {
  report: ReturnType<typeof useDevtime>;
  projectId?: string;
  days: AnalyticsDays;
  open: DevtimeMetric;
  onOpen: (metric: DevtimeMetric) => void;
}

function Body({ report, projectId, days, open, onOpen }: BodyProps) {
  const panelId = `an-dt-detail-${safeId(useId())}`;
  const data = report.data;
  if (data === undefined) {
    if (report.isError) {
      return (
        <ErrorNote onRetry={() => void report.refetch()} retryLabel="Try again">
          {unreachable("development time")}
        </ErrorNote>
      );
    }
    return <Skeleton label="Loading development time" lines={8} />;
  }
  const empty = data.totals.active_ms === 0 && data.totals.turns === 0 && data.tokens.by_model.length === 0;
  return (
    <>
      {report.isError ? <StaleNote dataUpdatedAt={report.dataUpdatedAt} /> : null}
      <QualityStrip report={data} />
      {empty ? (
        <p className="an-note">No sessions read in this window yet</p>
      ) : (
        <>
          <div className="an-metrics">
            {devtimeMetrics(data).map((metric) => (
              <MetricCard key={metric.key} metric={metric} open={metric.key === open} controls={panelId}
                          onOpen={() => onOpen(metric.key)} />
            ))}
          </div>
          <DevtimePanel id={panelId} report={data} metric={open} />
        </>
      )}
      {/* Outside the empty branch: it costs nothing until opened. */}
      <FindingsSection projectId={projectId} days={days} />
    </>
  );
}

const WHY_UNMAPPED = "Sessions whose folder maps to no registered project are not stored, so they are in no day and no project here. Counted in the last ingest cycle only.";
const WHY_DAEMON = "Sessions the daemon itself ran are runs, already in the run analytics above; they are skipped here. Counted in the last ingest cycle only.";

function QualityStrip({ report }: { report: DevtimeOverview }) {
  const { clauses, amber } = qualityLine(report.quality, Date.now());
  return (
    <p className={amber ? "an-dt-quality an-dt-quality-amber" : "an-dt-quality"}>
      {clauses.map((clause, index) => (
        <span key={clause} title={clause.includes("without a project") ? WHY_UNMAPPED : clause.includes("daemon") ? WHY_DAEMON : undefined}>
          {index > 0 ? " · " : ""}
          {clause}
        </span>
      ))}
    </p>
  );
}

/** Why the cost can read as a range: messages read before the cache split was recorded. */
const WHY_RANGE = "Older usage has no cache-duration split: the estimate prices its cache writes as 1-hour, the low end as 5-minute.";

/** The bounds of an estimate, when the unknown cache split moves it by more than 1%; else null. */
function costRange(low: number | null, high: number | null): [string, string] | null {
  if (low === null || high === null || high - low <= 0.01 * high) return null;
  return [usd(low), usd(high)];
}

function devtimeMetrics(report: DevtimeOverview): Metric<DevtimeMetric>[] {
  const { totals, tokens, days } = report;
  const series = report.days_series;
  const cost = costSeries(series);
  const range = costRange(tokens.cost_low, tokens.cost_high);
  return [
    {
      key: "active", label: "Active time", value: minutes(totals.active_ms),
      sub: versus(totals.active_ms, totals.previous.active_ms, days),
      spark: { data: activeByWaste(series), keys: WASTE_KEYS, tone: "ui-chart-useful", format: minutes },
    },
    {
      key: "waste", label: "Waste",
      value: totals.waste_ratio === null ? undefined : `${minutes(totals.wasted_ms)} · ${percent(totals.waste_ratio)}`,
      sub: versusPoints(totals.waste_ratio, totals.previous.waste_ratio, days),
      spark: { data: wastedByClass(series), keys: WASTE_KEYS.slice(1), tone: "ui-chart-rework", format: minutes },
    },
    {
      key: "interruptions", label: "Interruptions",
      value: totals.interruption_rate === null ? undefined : `${percent(totals.interruption_rate)} of turns`,
      sub: versusPoints(totals.interruption_rate, totals.previous.interruption_rate, days),
      spark: { data: turnBars(series), keys: TURN_KEYS, tone: "ui-chart-avoidable", format: count },
    },
    {
      key: "cost", label: "Estimated cost",
      value: tokens.cost_usd === null ? undefined : (
        <>
          {usd(tokens.cost_usd)}{" "}
          {range === null ? null : <><span title={WHY_RANGE}>{`${range[0]}–${range[1]}`}</span>{" "}</>}
          <Badge tone="info">estimated</Badge>
        </>
      ),
      sub: joined([
        versus(tokens.cost_usd, tokens.previous.cost_usd, days),
        tokens.unpriced_models.length > 0 ? `excludes ${tokens.unpriced_models.length} unpriced` : "",
      ]),
      // With no priced day there is no series: draw nothing rather than a zero line.
      spark: cost.length === 0 ? undefined : { data: cost, keys: COST_KEYS, tone: "ui-chart-cost", format: usd },
    },
  ];
}

/* --------------------------------------------------------- the detail panel -- */

function ActivePanel({ report }: { report: DevtimeOverview }) {
  const [by, setBy] = useState<"waste" | "kind">("waste");
  const data = by === "waste" ? activeByWaste(report.days_series) : activeByKind(report.days_series);
  const keys = by === "waste" ? WASTE_KEYS : KIND_KEYS;
  return (
    <>
      <div className="ui-switch" role="group" aria-label="Stack by">
        {(["waste", "kind"] as const).map((option) => (
          <button key={option} type="button" className="ui-switch-seg" aria-pressed={option === by} onClick={() => setBy(option)}>
            {option === "waste" ? "By waste" : "By kind"}
          </button>
        ))}
      </div>
      <AreaChart data={data} keys={keys} label={`Active time per day by ${by}, last ${report.days} days`}
                 format={minutes} empty={EMPTY} tone="ui-chart-useful" height={DETAIL_HEIGHT} />
      {summary(data, minutes)}
      <ShareBar items={windowShares(data, keys)} format={minutes} label={`Active time by ${by}, last ${report.days} days`} />
    </>
  );
}

function WastePanel({ report }: { report: DevtimeOverview }) {
  const data = wastedByClass(report.days_series);
  return (
    <>
      <AreaChart data={data} keys={WASTE_KEYS.slice(1)} label={`Wasted time per day, last ${report.days} days`}
                 format={minutes} empty={EMPTY} tone="ui-chart-rework" height={DETAIL_HEIGHT} />
      {summary(data, minutes)}
      <p className="an-dt-share-title">By rule</p>
      <ShareBar items={ruleShares(report.waste_by_rule)} format={minutes} label="Waste by rule" />
      <p className="an-dt-share-title">By lever</p>
      <ShareBar items={leverShares(report.waste_by_lever)} format={minutes} label="Waste by lever" />
    </>
  );
}

function InterruptionsPanel({ report }: { report: DevtimeOverview }) {
  const data = turnBars(report.days_series);
  return (
    <>
      <AreaChart data={data} keys={TURN_KEYS} label={`Interrupted and corrected turns per day, last ${report.days} days`}
                 format={count} empty={EMPTY} tone="ui-chart-avoidable" height={DETAIL_HEIGHT} />
      {summary(data, count)}
    </>
  );
}

function TokensPanel({ report }: { report: DevtimeOverview }) {
  const { tokens } = report;
  const refresh = useRefreshPrices();
  const range = costRange(tokens.cost_low, tokens.cost_high);
  const sum = (row: ModelTokens) => row.input + row.cache_read + row.cache_creation + row.output;
  const hit = (row: ModelTokens) => {
    const base = row.input + row.cache_read + row.cache_creation;
    return base === 0 ? "—" : percent(row.cache_read / base);
  };
  return (
    <>
      <table className="an-dt-tokens" aria-label="Tokens by model">
        <thead><tr><th>Model</th><th>Effort</th><th>Tokens</th><th>Cache hit</th><th>Cost</th></tr></thead>
        <tbody>
          {tokens.by_model.map((row) => (
            <tr key={`${row.model}:${row.effort}`}>
              <th scope="row">{row.model === "unknown" ? "Unknown" : row.model}</th>
              <td>{row.effort === "unknown" ? "—" : row.effort}</td>
              <td>{compactTokens(sum(row))}</td>
              <td>{hit(row)}</td>
              <td>{row.cost_usd === null ? "no price" : usd(row.cost_usd)}</td>
            </tr>
          ))}
        </tbody>
        <tfoot>
          <tr>
            <th scope="row">Total</th>
            <td />
            <td>{compactTokens(tokens.by_model.reduce((total, row) => total + sum(row), 0))}</td>
            <td>{tokens.cache_hit_ratio === null ? "—" : percent(tokens.cache_hit_ratio)}</td>
            <td>{tokens.cost_usd === null ? "—" : usd(tokens.cost_usd)}</td>
          </tr>
        </tfoot>
      </table>
      {tokens.unpriced_models.length > 0 ? (
        <p className="an-note">{`Cost does not include ${plural(tokens.unpriced_models.length, "model", "models")} without a price.`}</p>
      ) : null}
      {range !== null ? (
        <p className="an-note">{`Between ${range[0]} and ${range[1]}: older usage has no cache-duration split, so its cache writes are priced as 1-hour (the high end) and as 5-minute at the low end.`}</p>
      ) : null}
      {tokens.dropped_prices.length > 0 ? (
        <p className="an-note">{`Unreadable prices ignored in nucleos-models.yaml: ${tokens.dropped_prices.join("; ")}.`}</p>
      ) : null}
      <p className="an-note">
        {tokens.prices_as_of === null
          ? "No prices yet."
          : `Estimated, prices of ${tokens.prices_as_of}. Never compared with an invoice.`}{" "}
        <Button variant="quiet" disabled={refresh.isPending} onClick={() => refresh.mutate()}>
          {refresh.isPending ? "Refreshing…" : "Refresh prices"}
        </Button>
      </p>
      {tokens.price_refresh?.ok === false ? (
        <p className="an-note">{`Last price refresh failed: ${tokens.price_refresh.problems.join("; ")}`}</p>
      ) : null}
      {refresh.isError ? <ErrorNote>The price refresh could not be started.</ErrorNote> : null}
    </>
  );
}

interface PanelContent {
  title: string;
  aside?: (r: DevtimeOverview) => string;
  body: (r: DevtimeOverview) => ReactNode;
}

const PANELS: Record<DevtimeMetric, PanelContent> = {
  active: {
    title: "Active time per day",
    aside: ({ totals }) =>
      joined([`inferred waste: ${minutes(totals.inferred_waste_ms)}`, `wait for you: ${minutes(totals.wait_human_ms)}`]),
    body: (r) => <ActivePanel report={r} />,
  },
  waste: { title: "Waste per day", body: (r) => <WastePanel report={r} /> },
  interruptions: {
    title: "Turns per day",
    // Both shown (spec §6.4): a rate over few classified turns is not a rate over all of them.
    aside: ({ totals }) =>
      totals.correction_rate === null || totals.correction_coverage === null
        ? ""
        : `correction rate ${percent(totals.correction_rate)} over ${percent(totals.correction_coverage)} of turns`,
    body: (r) => <InterruptionsPanel report={r} />,
  },
  cost: { title: "Tokens and estimated cost", body: (r) => <TokensPanel report={r} /> },
};

function DevtimePanel({ id, report, metric }: { id: string; report: DevtimeOverview; metric: DevtimeMetric }) {
  const { title, aside, body } = PANELS[metric];
  return <Card id={id} label={title} aside={aside?.(report)} className="an-detail">{body(report)}</Card>;
}

/* ---------------------------------------------------------------- findings -- */

function FindingsSection({ projectId, days }: { projectId?: string; days: AnalyticsDays }) {
  const [opened, setOpened] = useState(false);
  const [cursor, setCursor] = useState<string | undefined>(undefined);
  const findings = useDevtimeFindings(projectId, days, { order: "cost", cursor }, opened);
  return (
    <details className="an-about an-dt-findings" onToggle={(event) => setOpened(event.currentTarget.open)}>
      <summary className="an-about-toggle">Findings</summary>
      {findings.data === undefined ? (
        findings.isError ? (
          <ErrorNote onRetry={() => void findings.refetch()}>{unreachable("findings")}</ErrorNote>
        ) : opened ? (
          <Skeleton label="Loading findings" lines={3} />
        ) : null
      ) : (
        <>
          {findings.data.thresholds_source !== "file" ? (
            <p className="an-note">
              {findings.data.thresholds_source === "last_good" ? "Using last valid devtime.yaml" : "Using default thresholds"}
            </p>
          ) : null}
          <ul className="an-dt-finding-list">
            {findings.data.items.map((item) => (
              <FindingRow key={item.finding_key} item={item} days={days} inProject={projectId !== undefined} />
            ))}
          </ul>
          {findings.data.items.length === 0 ? <p className="an-note">No findings in this window.</p> : null}
          {findings.data.next_cursor !== null ? (
            <button type="button" className="ui-btn" onClick={() => setCursor(findings.data?.next_cursor ?? undefined)}>
              Next page
            </button>
          ) : null}
        </>
      )}
    </details>
  );
}

/** Optimistic (spec §7.6): the local verdict flips at once, and reverts with an error if the POST fails. */
function FindingRow({ item, days, inProject }: { item: DevtimeFinding; days: AnalyticsDays; inProject: boolean }) {
  const mark = useMarkFinding(days);
  const [shown, setShown] = useState<Verdict | null>(item.feedback?.verdict ?? null);
  const [rule, setRule] = useState(item.rule);
  const [failed, setFailed] = useState(false);
  const titleId = `an-dt-f-${safeId(useId())}`;
  const choose = (verdict: Verdict) => {
    const before = shown;
    setShown(verdict);
    setFailed(false);
    mark.mutate(
      { findingKey: item.finding_key, verdict, cause: null },
      {
        onSuccess: (out) => { setShown(out.feedback.verdict); setRule(out.rule); },
        onError: () => { setShown(before); setFailed(true); },
      },
    );
  };
  return (
    <li className="an-dt-finding" aria-labelledby={titleId}>
      <span id={titleId} className="an-dt-finding-rule">{item.rule_id}</span>
      <span>{item.day}</span>
      <span>{item.lever}</span>
      <span>{item.confidence}</span>
      <span title="A sort weight, not minutes lost">{`weight ${Math.round(item.cost_ms / 1000)}`}</span>
      {rule === null ? null : <span>{precisionText(rule, inProject)}</span>}
      {rule?.low_precision ? <Badge tone="paused">unreliable rule</Badge> : null}
      <span className="ui-switch" role="group" aria-label="Verdict">
        <button type="button" className="ui-switch-seg" aria-pressed={shown === "confirmed"} onClick={() => choose("confirmed")}>Confirm</button>
        <button type="button" className="ui-switch-seg" aria-pressed={shown === "not_rework"} onClick={() => choose("not_rework")}>Not rework</button>
      </span>
      {failed ? <ErrorNote>The mark was not saved.</ErrorNote> : null}
    </li>
  );
}
