import type { AnalyticsDays } from "../data/analytics";
import { useContextFit, type ContextFitReport, type FitNode, type FitVerdict } from "../data/context-fit";
import { Badge, ErrorNote, Skeleton, StaleNote, StatCard, Table, Td, Th, type BadgeTone } from "../ui";
import { Card, unreachable } from "./sections";

/**
 * Context fit (spec encaixe-de-contexto-por-no §4): did each tree node get the right amount of
 * context. Verdict totals, a per-layer table, and the recent nodes that did not fit with the evidence
 * behind each verdict. Its own query, so its failure never takes the other sections down.
 */

export interface ContextFitSectionProps {
  days: AnalyticsDays;
  /** The section's id, for the page's section links. */
  anchor?: string;
}

const EMPTY = "No tree nodes measured yet";

const VERDICTS: { key: FitVerdict; label: string; meaning: string; tone: BadgeTone }[] = [
  { key: "under", label: "Under", meaning: "too little context", tone: "danger" },
  { key: "over", label: "Over", meaning: "too much context", tone: "pending" },
  { key: "split", label: "Split", meaning: "bad division of the task", tone: "paused" },
  { key: "fit", label: "Fit", meaning: "right context", tone: "active" },
];

const verdictOf = (key: FitVerdict) => VERDICTS.find((verdict) => verdict.key === key);

export function ContextFitSection({ days, anchor }: ContextFitSectionProps) {
  const report = useContextFit(days);
  const data = report.data;
  return (
    <Card id={anchor} label="Context fit" className="an-fit">
      {data === undefined ? (
        report.isError ? (
          <ErrorNote onRetry={() => void report.refetch()} retryLabel="Try again">
            {unreachable("context fit")}
          </ErrorNote>
        ) : (
          <Skeleton label="Loading context fit" lines={6} />
        )
      ) : (
        <>
          {report.isError ? <StaleNote dataUpdatedAt={report.dataUpdatedAt} /> : null}
          {data.nodes === 0 ? <p className="an-note">{EMPTY}</p> : <Body data={data} />}
        </>
      )}
    </Card>
  );
}

function Body({ data }: { data: ContextFitReport }) {
  const recent = data.recent.filter((node) => node.verdicts.some((verdict) => verdict !== "fit"));
  return (
    <>
      <div className="an-metrics">
        {VERDICTS.map((verdict) => (
          <StatCard key={verdict.key} label={verdict.label} value={String(data.by_verdict[verdict.key])} detail={verdict.meaning} />
        ))}
      </div>
      <Table label="Context fit by layer">
        <thead>
          <tr>
            <Th>Layer</Th>
            {VERDICTS.map((verdict) => (
              <Th key={verdict.key} num>
                {verdict.label}
              </Th>
            ))}
          </tr>
        </thead>
        <tbody>
          {data.by_layer.map((row) => (
            <tr key={row.layer}>
              <Td>{row.layer}</Td>
              {VERDICTS.map((verdict) => (
                <Td key={verdict.key} num>
                  {row[verdict.key]}
                </Td>
              ))}
            </tr>
          ))}
        </tbody>
      </Table>
      {recent.length === 0 ? null : (
        <Table label="Recent nodes that did not fit">
          <thead>
            <tr>
              <Th>Node</Th>
              <Th>Layer</Th>
              <Th>Verdict</Th>
              <Th>Evidence</Th>
            </tr>
          </thead>
          <tbody>
            {recent.map((node) => (
              <NodeRow key={`${node.tree_id}/${node.node_id}`} node={node} />
            ))}
          </tbody>
        </Table>
      )}
    </>
  );
}

function NodeRow({ node }: { node: FitNode }) {
  return (
    <tr>
      <Td title={`tree ${node.tree_id}`}>{node.node_id}</Td>
      <Td>{node.layer}</Td>
      <Td>
        {node.verdicts
          .filter((verdict) => verdict !== "fit")
          .map((verdict) => {
            const known = verdictOf(verdict);
            return (
              <Badge key={verdict} tone={known?.tone ?? "info"} title={known?.meaning}>
                {known?.label ?? verdict}
              </Badge>
            );
          })}
      </Td>
      <Td>{node.evidence.map((item) => `${item.signal} ${item.value} vs ${item.threshold}`).join(" · ")}</Td>
    </tr>
  );
}
