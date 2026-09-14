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
  /** One item of a job a team directs, named by the job it belongs to. */
  | { kind: "item"; job: Job; ordinal: number; status: string }
  | { kind: "unknown" }
  | { kind: "orphaned" };

/**
 * This slot's owner, or whatever can be said about it.
 *
 * The comparison uses the pair `(owner_kind, owner_id)` and never the id alone,
 * for the reason {@link ownerKey} gives: a join on the id would give a run's
 * card the description of the job that shares its number.
 *
 * **An item is resolved through its JOB and never through its own id**, and the
 * arm is explicit for that reason. It used to be the `else` — anything that was
 * not a job was looked up among the runs — so an item's slot took the
 * description of whatever run happened to carry its number, which is exactly the
 * defect the paragraph above is about. And `owner_id` for an item is
 * `job_items.id`, which is a number from a sequence nobody reads and which no
 * route lists. What makes this answerable is the pair the daemon joins onto the
 * slot: the job to point at, and which of its items this is.
 *
 * An item whose `job_id` is absent has lost the row it was a step of, which is a
 * leaked slot rather than a description problem — `unknown` until the sweep
 * takes it back, because `orphaned` is a claim about a LISTING that answered
 * without it, and no listing was consulted.
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
  if (slot.owner_kind === "item") {
    const { job_id: owner, ordinal, item_status: status } = slot;
    if (owner === null || ordinal === null || status === null) return { kind: "unknown" };
    if (jobs === undefined) return { kind: "unknown" };
    const found = jobs.find((job) => job.id === owner);
    if (found !== undefined) return { kind: "item", job: found, ordinal, status };
    return jobs.length >= limit ? { kind: "unknown" } : { kind: "orphaned" };
  }
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
 * What is wrong, or waiting, in one project — counted, so the page can say it before it draws it.
 *
 * Five facts and not a severity score: the headline names each one in its own words, and a
 * number that summed them would say "3" about a leak, a conflict and a pending approval that ask
 * three different things of the reader.
 */
export interface Exceptions {
  /** Slots whose owner a complete listing did not return: leaked, awaiting the sweep. */
  leaked: number;
  /** Items of a directed job whose merge into the job's branch conflicted. */
  conflicted: number;
  /** Pairs of trees the núcleo MEASURED overlapping. A prediction is not counted here. */
  collided: number;
  /** Jobs parked on a person's approval. */
  awaiting: number;
  /** Jobs held back by an exclusion rule in force. */
  excluded: number;
}

export function noExceptions(): Exceptions {
  return { leaked: 0, conflicted: 0, collided: 0, awaiting: 0, excluded: 0 };
}

/**
 * How bad a column's worst fact is, as a rank: lower is worse.
 *
 * Four rungs, in the order the headline says them. A fault — a leaked slot, two trees measured
 * writing the same file — outranks an item put down on a merge conflict, which is not a fault
 * (`core/src/job.rs`, `ItemState::Conflicted`: "a conflict is a question about two pieces of
 * work, not a verdict on either") but is work that stopped; that outranks a job waiting on a
 * person, which outranks a job a rule is holding back. Nothing at all is last.
 */
export function severity(found: Exceptions): number {
  if (found.leaked + found.collided > 0) return 0;
  if (found.conflicted > 0) return 1;
  if (found.awaiting > 0) return 2;
  if (found.excluded > 0) return 3;
  return 4;
}

/**
 * The columns, exceptions first, then the busiest, then by name.
 *
 * Exceptions first so that a problem in a quiet project never drifts off the right of the screen
 * behind three busy ones that are fine: exceptions dominate, the normal recedes (PRODUCT.md,
 * principle 2). Busyness second, because among projects with nothing wrong the one doing the most
 * is the one being watched. The name last, so two equal columns never swap places on a tick.
 *
 * An idle project is not dropped here. It is the page that lists idle projects apart from the
 * busy columns — this order is about which busy column comes first, and a project's position
 * among its peers is still the thing a reader memorises.
 */
export function orderColumns(columns: FleetColumn[]): FleetColumn[] {
  return [...columns].sort((left, right) => {
    const worse = severity(left.exceptions) - severity(right.exceptions);
    if (worse !== 0) return worse;
    const busier = right.project.slots.length - left.project.slots.length;
    return busier !== 0 ? busier : left.project.project_id.localeCompare(right.project.project_id);
  });
}

/** One column's exceptions, read off its cards and its project's own collision reading. */
export function exceptionsOf(project: ProjectConcurrency, cards: SlotCardModel[]): Exceptions {
  const found = noExceptions();
  for (const card of cards) {
    if (card.detail.kind === "orphaned") found.leaked += 1;
    // Off the SLOT, not off the detail: the daemon joins `item_status` onto the slot, so a
    // conflict is known even when the jobs listing that would describe the item failed.
    if (card.slot.owner_kind === "item" && card.slot.item_status === "conflicted") {
      found.conflicted += 1;
    }
    if (card.detail.kind === "job") {
      if (card.detail.job.status === "awaiting_approval") found.awaiting += 1;
      if (card.detail.job.status === "waiting" && card.detail.job.wait_reason === "excluded") {
        found.excluded += 1;
      }
    }
  }
  if (project.collision.observed.state === "collide") {
    found.collided = project.collision.observed.overlaps.length;
  }
  return found;
}

