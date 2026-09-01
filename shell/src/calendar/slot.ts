// §spec calendario-local
import type { EventOccurrence } from "../data/calendar";
import { clockOfInstant, clockOfStamp, localDateOfStamp, localStamp } from "../lib/calendar-grid";

/**
 * What the grid and the day sheet agree a *selection* is.
 *
 * The month grid and the week grid are two ways of pointing at the same thing,
 * and the sheet below them reads whichever one is on screen — so the pointer
 * has to be a shape neither view owns. Kept here rather than in either grid
 * because a type that lives in one of two peers is the one the other imports
 * badly.
 *
 * **The day is the selection; the hour is an optional refinement.** A month
 * cell can only ever name a day, and a week slot names a day and an hour. The
 * sheet is built to render the first and to prefer the second, which is what
 * lets one component serve both views instead of two that drift.
 */
export interface Slot {
  /** The local day, as a real date rather than a key — the sheet needs its span. */
  day: Date;
  /**
   * The hour of the clicked slot, 0–23, or `null` for a whole-day selection.
   *
   * `null` is not "midnight": it is *no hour was named*, and the draft form
   * treats the two differently — a day picked from the month opens at the
   * start of the working day, and a slot picked from the week opens at the
   * slot.
   */
  hour: number | null;
}

/**
 * A day's own `"YYYY-MM-DD"`, built from the same local getters `monthMatrix`
 * used to construct the day — never re-parsed through a `Date`, so a grid
 * cell's key can never disagree with the box that was placed into it.
 */
export function dateKeyOf(day: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${day.getFullYear()}-${pad(day.getMonth() + 1)}-${pad(day.getDate())}`;
}

/** Where one occurrence draws, and whether it is sitting where it was written. */
export interface OccurrencePlacement {
  /** The local day it happens on — the cell it belongs in. */
  day: Date;
  /** The instant it starts, for the week view's geometry. */
  startsAt: Date;
  /** The instant it ends. */
  endsAt: Date;
  /** `"HH:MM"`, local. */
  clock: string;
  /** Whether an exception has moved it away from its original local start. */
  moved: boolean;
}

/**
 * Where an occurrence is drawn — which is **not** what identifies it.
 *
 * The wire carries two different times for two different jobs, and conflating
 * them is a real defect this page shipped with: `occurrence_local` is the
 * ORIGINAL local start and is the identity `cancel`/`move` address by, while
 * `starts_at`/`ends_at` are the instants the occurrence CURRENTLY resolves to.
 * `recurrence.rs`'s `expand` decides whether an occurrence falls in the
 * requested window using the resolved instants, and then reports the original
 * local start beside them — so an occurrence moved from the 4th to the 20th
 * comes back in the 20th's window carrying `"...-04T09:00:00"`. Grouping the
 * month by that string drew it on the 4th: the daemon returned it because it
 * is on the 20th, and the grid put it on the day it had been moved off.
 *
 * So the day comes from the instant. The stamp is still preferred for the
 * clock when the two agree, because it is exact text with no zone in it —
 * which is the whole reason `occurrence_local` is on the wire — and the
 * comparison that decides is also the only way to know a move happened at all:
 * the payload carries no flag for it. That fact is worth surfacing rather than
 * swallowing, and `MonthGrid` and `DaySheet` both mark a moved occurrence.
 */
export function placementOf(occurrence: EventOccurrence): OccurrencePlacement {
  const startsAt = new Date(occurrence.starts_at);
  const endsAt = new Date(occurrence.ends_at);
  const asWritten = localDateOfStamp(occurrence.occurrence_local);
  const moved = asWritten === null || asWritten.getTime() !== startsAt.getTime();
  return {
    day: startsAt,
    startsAt,
    endsAt,
    clock: moved ? clockOfInstant(startsAt) : clockOfStamp(occurrence.occurrence_local),
    moved,
  };
}

/**
 * Occurrences grouped by the local day they actually happen on.
 *
 * Keyed off {@link placementOf}, never off `occurrence_local` — see there for
 * why the two differ and which one a grid is asking about.
 *
 * Sorted within each day, because the daemon's order is the daemon's and a
 * column of three chips in arrival order is unreadable. See
 * {@link compareOccurrences} for what "sorted" means and why.
 */
export function groupByLocalDay(occurrences: EventOccurrence[]): Map<string, EventOccurrence[]> {
  const map = new Map<string, EventOccurrence[]>();
  for (const occurrence of occurrences) {
    const key = dateKeyOf(placementOf(occurrence).day);
    const list = map.get(key);
    if (list === undefined) map.set(key, [occurrence]);
    else list.push(occurrence);
  }
  for (const list of map.values()) list.sort(compareOccurrences);
  return map;
}

/**
 * Earliest first, then by title, then by series.
 *
 * The tie-breaks are not decoration: two occurrences at 09:00 are ordinary —
 * that is what the overlap lanes exist for — and a comparator that returned 0
 * for them would leave their order to the daemon's, which is to say it would
 * change between polls and make the chips shuffle under the cursor every
 * thirty seconds.
 *
 * Compared on the resolved instant and not on `occurrence_local`, for the same
 * reason the grouping is: a moved occurrence sorted by the stamp it was
 * written at lands among the events of an hour it no longer occupies.
 */
export function compareOccurrences(a: EventOccurrence, b: EventOccurrence): number {
  const left = Date.parse(a.starts_at);
  const right = Date.parse(b.starts_at);
  if (left !== right) return left - right;
  if (a.title !== b.title) return a.title < b.title ? -1 : 1;
  return a.event_id - b.event_id;
}

/**
 * The local stamp a draft opened from this slot should start at.
 *
 * The whole reason `localStamp` exists, and it had no caller until the form
 * moved next to the grid: a day and an hour become a wall-clock string with no
 * offset, which is what the daemon stores. A slot with no hour falls back to
 * the hour given — the start of the working day, which the page reads from the
 * config rather than assuming 09:00 here.
 */
export function slotStamp(slot: Slot, fallbackHour: number): string {
  return localStamp(slot.day, slot.hour ?? fallbackHour);
}
