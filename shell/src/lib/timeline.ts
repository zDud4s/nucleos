import type { FeedEntry, FeedSeen } from "../data/feed";
import { UI_LOCALE } from "./locale";

/**
 * The Feed's arithmetic, pure: which window to draw, where its ticks and silences fall, and how
 * the list under the trace folds. Which lines make one sequence is `sequences.ts`.
 *
 * Out of the page for the reason `calendar-grid.ts` is: every one of these is a decision about
 * time that is easy to get subtly wrong (a midnight, a clamp, a gap measured from the wrong line)
 * and cheap to test without rendering anything. Lanes and gravity are NOT here — they are claims
 * about núcleo kinds and live beside the state map (`ui/lanes.ts`); this module is handed
 * them as functions.
 */

export const MINUTE = 60_000;
export const HOUR = 60 * MINUTE;
export const DAY = 24 * HOUR;

/* ---------------------------------------------------------------- window -- */

/** The three windows the page offers. */
export type FeedPreset = "seen" | "day" | "week";

/**
 * "Since you looked" is clamped on both sides, and each clamp is said on screen.
 *
 * Twelve hours at least, because a marker from ten minutes ago would draw an axis of ten minutes —
 * nothing to read a pattern in, and the night you came back to ask about already off the left
 * edge. Seven days at most, because a marker from a month ago would ask the route for a month
 * and draw marks a pixel apart.
 */
export const SEEN_WINDOW_MIN = 12 * HOUR;
export const SEEN_WINDOW_MAX = 7 * DAY;

export interface ResolvedWindow {
  preset: FeedPreset;
  /** Epoch ms. The window reaches from here to now. */
  start: number;
  /** When you last looked, if the daemon knows — the shaded region's left edge. */
  lookedAt: number | null;
  /**
   * How the start was arrived at, for the sentence that names the window:
   * `marker` is the marker itself, `min`/`max` are the two clamps, `unmarked` is no marker at all.
   */
  basis: "marker" | "min" | "max" | "unmarked" | "fixed";
}

/**
 * When the reader last looked, per the marker: `seen_at`, falling back to the marked line's time.
 *
 * `seen_at` and not `through_created_at` first, because the two differ by the quiet between the
 * last line and the moment it was marked — and "you looked · 21:10" is a sentence about the
 * moment. Every line written in that quiet would have had an id past `through`, so shading from
 * the later of the two never hides an unseen line.
 */
export function lookedAtOf(seen: FeedSeen | null | undefined): number | null {
  const at = seen?.seen_at ?? seen?.through_created_at ?? null;
  if (at === null) return null;
  const ms = Date.parse(at);
  return Number.isNaN(ms) ? null : ms;
}

export function resolveWindow(preset: FeedPreset, now: number, seen: FeedSeen | null | undefined): ResolvedWindow {
  const lookedAt = lookedAtOf(seen);
  if (preset === "day") return { preset, start: now - DAY, lookedAt, basis: "fixed" };
  if (preset === "week") return { preset, start: now - 7 * DAY, lookedAt, basis: "fixed" };
  if (lookedAt === null) return { preset, start: now - DAY, lookedAt, basis: "unmarked" };
  if (now - lookedAt < SEEN_WINDOW_MIN) return { preset, start: now - SEEN_WINDOW_MIN, lookedAt, basis: "min" };
  if (now - lookedAt > SEEN_WINDOW_MAX) return { preset, start: now - SEEN_WINDOW_MAX, lookedAt, basis: "max" };
  return { preset, start: lookedAt, lookedAt, basis: "marker" };
}

/* ------------------------------------------------------------ formatting -- */

const pad = (n: number) => String(n).padStart(2, "0");

/** `21:10`, local, 24-hour — the daemon's instants read in the machine's own clock. */
export function clock(ms: number): string {
  const at = new Date(ms);
  return `${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/** A local calendar day, as a key. */
export function dayKey(ms: number): string {
  const at = new Date(ms);
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`;
}

function startOfDay(ms: number): number {
  const at = new Date(ms);
  at.setHours(0, 0, 0, 0);
  return at.getTime();
}

