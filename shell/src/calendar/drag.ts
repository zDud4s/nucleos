// §spec calendario-local
import type { EventOccurrence } from "../data/calendar";
import { localStamp, occurrenceMinutes } from "../lib/calendar-grid";
import { placementOf } from "./slot";

/**
 * Where a dragged occurrence lands, as arithmetic with no DOM in it.
 *
 * Kept apart from both grids for the reason the rest of this pillar is: the
 * hard part of dragging is not the pointer, it is deciding what "here" means
 * on a day that might be 23 hours long — and that is a table test. The two
 * grids differ only in how much they can say about the drop (a month cell
 * names a day, a week slot names a day and an hour), and both funnel into the
 * one function below.
 *
 * **A translation, not a snap.** Dropping a 10:15 meeting on the 14:00 band
 * makes it 14:15, not 14:00. Dragging is how a person expresses *later* or
 * *another day*, and a grid that quietly rounded to the hour would turn a
 * one-hour drag into a forty-five minute move — a change nobody asked for,
 * silently, every time. The `datetime-local` in `DaySheet` is still the way to
 * say an exact time, and it is the only way for anyone not using a mouse.
 */

/** What a drop asks the daemon to do, or `null` when it asks for nothing. */
export interface Move {
  /** The NEW local datetime, `"YYYY-MM-DDTHH:MM:SS"`. */
  toLocal: string;
  /** Unchanged: a move moves, and does not resize. */
  durationMinutes: number;
}

/**
 * The move a drop means, or `null` when the drop changes nothing.
 *
 * `hour` is `null` for a month cell, which can only name a day — the
 * occurrence keeps the time it already had and only its date changes.
 *
 * **The identity is not here on purpose.** A caller sends
 * `occurrence.occurrence_local` alongside this, never `toLocal`: the original
 * local start is what an exception row is keyed by, and addressing the second
 * move by where the occurrence now sits forges a duplicate instead of
 * relocating the first. Returning only the destination makes it impossible for
 * this module to be the place that gets that wrong.
 */
export function moveFromDrop(
  occurrence: EventOccurrence,
  day: Date,
  hour: number | null,
): Move | null {
  const at = placementOf(occurrence).startsAt;
  const toLocal = localStamp(day, hour ?? at.getHours(), at.getMinutes());

  /*
    A drop that changes nothing sends nothing. Picking a block up and putting
    it back is the commonest gesture in any calendar — and here it would
    otherwise write an exception row saying "this occurrence is exactly where
    the series already puts it", which is a real row, in the database, for
    every twitch of the hand.
  */
  if (toLocal === localStamp(at, at.getHours(), at.getMinutes())) return null;

  return {
    toLocal,
    durationMinutes: occurrenceMinutes(occurrence.starts_at, occurrence.ends_at),
  };
}
