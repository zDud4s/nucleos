// §spec autopilot-job-graph
import { useMemo } from "react";
import type { Job, JobItem } from "../data/fleet";
import { DUMMY_W, NODE_H, layout, type Link as Edge } from "./layered";
import { buildProgress, type Lifecycle, type ProgressNode } from "./job-progress";

/**
 * A job's queue drawn as the DAG it is.
 *
 * The list beside it (`JobItemsPanel`) says everything about each item and nothing about their
 * SHAPE: which of them can run at the same time, and which is waiting on which. That shape is on
 * the wire in `depends_on` and is the whole answer to *why are these two items running and not
 * those two* -- a question a sequential queue never provoked and a directed one asks constantly.
 *
 * **SVG and `layered.ts`, not React Flow.** This draws inside `FleetCanvas`, which is itself a
 * React Flow canvas; a second one nested in a node of the first fights it for wheel and drag
 * events and ships a second copy of its machinery for a graph that is usually under ten boxes.
 * `layered.ts` already ranks, orders and routes -- with crossing minimisation and stable output --
 * and `MapCanvas` already draws its result by hand.
 *
 * The layout is memoised on the shape rather than on the statuses. The panel repolls every three
 * seconds, and a graph that relaid itself on every tick would move under the cursor of somebody
 * reading it while nothing structural had changed.
 */
export function JobProgressGraph({ job, items }: { job: Job; items: JobItem[] }) {
  const progress = useMemo(() => buildProgress(job, items), [job, items]);

  // The layout depends on ids, edges and labels -- never on a status. Statuses change on every
  // poll and must repaint boxes without moving them.
  const shape = useMemo(
    () =>
      JSON.stringify([
        progress.nodes.map((node) => [node.id, node.label]),
        progress.edges.map((edge) => [edge.from, edge.to]),
      ]),
    [progress.nodes, progress.edges],
  );

  const view = useMemo(() => {
    const names = progress.nodes.map((node) => node.id);
    const label = new Map(progress.nodes.map((node) => [node.id, node.label]));
    const links: Edge[] = progress.edges.map((edge) => ({
      from: edge.from,
      to: edge.to,
      weight: 1,
    }));
    return layout(names, links, 6, (id) => label.get(id) ?? id);
    // `shape` is the real dependency: same ids, same edges, same labels, same picture.

  }, [shape]);

  const byId = new Map(progress.nodes.map((node) => [node.id, node]));

  // Bends are invisible waypoints on a long edge. They carry no box, but a segment can end at
  // one, so an edge cannot be drawn from `nodes` alone.
  const at = new Map<string, { x: number; y: number; width: number }>();
  for (const placed of view.nodes) {
    at.set(placed.id, { x: placed.x, y: placed.y, width: placed.width });
  }
  for (const [id, point] of Object.entries(view.bends)) {
    at.set(id, { x: point.x, y: point.y, width: DUMMY_W });
  }

  if (progress.nodes.length === 0) return null;

  return (
    <div className="jp">
      <p className="jp-reading">{progress.reading}</p>
      <svg
        className="jp-svg"
        viewBox={`0 0 ${view.width} ${view.height}`}
        width={view.width}
        height={view.height}
        role="img"
        aria-label={`queue of job ${job.id} as a graph — ${progress.reading}`}
      >
        <g className="jp-edges">
          {view.segments.map((segment) => {
            const from = at.get(segment.from);
            const to = at.get(segment.to);
            if (from === undefined || to === undefined) return null;
            const x1 = from.x + from.width / 2;
            const y1 = from.y + NODE_H;
            const x2 = to.x + to.width / 2;
            const y2 = to.y;
            return (
              <path
                key={`${segment.from}->${segment.to}`}
                className={segment.reversed ? "jp-edge jp-edge-back" : "jp-edge"}
                d={`M ${x1} ${y1} C ${x1} ${y1 + 24}, ${x2} ${y2 - 24}, ${x2} ${y2}`}
              />
            );
          })}
        </g>
        <g className="jp-nodes">
          {view.nodes.map((placed) => {
            const node = byId.get(placed.id);
            if (node === undefined) return null;
            return (
              <NodeBox key={placed.id} node={node} placed={placed} jobRound={job.round} />
            );
          })}
        </g>
      </svg>
      <Legend />
    </div>
  );
}