function addExceptions(into: Exceptions, more: Exceptions): Exceptions {
  return {
    leaked: into.leaked + more.leaked,
    conflicted: into.conflicted + more.conflicted,
    collided: into.collided + more.collided,
    awaiting: into.awaiting + more.awaiting,
    excluded: into.excluded + more.excluded,
  };
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
 * How far apart the fallback positions sit.
 *
 * **A cell has to be bigger than a card.** The first pass at this in the old
 * shell used a cell exactly a card wide, and two nodes in the same grid column
 * overlapped on the very first paint — which jsdom cannot see, because it does
 * no layout. `ZONE_HEAD` is the room above a row for its project's label.
 */
const STEP_X = 320;
const STEP_Y = 400;
const MARGIN = 24;
export const ZONE_HEAD = 36;
export const ZONE_PAD = 12;

/**
 * The position of every live node, saved or derived — one row per project.
 *
 * A row per project because the canvas now draws each project as a labelled region, and a
 * region is only a region if its cards arrive next to each other. The positions used to be a
 * hash of the key over a 4×4 grid, which kept each card still but scattered a project across
 * the whole surface, so the one fact the columns carry — whose slots these are — was the one
 * fact the canvas could not show.
 *
 * `rows` is the busy projects' card keys, **sorted by project id and then by slot**, and the
 * sort is the caller's promise rather than an accident: both are stable across ticks, so a card
 * that nobody moved stays where it arrived for as long as its project and its slot do. The one
 * dent in that is a project emptying — the rows below it close up — and it is cheaper to accept
 * than an empty band left on the surface for every project that finished.
 *
 * A saved position always wins. It is somebody's arrangement, and a derivation has no business
 * overruling it.
 */
export function positionsFor(rows: string[][], saved: Layout): Layout {
  const layout: Layout = {};
  rows.forEach((keys, row) => {
    keys.forEach((key, column) => {
      layout[key] = saved[key] ?? {
        x: MARGIN + ZONE_PAD + column * STEP_X,
        y: MARGIN + ZONE_HEAD + row * STEP_Y,
      };
    });
  });
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
  /** Only the warnings about THIS owner. A project-wide `not_measured` is the column's. */
  badges: CollisionBadge[];
  partners: Partner[];
  end: ConnectionEnd;
  /**
   * The other jobs of this project this job could be asked never to share a slot with.
   *
   * The keyboard's way to the question the canvas asks by dragging a line: same project, both
   * jobs, and no rule or request already joining the pair — the three things `isValidConnection`
   * and the daemon's refusals check, answered before the control is drawn. Empty for anything
   * that is not a job.
   */
  peers: number[];
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
  exceptions: Exceptions;
  /**
   * What is true of the whole project rather than of one card — today, a source that did not
   * measure overlap. Said once, in the column head, instead of once per card: four cards
   * repeating "predicted — overlap not measured" was one fact taking four lines.
   */
  notes: CollisionBadge[];
  /** Nothing held. The page folds these into one quiet line under the busy columns. */
  idle: boolean;
}

export interface FleetModel {
  /** Every project, exceptions first — busy and idle alike. */
  columns: FleetColumn[];
  nodes: SlotNode[];
  edges: ExclusionFlowEdge[];
  /** Every edge, in either life, so a card can say what it is tied to. */
  exclusions: ExclusionEdge[];
  /** Node key → end, so a dropped line can be judged without walking the nodes. */
  ends: Record<string, ConnectionEnd>;
  /** Every column's exceptions, summed — the page headline's material. */
  totals: Exceptions;
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
  const joined = new Set(exclusions.map((edge) => `${edge.low}:${edge.high}`));

  const unordered: FleetColumn[] = (input.concurrency?.projects ?? []).map((project) => {
    const details = project.slots.map((slot) => slotDetail(slot, input.jobs, input.runs));
    const jobIds = details.flatMap((detail) => (detail.kind === "job" ? [detail.job.id] : []));
    const cards: SlotCardModel[] = project.slots.map((slot, index) => {
      const detail = details[index];
      const jobId = detail.kind === "job" ? detail.job.id : null;
      const key = ownerKey(slot);
      return {
        key,
        project,
        slot,
        detail,
        badges: collisionBadges(project, { kind: slot.owner_kind, id: slot.owner_id }).filter(
          (badge) => badge.state === "collide",
        ),
        partners: jobId === null ? [] : partnersOf(exclusions, jobId),
        end: { key, projectId: project.project_id, jobId },
        peers:
          jobId === null
            ? []
            : jobIds.filter(
                (other) =>
                  other !== jobId &&
                  !joined.has(`${Math.min(other, jobId)}:${Math.max(other, jobId)}`),
              ),
      };
    });
    return {
      project,
      cards,
      exceptions: exceptionsOf(project, cards),
      // The owner is nobody, so only the project-wide rows survive: a `collide` names owners,
      // and one that named nobody would be a warning about no card.
      notes: collisionBadges(project, { kind: "", id: -1 }).filter(
        (badge) => badge.state === "not_measured",
      ),
      idle: project.slots.length === 0,
    };
  });
  const columns = orderColumns(unordered);

  const cards = columns.flatMap((column) => column.cards);
  // By project id and by slot, NOT in the exceptions-first order above: a column may move
  // across the page when something goes wrong in it, but a region on the canvas must not jump
  // rows because its project's news changed.
  const rows = [...columns]
    .filter((column) => column.cards.length > 0)
    .sort((left, right) => left.project.project_id.localeCompare(right.project.project_id))
    .map((column) =>
      [...column.cards].sort((left, right) => left.slot.slot - right.slot.slot).map((card) => card.key),
    );
  const positions = positionsFor(rows, input.layout);
  const nodes: SlotNode[] = cards.map((card) => ({
    id: card.key,
    type: "slotCard",
    position: positions[card.key],
    data: { card },
    // xyflow names the node wrapper with this, and the wrapper is what takes focus on the
    // surface — without it a screen reader lands on a node that is called nothing at all.
    ariaLabel: nodeName(card),
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
        // The library's own default is "Edge from job:55 to job:56": two storage keys and a
        // direction the rule does not have. An exclusion is symmetric, and this is what it says.
        ariaLabel: edgeName(exclusion),
      },
    ];
  });

  const totals = columns.reduce(
    (sum, column) => addExceptions(sum, column.exceptions),
    noExceptions(),
  );
  return { columns, nodes, edges, exclusions, ends, totals };
}

