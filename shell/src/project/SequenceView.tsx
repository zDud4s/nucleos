// §spec atlas-sequence
import { DAEMON_NAME } from "../copy";
import { useId } from "react";
import { layoutSequence, useMapFlows, type FlowCheck } from "../data/map-flows";
import { absolutePath } from "../data/project-code";
import { openInVscode, vscodeUrl } from "../lib/vscode";
import { ErrorNote } from "../ui";

const DANGER = "var(--tone-danger-fg)";
const COLUMN = 170;
const HOW_TO_DECLARE =
  "No flows are declared for this project. Declare them in .nucleos/flows.yaml as " +
  `flows: [{ name, steps: [{ participant, path, fn }] }], and the ${DAEMON_NAME} verifies each step ` +
  "against the code.";

/**
 * Every declared flow of the project as a sequence diagram: one lifeline per participant, one
 * arrow per call labelled with the fn. The núcleo verified each step against the code, so a
 * flow that drifted says which step broke instead of drawing a diagram that lies.
 */
export function SequenceView({ projectId }: { projectId: string }) {
  const flows = useMapFlows(projectId);
  if (flows.isError) {
    return <ErrorNote>The {DAEMON_NAME} could not read the declared flows of this project.</ErrorNote>;
  }
  if (flows.data === undefined) {
    return <p className="text-sm text-text-faint">Reading the declared flows…</p>;
  }
  const { data } = flows;
  if (data.error !== null) {
    return <ErrorNote>{`${data.file} could not be read: ${data.error}`}</ErrorNote>;
  }
  if (!data.declared || data.flows.length === 0) {
    return <p className="m-0 text-sm text-text-muted">{HOW_TO_DECLARE}</p>;
  }
  return (
    <div className="flex flex-col gap-8">
      {data.flows.map((flow) => (
        <FlowSection key={flow.name} flow={flow} root={data.root} />
      ))}
    </div>
  );
}

function FlowSection({ flow, root }: { flow: FlowCheck; root: string }) {
  const marker = useId();
  const { participants, arrows } = layoutSequence(flow);
  const x = (index: number) => 90 + COLUMN * index;
  const height = 60 + 34 * arrows.length;
  const width = 90 + COLUMN * Math.max(participants.length - 1, 0) + 120;
  const broken = flow.broken_step === null ? undefined : flow.steps[flow.broken_step];

  return (
    <section aria-label={`Flow ${flow.name}`} className="flex flex-col gap-2">
      <h3 className="m-0 text-base">{flow.name}</h3>
      {flow.stale && broken !== undefined && flow.broken_step !== null ? (
        <p className="m-0 text-sm" style={{ color: DANGER }}>
          {`Stale: step ${flow.broken_step + 1} (${broken.fn}) broke: ${broken.reason ?? "unknown"}`}
        </p>
      ) : flow.stale ? (
        <p className="m-0 text-sm" style={{ color: DANGER }}>
          Stale: the flow no longer matches the code.
        </p>
      ) : (
        <p className="m-0 text-sm text-text-faint">Verified against the code</p>
      )}
      <svg
        role="img"
        aria-label={`Sequence of ${flow.name}`}
        width={width}
        height={height}
        viewBox={`0 0 ${width} ${height}`}
        style={{ maxWidth: "100%", overflow: "visible" }}
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
            <path d="M 0 0 L 10 5 L 0 10 z" fill="context-stroke" />
          </marker>
        </defs>
        {participants.map((participant, i) => (
          <g key={participant}>
            <rect
              x={x(i) - 60}
              y={8}
              width={120}
              height={24}
              rx={4}
              fill="none"
              stroke="currentColor"
              opacity={0.5}
            />
            <text x={x(i)} y={24} textAnchor="middle" fontSize={12} fill="currentColor">
              {participant}
            </text>
            <line
              x1={x(i)}
              y1={32}
              x2={x(i)}
              y2={height}
              stroke="currentColor"
              opacity={0.35}
              strokeDasharray="4 4"
            />
          </g>
        ))}
        {arrows.map((arrow, j) => {
          const y = 70 + 34 * j;
          const step = flow.steps[arrow.step];
          const stroke = arrow.broken ? DANGER : "currentColor";
          const dash = arrow.kind === "async" ? "6 3" : undefined;
          const x1 = x(arrow.from);
          const x2 = x(arrow.to);
          const self = arrow.from === arrow.to;
          return (
            <g key={arrow.step} data-step={arrow.step} opacity={arrow.unchecked ? 0.4 : 1}>
              <title>{`${step.fn} (${step.path}:${step.line ?? "?"})`}</title>
              {self ? (
                <path
                  d={`M ${x1} ${y} h 28 v 12 h -28`}
                  fill="none"
                  stroke={stroke}
                  strokeDasharray={dash}
                  markerEnd={`url(#${marker})`}
                />
              ) : (
                <line
                  x1={x1}
                  y1={y}
                  x2={x2}
                  y2={y}
                  stroke={stroke}
                  strokeDasharray={dash}
                  markerEnd={`url(#${marker})`}
                />
              )}
              <text
                x={self ? x1 + 34 : (x1 + x2) / 2}
                y={y - 6}
                textAnchor={self ? "start" : "middle"}
                fontSize={11}
                fill={stroke}
              >
                {arrow.label}
              </text>
            </g>
          );
        })}
      </svg>
      <ol className="m-0 flex flex-col gap-1 pl-5 text-sm">
        {flow.steps.map((step, i) => {
          const target = absolutePath(root, step.path);
          // The broken step's reason is already in the stale line above the diagram.
          const showReason = !step.ok && step.reason !== null && i !== flow.broken_step;
          return (
            <li key={i}>
              <span>{`${step.participant} · ${step.fn} `}</span>
              {target === null ? null : (
                <a
                  href={vscodeUrl(target, step.line)}
                  onClick={(event) => {
                    event.preventDefault();
                    void openInVscode(target, step.line);
                  }}
                >
                  {step.line === null ? step.path : `${step.path}:${step.line}`}
                </a>
              )}
              {showReason ? <span style={{ color: DANGER }}>{` (${step.reason})`}</span> : null}
            </li>
          );
        })}
      </ol>
    </section>
  );
}
