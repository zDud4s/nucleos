// Pure adapters for the devtime turn-efficiency panel (metric 9, follow-up B). The daemon sends a
// project, a day and counts per slow turn, never a session id, a worktree, a prompt or a path
// (spec analytics-devtime §8).
import type { DevtimeTurnClasses, TurnClassRow, UnexplainedCriteria, UnexplainedItem } from "../data/devtime";
import { humanMinutes } from "../data/project-readings";
import { percentText } from "../lib/format";

/** At most this many slow turns are drawn by default. */
export const TOP_SLOW_TURNS = 10;

function calls(n: number): string {
  return n === 1 ? "1 call" : `${n} calls`;
}

/**
 * A class by its call range: "No tool calls", "1 call", "1–5 calls", "21+ calls". A class whose
 * range the daemon could not state (a token past its bounds) shows its raw token.
 */
export function classLabel(row: TurnClassRow): string {
  const { min_calls: min, max_calls: max } = row;
  if (min === null) return row.turn_class;
  if (max === null) return `${min}+ calls`;
  if (max === 0) return "No tool calls";
  if (min === max) return calls(min);
  return `${min}–${max} calls`;
}

/** Time per tool call: seconds under a minute, then the page's minutes/hours; "—" with no calls. */
export function perCallText(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms)) return "—";
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds} s`;
  return humanMinutes(Math.round(ms / 60_000));
}

/** The share of the class's active time a rule explains; "—" when it had no active time. */
export function explainedText(row: TurnClassRow): string {
  return percentText(row.explained_ratio);
}

/** The slow-turn criteria in one line, as the picker applies them. */
export function criteriaText(criteria: UnexplainedCriteria): string {
  const minutes = humanMinutes(Math.round(criteria.min_turn_ms / 60_000));
  return `At least ${criteria.median_multiple}× the median of turns of its size, ${minutes} or longer, ` +
    `at most ${percentText(criteria.max_explained_fraction)} explained by a rule · medians since ${criteria.medians_from}`;
}

/** The first `max` slow turns, in the daemon's order (largest first). */
export function slowTurns(items: UnexplainedItem[], max = TOP_SLOW_TURNS): UnexplainedItem[] {
  return items.slice(0, max);
}

/** Whether there is anything to show: a missing block (older daemon) or one with no class has nothing. */
export function hasTurnClasses(block: DevtimeTurnClasses | undefined): block is DevtimeTurnClasses {
  return block !== undefined && block.classes.length > 0;
}
