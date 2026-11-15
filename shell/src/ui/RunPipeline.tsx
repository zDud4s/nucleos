import { useState, type KeyboardEvent } from "react";

import { chainLegs, type ChainItem, type ChainRun } from "./runChain";
import { runStages, type RunShape, type Stage, type StageKey } from "./runStages";

// The stage reading lives in `runStages.ts` so imports run one way (RunPipeline -> runChain ->
// runStages); these re-exports keep the public names where they always were.
export { runStages, type RunShape, type Stage, type StageKey };

/**
 * A run's four stages, drawn as the line they are — and, when the run was one leg of a longer
 * piece of work, the whole chain of runs it went through.
 *
 * The page already says everything about a run and nothing about its SHAPE: which stage it
 * reached, and where it is standing now. That is the question somebody opens a run to answer
 * first — *is it still thinking, is it at the gate, did it come out the other side* — and until
 * now it was assembled by reading a badge, a second badge and an exit code in three different
 * blocks.
 *
 * **A chain replaces PROMPT and AGENT with one box per leg.** A run that filled its context, or
 * waited on an approval, is continued by a successor; the legs are drawn left to right, the one
 * being viewed is highlighted, the others open their own page, and GATE and RESULT are the LAST
 * leg's — the outcome of the work, not of the run the reader happens to be on. A long chain
 * collapses its middle into a "… N runs" box that expands on click. A chain of one (or an old
 * response with no lineage) is drawn exactly as the four stages.
 *
 * **Nothing here is a second reading of a number.** `Instruments` above it carries the cost, the
 * token counts and the context bar, and a figure that changes every three seconds must have
 * exactly one place on a page or the two drift by a poll and read as a contradiction. This draws
 * the stages and their states; it never draws a total (a leg shows its turns, which are fixed
 * once the leg ended, and never a cost).
 *
 * **It authors no tone.** Every colour on it comes out of `state-map.ts` through `readState`, for
 * the same reason every badge does: the run and gate vocabularies are §7 distinctions, and a
 * fourteenth surface picking its own green is how they stop meaning one thing. The two stages
 * that are not núcleo states — the prompt that went in and the exit code that came out — are
 * drawn in the neutral grey that makes no claim at all, because neither of them is a state.
 *
 * **Hand-drawn SVG, not React Flow.** The graph is a few boxes on a straight line with no
 * panning or dragging; the canvas library on the other side of this app exists for graphs that
 * are panned and dragged, and shipping its machinery to draw a fixed row would be paying for a
 * runtime this never uses. Because an `<svg>` cannot hold a router `<Link>`, a leg that opens
 * another run is a `role="link"` group that calls `onOpenRun`.
 */

/* --------------------------------------------------------------- geometry -- */

/**
 * The box row, in viewBox units: `[x, width]` per stage, with the same 40-unit gap between each
 * pair. Equal gaps are not a taste — the flow marks travel a gap with one CSS animation whose
 * distance is written once in `ui.css`, and a gap of another length would need a second one.
 */
const BOX: Record<StageKey, readonly [number, number]> = {
  prompt: [8, 160],
  agent: [208, 190],
  gate: [438, 160],
  result: [638, 160],
};
/** The viewBox: the row's right edge plus the same 8-unit margin it starts with. */
const VIEW_W = 806;
const VIEW_H = 100;
const MID = 50;
/** The agent box is the tall one: it is where a live run actually is. */
const TALL = 88;
const SHORT = 66;
const GAP = 40;
const MARGIN = 8;
/** Chain box widths: a leg, the "… N runs" box, and the closing gate/result boxes. */
const LEG_W = 150;
const HIDDEN_W = 90;
const END_W = 130;

/**
 * Where each line sits inside its box, from the box's top. Three lines and, in the agent box, the
 * working dots under them: each baseline is far enough below the last that the title's
 * descenders (`--text-sm`, 13px) clear the `--text-xs` (11px) line under it, which they did not at
 * 56 units tall.
 */
const LINES = {
  short: { kind: 19, title: 40, sub: 56 },
  tall: { kind: 20, title: 43, sub: 60, dots: 76 },
};
/** A leg can carry two under-lines (why it ended, its turns); the second sits this far below the first. */
const SUB_STEP = 15;

