import { useId, useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  DEFAULT_DAYS,
  WINDOWS,
  useAnalytics,
  useQuotaHistory,
  type AnalyticsDays,
  type AnalyticsReport,
  type QuotaHistory,
} from "../data/analytics";
import { ErrorNote, SectionNav, Skeleton, StaleNote, StatCard } from "../ui";
import { DetailPanel, MetricCard, QuotaSection, TopRunsSection, metrics, unreachable, type MetricKey } from "./sections";
import { PipelineSection } from "./PipelineSection";
import { DevtimeSection } from "./DevtimeSection";
import "./analytics.css";

/**
 * Trends, for the whole machine or one project (spec analytics-fundacao §9).
 *
 * One component in two places: the `/analytics` page (global, window in `?days=`) and a project's
 * Analytics mode (`projectId` set, window in local state). Global breaks cost down by project and
 * adds quota; a project breaks it down by mode and has no quota (§9.8).
 *
 * The window is controlled when `days` and `onDaysChange` are passed (the page) and local otherwise
 * (the Workspace mode).
 *
 * Numbers first: four metric cards, each a button that opens its chart in the one detail panel
 * under them (spend by default, and always exactly one), then the costliest sessions and quota.
 */

export interface AnalyticsViewProps {
  projectId?: string;
  days?: AnalyticsDays;
  onDaysChange?: (days: AnalyticsDays) => void;
}

/**
 * §9.6. The totals here and the autonomy ceiling's are built differently, on purpose. Behind the
 * "About these numbers" disclosure beside the range, rather than a paragraph over the numbers.
 */
const COVERAGE =
  "Every run counts here, in every mode. The global autonomy ceiling counts only some modes, so these totals can be higher than its. Run cost is dated by when a run finished; the ceiling dates it by when it started. The judge's cost is included as its own part, dated by its verdict.";

/** Local wall-clock, 24-hour — the same reading `ModeState`'s readings line gives. */
function clockOf(at: number): string {
  return new Date(at).toTimeString().slice(0, 5);
}

export function AnalyticsView({ projectId, days: asked, onDaysChange }: AnalyticsViewProps) {
  const [local, setLocal] = useState<AnalyticsDays>(DEFAULT_DAYS);
  // Kept across a window change and a refetch: switching 30 to 7 days keeps the chart being read.
  const [open, setOpen] = useState<MetricKey>("cost");
  const days = asked ?? local;
  const choose = onDaysChange ?? setLocal;
  const global = projectId === undefined;
  const report = useAnalytics(projectId, days);
  const quota = useQuotaHistory(days, global);
  const data = report.data;
  const base = `an-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const runsId = `${base}-runs`;
  const pipelineId = `${base}-pipeline`;
  const devtimeId = `${base}-devtime`;

  return (
    <div className="an-view">
      <div className="an-head">
        <div className="ui-switch" role="group" aria-label="Window">
          {WINDOWS.map((option) => (
            <button
              key={option}
              type="button"
              className="ui-switch-seg"
              aria-pressed={option === days}
              onClick={() => {
                if (option !== days) choose(option);
              }}
            >
              {option} days
            </button>
          ))}
        </div>
        <div className="an-range">
          <p className="an-head-note">
            {data === undefined
              ? "days in UTC"
              : `${data.window.from} to ${data.window.to}, days in UTC · read at ${clockOf(report.dataUpdatedAt)}`}
          </p>
          {data === undefined || data.counted.modes === "all" ? (
            <details className="an-about">
              <summary className="an-about-toggle">About these numbers</summary>
              <p className="an-about-text">{COVERAGE}</p>
            </details>
          ) : null}
        </div>
      </div>
      <SectionNav
        label="Sections"
        items={[
          { id: runsId, label: "Summary" },
          { id: pipelineId, label: "Pipeline" },
          { id: devtimeId, label: "Development time" },
        ]}
      />
      <section id={runsId} className="an-runs" aria-labelledby={`${runsId}-title`}>
        <h2 id={`${runsId}-title`} className="an-runs-title">
          Summary
        </h2>
        <Readout
          data={data}
          failed={report.isError}
          missing={isApiRefusal(report.error) && report.error.status === 404}
          updatedAt={report.dataUpdatedAt}
          global={global}
          quota={quota.data}
          quotaFailed={quota.isError}
          quotaUpdatedAt={quota.dataUpdatedAt}
          open={open}
          onOpen={setOpen}
          onRetry={() => void report.refetch()}
          onRetryQuota={() => void quota.refetch()}
        />
      </section>
      {/* Its own query and its own failure: the pipeline never goes down with the run analytics. */}
      <PipelineSection projectId={projectId} days={days} anchor={pipelineId} />
      {/* Likewise development time. */}
      <DevtimeSection projectId={projectId} days={days} anchor={devtimeId} />
    </div>
  );
}

/** An em dash and a reason, never a zero — `ModeState`'s `Absent`, which that file does not export. */
function Absent({ label, why }: { label: string; why: string }) {
  return <StatCard label={label} value={undefined} detail={why} />;
}

interface ReadoutProps {
  data: AnalyticsReport | undefined;
  failed: boolean;
  missing: boolean;
  updatedAt: number;
  global: boolean;
  quota: QuotaHistory | undefined;
  quotaFailed: boolean;
  quotaUpdatedAt: number;
  open: MetricKey;
  onOpen: (metric: MetricKey) => void;
  onRetry: () => void;
  onRetryQuota: () => void;
}

/** Absent, refused, stale and read are four different answers (§9.7), and never render as one. */
function Readout({ data, failed, missing, updatedAt, global, quota, quotaFailed, quotaUpdatedAt, open, onOpen, onRetry, onRetryQuota }: ReadoutProps) {
  const panelId = `an-detail-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  if (data === undefined) {
    // A 404 only means "no such project" when a project was asked for; the global page has none to miss.
    if (missing && !global) return <Absent label="Analytics" why="No project by that name." />;
    if (failed || missing) return <ErrorNote onRetry={onRetry}>{unreachable("analytics")}</ErrorNote>;
    // Height reserved for the cards and the panel, so the page does not jump when they land.
    return <Skeleton label="Loading analytics" lines={14} />;
  }
  return (
    <>
      {failed ? <StaleNote dataUpdatedAt={updatedAt} /> : null}
      <div className="an-metrics">
        {metrics(data).map((metric) => (
          <MetricCard
            key={metric.key}
            metric={metric}
            open={metric.key === open}
            controls={panelId}
            // Clicking the open card keeps it open: there is no "none", so the page never loses its chart.
            onOpen={() => onOpen(metric.key)}
          />
        ))}
      </div>
      <DetailPanel id={panelId} report={data} metric={open} />
      <div className={global ? "an-lower an-lower-split" : "an-lower"}>
        <TopRunsSection report={data} />
        {global ? (
          <QuotaSection
            report={data}
            history={quota}
            failed={quotaFailed}
            staleSince={quota !== undefined && quotaFailed ? quotaUpdatedAt : undefined}
            onRetry={onRetryQuota}
          />
        ) : null}
      </div>
    </>
  );
}
