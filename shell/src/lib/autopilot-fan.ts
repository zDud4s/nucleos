import type { ProjectSummary } from "../data/system";
import { readState } from "../ui/state-map";
import { PROMOTION_ACTING, PROMOTION_EARNED, promotionBlocker } from "./mode";

/**
 * The Autopilot carousel's rules, geometry and curve, kept pure so they are tested without layout.
 *
 * Which card comes first, what the page's one sentence says, what each card says about its gate,
 * where each card sits for one value of the slide, and how long the slide takes. Everything here is
 * arithmetic over the roster rows the daemon sent: nothing the núcleo already decided (`promotable`,
 * `queue_full`) is recomputed, and nothing reads the DOM — the component draws what these answer.
 */

/* ------------------------------------------------------------------ order -- */

/**
 * How urgent one project is, lowest first: what needs you, what is acting, what has earned the
 * third setting, what is still watching, what is off.
 *
 * An exception is a full queue or a setting the núcleo refused, whatever the mode. `promotable`
 * only ranks a project still in shadow — an acting one has already been let out.
 */
export function urgencyOf(p: ProjectSummary, refused = false): number {
  if (p.queue_full || refused) return 0;
  if (p.mode === "active") return 1;
  if (p.mode === "shadow" && p.promotable) return 2;
  if (p.mode === "shadow") return 3;
  return 4;
}

/**
 * The roster's ids, most urgent first.
 *
 * A tie keeps the roster's own order: sorting ties by anything else would move two cards past each
 * other on a poll that changed nothing about either of them.
 */
export function urgencyOrder(
  rows: readonly ProjectSummary[],
  refused: ReadonlySet<string>,
): string[] {
  return rows
    .map((p, index) => ({ id: p.project_id, rank: urgencyOf(p, refused.has(p.project_id)), index }))
    .sort((a, b) => a.rank - b.rank || a.index - b.index)
    .map((row) => row.id);
}

/**
 * The order on screen, ranked once and then held.
 *
 * Re-sorting after every change would move the card somebody just acted on out from under their
 * hand, so a held id is never re-ranked: the ids already held keep their places (those that left the
 * roster leave the order), and only newcomers are ranked — among themselves, after everything held.
 * Nothing held yet is simply the urgency order.
 */
export function holdOrder(
  held: readonly string[],
  rows: readonly ProjectSummary[],
  refused: ReadonlySet<string>,
): string[] {
  if (held.length === 0) return urgencyOrder(rows, refused);
  const present = new Set(rows.map((p) => p.project_id));
  const holding = new Set(held);
  const kept = held.filter((id) => present.has(id));
  const newcomers = rows.filter((p) => !holding.has(p.project_id));
  return [...kept, ...urgencyOrder(newcomers, refused)];
}

/* --------------------------------------------------------------- readings -- */

/** Ids a sentence can carry — at most two, joined by "and" — or a count once it cannot. */
function named(list: readonly ProjectSummary[]): string {
  return list.length <= 2 ? list.map((p) => p.project_id).join(" and ") : `${list.length} projects`;
}

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

/**
 * The page's one sentence, in plain text; `undefined` until the roster has answered.
 *
 * Names only for what a glance must find — a full queue, and whatever is acting — and only while two
 * of them fit; everything else is a count. A headline that grows by a name whenever a project is
 * promoted moves the whole page under the hand that promoted it. Acting, shadow and off partition
 * the roster, so every project is counted once; "ready to be let out" is a sub-clause of shadow.
 */
export function headlineFor(
  rows: readonly ProjectSummary[],
  answered: boolean,
): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "no project is under autopilot";

  const full = rows.filter((p) => p.queue_full);
  const acting = rows.filter((p) => p.mode === "active");
  const shadow = rows.filter((p) => p.mode === "shadow");
  const ready = shadow.filter((p) => p.promotable);
  const off = rows.filter((p) => p.mode === "off");

  const parts: string[] = [];
  if (full.length === 1) parts.push(`${full[0].project_id}’s queue is full`);
  else if (full.length > 1) parts.push(`queues are full on ${named(full)}`);
  // Nothing acting is said rather than left out: it is the fact this page is opened to learn.
  if (acting.length === 0) parts.push("nothing is acting on its own");
  else parts.push(`${named(acting)} ${acting.length === 1 ? "acts on its own" : "act on their own"}`);
  if (shadow.length > 0) {
    const readyClause = ready.length > 0 ? `, ${ready.length} of them ready to be let out` : "";
    parts.push(`${shadow.length} watching in shadow${readyClause}`);
  }
  if (off.length > 0) parts.push(`${off.length} off`);
  return parts.join("; ");
}