/** Roughly how wide a character of the title is at `--text-sm` (13px, medium) — enough to cut a long name before the edge. */
const TITLE_CHAR = 7.2;

/** The title cut to its box, from the end, with an ellipsis — the full one is in the aria-label. */
function fit(text: string, width: number): string {
  const room = Math.floor((width - 20) / TITLE_CHAR);
  return text.length <= room ? text : `${text.slice(0, Math.max(1, room - 1))}…`;
}

/** A stage's tone as a class suffix. `neutral` is the grey that claims nothing, not an eighth tone. */
function tone(stage: { tone: Stage["tone"] }): string {
  return stage.tone ?? "neutral";
}

/* ------------------------------------------------------------- component -- */

export interface RunPipelineProps {
  run: RunShape & ChainRun;
  /**
   * Whether the run is still going. The only thing on this component that moves, and it moves
   * for one reason: a stage that is *happening* is not the same fact as a stage that finished,
   * and on a still image the two are identical.
   */
  alive: boolean;
  /** Opens another leg of the chain. Without it the legs are drawn but are not links. */
  onOpenRun?: (id: number) => void;
}

export function RunPipeline({ run, alive, onOpenRun }: RunPipelineProps) {
  const [expanded, setExpanded] = useState(false);
  const chain = chainLegs(run, expanded);
  if (chain.items.length <= 1) return <SingleRun run={run} alive={alive} />;

  // Lay the row out from the item count: legs and gaps, then GATE and RESULT.
  const cells: { x: number; width: number }[] = [];
  let cursor = MARGIN;
  const widths = [
    ...chain.items.map((item) => (item.kind === "leg" ? LEG_W : HIDDEN_W)),
    END_W,
    END_W,
  ];
  for (const width of widths) {
    cells.push({ x: cursor, width });
    cursor += width + GAP;
  }
  const viewW = cursor - GAP + MARGIN;
  const ends: Stage[] = [chain.gate, chain.result];
  const nItems = chain.items.length;

  const label = `Chain of runs: ${chain.items
    .map((item) =>
      item.kind === "leg"
        ? `run ${item.id} ${item.title}${item.current ? " (this run)" : ""}`
        : `runs ${item.hiddenIds.join(", ")} hidden`,
    )
    .join(", ")}, gate ${chain.gate.title}, result ${chain.result.title}${chain.truncated ? ", chain truncated" : ""}`;

  /** What is drawn at position `i` of the row, with the facts an edge into it needs. */
  const reachedAt = (i: number): boolean =>
    i < nItems ? true : ends[i - nItems].reached;

  return (
    <div className="ui-runchain">
      <svg
        className="ui-runpipe"
        style={{ width: viewW }}
        viewBox={`0 0 ${viewW} ${VIEW_H}`}
        role="img"
        aria-label={label}
      >
        {cells.slice(0, -1).map((cell, i) => {
          const next = cells[i + 1];
          const from = cell.x + cell.width;
          const reached = reachedAt(i + 1);
          const into = chain.items[i + 1];
          // The mark travels only into a leg that is going, in that leg's tone.
          const target = into !== undefined && into.kind === "leg" && into.alive ? into : null;
          return (
            <g key={`edge-${i}`}>
              <path
                className={`ui-runpipe-edge${reached ? "" : " ui-runpipe-unreached"}`}
                d={`M${from},${MID} L${next.x - 4},${MID}`}
              />
              <path
                className={`ui-runpipe-edge${reached ? "" : " ui-runpipe-unreached"}`}
                d={`M${next.x - 9},${MID - 4} L${next.x - 3},${MID} L${next.x - 9},${MID + 4}`}
              />
              {target !== null && (
                <g className={`ui-runpipe-${tone(target)}`} transform={`translate(${from} ${MID})`}>
                  <circle className="ui-runpipe-flow" r={2.6} />
                  <circle className="ui-runpipe-flow" r={2} />
                  <circle className="ui-runpipe-flow" r={1.5} />
                </g>
              )}
            </g>
          );
        })}

        {chain.items.map((item, i) =>
          item.kind === "leg" ? (
            <LegBox
              key={`leg-${item.id}`}
              item={item}
              x={cells[i].x}
              width={cells[i].width}
              onOpenRun={onOpenRun}
            />
          ) : (
            <GapBox
              key={`gap-${item.hiddenIds[0]}`}
              hidden={item.hiddenIds}
              x={cells[i].x}
              width={cells[i].width}
              onExpand={() => setExpanded(true)}
            />
          ),
        )}

        {ends.map((stage, j) => (
          <StageBox key={stage.key} stage={stage} x={cells[nItems + j].x} width={END_W} />
        ))}
      </svg>
      {chain.truncated && <p className="ui-runchain-note">chain truncated</p>}
    </div>
  );
}

