/**
 * Which one thing the workspace leads with.
 *
 * The design asks two things of the Estado mode that only work together: every
 * screen answers *is everything all right?* first, and exceptions dominate while
 * the normal disappears. Put together they forbid a fixed hero card and require
 * the top of the page to depend on the state — so something has to decide what
 * that state *is*, and this is it.
 *
 * Pure, and in its own module with no React in it, because this is the decision
 * the page is built around: it is worth being able to read the ladder without
 * rendering anything, and worth being able to test a tie without a daemon.
 *
 * What this deliberately does NOT do is decide how loud the answer looks. The
 * page renders the same sections in the same order whatever comes back here —
 * only the weight of the top changes — because a page that grew and shrank
 * panels would move under the eye of somebody reading it when a proposal lands.
 */

/** Every concern that can lead, strongest first. Calm is not one — it is what is left. */
export const CONCERN_ORDER = [
  "kill-switch",
  "budget-paused",
  "proposal-waiting",
  "gate-failed",
  "run-interrupted",
  "workflow-drift",
] as const;

export type ConcernKind = (typeof CONCERN_ORDER)[number] | "calm" | "unknown";

/**
 * What the shell knows about a project right now.
 *
 * Flat booleans and counts rather than the raw API rows: the ladder is a
 * decision about *facts*, and a module that took `ProjectSummary` would be a
 * module that changes when a route grows a field.
 */
export interface ProjectConcerns {
  /** The machine-wide switch. Nothing autonomous starts anywhere while it is on. */
  killSwitch: boolean;
  /** The budget is holding work — a ceiling reached, not a failure. */
  budgetPaused: boolean;
  /** Decisions waiting on a person in this project. */
  openProposals: number;
  /**
   * Gates that failed and were not rescued.
   *
   * A failure a rescue already picked up is being dealt with; one nobody has
   * picked up is waiting for a person and does not know it.
   */
  failedGatesWithoutRescue: number;
  /** Runs that stopped without finishing and without being asked to. */
  interruptedRuns: number;
  /** This project's workflow differs from the bundle it references. */
  workflowDrift: boolean;
}

export interface LeadingConcern {
  kind: ConcernKind;
  /**
   * How many, when the concern is a count of things.
   *
   * `null` for the ones that are a state rather than a quantity — the kill
   * switch is on or off, and a `0` beside it would read as a count of zero
   * rather than as "this does not count".
   */
  count: number | null;
}

/**
 * The one concern that leads, or calm, or nothing known yet.
 *
 * `null` in means the shell has not been told anything about this project yet,
 * and the answer is `unknown` rather than `calm`. They render differently and
 * they must: calm is a measurement, and an app that reports a measurement it has
 * not taken is worse than one that admits it is still reading.
 */
export function leadingConcern(concerns: ProjectConcerns | null): LeadingConcern {
  if (concerns === null) return { kind: "unknown", count: null };

  if (concerns.killSwitch) return { kind: "kill-switch", count: null };
  if (concerns.budgetPaused) return { kind: "budget-paused", count: null };
  if (concerns.openProposals > 0) {
    return { kind: "proposal-waiting", count: concerns.openProposals };
  }
  if (concerns.failedGatesWithoutRescue > 0) {
    return { kind: "gate-failed", count: concerns.failedGatesWithoutRescue };
  }
  if (concerns.interruptedRuns > 0) {
    return { kind: "run-interrupted", count: concerns.interruptedRuns };
  }
  if (concerns.workflowDrift) return { kind: "workflow-drift", count: null };
  return { kind: "calm", count: null };
}

/**
 * Which tone the top of the page takes.
 *
 * Named after the token, not after the feeling, so that a designer changing what
 * `paused` looks like changes it in one place. `calm` and `unknown` share no
 * tone at all — neither is an exception, and the design says the normal
 * disappears rather than being drawn quietly.
 */
export function toneFor(kind: ConcernKind): string | null {
  switch (kind) {
    case "kill-switch":
      return "danger";
    case "budget-paused":
      return "paused";
    case "proposal-waiting":
      return "pending";
    case "gate-failed":
      return "danger";
    case "run-interrupted":
      return "paused";
    case "workflow-drift":
      return "info";
    default:
      return null;
  }
}
