import type { Edge, Node } from "@xyflow/react";
import {
  LIVE_LIST_LIMIT,
  type Collisions,
  type Concurrency,
  type FleetExclusion,
  type HeldSlot,
  type Job,
  type OwnerRef,
  type ProjectConcurrency,
  type RunSearchResult,
} from "../data/fleet";
import type { Proposal } from "../data/system";

/**
 * The fleet as data, with no React in it.
 *
 * Everything both views of the page draw is decided here: which cards exist,
 * what is known about each one, which lines join them, and where the nodes sit.
 * Two views rendering from two derivations is how a canvas and a column end up
 * disagreeing about the same job — so there is one derivation, and the views
 * are two arrangements of its output.
 *
 * Pure, and tested without mounting anything. The only import that is not a
 * type is `LIVE_LIST_LIMIT`, which is a number.
 */

/** A point on the canvas, in flow coordinates from the top left. */
export interface Point {
  x: number;
  y: number;
}

/** Where each node sits, keyed the same way the nodes are. */
export type Layout = Record<string, Point>;

/**
 * Identifies an owner across ticks, and identifies a node on the canvas.
 *
 * `"job:41"` and never `41`: job ids and run ids come from different sequences
 * and collide constantly, so a layout or a lookup keyed on the bare number
 * would drop a run on top of the job that shares it.
 */
export function ownerKey(slot: HeldSlot): string {
  return `${slot.owner_kind}:${slot.owner_id}`;
}

/**
 * What is known about a slot's owner.
 *
 * `unknown` and `orphaned` are different states and it matters that they are.
 * The first is ordinary — a listing failed, or came back full and may have been
 * cut — and reads as *slot taken, detail unavailable*. The second is a sign of
 * a defect: a whole listing answered and the owner was not in it, so the slot is
 * leaked and waiting on `reconcile_orphaned_slots`. Collapsing the two would
 * teach the reader to ignore the second.
 */
export type SlotDetail =
  | { kind: "job"; job: Job }
  | { kind: "run"; run: RunSearchResult }
  | { kind: "unknown" }
  | { kind: "orphaned" };

/**
 * This slot's owner, or whatever can be said about it.
 *
 * The comparison uses the pair `(owner_kind, owner_id)` and never the id alone,
 * for the reason {@link ownerKey} gives: a join on the id would give a run's
 * card the description of the job that shares its number.
 *
 * **An item is named and not resolved, and the arm is explicit for that reason.**
 * It used to be the `else` — anything that was not a job was looked up among the
 * runs — so an item's slot took the description of whatever run happened to
 * carry its number, which is the exact defect the paragraph above is about. The
 * page has no listing of items by id to look it up in: `GET /jobs/<id>` carries
 * them, and the id on the slot does not say which job to ask. So the honest
 * answer is `unknown` — *slot taken, detail unavailable* — and not `orphaned`,
 * which claims a listing answered without it.
 */
export function slotDetail(
  slot: HeldSlot,
  jobs: Job[] | undefined,
  runs: RunSearchResult[] | undefined,
  limit: number = LIVE_LIST_LIMIT,
): SlotDetail {
  if (slot.owner_kind === "job") {
    if (jobs === undefined) return { kind: "unknown" };
    const found = jobs.find((job) => job.id === slot.owner_id);
    if (found !== undefined) return { kind: "job", job: found };
    return jobs.length >= limit ? { kind: "unknown" } : { kind: "orphaned" };
  }
  if (slot.owner_kind === "item") return { kind: "unknown" };
  if (runs === undefined) return { kind: "unknown" };
  const found = runs.find((run) => run.id === slot.owner_id);
  if (found !== undefined) return { kind: "run", run: found };
  return runs.length >= limit ? { kind: "unknown" } : { kind: "orphaned" };
}

/** The literal `readState("slot", …)` reads, so the badge and the model cannot drift. */
export function slotStateLiteral(detail: SlotDetail): string | null {
  return detail.kind === "unknown" || detail.kind === "orphaned" ? detail.kind : null;
}

/**
 * The columns, with the work in flight on the left.
 *
 * An idle project **keeps** its column, showing `0/N`: a column that vanishes
 * when it empties makes the layout jump every night that ends, and a project's
 * position on screen is the one thing the reader memorises.
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
  /** Only the two that have something to say; `clean` is the silence around them. */
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
 * 3. `not_measured` → a muted warning, **but only if the project holds two or
 *    more slots**. With one there is nothing to collide with, and saying "not
 *    measured" about a question that does not arise is noise that teaches the
 *    reader to ignore the warning when it matters.
 *
 * The observed source comes first because it is the stronger one.
 */
