import { DAEMON_NAME } from "../copy";
import { useId, useState } from "react";
import { useProjectStates, type StateMachine } from "../data/project-states";
import { absolutePath } from "../data/project-code";
import { openInVscode } from "../lib/vscode";
import { layoutMachine, type MachineLayout } from "./states-model";
import { Button, ErrorNote } from "../ui";

/**
 * The lifecycles the code draws: one graph per (table, column), every edge labelled with the
 * functions that make it and every transition anchored to the line that does it.
 */

const NODE_W = 124;
const NODE_H = 30;
const GAP_X = 70;
const GAP_Y = 22;
const PAD = 16;

export function StatesView({ projectId }: { projectId: string }) {
  const states = useProjectStates(projectId);
  const [failed, setFailed] = useState(false);

  if (states.isError && states.data === undefined) {
    return (
      <ErrorNote>
        The {DAEMON_NAME} could not read this project&rsquo;s state machines.{" "}
        <Button variant="quiet" onClick={() => void states.refetch()} disabled={states.isFetching}>
          {states.isFetching ? "reading…" : "read it again"}
        </Button>
      </ErrorNote>
    );
  }
  if (states.data === undefined) {
    return <p className="text-sm text-text-faint">Reading the state machines…</p>;
  }
  const { root, machines } = states.data;
  if (machines.length === 0) {
    return (
      <p className="m-0 text-sm text-text-muted">
        No state machine found: nothing in this project&rsquo;s source sets a status with a guard.
      </p>
    );
  }

  const open = (path: string, line: number) => {
    setFailed(false);
    openInVscode(absolutePath(root, path) ?? path, line).catch(() => setFailed(true));
  };

  return (
    <div className="flex flex-col gap-6">
      {failed ? (
        <ErrorNote>
          VS Code gave no reply. Is it installed, and does it open <code>vscode://</code> links on
          this machine?
        </ErrorNote>
      ) : null}
      {machines.map((machine) => (
        <Machine
          key={`${machine.table}.${machine.column}`}
          machine={machine}
          onOpen={open}
        />
      ))}
    </div>
  );
}

function Machine({
  machine,
  onOpen,
}: {
  machine: StateMachine;
  onOpen: (path: string, line: number) => void;
}) {
  const title = `${machine.table}.${machine.column}`;
  const layout = layoutMachine(machine);
  return (
    <section aria-label={title} className="flex flex-col gap-2">
      <h3 className="m-0 text-sm font-medium text-text">{title}</h3>
      <Graph title={title} layout={layout} />
      <ul className="m-0 flex list-none flex-col gap-1 p-0 text-sm text-text-muted">
        {machine.transitions.map((t, index) => (
          <li key={`${t.path}:${t.line}:${index}`} className="flex flex-wrap items-center gap-2">
            <span>
              {t.from} → {t.to} · {t.function ?? "top level"}
            </span>
            <Button variant="link" onClick={() => onOpen(t.path, t.line)}>
              {`${t.path}:${t.line}`}
            </Button>
          </li>
        ))}
      </ul>
    </section>
  );
}

function Graph({ title, layout }: { title: string; layout: MachineLayout }) {
  const marker = `arrow-${useId().replace(/[^a-zA-Z0-9]/g, "")}`;
  const centre = new Map(
    layout.nodes.map((n) => [
      n.id,
      {
        x: PAD + n.column * (NODE_W + GAP_X) + NODE_W / 2,
        y: PAD + n.row * (NODE_H + GAP_Y) + NODE_H / 2,
      },
    ]),
  );
  const width = PAD * 2 + layout.columns * NODE_W + Math.max(layout.columns - 1, 0) * GAP_X;
  const height = PAD * 2 + layout.rows * NODE_H + Math.max(layout.rows - 1, 0) * GAP_Y;

  return (
    <svg
      role="img"
      aria-label={`States of ${title}`}
      viewBox={`0 0 ${width} ${height}`}
      width={width}
      height={height}
      style={{ maxWidth: "100%", height: "auto" }}
    >
      <defs>
        <marker
          id={marker}
          viewBox="0 0 10 10"
          refX="9"
          refY="5"
          markerWidth="7"
          markerHeight="7"
          orient="auto-start-reverse"
        >
          <path d="M 0 0 L 10 5 L 0 10 z" fill="var(--text-muted)" />
        </marker>
      </defs>
      {layout.edges.map((edge) => {
        const a = centre.get(edge.from);
        const b = centre.get(edge.to);
        if (a === undefined || b === undefined) return null;
        const label = edge.functions.join(", ");
        const key = `${edge.from}->${edge.to}`;
        if (edge.from === edge.to) {
          const top = a.y - NODE_H / 2;
          const [l, r] = [a.x - 12, a.x + 12];
          return (
            <g key={key}>
              <path
                d={`M ${l} ${top} C ${l} ${top - 22}, ${r} ${top - 22}, ${r} ${top}`}
                fill="none"
                stroke="var(--text-muted)"
                markerEnd={`url(#${marker})`}
              />
              <text x={a.x} y={top - 14} textAnchor="middle" fontSize="10" fill="var(--text-muted)">
                {label}
              </text>
            </g>
          );
        }
        // Clip both ends at the node boxes so the arrow head stays visible.
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const reach = Math.min(
          dx === 0 ? Infinity : NODE_W / 2 / Math.abs(dx),
          dy === 0 ? Infinity : NODE_H / 2 / Math.abs(dy),
        );
        const x1 = a.x + dx * reach;
        const y1 = a.y + dy * reach;
        const x2 = b.x - dx * reach;
        const y2 = b.y - dy * reach;
        return (
          <g key={key}>
            <line
              x1={x1}
              y1={y1}
              x2={x2}
              y2={y2}
              stroke="var(--text-muted)"
              markerEnd={`url(#${marker})`}
            />
            <text
              x={(x1 + x2) / 2}
              y={(y1 + y2) / 2 - 4}
              textAnchor="middle"
              fontSize="10"
              fill="var(--text-muted)"
            >
              {label}
            </text>
          </g>
        );
      })}
      {layout.nodes.map((n) => {
        const c = centre.get(n.id);
        if (c === undefined) return null;
        return (
          <g key={n.id}>
            <rect
              x={c.x - NODE_W / 2}
              y={c.y - NODE_H / 2}
              width={NODE_W}
              height={NODE_H}
              rx={8}
              fill="none"
              stroke="var(--text-muted)"
            />
            <text
              x={c.x}
              y={c.y + 4}
              textAnchor="middle"
              fontSize="12"
              fill="var(--text)"
            >
              {n.label}
            </text>
          </g>
        );
      })}
    </svg>
  );
}
