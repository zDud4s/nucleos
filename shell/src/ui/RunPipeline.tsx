import { readState, type StateDomain } from "./state-map";
import type { BadgeTone } from "./Badge";

/**
 * A run's four stages, drawn as the line they are.
 *
 * The page already says everything about a run and nothing about its SHAPE: which stage it
 * reached, and where it is standing now. That is the question somebody opens a run to answer
 * first — *is it still thinking, is it at the gate, did it come out the other side* — and until
 * now it was assembled by reading a badge, a second badge and an exit code in three different
 * blocks.
 *
 * **Nothing here is a second reading of a number.** `Instruments` above it carries the cost, the
 * token counts and the context bar, and a figure that changes every three seconds must have
 * exactly one place on a page or the two drift by a poll and read as a contradiction. This draws
 * the stages and their states; it never draws a total.
 *
 * **It authors no tone.** Every colour on it comes out of `state-map.ts` through `readState`, for
 * the same reason every badge does: the run and gate vocabularies are §7 distinctions, and a
 * fourteenth surface picking its own green is how they stop meaning one thing. The two stages
 * that are not núcleo states — the prompt that went in and the exit code that came out — are
 * drawn in the neutral grey that makes no claim at all, because neither of them is a state.
 *
 * **Hand-drawn SVG, not React Flow.** The graph is four boxes on a straight line with no
 * interaction; the canvas library on the other side of this app exists for graphs that are
 * panned and dragged, and shipping its machinery to draw a fixed row would be paying for a
 * runtime this never uses.
 */

/* --------------------------------------------------------------- geometry -- */

/**
 * The box row, in viewBox units: `[x, width]` per stage, with the same 40-unit gap between each
 * pair. Equal gaps are not a taste — the flow marks travel a gap with one CSS animation whose
 * distance is written once in `ui.css`, and a gap of another length would need a second one.
 */
const BOX: Record<StageKey, readonly [number, number]> = {
  prompt: [8, 150],
  agent: [198, 180],
  gate: [418, 150],
  result: [608, 104],
};
const MID = 48;
/** The agent box is the tall one: it is where a live run actually is. */
const TALL = 76;
const SHORT = 56;

export type StageKey = "prompt" | "agent" | "gate" | "result";

export interface Stage {
  key: StageKey;
  /** The stage's name, in the small caps line above the reading. */
  kind: string;
  /** The reading itself — a mapped label where the stage is a state, a fact where it is not. */
  title: string;
  /** The line under it: a count, an exit code, a qualifier. Empty when there is nothing to add. */
  sub: string;
  /**
   * The tone from the map, or `null` for a stage that is not a state and must not borrow one.
   */
  tone: BadgeTone | null;
  /** Whether the run got this far. A stage it never reached is drawn as an outline. */
  reached: boolean;
}

/** What this component needs off a run. A subset, so a test does not have to build thirty fields. */
export interface RunShape {
  status: string;
  project_id: string | null;
  steerable: boolean;
  num_turns: number | null;
  gate_status: string | null;
  gate_exit_code: number | null;
  exit_code: number | null;
  successor_run_id: number | null;
}

const read = (domain: StateDomain, state: string | null) => readState(domain, state);

/**
 * The four stages, as a pure function of the run.
 *
 * Pure and exported so the reading of each stage can be asserted without rendering anything —
 * the interesting failures here are "a run with no gate is drawn as a failure" and "a run that
 * never reached the gate is drawn as having passed it", and both are decided in this function.
 */