/** A canvas node's accessible name: whose slot, in which project. */
export function nodeName(card: SlotCardModel): string {
  const { detail, slot, project } = card;
  const owner =
    detail.kind === "item"
      ? `item ${detail.ordinal + 1} of job ${detail.job.id}`
      : `${slot.owner_kind} ${slot.owner_id}`;
  return `${project.project_id}, slot ${slot.slot} — ${owner}`;
}

/** An exclusion edge's accessible name, in the words the cards use. */
export function edgeName(exclusion: ExclusionEdge): string {
  return exclusion.state === "active"
    ? `job ${exclusion.low} and job ${exclusion.high} never run at the same time`
    : `asked: job ${exclusion.low} and job ${exclusion.high} never at the same time — waiting for a decision`;
}

/* ----------------------------------------------------------------- zones -- */

/** One project's region on the canvas: where it is drawn, and what its label says. */
export interface Zone {
  projectId: string;
  /** `alpha 2/3` — the column head's reading, so the two views agree. */
  label: string;
  /** A project-wide fact, when there is one — the column head's note, said once here too. */
  note: string | null;
  x: number;
  y: number;
  width: number;
  height: number;
}

/** What a card measures before xyflow has measured it: the node's CSS width and a typical height. */
const CARD_FALLBACK = { width: 304, height: 200 };

/**
 * The region around each project's cards, from wherever those cards are NOW.
 *
 * Derived from the live positions and the measured sizes rather than laid out in advance, so a
 * region follows a card that somebody dragged and grows with one whose item list was opened.
 * A card with no measurement yet — the first frame, or jsdom, which measures nothing — counts at
 * the card's CSS width and a typical height, which is close enough for one frame.
 *
 * Regions of two projects can overlap if somebody drags a card deep into another project's
 * space. That is left alone: it is their arrangement, and a region that pushed cards around to
 * keep itself tidy would be the canvas overruling a person.
 */
export function zonesFor(
  nodes: Array<Pick<SlotNode, "position" | "data"> & { measured?: { width?: number; height?: number } }>,
): Zone[] {
  const byProject = new Map<string, { project: ProjectConcurrency; nodes: typeof nodes }>();
  for (const node of nodes) {
    const project = node.data.card.project;
    const entry = byProject.get(project.project_id) ?? { project, nodes: [] };
    entry.nodes.push(node);
    byProject.set(project.project_id, entry);
  }
  return [...byProject.values()].map(({ project, nodes: members }) => {
    let left = Infinity;
    let top = Infinity;
    let right = -Infinity;
    let bottom = -Infinity;
    for (const node of members) {
      const width = node.measured?.width ?? CARD_FALLBACK.width;
      const height = node.measured?.height ?? CARD_FALLBACK.height;
      left = Math.min(left, node.position.x);
      top = Math.min(top, node.position.y);
      right = Math.max(right, node.position.x + width);
      bottom = Math.max(bottom, node.position.y + height);
    }
    const unmeasured = collisionBadges(project, { kind: "", id: -1 }).some(
      (badge) => badge.state === "not_measured",
    );
    return {
      projectId: project.project_id,
      label: `${project.project_id} ${project.slots.length}/${project.limit}`,
      note: unmeasured ? "overlap not measured" : null,
      x: left - ZONE_PAD,
      y: top - ZONE_HEAD,
      width: right - left + 2 * ZONE_PAD,
      height: bottom - top + ZONE_HEAD + ZONE_PAD,
    };
  });
}