/** Enter and Space activate a group that is a link or a button, as they would a real one. */
function activateOnKey(action: () => void) {
  return (event: KeyboardEvent) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      action();
    }
  };
}

function LegBox({
  item,
  x,
  width,
  onOpenRun,
}: {
  item: Extract<ChainItem, { kind: "leg" }>;
  x: number;
  width: number;
  onOpenRun?: (id: number) => void;
}) {
  const tall = item.current;
  const height = tall ? TALL : SHORT;
  const y = MID - height / 2;
  const cx = x + width / 2;
  const at = tall ? LINES.tall : LINES.short;
  const open = !item.current && onOpenRun !== undefined ? () => onOpenRun(item.id) : null;
  const subs = item.sub === "" ? [] : item.sub.split(" / ");
  return (
    <g
      className={`ui-runpipe-stage ui-runpipe-leg ui-runpipe-${tone(item)}${item.current ? " ui-runpipe-current" : ""}`}
      aria-current={item.current ? "step" : undefined}
      {...(open
        ? {
            role: "link",
            tabIndex: 0,
            "aria-label": `Open run ${item.id}`,
            onClick: open,
            onKeyDown: activateOnKey(open),
          }
        : {})}
    >
      <rect className="ui-runpipe-box" x={x} y={y} width={width} height={height} rx={tall ? 10 : 8} />
      <text className="ui-runpipe-kind" x={cx} y={y + at.kind} textAnchor="middle">
        {`RUN ${item.id}`}
      </text>
      <text className="ui-runpipe-title" x={cx} y={y + at.title} textAnchor="middle">
        {fit(item.title, width)}
      </text>
      {subs.map((line, n) => (
        <text key={n} className="ui-runpipe-sub" x={cx} y={y + at.sub + n * SUB_STEP} textAnchor="middle">
          {line}
        </text>
      ))}
      {tall && item.alive && (
        <g className="ui-runpipe-work">
          {[-12, 0, 12].map((dx) => (
            <circle key={dx} className="ui-runpipe-workdot" cx={cx + dx} cy={y + LINES.tall.dots} r={2.6} />
          ))}
        </g>
      )}
    </g>
  );
}

function GapBox({
  hidden,
  x,
  width,
  onExpand,
}: {
  hidden: number[];
  x: number;
  width: number;
  onExpand: () => void;
}) {
  const y = MID - SHORT / 2;
  return (
    <g
      className="ui-runpipe-stage ui-runpipe-gap"
      role="button"
      tabIndex={0}
      aria-label={`Show ${hidden.length} hidden runs: ${hidden.join(", ")}`}
      onClick={onExpand}
      onKeyDown={activateOnKey(onExpand)}
    >
      <rect className="ui-runpipe-box" x={x} y={y} width={width} height={SHORT} rx={8} />
      <text className="ui-runpipe-title" x={x + width / 2} y={MID + 4} textAnchor="middle">
        {`… ${hidden.length} run${hidden.length === 1 ? "" : "s"}`}
      </text>
    </g>
  );
}

