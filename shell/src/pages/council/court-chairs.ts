import { MAX_COUNCIL_SEATS } from "./court-constants";
import type { CourtDraft } from "./court-draft";

/**
 * Which setup chair each member of a just-convened council sat in.
 *
 * `requestFrom` compacts the members (chairs 0, 2, 5 become members 0, 1, 2) and
 * the daemon's view carries only the compact `seat_idx`, so the session room
 * cannot tell on its own where a seat came from. The setup page, which still
 * holds the draft at the moment the council is created, writes the answer here
 * under the new council's id and the session page reads it to settle each seat
 * in from its own chair. Shell-only and best effort: a council opened from the
 * list, or after a reload, has no entry and settles in from the compact chairs.
 */
const convenedFrom = new Map<string, number[]>();

/** The occupied chairs of `draft`, in the order `requestFrom` sends them as members. */
export function rememberChairs(councilId: string, draft: CourtDraft): void {
  const chairs: number[] = [];
  draft.chairs.forEach((occupant, chair) => {
    if (occupant !== null) chairs.push(chair);
  });
  convenedFrom.set(councilId, chairs);
}

/** The setup chair of each member, or `undefined` when this page never convened the council. */
export function chairsOf(councilId: string): number[] | undefined {
  return convenedFrom.get(councilId);
}

/** Whether `chairs` can place `n` members on the eight setup chairs. */
export function fitsRoom(chairs: number[], n: number): boolean {
  return chairs.length === n && chairs.every((c) => Number.isInteger(c) && c >= 0 && c < MAX_COUNCIL_SEATS);
}
