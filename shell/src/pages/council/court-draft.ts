import type { ModelChoice } from "../../data/chats";
import type { ConfiguredSeat, CouncilConfig, RosterSeat } from "../../data/council";
import { offeredSeat, type CouncilDraft } from "../council-draft";
import { MAX_COUNCIL_SEATS } from "./court-constants";

/**
 * The courtroom while it is being set up: who sits in each of the eight
 * chairs, who presides, and what role each chair plays. PURE.
 *
 * `touched` is the request's property from `data/council.ts`'s header: the
 * room opens on the configured roster and, while nobody has moved anyone,
 * `POST /council` carries no `roster` — the request this page has always made.
 */
export interface Occupant {
  /** What travels; `null` for a configured seat that names no model (`ref: null`) and cannot be restated. */
  seat: RosterSeat | null;
  name: string;
}

export interface CourtDraft {
  chairman: Occupant | null;
  chairs: (Occupant | null)[];
  roles: (string | null)[];
  touched: boolean;
}

const emptyChairs = <T>(): (T | null)[] => Array.from({ length: MAX_COUNCIL_SEATS }, () => null);

export function configuredName(seat: ConfiguredSeat): string {
  if ("agent" in seat) return seat.agent;
  return seat.ref ?? seat.kind ?? "default";
}

export function occupantOfConfigured(seat: ConfiguredSeat): Occupant {
  if ("agent" in seat) return { seat: { agent: seat.agent }, name: seat.agent };
  const restated: RosterSeat | null = seat.ref === null ? null : { kind: seat.kind === "local" ? "local" : "cloud", ref: seat.ref };
  return { seat: restated, name: configuredName(seat) };
}

export function draftFromConfig(config: CouncilConfig | null): CourtDraft {
  const chairs = emptyChairs<Occupant>();
  const roster = config?.configured ? config.default_roster : null;
  roster?.members.slice(0, MAX_COUNCIL_SEATS).forEach((member, i) => { chairs[i] = occupantOfConfigured(member); });
  return {
    chairman: roster !== null && roster !== undefined ? occupantOfConfigured(roster.chairman) : null,
    chairs,
    roles: emptyChairs<string>(),
    touched: false,
  };
}

const nameOfSeat = (seat: RosterSeat) => ("agent" in seat ? seat.agent : seat.ref);

export function draftFromAskAgain(old: CouncilDraft): CourtDraft {
  const chairs = emptyChairs<Occupant>();
  const roles = emptyChairs<string>();
  old.members.slice(0, MAX_COUNCIL_SEATS).forEach((seat, i) => {
    chairs[i] = seat === null ? null : { seat, name: nameOfSeat(seat) };
    roles[i] = seat === null ? null : old.roles[i] ?? null;
  });
  return {
    chairman: old.chairman === null ? null : { seat: old.chairman, name: nameOfSeat(old.chairman) },
    chairs, roles, touched: true,
  };
}

export function seatAt(d: CourtDraft, chair: number, who: Occupant): CourtDraft {
  return { ...d, chairs: d.chairs.map((c, i) => (i === chair ? who : c)), touched: true };
}

export function release(d: CourtDraft, chair: number): CourtDraft {
  return {
    ...d,
    chairs: d.chairs.map((c, i) => (i === chair ? null : c)),
    roles: d.roles.map((r, i) => (i === chair ? null : r)),
    touched: true,
  };
}

/** Roles are not exclusive, and a role alone does not touch the roster: `roles` travels without one. */
export function withRole(d: CourtDraft, chair: number, role: string | null): CourtDraft {
  return { ...d, roles: d.roles.map((r, i) => (i === chair ? role : r)) };
}

export function withJudge(d: CourtDraft, who: Occupant): CourtDraft {
  return { ...d, chairman: who, touched: true };
}

export function occupiedCount(d: CourtDraft): number {
  return d.chairs.filter((c) => c !== null).length;
}

/** `local` is the one brain a seat may call local (see the old `seatFromChoice`). */
export function seatOfModel(model: ModelChoice): RosterSeat {
  return { kind: model.brain === "local" ? "local" : "cloud", ref: model.id };
}

/**
 * Seats the catalogues no longer offer become empty chairs (or no chairman).
 * Returns the SAME object when nothing changed, so an effect settles in one pass.
 */
export function pruneUnoffered(d: CourtDraft, agentIds: ReadonlySet<string>, modelIds: ReadonlySet<string>): CourtDraft {
  const keep = (o: Occupant | null): Occupant | null => {
    if (o === null || o.seat === null) return o;
    return offeredSeat(o.seat, agentIds, modelIds) === null ? null : o;
  };
  const chairman = keep(d.chairman);
  const chairs = d.chairs.map(keep);
  if (chairman === d.chairman && chairs.every((c, i) => c === d.chairs[i])) return d;
  return { ...d, chairman, chairs, roles: d.roles.map((r, i) => (chairs[i] === null ? null : r)) };
}