const WEEKDAY_SHORT = new Intl.DateTimeFormat(UI_LOCALE, { weekday: "short" });
const WEEKDAY_LONG = new Intl.DateTimeFormat(UI_LOCALE, { weekday: "long" });
const DAY_MONTH = new Intl.DateTimeFormat(UI_LOCALE, { day: "numeric", month: "long" });

/** `21:10` today, `Mon 21:10` on any other day — the shortest stamp that is still unambiguous. */
export function stamp(ms: number, now: number): string {
  if (dayKey(ms) === dayKey(now)) return clock(ms);
  return `${WEEKDAY_SHORT.format(ms)} ${clock(ms)}`;
}

/** A day group's heading: `Today`, `Yesterday` or the weekday, and the full date beside it. */
export function dayHeading(ms: number, now: number): { title: string; date: string } {
  const date = `${WEEKDAY_LONG.format(ms)} ${DAY_MONTH.format(ms)}`;
  const days = Math.round((startOfDay(now) - startOfDay(ms)) / DAY);
  if (days === 0) return { title: "Today", date };
  if (days === 1) return { title: "Yesterday", date };
  return { title: WEEKDAY_LONG.format(ms), date: DAY_MONTH.format(ms) };
}

/**
 * A span of time as it is said aloud: `4 h 05`, `35 min`, `2 d 3 h`.
 *
 * Minutes are zero-padded after hours because "4 h 5" reads as a typo; the unit after them is
 * the caller's (`quiet 4 h 05` on the axis, where room is short, `4 h 05 min` in the list).
 */
