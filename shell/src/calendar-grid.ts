/**
 * The geometry of a calendar grid, as pure functions.
 *
 * Kept out of the component for the same reason `recurrence.rs` is kept out of `calendar.rs`: the
 * hard questions here are arithmetic, not rendering. A day is not always 24 hours, two meetings
 * that overlap have to be placed side by side, and a month grid has to include days from the
 * months either side of it. Each of those is a table test with no DOM.
 *
 * Everything takes explicit bounds rather than reading a clock or a zone, so the tests do not have
 * to run under a particular `TZ` to exercise a 23-hour day.
 */

const HOUR_MS = 3_600_000;

/** Where a block sits inside its day, as fractions of that day's own height. */
export interface Placement {
  top: number;
  height: number;
}

/**
 * The instants a local calendar day spans.
 *
 * On the two transition days of the year this is 23 or 25 hours, not 24, and it matters: a week
 * grid that assumes 24 draws every event on that day at the wrong height. JS `Date` arithmetic in
 * local time already accounts for the shift — the trick is to ASK for the next midnight rather
 * than adding 86 400 000 milliseconds to this one.
 */
export function dayBounds(day: Date): [Date, Date] {
  const start = new Date(day.getFullYear(), day.getMonth(), day.getDate());
  const end = new Date(day.getFullYear(), day.getMonth(), day.getDate() + 1);
  return [start, end];
}

/** How many whole hours the span covers: 23, 24 or 25. */
export function hoursInSpan(start: Date, end: Date): number {
  return Math.round((end.getTime() - start.getTime()) / HOUR_MS);
}

/**
 * The hour gridlines for one day, positioned as fractions of that day's span.
 *
 * The `hour` is read back from the instant rather than counted up from zero, so a spring-forward
 * day simply has no 02:00 line and an autumn day genuinely shows 02:00 twice. Both are the truth,
 * and both are what a person living that day experiences.
 */
export function hourMarks(start: Date, end: Date): { hour: number; fraction: number }[] {
  const hours = hoursInSpan(start, end);
  const marks: { hour: number; fraction: number }[] = [];
  for (let index = 1; index < hours; index += 1) {
    const at = new Date(start.getTime() + index * HOUR_MS);
    marks.push({ hour: at.getHours(), fraction: index / hours });
  }
  return marks;
}

/**
 * Where a block sits inside a day, or `null` when it does not touch that day at all.
 *
 * Clamped at both ends, so a meeting running past midnight draws to the bottom edge of one column
 * and from the top edge of the next instead of overflowing the grid.
 */
export function placeInDay(
  startsAt: Date,
  endsAt: Date,
  dayStart: Date,
  dayEnd: Date,
): Placement | null {
  const span = dayEnd.getTime() - dayStart.getTime();
  if (span <= 0) return null;

  const from = Math.max(startsAt.getTime(), dayStart.getTime());
  const to = Math.min(endsAt.getTime(), dayEnd.getTime());
  if (to <= from) return null;

  return {
    top: (from - dayStart.getTime()) / span,
    // A zero-length event would be invisible, and an event you cannot see is worse than one drawn
    // slightly too tall. One percent of the day is about fifteen minutes.
    height: Math.max((to - from) / span, 0.01),
  };
}

export interface Lane {
  /** Which column within the cluster, from 0. */
  lane: number;
  /** How many columns the cluster needs, so the caller can size them. */
  lanes: number;
}

/**
 * Assigns side-by-side columns to overlapping blocks.
 *
 * Blocks that do not overlap reuse the same column, so a day with a 09:00 and a 15:00 stays
 * full-width rather than being halved by a meeting hours away. The cluster — a run of blocks
 * connected by overlap — is what decides the column count, which is why the second pass exists:
 * every member of a cluster has to agree on the width, including the ones that got lane 0.
 */
export function overlapLanes<T>(
  items: T[],
  startOf: (item: T) => Date,
  endOf: (item: T) => Date,
): Map<T, Lane> {
  const ordered = [...items].sort((a, b) => startOf(a).getTime() - startOf(b).getTime());
  const placed = new Map<T, Lane>();

  let cluster: T[] = [];
  let clusterEnd = -Infinity;
  let laneEnds: number[] = [];

  const closeCluster = () => {
    for (const member of cluster) {
      const current = placed.get(member);
      if (current) placed.set(member, { ...current, lanes: laneEnds.length });
    }
    cluster = [];
    laneEnds = [];
    clusterEnd = -Infinity;
  };

  for (const item of ordered) {
    const start = startOf(item).getTime();
    const end = endOf(item).getTime();

    if (start >= clusterEnd && cluster.length > 0) closeCluster();

    let lane = laneEnds.findIndex((laneEnd) => laneEnd <= start);
    if (lane === -1) {
      lane = laneEnds.length;
      laneEnds.push(end);
    } else {
      laneEnds[lane] = end;
    }

    placed.set(item, { lane, lanes: laneEnds.length });
    cluster.push(item);
    clusterEnd = Math.max(clusterEnd, end);
  }
  if (cluster.length > 0) closeCluster();

  return placed;
}