/** The shadow record in one phrase: how many exercised classes clear the bar. */
export function evidence(p: ProjectSummary): string {
  return p.classes_total === 0
    ? "no action class recorded"
    : `${p.classes_ready} of ${p.classes_total} classes clear the bar`;
}

/** The visor's caption. Words, not a new colour, for a condition that is not a tone. */
export function caption(p: ProjectSummary): string {
  if (p.classes_total === 0) return "no evidence yet";
  if (p.classes_ready < p.classes_total) return `${p.classes_total - p.classes_ready} short of the bar`;
  if ((p.withheld_classes_ready ?? 0) === 0) return "no restraint shown yet";
  if (p.promotable) return "earned — it can be let out";
  return "not offered for promotion";
}

/**
 * One sentence a screen reader gets for a project — everything its card shows.
 *
 * It starts with the id, so a list of them is read by name first.
 */
export function describeProject(p: ProjectSummary, refused = false): string {
  const bits = [readState("autopilot", p.mode)?.label ?? p.mode];
  if (p.queue_full) bits.push("queue full");
  if (refused) bits.push("setting refused");
  const ceiling = p.wip_limit === null ? ", no ceiling" : ` of ${p.wip_limit}`;
  return `${p.project_id}: ${bits.join(", ")}; ${evidence(p)}; ${plural(p.pending, "shadow decision")} to review; ${plural(p.open_proposals, "proposal")} open${ceiling}`;
}

/**
 * What a card says about the way to the third setting, and whether that way is open.
 *
 * Every card answers. An acting project has passed the gate, so its card says how to stop it
 * instead; an earned one says so; any other is locked, explained in the daemon's own terms through
 * `promotionBlocker`. An absent withheld count reads as zero — the direction that keeps it locked.
 */
export function gateFor(p: ProjectSummary): { text: string; open: boolean } {
  if (p.mode === "active") return { text: PROMOTION_ACTING, open: true };
  if (p.promotable) return { text: PROMOTION_EARNED, open: true };
  return { text: promotionBlocker(p, p.withheld_classes_ready ?? 0), open: false };
}

/**
 * A folder, short enough for a card.
 *
 * The end of a path is what tells two projects apart, so a long one loses whole segments from its
 * middle and keeps its root and its last folders. Split on `\` when the path has one, else on `/`.
 * A last folder too long for any of that is cut in its own middle instead.
 */
export function shortPath(path: string, max: number): string {
  if (path.length <= max) return path;
  const sep = path.includes("\\") ? "\\" : "/";
  const parts = path.split(sep);
  for (let keep = parts.length - 2; keep >= 1; keep--) {
    const shortened = `${parts[0]}${sep}…${sep}${parts.slice(-keep).join(sep)}`;
    if (shortened.length <= max) return shortened;
  }
  const head = Math.ceil((max - 1) / 2);
  const tail = max - 1 - head;
  return `${path.slice(0, head)}…${tail > 0 ? path.slice(-tail) : ""}`;
}

/* ----------------------------------------------------------------- motion -- */

/** A CSS `cubic-bezier()`, solved for x so a script moves on the same curve as the stylesheet. */
export function cubicBezier(p1x: number, p1y: number, p2x: number, p2y: number): (x: number) => number {
  const cx = 3 * p1x;
  const bx = 3 * (p2x - p1x) - cx;
  const ax = 1 - cx - bx;
  const cy = 3 * p1y;
  const by = 3 * (p2y - p1y) - cy;
  const ay = 1 - cy - by;
  const sampleX = (t: number) => ((ax * t + bx) * t + cx) * t;
  const sampleY = (t: number) => ((ay * t + by) * t + cy) * t;
  const slopeX = (t: number) => (3 * ax * t + 2 * bx) * t + cx;
  return (x) => {
    if (x <= 0) return 0;
    if (x >= 1) return 1;
    // Newton's method first: a few steps land on almost every x.
    let t = x;
    for (let i = 0; i < 8; i++) {
      const error = sampleX(t) - x;
      if (Math.abs(error) < 1e-6) return sampleY(t);
      const slope = slopeX(t);
      if (Math.abs(slope) < 1e-6) break;
      t -= error / slope;
    }
    // Bisection where the slope is too flat for Newton.
    let lo = 0;
    let hi = 1;
    t = x;
    while (hi - lo > 1e-6) {
      const value = sampleX(t);
      if (Math.abs(value - x) < 1e-6) break;
      if (x > value) lo = t;
      else hi = t;
      t = (lo + hi) / 2;
    }
    return sampleY(t);
  };
}

/**
 * The app's one curve: `--ease`, `cubic-bezier(0.25, 1, 0.5, 1)` in `tokens.css`.
 *
 * An ease-out that settles rather than springs — a card that overshot its place would swing past
 * the reader and back.
 */