export function span(ms: number): string {
  const minutes = Math.max(0, Math.round(ms / MINUTE));
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ${pad(minutes % 60)}`;
  const days = Math.floor(hours / 24);
  return hours % 24 === 0 ? `${days} d` : `${days} d ${hours % 24} h`;
}

/* ----------------------------------------------------------------- ticks -- */

export interface Tick {
  at: number;
  label: string;
  /** A local midnight — drawn solid, and labelled with the day it opens. */
  midnight: boolean;
}

/** Steps an axis may use, finest first. */
const STEPS = [HOUR, 2 * HOUR, 3 * HOUR, 6 * HOUR, 12 * HOUR, DAY, 2 * DAY];

/** The least room a tick label gets. `21:00` in the mono face at 11px is ~36px; this is two of it. */
export const TICK_MIN_PX = 72;

/**
 * Ticks for an axis `width` pixels wide over `[start, end]`, on local clock boundaries.
 *
 * The finest step that still leaves {@link TICK_MIN_PX} between labels: hours over a night,
 * days over a week. Hour ticks sit on hours divisible by the step, so a 3-hour axis reads
 * 00 · 03 · 06 rather than 01 · 04 · 07, and a midnight is labelled by the day it opens — the
 * day boundary is the one fact about a long axis a reader loses first.
 */
export function timeTicks(start: number, end: number, width: number): Tick[] {
  const extent = Math.max(end - start, MINUTE);
  const step = STEPS.find((candidate) => (candidate / extent) * width >= TICK_MIN_PX) ?? STEPS[STEPS.length - 1];
  const ticks: Tick[] = [];
  if (step < DAY) {
    const first = new Date(start);
    first.setMinutes(0, 0, 0);
    if (first.getTime() < start) first.setHours(first.getHours() + 1);
    const stepHours = step / HOUR;
    for (let at = first.getTime(); at <= end; at += HOUR) {
      const hours = new Date(at).getHours();
      if (hours % stepHours !== 0) continue;
      const midnight = hours === 0;
      ticks.push({ at, label: midnight ? WEEKDAY_SHORT.format(at) : `${pad(hours)}:00`, midnight });
    }
    return ticks;
  }
  const stepDays = step / DAY;
  let index = 0;
  for (let at = startOfDay(start) + (startOfDay(start) < start ? DAY : 0); at <= end; ) {
    if (index % stepDays === 0) {
      ticks.push({ at, label: `${WEEKDAY_SHORT.format(at)} ${new Date(at).getDate()}`, midnight: true });
    }
    index += 1;
    // Via the calendar and not `+= DAY`, so a daylight-saving night does not walk the ticks an
    // hour off midnight for the rest of the axis.
    const next = new Date(at);
    next.setDate(next.getDate() + 1);
    at = next.getTime();
  }
  return ticks;
}

/* ---------------------------------------------------------------- gaps -- */

/** How long nothing has to happen, across every lane, for the silence to be named. */
export const QUIET_GAP = 2 * HOUR;

export interface QuietGap {
  from: number;
  to: number;
}

/**
 * Silences longer than {@link QUIET_GAP} between two consecutive lines.
 *
 * Between lines only, not from the window's edge to its first line: a window that opens on
 * silence says so through its empty stretch of axis, and a label there would be measuring the
 * window the reader chose rather than anything the machine did.
 */
export function quietGaps(times: number[], minimum = QUIET_GAP): QuietGap[] {
  const sorted = [...times].sort((a, b) => a - b);
  const gaps: QuietGap[] = [];
  for (let i = 1; i < sorted.length; i += 1) {
    if (sorted[i] - sorted[i - 1] > minimum) gaps.push({ from: sorted[i - 1], to: sorted[i] });
  }
  return gaps;
}

/* ------------------------------------------------------------------ list -- */

export type FeedListItem =
  | { type: "line"; entry: FeedEntry }
  | { type: "routine"; key: string; entries: FeedEntry[] }
  | { type: "gap"; from: number; to: number }
  | { type: "seen"; at: number | null };

export interface FeedDay {
  key: string;
  /** Any instant inside the day, for its heading. */
  at: number;
  items: FeedListItem[];
}

/**
 * The drill-down list, newest first: grouped by local day, routine folded, silences and the seen
 * marker drawn between the lines they fall between.
 *
 * A run of consecutive routine lines folds into one row; a single routine line between two
 * exceptions stays a line, because "1 routine" is a longer way to say less. A fold is broken by
 * anything that is not routine — an exception, a day boundary, a silence, the seen rule — so a
 * folded row never hides WHERE something happened, only what was ordinary about it.
 *
 * `seenThrough` places the rule between the newest line you had seen and the oldest you had not.
 * `gapAfter` is `Infinity` when the list is narrowed to one lane or owner: a silence in a subset
 * is not a quiet machine, and saying "quiet for 4 h" there would be the list lying about the
 * whole from a part.
 */
export function buildFeedDays(
  entries: FeedEntry[],
  options: { isRoutine: (entry: FeedEntry) => boolean; seenThrough: number | null; lookedAt: number | null; gapAfter: number },
): FeedDay[] {
  const { isRoutine, seenThrough, lookedAt, gapAfter } = options;
  const newestFirst = [...entries].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at) || b.id - a.id);
  const days: FeedDay[] = [];
  let day: FeedDay | null = null;
  let routine: FeedEntry[] = [];
  let previous: FeedEntry | null = null;

  const flush = () => {
    if (day === null || routine.length === 0) return;
    if (routine.length === 1) day.items.push({ type: "line", entry: routine[0] });
    else day.items.push({ type: "routine", key: `routine-${routine[0].id}`, entries: routine });
    routine = [];
  };

  for (const entry of newestFirst) {
    const at = Date.parse(entry.created_at);
    const unseenAbove = previous !== null && seenThrough !== null && previous.id > seenThrough && entry.id <= seenThrough;
    if (previous === null && seenThrough !== null && entry.id <= seenThrough) {
      // Nothing in this window is newer than what you had seen: the rule opens the list.
      days.push((day = { key: dayKey(at), at, items: [{ type: "seen", at: lookedAt }] }));
    }
    if (unseenAbove) {
      flush();
      day?.items.push({ type: "seen", at: lookedAt });
    }
    if (day === null || day.key !== dayKey(at)) {
      flush();
      day = { key: dayKey(at), at, items: [] };
      days.push(day);
    } else if (previous !== null && Date.parse(previous.created_at) - at > gapAfter) {
      flush();
      day.items.push({ type: "gap", from: at, to: Date.parse(previous.created_at) });
    }
    if (isRoutine(entry)) routine.push(entry);
    else {
      flush();
      day.items.push({ type: "line", entry });
    }
    previous = entry;
  }
  flush();
  return days;
}
