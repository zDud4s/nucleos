import { useState } from "react";
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
import { ErrorNote, StaleNote, StatCard } from "../ui";
import { CostSection, DeliveredSection, QuotaSection, ThroughputSection, TopRunsSection } from "./sections";
import "./analytics.css";

/**
 * Trends, for the whole machine or one project (spec analytics-fundacao §9).
 *
 * One component in two places: the `/analytics` page (global, window in `?days=`) and a project's
 * Analytics mode (`projectId` set, window in local state). Global stacks cost by project and adds
 * quota; a project stacks by mode and has no quota (§9.8).
 *
 * The window is controlled when `days` and `onDaysChange` are passed (the page) and local otherwise
 * (the Workspace mode).
 */

export interface AnalyticsViewProps {
  projectId?: string;
  days?: AnalyticsDays;
  onDaysChange?: (days: AnalyticsDays) => void;
}

/** §9.6, fixed at the top. The totals here and the autonomy ceiling's are built differently, on purpose. */
const COVERAGE =
  "Every run counts here, in every mode. The global autonomy ceiling counts only some modes, so these totals can be higher than its. Run cost is dated by when a run finished; the ceiling dates it by when it started. The judge's cost is included as its own part, dated by its verdict.";

/** Local wall-clock, 24-hour — the same reading `ModeState`'s readings line gives. */
function clockOf(at: number): string {
  return new Date(at).toTimeString().slice(0, 5);
}

export function AnalyticsView({ projectId, days: asked, onDaysChange }: AnalyticsViewProps) {
  const [local, setLocal] = useState<AnalyticsDays>(DEFAULT_DAYS);
  const days = asked ?? local;
  const choose = onDaysChange ?? setLocal;
  const global = projectId === undefined;
  const report = useAnalytics(projectId, days);
  const quota = useQuotaHistory(days, global);
  const data = report.data;

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
        <p className="an-head-note">
          {data === undefined
            ? "days in UTC"
            : `${data.window.from} to ${data.window.to}, days in UTC · read at ${clockOf(report.dataUpdatedAt)}`}
        </p>
      </div>
      {data === undefined || data.counted.modes === "all" ? <p className="an-coverage">{COVERAGE}</p> : null}
      <Readout
        data={data}
        failed={report.isError}
        missing={isApiRefusal(report.error) && report.error.status === 404}
        updatedAt={report.dataUpdatedAt}
        global={global}
        quota={quota.data}
        quotaFailed={quota.isError}
      />
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
}

/** Absent, refused, stale and read are four different answers (§9.7), and never render as one. */
function Readout({ data, failed, missing, updatedAt, global, quota, quotaFailed }: ReadoutProps) {
  if (data === undefined) {
    if (missing) return <Absent label="Analytics" why="No project by that name." />;
    if (failed) return <ErrorNote>The núcleo did not answer for analytics.</ErrorNote>;
    return <Absent label="Analytics" why="reading…" />;
  }
  return (
    <>
      {failed ? <StaleNote dataUpdatedAt={updatedAt} /> : null}
      <CostSection report={data} />
      <ThroughputSection report={data} />
      <DeliveredSection report={data} />
      <TopRunsSection report={data} />
      {global ? <QuotaSection report={data} history={quota} failed={quotaFailed} /> : null}
    </>
  );
}