export function runStages(run: RunShape): Stage[] {
  const agent = read("run", run.status);
  const gate = read("gate", run.gate_status);
  // A NULL gate_status carries two different facts depending on where the run ended up, which
  // `neverReachedGate` below is the whole of: nobody wrote a suite, or nobody got to run one.
  const gateMeasured = run.gate_status !== null && run.gate_status.trim() !== "";
  const reachedGate = gateMeasured || !neverReachedGate(run);
  const finished = run.exit_code !== null;

  return [
    {
      key: "prompt",
      kind: "PROMPT",
      title: run.project_id ?? "no project",
      sub: run.steerable ? "steerable" : "not steerable",
      tone: null,
      reached: true,
    },
    {
      key: "agent",
      kind: "AGENT",
      // The status literal itself when the map has no reading for it — the same admission of
      // ignorance `StateBadge` makes, rather than a plausible-looking label nobody can trust.
      title: agent?.label ?? run.status,
      sub: run.num_turns === null ? "no turns recorded" : `${run.num_turns} turns`,
      tone: agent?.tone ?? null,
      reached: true,
    },
    {
      key: "gate",
      kind: "GATE",
      // "not reached" and "no gate configured" are the two absences and they are not the same
      // one. The map's reading of a NULL gate_status is a fact about the PROJECT — nobody wrote a
      // suite — and printing it on a run the núcleo stopped at turn nine would answer a question
      // that was never asked.
      title: reachedGate ? (gate?.label ?? "gate") : "not reached",
      sub: run.gate_exit_code === null ? "" : `exit ${run.gate_exit_code}`,
      tone: reachedGate ? (gate?.tone ?? null) : null,
      reached: reachedGate,
    },
    {
      key: "result",
      kind: "RESULT",
      // Absent is not zero: a run killed before it reported has no exit code, and saying "exit 0"
      // there would be the page inventing the one number somebody came to check.
      title: finished ? `exit ${run.exit_code}` : "none recorded",
      sub: run.successor_run_id === null ? "" : `continued as ${run.successor_run_id}`,
      tone: null,
      reached: finished,
    },
  ];
}

/**
 * The statuses where an absent gate means *the run never got there*.
 *
 * A run still going has not reached its gate yet, and one that was interrupted, cancelled or
 * timed out never will. Reading the map's *no gate configured* onto either — solid, settled, a
 * fact about how the PROJECT is set up — claims something nobody measured, and claims it on the
 * page where somebody is deciding whether to wait or to intervene. Every other status ran to the
 * end, and there a NULL really is the project's answer.
 */
function neverReachedGate(run: RunShape): boolean {
  return ["pending", "running", "interrupted", "cancelled", "timed_out"].includes(run.status);
}

/** A stage's tone as a class suffix. `neutral` is the grey that claims nothing, not an eighth tone. */
function tone(stage: Stage): string {
  return stage.tone ?? "neutral";
}

/* ------------------------------------------------------------- component -- */

export interface RunPipelineProps {
  run: RunShape;
  /**
   * Whether the run is still going. The only thing on this component that moves, and it moves
   * for one reason: a stage that is *happening* is not the same fact as a stage that finished,
   * and on a still image the two are identical.
   */
  alive: boolean;
}

export function RunPipeline({ run, alive }: RunPipelineProps) {
  const stages = runStages(run);
  const order: StageKey[] = ["prompt", "agent", "gate", "result"];

  return (
    <svg
      className="ui-runpipe"
      viewBox="0 0 720 96"
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
        return (
          <g
            key={stage.key}
            className={`ui-runpipe-stage ui-runpipe-${tone(stage)}${stage.reached ? "" : " ui-runpipe-unreached"}`}
          >
            <rect className="ui-runpipe-box" x={x} y={y} width={width} height={height} rx={tall ? 10 : 8} />
            <text className="ui-runpipe-kind" x={cx} y={y + 19} textAnchor="middle">
              {stage.kind}
            </text>
            <text className="ui-runpipe-title" x={cx} y={y + (tall ? 42 : 39)} textAnchor="middle">
              {stage.title}
            </text>
            {stage.sub !== "" && (
              <text className="ui-runpipe-sub" x={cx} y={y + (tall ? 58 : 51)} textAnchor="middle">
                {stage.sub}
              </text>
            )}
            {tall && alive && (
              <g className="ui-runpipe-work">
                {[-12, 0, 12].map((dx) => (
                  <circle key={dx} className="ui-runpipe-workdot" cx={cx + dx} cy={y + height - 14} r={2.6} />
                ))}
              </g>
            )}
          </g>
        );
      })}
    </svg>
  );
}