function NodeBox({
  node,
  placed,
  jobRound,
}: {
  node: ProgressNode;
  placed: { x: number; y: number; width: number; lines: string[] };
  jobRound: number;
}) {
  // `title` is the accessible reading and the hover text at once. It carries the things the box
  // has no room for and a person still needs: who is on it, what it said it would touch, and --
  // for a gate verdict the wire cannot resolve -- that the view does not know.
  const detail = [node.reading];
  if (node.agentName !== null) detail.push(`given to ${node.agentName}`);
  if (node.gateStatus !== null) detail.push(`gate: ${node.gateStatus}`);
  if (node.runId !== null) detail.push(`run ${node.runId}`);
  if (node.files.length > 0) detail.push(node.files.join(", "));
  if (node.undecided) {
    detail.push("the queue may still owe it another attempt — the wire cannot say which");
  }
  if (node.round !== jobRound && node.kind === "item") detail.push(`round ${node.round + 1}`);

  return (
    <g
      className={`jp-node jp-${node.lifecycle}${node.undecided ? " jp-undecided" : ""}`}
      transform={`translate(${placed.x}, ${placed.y})`}
    >
      <title>{detail.join(" · ")}</title>
      <rect className="jp-box" width={placed.width} height={NODE_H} rx={6} />
      {placed.lines.map((line, index) => (
        <text
          key={line + index}
          className="jp-label"
          x={placed.width / 2}
          y={NODE_H / 2 + (index - (placed.lines.length - 1) / 2) * 12 + 4}
          textAnchor="middle"
        >
          {line}
        </text>
      ))}
      {node.ordinal !== null && (
        <text className="jp-ordinal" x={6} y={12}>
          {node.ordinal + 1}
        </text>
      )}
    </g>
  );
}

/**
 * Seven readings, named.
 *
 * A colour key and not a decoration: `waiting`, `gated`, `stopped` and `withdrawn` are all "not
 * moving", and somebody deciding whether to step in needs to know that exactly one of them is
 * asking them a question.
 */
const LEGEND: Array<[Lifecycle, string]> = [
  ["done", "done"],
  ["running", "running"],
  ["todo", "to do"],
  ["waiting", "waiting on a person"],
  ["gated", "the gate said no"],
  ["stopped", "broke"],
  ["withdrawn", "withdrawn"],
];

function Legend() {
  return (
    <ul className="jp-legend">
      {LEGEND.map(([life, reading]) => (
        <li key={life} className={`jp-legend-item jp-${life}`}>
          <span className="jp-swatch" aria-hidden="true" />
          {reading}
        </li>
      ))}
    </ul>
  );
}

/**
 * The one-line version, for a card that is closed.
 *
 * Answers "where is it" without opening anything, which is the question the fleet view is for.
 * The graph answers "what exactly", and that one is worth a click.
 */
export function JobProgressLine({ job, items }: { job: Job; items: JobItem[] }) {
  const progress = useMemo(() => buildProgress(job, items), [job, items]);
  const { done, running, todo, attention, total } = progress.tally;
  if (total === 0) return <span className="jp-line">{progress.reading}</span>;

  return (
    <span className="jp-line" title={progress.reading}>
      <span className="jp-bar" aria-hidden="true">
        {([["done", done], ["running", running], ["todo", todo], ["attention", attention]] as const)
          .filter(([, count]) => count > 0)
          .map(([kind, count]) => (
            <span key={kind} className={`jp-seg jp-${kind}`} style={{ flexGrow: count }} />
          ))}
      </span>
      {progress.reading}
    </span>
  );
}
