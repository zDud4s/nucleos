import { readState, type StateDomain } from "./state-map";
import type { BadgeTone } from "./Badge";

/**
 * A run's four stages as pure functions, shared by `RunPipeline.tsx` (which draws them) and
 * `runChain.ts` (which reads the gate and result off the last leg of a chain). Kept apart so the
 * imports run one way: RunPipeline -> runChain -> runStages.
 *
 * It authors no tone: every one comes out of `state-map.ts` through `readState`.
 */

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

/** What the stages need off a run. A subset, so a test does not have to build thirty fields. */
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
    gateStage(run),
    resultStage(run, run.successor_run_id),
  ];
}

/** The gate stage of a run. Shared with the chain, which reads it off the last leg. */
export function gateStage(
  run: Pick<RunShape, "status" | "gate_status" | "gate_exit_code">,
): Stage {
  const gate = read("gate", run.gate_status);
  // A NULL gate_status carries two different facts depending on where the run ended up, which
  // `neverReachedGate` below is the whole of: nobody wrote a suite, or nobody got to run one.
  const gateMeasured = run.gate_status !== null && run.gate_status.trim() !== "";
  const reachedGate = gateMeasured || !neverReachedGate(run);
  return {
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
  };
}

/** The result stage of a run; `continuedAs` is the successor's id, if any. */
export function resultStage(
  run: Pick<RunShape, "status" | "exit_code">,
  continuedAs: number | null,
): Stage {
  const finished = run.exit_code !== null;
  return {
    key: "result",
    kind: "RESULT",
    // Absent is not zero: a run killed before it reported has no exit code, and saying "exit 0"
    // there would be the page inventing the one number somebody came to check.
    title: finished ? `exit ${run.exit_code}` : "none recorded",
    sub: continuedAs === null ? "" : `continued as ${continuedAs}`,
    tone: null,
    reached: finished,
  };
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
function neverReachedGate(run: Pick<RunShape, "status">): boolean {
  return ["pending", "running", "interrupted", "cancelled", "timed_out"].includes(run.status);
}