function StageBox({ stage, x, width }: { stage: Stage; x: number; width: number }) {
  const y = MID - SHORT / 2;
  const cx = x + width / 2;
  return (
    <g
      className={`ui-runpipe-stage ui-runpipe-${tone(stage)}${stage.reached ? "" : " ui-runpipe-unreached"}`}
    >
      <rect className="ui-runpipe-box" x={x} y={y} width={width} height={SHORT} rx={8} />
      <text className="ui-runpipe-kind" x={cx} y={y + LINES.short.kind} textAnchor="middle">
        {stage.kind}
      </text>
      <text className="ui-runpipe-title" x={cx} y={y + LINES.short.title} textAnchor="middle">
        {fit(stage.title, width)}
      </text>
      {stage.sub !== "" && (
        <text className="ui-runpipe-sub" x={cx} y={y + LINES.short.sub} textAnchor="middle">
          {stage.sub}
        </text>
      )}
    </g>
  );
}

/** The four classic stages of a run that is not part of a longer chain. */
function SingleRun({ run, alive }: { run: RunShape; alive: boolean }) {
  const stages = runStages(run);
  const order: StageKey[] = ["prompt", "agent", "gate", "result"];

  return (
    <svg
      className="ui-runpipe"
      viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
      role="img"
      aria-label={`This run: ${stages.map((stage) => `${stage.kind.toLowerCase()} ${stage.title}`).join(", ")}`}
    >
      {order.slice(0, -1).map((key, index) => {
        const [x, width] = BOX[key];
        const from = x + width;
        const next = BOX[order[index + 1]];
        const reached = stages[index + 1].reached;
        // The mark travels the gap into the agent, and only while the run is going. The other two
        // gaps carry no marks even then: nothing is flowing into a gate that has not been asked
        // to run, and a mark there would animate a thing that is not happening.
        const flowing = alive && order[index + 1] === "agent";
        return (
          <g key={key}>
            <path
              className={`ui-runpipe-edge${reached ? "" : " ui-runpipe-unreached"}`}
              d={`M${from},${MID} L${next[0] - 4},${MID}`}
            />
            <path
              className={`ui-runpipe-edge${reached ? "" : " ui-runpipe-unreached"}`}
              d={`M${next[0] - 9},${MID - 4} L${next[0] - 3},${MID} L${next[0] - 9},${MID + 4}`}
            />
            {flowing && (
              /* The marks wear the tone of the stage they are travelling into, rather than the
                 one brand colour: `--accent` is the wordmark, links and the focus ring, and a
                 moving accent-coloured dot in the middle of a page reads as something to click. */
              <g className={`ui-runpipe-${tone(stages[1])}`} transform={`translate(${from} ${MID})`}>
                <circle className="ui-runpipe-flow" r={2.6} />
                <circle className="ui-runpipe-flow" r={2} />
                <circle className="ui-runpipe-flow" r={1.5} />
              </g>
            )}
          </g>
        );
      })}

      {stages.map((stage) => {
        const [x, width] = BOX[stage.key];
        const tall = stage.key === "agent";
        const height = tall ? TALL : SHORT;
        const y = MID - height / 2;
        const cx = x + width / 2;
        const at = tall ? LINES.tall : LINES.short;
        return (
          <g
            key={stage.key}
            className={`ui-runpipe-stage ui-runpipe-${tone(stage)}${stage.reached ? "" : " ui-runpipe-unreached"}`}
          >
            <rect className="ui-runpipe-box" x={x} y={y} width={width} height={height} rx={tall ? 10 : 8} />
            <text className="ui-runpipe-kind" x={cx} y={y + at.kind} textAnchor="middle">
              {stage.kind}
            </text>
            <text className="ui-runpipe-title" x={cx} y={y + at.title} textAnchor="middle">
              {fit(stage.title, width)}
            </text>
            {stage.sub !== "" && (
              <text className="ui-runpipe-sub" x={cx} y={y + at.sub} textAnchor="middle">
                {stage.sub}
              </text>
            )}
            {tall && alive && (
              <g className="ui-runpipe-work">
                {[-12, 0, 12].map((dx) => (
                  <circle key={dx} className="ui-runpipe-workdot" cx={cx + dx} cy={y + LINES.tall.dots} r={2.6} />
                ))}
              </g>
            )}
          </g>
        );
      })}
    </svg>
  );
}
