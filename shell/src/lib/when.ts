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
