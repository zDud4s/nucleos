import { useId, useState } from "react";
import { isApiRefusal } from "../data/client";
import type { AnalyticsDays } from "../data/analytics";
import { isBlockError, usePipelineAnalytics } from "../data/analytics-pipeline";
import { ErrorNote, StaleNote } from "../ui";
import { Card, MetricCard, unreachable } from "./sections";
import { pendingMetrics, pipelineMetrics, type PipelineKey } from "./pipeline/cards";
import { pipelinePanel } from "./pipeline/panels";
import "./analytics.css";

/**
 * Pipeline health (spec analytics-pipeline §8): the landing queue, the gates, jobs and autopilot,
 * under F1's readout with the same window and project. Its own query, so a failure here never takes
 * the run analytics down, and the other way round. Within it each block fails alone: an `{error}`
 * block is a card that says so and leaves the others (D5).
 *
 * Same shape as F1: numbers-first cards, each a button opening its chart in the one panel under
 * them. Cards live in `pipeline/cards.ts`, panels in `pipeline/panels.tsx`.
 */

export interface PipelineSectionProps {
  projectId?: string;
  days: AnalyticsDays;
  /** The section's id, for the page's section links. */
  anchor?: string;
}

export function PipelineSection({ projectId, days, anchor }: PipelineSectionProps) {
  const report = usePipelineAnalytics(projectId, days);
  const [picked, setPicked] = useState<PipelineKey | undefined>(undefined);
  const headingId = `an-pipeline-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const panelId = `${headingId}-detail`;
  const data = report.data;

  // The view above already says "No project by that name."
  if (data === undefined && isApiRefusal(report.error) && report.error.status === 404) return null;

  // Red is what matters most, so it opens first; after a click the choice is kept across a window change.
  const open: PipelineKey = picked ?? (data !== undefined && !isBlockError(data.postgate) && data.postgate.red_now ? "postgate" : "landing");
  const panel = data === undefined ? undefined : pipelinePanel(open, data);
  const cards = data === undefined ? pendingMetrics() : pipelineMetrics(data);

  return (
    <section id={anchor} className="an-pipeline" aria-labelledby={headingId}>
      <h2 id={headingId} className="an-pipeline-title">
        Pipeline
      </h2>
      {data === undefined && report.isError ? (
        <ErrorNote onRetry={() => void report.refetch()}>{unreachable("the pipeline")}</ErrorNote>
      ) : (
        <>
          {report.isError ? <StaleNote dataUpdatedAt={report.dataUpdatedAt} /> : null}
          <div className="an-metrics">
            {cards.map((metric) => (
              <MetricCard
                key={metric.key}
                metric={metric}
                open={data !== undefined && metric.key === open}
                controls={panelId}
                onOpen={() => setPicked(metric.key)}
              />
            ))}
          </div>
          {panel === undefined ? null : (
            <Card id={panelId} label={panel.title} aside={panel.aside} className="an-detail">
              {panel.body}
            </Card>
          )}
        </>
      )}
    </section>
  );
}
