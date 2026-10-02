import type { CouncilView, RosterSeat } from "../data/council";

/**
 * What "Ask again" hands the composer: the question, the panel and the rounds
 * of a council already held, so asking it again is one press of Convene.
 *
 * Kept in a module variable rather than in the URL or the router state: it is
 * a one-shot hand-off between two renders of the same page, never a place
 * somebody can link to, and a reload that loses it loses nothing worth keeping.
 */
export type CouncilDraft = {
  question: string;
  chairman: RosterSeat | null;
  members: (RosterSeat | null)[];
  roles: Record<number, string>;
  rounds: number;
};

let pending: CouncilDraft | null = null;

/** Leave a draft for the next composer that mounts. */
export function offerDraft(draft: CouncilDraft): void {
  pending = draft;
}

/**
 * The draft on offer, without taking it. The composer's `useState`
 * initialisers read it, and StrictMode runs those twice — consuming here would
 * hand the second run nothing.
 */
export function peekDraft(): CouncilDraft | null {
  return pending;
}

/** Drop the draft once a composer has mounted with it. */
export function clearDraft(): void {
  pending = null;
}

/**
 * PURE: one recorded seat as the roster seat that would fill it again.
 *
 * An agent still in the catalogue is asked by id, so it brings its prompt and
 * persona back with it. A deleted agent becomes `null` — an unchosen row the
 * composer refuses to convene with — rather than its model quietly standing in
 * for an agent that is gone. A model-named seat keeps its model; `local` is the
 * one kind that stays local, as in `seatFromChoice`.
 */
function rosterSeatOf(kind: string, ref: string, agentId: string | null, agentName: string | null): RosterSeat | null {
  if (agentId !== null) return agentName !== null ? { agent: agentId } : null;
  return { kind: kind === "local" ? "local" : "cloud", ref };
}

/** PURE: the draft a held council asks again with. Roles are keyed by the row's position. */
export function draftFrom(view: CouncilView): CouncilDraft {
  const seats = [...view.seats].sort((a, b) => a.seat_idx - b.seat_idx);
  const roles: Record<number, string> = {};
  seats.forEach((seat, at) => {
    if (seat.role !== null && seat.role !== "") roles[at] = seat.role;
  });
  return {
    question: view.question,
    chairman: rosterSeatOf(view.chairman_kind, view.chairman_ref, view.chairman_agent_id, view.chairman_agent_name),
    // A council with no recorded seat still opens the picker on one empty row,
    // the picker's own starting shape.
    members:
      seats.length === 0
        ? [null]
        : seats.map((seat) => rosterSeatOf(seat.kind, seat.ref, seat.agent_id, seat.agent_name)),
    roles,
    rounds: view.rounds,
  };
}