export const EASE = cubicBezier(0.25, 1, 0.5, 1);

/** The app's one slide duration, `--dur-slide`, in milliseconds. */
export const SLIDE_MS = 240;

/**
 * How long a slide of `steps` cards takes: `SLIDE_MS`, half as long again past one card, and none
 * at all under reduced motion or for a slide that goes nowhere — the fan is simply redrawn.
 */
export function slideDuration(steps: number, reduced: boolean): number {
  if (reduced || steps < 1e-3) return 0;
  return SLIDE_MS * (steps > 1.2 ? 1.5 : 1);
}

/* ----------------------------------------------------------------- layout -- */

/** The fan's measurements for one stage width, in pixels and degrees. */
export interface FanConfig {
  cardW: number;
  cardH: number;
  /** How far apart two neighbouring cards sit. */
  xStep: number;
  /** How far a card drops per step away from the focus. */
  y: number;
  /** How far a card turns per step away from the focus. */
  rot: number;
  /** How much smaller a card gets per step away from the focus. */
  scaleRed: number;
  /** How many cards each side are drawn whole. */
  reach: number;
  compact: boolean;
}

/** Sized by the stage, not the window: inside the app the rail takes 15rem of it. */
export function fanConfig(width: number): FanConfig {
  if (width < 560) {
    return { cardW: 184, cardH: 244, xStep: 108, y: 16, rot: 7, scaleRed: 0.07, reach: 1, compact: true };
  }
  if (width < 1000) {
    return { cardW: 232, cardH: 316, xStep: 146, y: 28, rot: 9, scaleRed: 0.09, reach: 2, compact: false };
  }
  return { cardW: 256, cardH: 316, xStep: 168, y: 34, rot: 10, scaleRed: 0.1, reach: 3, compact: false };
}

/**
 * Where card `i` sits relative to the focus, in cards, for one value of the slide.
 *
 * Three projects or more are a ring — the last card sits one to the left of the first, not a long
 * way to the right. Two are a line: a ring of two would put the same card on both sides.
 */
export function circularOffset(i: number, progress: number, total: number): number {
  if (total < 3) return i - progress;
  let d = (i - progress) % total;
  if (d > total / 2) d -= total;
  if (d < -total / 2) d += total;
  return d;
}

/**
 * The index a slide towards card `i` should end on, starting from `base`: the near way round a
 * ring, so the fan never travels the long way to reach a card one step behind it.
 */
export function nearestTarget(i: number, base: number, total: number): number {
  return Math.round(base + circularOffset(i, base, total));
}

/** Piecewise-linear interpolation of `x` over the points (`xs`, `ys`), flat beyond both ends. */
function lerp(x: number, xs: readonly number[], ys: readonly number[]): number {
  if (x <= xs[0]) return ys[0];
  for (let k = 1; k < xs.length; k++) {
    if (x <= xs[k]) return ys[k - 1] + ((x - xs[k - 1]) / (xs[k] - xs[k - 1])) * (ys[k] - ys[k - 1]);
  }
  return ys[ys.length - 1];
}

/** The transform and paint of one card. */
export interface CardPose {
  x: number;
  y: number;
  /** Degrees. */
  rot: number;
  scale: number;
  opacity: number;
  z: number;
  /** The darkening laid over a card away from the focus, 0 to 1. */
  dim: number;
  factsOpacity: number;
  hidden: boolean;
}

/**
 * The pose of a card `offset` cards from the focus.
 *
 * The card in focus is square to the reader — no turn, no drop, full size — and its neighbours fan
 * out turned, lowered, smaller and beneath it. On a ring, a card past the fan's reach fades over half
 * a step and is then hidden, so a ring of twelve never draws twelve cards on top of each other; a
 * line of two never fades.
 */
export function cardPose(offset: number, total: number, cfg: FanConfig): CardPose {
  const edge = total >= 3 ? Math.min(total / 2, cfg.reach + 0.5) : Infinity;
  const a = Math.abs(offset);
  const opacity = a <= edge - 0.5 ? 1 : a >= edge ? 0 : (edge - a) / 0.5;
  return {
    x: offset * cfg.xStep,
    y: a < 0.05 ? 0 : a * cfg.y,
    rot: a < 0.05 ? 0 : offset * cfg.rot,
    scale: 1 - a * cfg.scaleRed,
    opacity,
    z: Math.round(100 - a * 10),
    dim: lerp(a, [0, 0.5, 1, 2], [0, 0.18, 0.3, 0.55]),
    factsOpacity: lerp(a, [0, 0.6], [1, 0]),
    hidden: opacity === 0,
  };
}