/**
 * Six weeks of dates covering `anchor`'s month, Monday first.
 *
 * Always six rows, even when five would do. A grid that changes height as you page through the
 * year makes the whole view jump under the cursor, and the empty row costs less than that.
 */
export function monthMatrix(anchor: Date): Date[][] {
  const first = new Date(anchor.getFullYear(), anchor.getMonth(), 1);
  const offset = (first.getDay() + 6) % 7;
  const weeks: Date[][] = [];
  for (let week = 0; week < 6; week += 1) {
    const row: Date[] = [];
    for (let day = 0; day < 7; day += 1) {
      row.push(new Date(first.getFullYear(), first.getMonth(), 1 - offset + week * 7 + day));
    }
    weeks.push(row);
  }
  return weeks;
}

/** The seven days of `anchor`'s week, Monday first. */
export function weekOf(anchor: Date): Date[] {
  const offset = (anchor.getDay() + 6) % 7;
  return Array.from(
    { length: 7 },
    (_, index) =>
      new Date(anchor.getFullYear(), anchor.getMonth(), anchor.getDate() - offset + index),
  );
}

export function sameDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

/**
 * The fraction of `day` that has already passed, or `null` when `now` is not that day.
 *
 * Drives the "now" line. Returning `null` off-day is what keeps the line from being drawn seven
 * times across a week.
 */
export function nowFraction(now: Date, day: Date): number | null {
  if (!sameDay(now, day)) return null;
  const [start, end] = dayBounds(day);
  return (now.getTime() - start.getTime()) / (end.getTime() - start.getTime());
}

/**
 * A local wall-clock string the daemon accepts, built from a day and an hour.
 *
 * No offset and no `toISOString`, deliberately: the daemon stores the wall clock, and
 * `toISOString` would convert to UTC and land the event an hour away twice a year.
 */
export function localStamp(day: Date, hour: number, minute = 0): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return (
    `${day.getFullYear()}-${pad(day.getMonth() + 1)}-${pad(day.getDate())}` +
    `T${pad(hour)}:${pad(minute)}:00`
  );
}

/**
 * What `<input type="datetime-local">` produced, in the spelling the daemon parses.
 *
 * The two formats differ by exactly the seconds field: the control yields `2026-08-03T09:00` and
 * `calendar::LOCAL_FORMAT` is `%Y-%m-%dT%H:%M:%S`, which does not treat them as optional. A browser
 * that includes seconds (some do, once the step attribute allows them) is passed through unchanged.
 *
 * `null` for anything else, including the empty string a cleared control gives — the daemon answers
 * 400 for an unparseable stamp, and asking it to say so is a round trip to learn what the shape of
 * the string already said.
 */
export function stampFromInput(value: string): string | null {
  if (/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(value)) return `${value}:00`;
  if (/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}$/.test(value)) return value;
  return null;
}

/**
 * The inverse, for prefilling the control from a stamp the daemon gave us.
 *
 * Truncating rather than reformatting: the stamp is already local wall-clock text, and putting it
 * through a `Date` would apply this machine's offset to a string that never had one.
 */
export function inputFromStamp(stamp: string): string {
  return stamp.slice(0, 16);
}

/**
 * How long one occurrence runs, in whole minutes.
 *
 * Needed because moving an occurrence writes a whole exception row, and the daemon rejects one with
 * a non-positive duration — a moved occurrence with no length is not a shorter event but an
 * unreadable one. Derived from the occurrence in hand so that "move" means move and nothing else.
 *
 * Rounded rather than floored: a 30-minute block whose ends arrive a millisecond apart from the
 * clock should stay 30 minutes, not become 29.
 */
export function occurrenceMinutes(startsAt: string, endsAt: string): number {
  const minutes = Math.round(
    (new Date(endsAt).getTime() - new Date(startsAt).getTime()) / 60_000,
  );
  return Number.isFinite(minutes) ? minutes : 0;
}