export function collisionBadges(project: ProjectConcurrency, owner: OwnerRef): CollisionBadge[] {
  const couldCollide = project.slots.length >= 2;
  const order = ["observed", "predicted"] as const;
  const sources: Record<"observed" | "predicted", keyof Collisions> = {
    observed: "observed",
    predicted: "declared",
  };

  // The explicit type parameter is not decoration: without it TS infers the
  // element type from the callback's branches, which do not unify.
  return order.flatMap<CollisionBadge>((source) => {
    const found = project.collision[sources[source]];
    if (found.state === "clean") return [];
    if (found.state === "not_measured") {
      return couldCollide ? [{ source, state: "not_measured" as const, paths: [], others: [] }] : [];
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
 * `pending` is a question somebody asked and `active` is a rule in force, and
 * the screen must not show them as the same thing: a pending edge is changing
 * nothing at all yet, and drawing it as a constraint would have somebody
 * wondering why both jobs are still running.
 */
export interface ExclusionEdge {
  low: number;
  high: number;
  state: "pending" | "active";
  /** The proposal to answer while pending, and the rule to lift once active. */
  id: number;
}

/**
 * Every edge the page can draw, from the rules in force and the requests still
 * waiting.
 *
 * Active wins over pending for the same pair. That combination is not supposed
 * to arise — the daemon refuses a second request for a pair that already has a
 * rule — but drawing one pair twice is a defect a reader would have to
 * diagnose, and of the two the rule is the one actually doing something.
 *
 * A proposal whose `tool_input` does not parse is **dropped** rather than
 * guessed at: it is not a shape this daemon writes, so the honest reading is
 * that whatever wrote it is not something this screen knows how to draw.
 */
export function exclusionEdges(
  rules: FleetExclusion[] | undefined,
  proposals: Proposal[] | undefined,
): ExclusionEdge[] {
  const edges: ExclusionEdge[] = (rules ?? []).map((rule) => ({
    low: rule.job_low,
    high: rule.job_high,
    state: "active" as const,
    id: rule.id,
  }));
  const seen = new Set(edges.map((edge) => `${edge.low}:${edge.high}`));

  for (const proposal of proposals ?? []) {
    if (proposal.status !== "pending") continue;
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
   * Whether **this** job is the one held back. Only the higher id waits, so the
   * same edge reads differently from its two ends, and a card that said
   * "waiting on the other" at both ends would describe a deadlock the daemon
   * cannot produce.
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

/* ------------------------------------------------------------------ layout -- */

/**
 * How far apart the fallback positions sit, and how wide a row is before it
 * wraps.
 *
 * **A cell has to be bigger than a card.** The first pass at this in the old
 * shell used a cell exactly a card wide, and two nodes in the same grid column
 * overlapped on the very first paint — which jsdom cannot see, because it does
 * no layout.
 */
const STEP_X = 320;
const STEP_Y = 360;
const PER_ROW = 4;
const MARGIN = 24;

/** How many cells the derived positions spread over before they repeat. */
const CELLS = PER_ROW * PER_ROW;

/** Where the nth cell is. Defined for every n, not only the first `CELLS`. */
function cellPosition(index: number): Point {
  return {
    x: MARGIN + (index % PER_ROW) * STEP_X,
    y: MARGIN + Math.floor(index / PER_ROW) * STEP_Y,
  };
}

/**
 * A small, stable, order-independent hash of the key (FNV-1a, 32 bits).
 *
 * It is here so a node with no saved position is **derived from its key** and
 * never dropped at the origin: the first time somebody opens the canvas nothing
 * has a saved position, and a fleet piled in one corner reads as a broken
 * canvas. Derived and not random, so the arrangement a person learns stays
 * learned until they move something.
 */
function hash(key: string): number {
  let value = 0x811c9dc5;
  for (let index = 0; index < key.length; index += 1) {
    value ^= key.charCodeAt(index);
    value = Math.imul(value, 0x01000193) >>> 0;
  }
  return value;
}

/**
 * The position of every live node, saved or derived.
 *
 * Two keys that want the same cell are separated here, and that is a deliberate
 * dent in the "a node never moves because its neighbours changed" rule. It has
 * to be: any position derived from the key alone collides, and a card exactly
 * underneath another cannot be read, cannot be clicked, and cannot be dragged
 * out from under. The tie is broken by the key rather than by arrival order, so
 * it is the same node that gives way every time.
 */
export function positionsFor(nodeKeys: string[], saved: Layout): Layout {
  const layout: Layout = {};
  const taken = new Set<number>();
  for (const key of [...nodeKeys].sort()) {
    const chosen = saved[key];
    if (chosen !== undefined) {
      layout[key] = chosen;
      continue;
    }
    let cell = hash(key) % CELLS;
    while (taken.has(cell)) cell += 1;
    taken.add(cell);
    layout[key] = cellPosition(cell);
  }
  return layout;
}

/**
 * A whole-pixel position no node can be lost at.
 *
 * Nothing lives in negative space: a card dropped past the top or the left edge
 * is partly unreachable, and one dropped far enough past it is gone altogether
 * with no way back short of clearing storage by hand.
 */
export function clamped(at: Point): Point {
  return { x: Math.max(0, Math.round(at.x)), y: Math.max(0, Math.round(at.y)) };
}

/**
 * Drops entries for nodes that are no longer there.
 *
 * Called before writing rather than before reading — {@link positionsFor}
 * already ignores dead entries — so this exists only to stop the stored object
 * growing by one entry per job for the life of the machine.
 */
export function prunedLayout(layout: Layout, nodeKeys: string[]): Layout {
  const live = new Set(nodeKeys);
  const kept: Layout = {};
  for (const [key, point] of Object.entries(layout)) {
    if (live.has(key)) kept[key] = point;
  }
  return kept;
}

/**
 * Where the arrangement is kept.
 *
 * `.v2` is a fresh key rather than a migration of the old shell's
 * `nucleos.fleet.layout`. The coordinates mean something different now — they
 * are flow coordinates inside a pannable viewport, not pixels on a scrolling
 * surface — and a layout is cheap to redo, so importing numbers that no longer
 * mean what they meant would buy nothing and could scatter a fleet.
 */
const LAYOUT_KEY = "nucleos.fleet.layout.v2";

/**
 * Reads the saved layout, and treats anything it does not recognise as nothing.
 *
 * `localStorage` holds text somebody else wrote — a previous version of this
 * app, an extension, a person with the console open — so every field is checked
 * rather than trusted. **Nothing in here throws.** A malformed entry costs one
 * node its saved position; a malformed entry that got through would cost a
 * `NaN` in a transform, which draws nothing and reports nothing.
 */
export function loadLayout(storage: Pick<Storage, "getItem"> = localStorage): Layout {
  let raw: string | null = null;
  try {
    raw = storage.getItem(LAYOUT_KEY);
  } catch {
    // A locked-down profile answers by throwing. No layout, then.
    return {};
  }
  if (raw === null) return {};
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const layout: Layout = {};
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      const point = value as Partial<Point> | null;
      if (
        point !== null &&
        typeof point === "object" &&
        typeof point.x === "number" &&
        typeof point.y === "number" &&
        Number.isFinite(point.x) &&
        Number.isFinite(point.y)
      ) {
        layout[key] = { x: point.x, y: point.y };
      }
    }
    return layout;
  } catch {
    return {};
  }
}

/** Writes the layout, and says nothing when it cannot. */
export function saveLayout(
  layout: Layout,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  try {
    storage.setItem(LAYOUT_KEY, JSON.stringify(layout));
  } catch {
    // Private mode, a full quota, a locked-down profile. Losing an arrangement
    // is not worth taking the canvas down over, and there is nothing the person
    // could do about it if told.
  }
}

/* -------------------------------------------------------------- connection -- */

/**
 * One end of a line the reader is pulling.
 *
 * The project travels with the id because the rule is about projects, and the
 * canvas is the one place where two jobs of two different projects are visible
 * side by side.
 */
export interface ConnectionEnd {
  key: string;
  projectId: string;
  /** `null` for a run or an undescribed slot: an exclusion names two **jobs**. */
  jobId: number | null;
}

/**
 * May these two ends be joined?
 *
 * The daemon's three refusals, answered before the gesture rather than after
 * it: a job cannot be excluded from itself, both ends have to be jobs, and the
 * two must share a project or they share no slots to serialise. Offering a
 * gesture whose only possible outcome is a 400 is worse than not offering it.
 */
export function isValidConnection(
  a: ConnectionEnd | undefined,
  b: ConnectionEnd | undefined,
): boolean {
  if (a === undefined || b === undefined) return false;
  if (a.jobId === null || b.jobId === null) return false;
  if (a.key === b.key || a.jobId === b.jobId) return false;
  return a.projectId === b.projectId;
}

/* ------------------------------------------------------------- flow model -- */

/** Everything one card knows about itself, whichever view is drawing it. */
export interface SlotCardModel {
  key: string;
  project: ProjectConcurrency;
  slot: HeldSlot;
  detail: SlotDetail;
  badges: CollisionBadge[];
  partners: Partner[];
  end: ConnectionEnd;
}

/**
 * A node's payload.
 *
 * Extends `Record<string, unknown>` because xyflow's `Node` requires it; the
 * one real field is the card, so both views render from the same object.
 */
export interface SlotNodeData extends Record<string, unknown> {
  card: SlotCardModel;
}

export type SlotNode = Node<SlotNodeData, "slotCard">;

export interface ExclusionEdgeData extends Record<string, unknown> {
  exclusion: ExclusionEdge;
}

export type ExclusionFlowEdge = Edge<ExclusionEdgeData, "exclusion">;

/** One column: the project, and the cards its slots resolved to. */
export interface FleetColumn {
  project: ProjectConcurrency;
  cards: SlotCardModel[];
}

export interface FleetModel {
  columns: FleetColumn[];
  nodes: SlotNode[];
  edges: ExclusionFlowEdge[];
  /** Every edge, in either life, so a card can say what it is tied to. */
  exclusions: ExclusionEdge[];
  /** Node key → end, so a dropped line can be judged without walking the nodes. */
  ends: Record<string, ConnectionEnd>;
}

export interface FleetInput {
  concurrency: Concurrency | undefined;
  jobs: Job[] | undefined;
  runs: RunSearchResult[] | undefined;
  exclusions: FleetExclusion[] | undefined;
  requests: Proposal[] | undefined;
  layout: Layout;
}

/**
 * The whole page, derived once.
 *
 * Both views come out of this call, which is what stops the canvas and the
 * columns from disagreeing about a job. An edge whose two ends are not both on
 * screen is **dropped**: xyflow cannot draw a line to a node that is not there,
 * and a half-drawn constraint is worse than an absent one — the card still
 * carries the fact in words.
 */
export function buildFleet(input: FleetInput): FleetModel {
  const exclusions = exclusionEdges(input.exclusions, input.requests);
  const projects = orderColumns(input.concurrency?.projects ?? []);

  const columns: FleetColumn[] = projects.map((project) => ({
    project,
    cards: project.slots.map((slot) => {
      const detail = slotDetail(slot, input.jobs, input.runs);
      const jobId = detail.kind === "job" ? detail.job.id : null;
      const key = ownerKey(slot);
      return {
        key,
        project,
        slot,
        detail,
        badges: collisionBadges(project, { kind: slot.owner_kind, id: slot.owner_id }),
        partners: jobId === null ? [] : partnersOf(exclusions, jobId),
        end: { key, projectId: project.project_id, jobId },
      };
    }),
  }));

  const cards = columns.flatMap((column) => column.cards);
  const positions = positionsFor(
    cards.map((card) => card.key),
    input.layout,
  );
  const nodes: SlotNode[] = cards.map((card) => ({
    id: card.key,
    type: "slotCard",
    position: positions[card.key],
    data: { card },
  }));

  const ends: Record<string, ConnectionEnd> = {};
  for (const card of cards) ends[card.key] = card.end;

  const drawn = new Set(nodes.map((node) => node.id));
  const edges: ExclusionFlowEdge[] = exclusions.flatMap((exclusion) => {
    const source = `job:${exclusion.low}`;
    const target = `job:${exclusion.high}`;
    if (!drawn.has(source) || !drawn.has(target)) return [];
    return [
      {
        id: `${exclusion.state}:${exclusion.id}`,
        source,
        target,
        type: "exclusion" as const,
        data: { exclusion },
      },
    ];
  });

  return { columns, nodes, edges, exclusions, ends };
}
