import {
  LIVE_LIST_LIMIT,
  type Collisions,
  type FleetExclusion,
  type HeldSlot,
  type Job,
  type OwnerRef,
  type ProjectConcurrency,
  type Proposal,
  type RunSearchResult,
} from "./api";

/**
 * What is known about a slot's owner.
 *
 * `unknown` and `orphaned` are different states and it matters that they are. The first is
 * ordinary — a listing failed, or came back full and may have been cut — and reads as *slot taken,
 * detail unavailable*. The second is a sign of a defect: a whole listing answered and the owner was
 * not in it, so the slot is leaked and waiting on `reconcile_orphaned_slots`. Collapsing the two
 * would teach the reader to ignore the second.
 */
export type SlotDetail =
  | { kind: "job"; job: Job }
  | { kind: "run"; run: RunSearchResult }
  | { kind: "unknown" }
  | { kind: "orphaned" };

/**
 * This slot's owner, or whatever can be said about it.
 *
 * The comparison uses the PAIR `(owner_kind, owner_id)` and never the id alone: job ids and run ids
 * come from different sequences and collide constantly, and a join on the id would give a run's
 * card the description of the job that shares its number.
 */
export function slotDetail(
  slot: HeldSlot,
  jobs: Job[] | null,
  runs: RunSearchResult[] | null,
  limit: number = LIVE_LIST_LIMIT,
): SlotDetail {
  if (slot.owner_kind === "job") {
    if (jobs === null) return { kind: "unknown" };
    const found = jobs.find((job) => job.id === slot.owner_id);
    if (found !== undefined) return { kind: "job", job: found };
    return jobs.length >= limit ? { kind: "unknown" } : { kind: "orphaned" };
  }
  if (runs === null) return { kind: "unknown" };
  const found = runs.find((run) => run.id === slot.owner_id);
  if (found !== undefined) return { kind: "run", run: found };
  return runs.length >= limit ? { kind: "unknown" } : { kind: "orphaned" };
}

/**
 * The columns, with the work in flight on the left.
 *
 * An idle project KEEPS its column, showing `0/N`: a column that vanishes when it empties makes the
 * layout jump every night that ends, and a project's position on screen is the one thing the reader
 * memorises.
 */
export function orderColumns(projects: ProjectConcurrency[]): ProjectConcurrency[] {
  return [...projects].sort((left, right) => {
    const busier = right.slots.length - left.slots.length;
    return busier !== 0 ? busier : left.project_id.localeCompare(right.project_id);
  });
}

export interface CollisionBadge {
  /** Distinct on screen: one says "this will collide", the other "this collided". */
  source: "observed" | "predicted";
  state: "collide" | "not_measured";
  paths: string[];
  /** With whom, when that is known. */
  others: OwnerRef[];
}

/**
 * What this card has to say about collision, per source.
 *
 * Three rules, and the third is the one worth the argument:
 *
 * 1. `collide` → one warning per source, naming the paths and with whom.
 * 2. `clean` → silence. It is the default reading of a card without a badge.
 * 3. `not_measured` → a muted warning, **but only if the project holds two or more slots**. With
 *    one there is nothing to collide with, and saying "not measured" about a question that does not
 *    arise is noise that teaches the reader to ignore the warning when it matters.
 *
 * The observed source comes first because it is the stronger one.
 */
