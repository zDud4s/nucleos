/**
 * Which day a timestamp belongs to, read as a person would say it.
 *
 * Pure, and its own module rather than a helper inside a page, for the reason `turns.ts`
 * gives: a rule about SHAPE is worth asserting without mounting anything. `now` is always
 * passed in — a function that reads the clock itself is one whose tests are different every
 * day at midnight.
 */

/** The bands a conversation list is grouped into, newest first. */
export type Band = "today" | "yesterday" | "earlier" | "quiet";

/** What each band is called where it is drawn. */
export const BAND_TITLE: Record<Band, string> = {
  today: "Today",
  yesterday: "Yesterday",
  earlier: "Earlier",
  quiet: "Nothing said yet",
};

/**
 * How long a turn has been running, as a clock rather than a phrase.
 *
 * NOT `relativeText`, which says "2min ago" and is right for something that already happened.
 * A turn in flight is being waited ON, and what a person watching one wants is the reading a
 * stopwatch gives: it moves every second, and the seconds are the part that says it is alive.
 *
 * Never negative. A daemon whose clock is a second ahead of this window's would otherwise draw
 * a run that has not started yet, which is a bug report about a clock rather than a wait.
 */
export function elapsedText(started: number, now: number): string {
  const seconds = Math.max(0, Math.floor((now - started) / 1000));
  const pad = (value: number) => String(value).padStart(2, "0");
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}:${pad(seconds % 60)}`;
  return `${Math.floor(minutes / 60)}:${pad(minutes % 60)}:${pad(seconds % 60)}`;
}

/**
 * A length of time as a person would say it: "12s", "4m", "1h 05m", "2h".
 *
 * The compact reading for a span that is not being watched — how long is left, how long a run
 * took. NOT `elapsedText`, which is the stopwatch (`4:05`) for a thing in flight. The unit steps
 * with the size, and only the two largest are ever drawn: seconds under a minute, whole minutes
 * under an hour, then hours and zero-padded minutes (the pad keeps "1h 05m" from reading as 1h 5m
 * beside "1h 45m" in a column), with an exact hour left bare.
 *
 * `round` says which way a partial unit goes, because the two honest answers differ by use:
 * `floor` (the default) for time that has passed — a 119 s run took "1m", it has not taken two —
 * and `ceil` for time that is left, so a chip never reads "0m" while something still remains.
 * Never negative: a span a clock error made backwards reads as nought.
 */
export function formatSpan(ms: number, { round = "floor" }: { round?: "floor" | "ceil" } = {}): string {
  const whole = round === "ceil" ? Math.ceil : Math.floor;
  const seconds = Math.max(0, whole(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = whole(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours}h` : `${hours}h ${String(rest).padStart(2, "0")}m`;
}

/**
 * The time of day on a 24-hour clock, "07:05", in the reader's own zone.
 *
 * Hand-built from the local hours and minutes rather than `toLocaleTimeString`, which answers
 * "7:05 AM" or "07.05" depending on the machine's locale — and a column of times that changes
 * shape between two people's windows is not one a test can pin.
 */
export function clockText(ms: number): string {
  const at = new Date(ms);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/** Local midnight before `ms`. Local, because a person's "yesterday" is their own. */
function midnight(ms: number): number {
  const day = new Date(ms);
  day.setHours(0, 0, 0, 0);
  return day.getTime();
}

const A_DAY = 86_400_000;

/**
 * The band a timestamp falls in.
 *
 * Calendar days apart, never hours: something said at 23:50 and read at 00:10 is "yesterday",
 * and an elapsed-hours rule would call it twenty minutes ago and file it under today.
 *
 * A timestamp in the future reads as today. Clocks disagree — the daemon's and this window's
 * most of all, on a machine that just woke up — and a row filed under a band called "later"
 * would be a bug report about a clock rather than a conversation anybody could use.
 *
 * An unparseable timestamp falls to `earlier` rather than throwing: the row still exists and
 * still has to be somewhere, and the bottom of the list is where a row nothing is known about
 * belongs.
 */
export function bandOf(at: string | null, now: number): Band {
  if (at === null) return "quiet";
  const when = Date.parse(at);
  if (Number.isNaN(when)) return "earlier";
  const days = Math.round((midnight(now) - midnight(when)) / A_DAY);
  if (days <= 0) return "today";
  if (days === 1) return "yesterday";
  return "earlier";
}

/**
 * Rows in the order they arrived, cut into bands.
 *
 * Order is preserved exactly — this only inserts the cuts. It never sorts, because the caller
 * already did, and a second opinion about order here is how a list comes to disagree with the
 * one thing that decides it.
 *
 * A band with nothing in it is not emitted, so a day with no conversations leaves no heading
 * standing over an empty stretch.
 */
export function inBands<T>(
  rows: T[],
  at: (row: T) => string | null,
  now: number,
): { band: Band; rows: T[] }[] {
  const bands: { band: Band; rows: T[] }[] = [];
  for (const row of rows) {
    const band = bandOf(at(row), now);
    const last = bands[bands.length - 1];
    if (last !== undefined && last.band === band) last.rows.push(row);
    else bands.push({ band, rows: [row] });
  }
  return bands;
}