export function collisionBadges(
  project: ProjectConcurrency,
  owner: OwnerRef,
): CollisionBadge[] {
  const couldCollide = project.slots.length >= 2;
  const order = ["observed", "predicted"] as const;
  const sources: Record<"observed" | "predicted", keyof Collisions> = {
    observed: "observed",
    predicted: "declared",
  };

  // The explicit type parameter is not decoration: without it TS infers `U` from the callback's
  // branches (`{state:"not_measured"}[] | {state:"collide"}[]`), which do not unify, and reports
  // TS2345.
  return order.flatMap<CollisionBadge>((source) => {
    const found = project.collision[sources[source]];
    if (found.state === "clean") return [];
    if (found.state === "not_measured") {
      return couldCollide
        ? [{ source, state: "not_measured" as const, paths: [], others: [] }]
        : [];
    }
    const mine = found.overlaps.filter(
      (overlap) => sameOwner(overlap.a, owner) || sameOwner(overlap.b, owner),
    );
    if (mine.length === 0) return [];
    return [
      {
        source,
        state: "collide" as const,
        paths: [...new Set(mine.flatMap((overlap) => overlap.paths))].sort(),
        others: mine.map((overlap) => (sameOwner(overlap.a, owner) ? overlap.b : overlap.a)),
      },
    ];
  });
}

function sameOwner(left: OwnerRef, right: OwnerRef): boolean {
  return left.kind === right.kind && left.id === right.id;
}

/**
 * One "these two must not run at the same time", in either of its two lives.
 *
 * `pending` is a question somebody asked and `active` is a rule in force, and the screen must not
 * show them as the same thing: a pending edge is changing nothing at all yet, and drawing it as a
 * constraint would have somebody wondering why both jobs are still running.
 */
export interface ExclusionEdge {
  low: number;
  high: number;
  state: "pending" | "active";
  /** The proposal to answer while pending, and the rule to lift once active. */
  id: number;
}

/**
 * Every edge the canvas can draw, from the rules in force and the requests still waiting.
 *
 * Active wins over pending for the same pair. That combination is not supposed to arise — the daemon
 * refuses a second request for a pair that already has a rule — but drawing one pair twice is a
 * defect a reader would have to diagnose, and of the two the rule is the one that is actually doing
 * something.
 *
 * A proposal whose `tool_input` does not parse is DROPPED rather than guessed at. It is not a shape
 * this daemon writes, so the honest reading is that whatever wrote it is not something this screen
 * knows how to draw.
 */
export function exclusionEdges(
  rules: FleetExclusion[] | null,
  proposals: Proposal[] | null,
): ExclusionEdge[] {
  const edges: ExclusionEdge[] = (rules ?? []).map((rule) => ({
    low: rule.job_low,
    high: rule.job_high,
    state: "active" as const,
    id: rule.id,
  }));
  const seen = new Set(edges.map((edge) => `${edge.low}:${edge.high}`));

  for (const proposal of proposals ?? []) {
    if (proposal.kind !== "fleet-exclusion" || proposal.status !== "pending") continue;
    const pair = requestedPair(proposal);
    if (pair === null || seen.has(`${pair.low}:${pair.high}`)) continue;
    seen.add(`${pair.low}:${pair.high}`);
    edges.push({ ...pair, state: "pending", id: proposal.id });
  }
  return edges;
}

function requestedPair(proposal: Proposal): { low: number; high: number } | null {
  if (proposal.tool_input === null) return null;
  try {
    const input: unknown = JSON.parse(proposal.tool_input);
    if (typeof input !== "object" || input === null) return null;
    const { job_low: low, job_high: high } = input as Record<string, unknown>;
    if (typeof low !== "number" || typeof high !== "number") return null;
    return { low, high };
  } catch {
    return null;
  }
}

/** One job's side of an edge: who it is tied to, and whether this job is the one that waits. */
export interface Partner extends ExclusionEdge {
  partner: number;
  /**
   * Whether THIS job is the one held back. Only the higher id waits, so the same edge reads
   * differently from its two ends, and a card that said "waiting on the other" at both ends would
   * describe a deadlock the daemon cannot produce.
   */
  waits: boolean;
}

export function partnersOf(edges: ExclusionEdge[], jobId: number): Partner[] {
  return edges
    .filter((edge) => edge.low === jobId || edge.high === jobId)
    .map((edge) => ({
      ...edge,
      partner: edge.low === jobId ? edge.high : edge.low,
      waits: edge.high === jobId,
    }));
}
